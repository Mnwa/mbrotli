#![cfg(all(feature = "decompression", feature = "experimental"))]
//! `FramedDecoderSessionOwned` against the borrowed `FramedDecoderSession`.
//!
//! Each call's result, including any lent event, is recorded as its debug
//! text while the borrow is alive; the two facades must produce the same
//! record for the same input splits and output widths.

use mbrotli::framing::*;
use mbrotli::{DecodeOperation, OutputSize};
use std::sync::Arc;

fn var(mut n: u64) -> Vec<u8> {
    let mut b = Vec::new();
    loop {
        let low = (n & 127) as u8;
        n >>= 7;
        b.push(low | if n == 0 { 0 } else { 128 });
        if n == 0 {
            return b;
        }
    }
}
fn chunk(header: &[u8], content: &[u8]) -> Vec<u8> {
    let mut b = var((header.len() + content.len()) as u64);
    b.extend_from_slice(header);
    b.extend_from_slice(content);
    b
}
/// A container with a footer after `chunks`.
fn full(chunks: &[Vec<u8>]) -> Vec<u8> {
    let mut b = vec![0x91, 10, 66, 82, 4];
    for c in chunks {
        b.extend(c);
    }
    b.extend(chunk(&[10], &[0, 0]));
    b
}
fn hex(text: &str) -> Vec<u8> {
    let text = text.trim();
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("hex"))
        .collect()
}

/// Metadata, padding and resources from the checked-in writer fixtures, plus
/// hand-built containers.
fn corpus() -> Vec<(&'static str, Vec<u8>)> {
    let mut corpus = vec![
        ("one resource", full(&[chunk(&[2, 0, 0], b"abc")])),
        (
            "two resources",
            full(&[chunk(&[2, 0, 0], b"abc"), chunk(&[2, 0, 0], b"xyz")]),
        ),
        (
            "metadata then resources",
            full(&[
                chunk(&[7, 0], b"AA\x00"),
                chunk(&[2, 0, 0], b"abc"),
                chunk(&[2, 0, 0], b"xyz"),
            ]),
        ),
    ];
    for (name, text) in [
        (
            "all chunk types",
            include_str!(
                "../testdata/framing-legacy/all_chunk_types_have_rfc_headers_and_the_compressed_resource_interoperates.hex"
            ),
        ),
        (
            "compressed metadata",
            include_str!(
                "../testdata/framing-legacy/compressed_metadata_and_selected_repeats_decode_independently.hex"
            ),
        ),
        (
            "repeated fields",
            include_str!(
                "../testdata/framing-legacy/repeated_field_selection_validates_codes_and_empty_selection.hex"
            ),
        ),
        (
            "dictionary references",
            include_str!(
                "../testdata/framing-legacy/dictionary_references_and_explicit_ids_have_their_rfc_wire_forms.hex"
            ),
        ),
    ] {
        corpus.push((name, hex(text)));
    }
    corpus
}

/// The calls both facades answer, recorded rather than returned so the lent
/// event never outlives its call.
trait Facade {
    fn record(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        operation: DecodeOperation,
    ) -> (String, Option<(usize, bool)>);
    fn counters(&self) -> String;
    /// `flush` (for `Process`) or `finish` (for `Finish`), recorded like `record`.
    fn record_shorthand(
        &mut self,
        output: &mut [u8],
        operation: DecodeOperation,
    ) -> (String, Option<(usize, bool)>);
}

fn describe(
    result: Result<FramedDecodeProgress<'_>, FramedDecodeFailure>,
) -> (String, Option<(usize, bool)>) {
    let text = format!("{result:?}");
    let next = result.ok().map(|p| {
        (
            p.consumed,
            matches!(p.status, FramedDecoderStatus::Finished),
        )
    });
    (text, next)
}

macro_rules! facade {
    ($type:ty) => {
        impl Facade for $type {
            fn record(
                &mut self,
                input: &[u8],
                output: &mut [u8],
                operation: DecodeOperation,
            ) -> (String, Option<(usize, bool)>) {
                describe(self.process(input, output, operation))
            }
            fn record_shorthand(
                &mut self,
                output: &mut [u8],
                operation: DecodeOperation,
            ) -> (String, Option<(usize, bool)>) {
                describe(match operation {
                    DecodeOperation::Process => self.flush(output),
                    DecodeOperation::Finish => self.finish(output),
                })
            }
            fn counters(&self) -> String {
                format!(
                    "{} {} {} {} {} {:?}",
                    self.total_in(),
                    self.total_out(),
                    self.total_decoded(),
                    self.resources_decoded(),
                    self.is_finished(),
                    self.input_format()
                )
            }
        }
    };
}
facade!(FramedDecoderSession<'_, '_>);
impl<R: DictionaryResolver + 'static> Facade for FramedDecoderSessionOwned<R> {
    fn record(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        operation: DecodeOperation,
    ) -> (String, Option<(usize, bool)>) {
        describe(self.process(input, output, operation))
    }
    fn record_shorthand(
        &mut self,
        output: &mut [u8],
        operation: DecodeOperation,
    ) -> (String, Option<(usize, bool)>) {
        describe(match operation {
            DecodeOperation::Process => self.flush(output),
            DecodeOperation::Finish => self.finish(output),
        })
    }
    fn counters(&self) -> String {
        format!(
            "{} {} {} {} {} {:?}",
            self.total_in(),
            self.total_out(),
            self.total_decoded(),
            self.resources_decoded(),
            self.is_finished(),
            self.input_format()
        )
    }
}

/// Feeds `input` in `split`-byte pieces through output buffers cycling over
/// `widths`, declaring EOF on the last piece; stops at the first failure.
fn drive(session: &mut impl Facade, input: &[u8], split: usize, widths: &[usize]) -> Vec<String> {
    let mut log = Vec::new();
    let mut offset = 0usize;
    for turn in 0.. {
        let end = input.len().min(offset.saturating_add(split));
        let operation = if end == input.len() {
            DecodeOperation::Finish
        } else {
            DecodeOperation::Process
        };
        let mut output = vec![0; widths[turn % widths.len()]];
        let (text, next) = session.record(&input[offset..end], &mut output, operation);
        log.push(text);
        log.push(session.counters());
        let Some((consumed, finished)) = next else {
            break;
        };
        offset += consumed;
        if finished {
            // Finished is idempotent.
            let (text, _) = session.record(&[], &mut [0; 4], DecodeOperation::Finish);
            log.push(text);
            break;
        }
        assert!(turn < 1 << 20, "no progress");
    }
    log
}

const SCHEDULES: [(usize, &[usize]); 4] = [
    (1, &[0, 1, 2]),
    (3, &[1]),
    (17, &[2, 0, 64]),
    (usize::MAX, &[4096]),
];

fn decoder(config: FramedDecodeConfig) -> FramedDecompressor {
    FramedDecompressor::new(config).expect("config")
}

/// Runs one schedule through a borrowed and an owned session and requires
/// identical records, returning the owned one.
fn parity(
    config: FramedDecodeConfig,
    stream: FramedDecodeStreamConfig,
    input: &[u8],
    split: usize,
    widths: &[usize],
) -> Vec<String> {
    let mut owner = decoder(config);
    let expected = drive(
        &mut owner.start(stream).expect("borrowed start"),
        input,
        split,
        widths,
    );
    let mut session = decoder(config).into_session(stream).expect("owned start");
    let actual = drive(&mut session, input, split, widths);
    assert_eq!(actual, expected, "split {split}, widths {widths:?}");
    actual
}

fn finished(log: &[String]) -> bool {
    log.iter().any(|line| line.contains("status: Finished"))
}

#[test]
fn events_and_payloads_are_identical_through_both_facades() {
    for (name, bytes) in corpus() {
        for (split, widths) in SCHEDULES {
            let log = parity(
                Default::default(),
                Default::default(),
                &bytes,
                split,
                widths,
            );
            if !name.starts_with("dictionary") {
                assert!(finished(&log), "{name}: {log:?}");
            }
        }
    }
}

#[test]
fn every_truncated_prefix_fails_identically_through_both_facades() {
    for (name, bytes) in corpus().into_iter().take(4) {
        for cut in 0..bytes.len() {
            for (split, widths) in [SCHEDULES[0], SCHEDULES[3]] {
                let log = parity(
                    Default::default(),
                    Default::default(),
                    &bytes[..cut],
                    split,
                    widths,
                );
                assert!(!finished(&log), "{name} cut at {cut}");
            }
        }
    }
}

/// Starts a `Finish`, then shortens the declared final suffix.
fn shorten_the_final_suffix(session: &mut impl Facade, input: &[u8]) -> Vec<String> {
    let (first, next) = session.record(input, &mut [0; 1], DecodeOperation::Finish);
    let (consumed, _) = next.expect("the first call decodes");
    let rest = &input[consumed..];
    let (second, _) = session.record(
        &rest[..rest.len() - 1],
        &mut [0; 1],
        DecodeOperation::Finish,
    );
    let (third, _) = session.record(&[], &mut [0; 1], DecodeOperation::Finish);
    vec![first, second, third]
}

#[test]
fn the_final_suffix_contract_is_identical_through_both_facades() {
    let bytes = full(&[chunk(&[2, 0, 0], b"abcdef")]);
    let mut owner = decoder(Default::default());
    let expected =
        shorten_the_final_suffix(&mut owner.start(Default::default()).expect("start"), &bytes);
    let actual = shorten_the_final_suffix(
        &mut decoder(Default::default())
            .into_session(Default::default())
            .expect("start"),
        &bytes,
    );
    assert_eq!(actual, expected);
    assert!(actual[1].contains("InvalidState"), "{actual:?}");
    assert!(actual[2].contains("InvalidState"), "{actual:?}");
}

#[test]
fn auto_input_decodes_raw_brotli_identically() {
    let auto = FramedDecodeConfig::default().with_input_mode(InputMode::Auto);
    let hello = [0x0b, 0x02, 0x80, b'h', b'e', b'l', b'l', b'o', 0x03];
    for input in [&[0x3b][..], &hello[..], &full(&[chunk(&[2, 0, 0], b"abc")])] {
        for (split, widths) in SCHEDULES {
            let log = parity(auto, Default::default(), input, split, widths);
            assert!(finished(&log), "{log:?}");
        }
    }
}

struct Resolver {
    bytes: Vec<u8>,
}
impl DictionaryResolver for Resolver {
    fn resolve(&self, request: ExternalDictionaryRequest) -> Option<&[u8]> {
        (request.id == DictionaryId([7; 32])).then_some(&self.bytes)
    }
}
fn external_header(flags: u8) -> Vec<u8> {
    let mut h = vec![2, 3, 0, 1, flags, 3];
    h.extend([7; 32]);
    h.push(0);
    h
}

fn with_dictionaries<R: DictionaryResolver + 'static>(resolver: R, input: &[u8]) -> Vec<String> {
    let mut session = decoder(Default::default())
        .into_session_with_dictionaries(resolver, Default::default())
        .expect("start");
    let log = drive(&mut session, input, 5, &[1, 0, 3]);
    let mut returned = session.into_framed_decompressor();
    assert!(
        returned
            .decompress(&full(&[chunk(&[2, 0, 0], b"x")]))
            .is_ok()
    );
    log
}

#[test]
fn owned_resolvers_decode_external_references_like_borrowed_ones() {
    let resolver = Resolver {
        bytes: vec![0x91, 0],
    };
    for input in [
        full(&[chunk(&external_header(2), &[0x3b])]),
        // An invalid attachment reaches the same dictionary error.
        full(&[chunk(&external_header(6), &[0x3b])]),
    ] {
        let mut owner = decoder(Default::default());
        let expected = drive(
            &mut owner
                .start_with_dictionaries(&resolver, Default::default())
                .expect("borrowed"),
            &input,
            5,
            &[1, 0, 3],
        );
        let leaked: &'static Resolver = Box::leak(Box::new(Resolver {
            bytes: resolver.bytes.clone(),
        }));
        let shared = Arc::new(Resolver {
            bytes: resolver.bytes.clone(),
        });
        assert_eq!(with_dictionaries(Arc::clone(&shared), &input), expected);
        assert_eq!(Arc::strong_count(&shared), 1);
        assert_eq!(
            with_dictionaries(
                Resolver {
                    bytes: resolver.bytes.clone()
                },
                &input
            ),
            expected
        );
        assert_eq!(with_dictionaries(leaked, &input), expected);
        let boxed: Box<dyn DictionaryResolver> = Box::new(Resolver {
            bytes: resolver.bytes.clone(),
        });
        assert_eq!(with_dictionaries(boxed, &input), expected);
    }

    // Without a resolver, both facades report the same missing dictionary.
    let input = full(&[chunk(&external_header(2), &[0x3b])]);
    let log = parity(Default::default(), Default::default(), &input, 5, &[1]);
    assert!(log.iter().any(|line| line.contains("MissingDictionary")));
    // A resolver that resolves nothing is not the same as no resolver.
    let log = with_dictionaries(NoDictionaries, &input);
    assert!(!finished(&log));
}

#[test]
fn limits_fail_identically_through_both_facades() {
    let bytes = full(&[
        chunk(&[7, 0], b"AA\x00"),
        chunk(&[2, 0, 0], b"abc"),
        chunk(&[2, 0, 0], b"xyz"),
    ]);
    type LimitSetter = fn(FramedDecodeLimits, Option<u64>) -> FramedDecodeLimits;
    let cases: &[(LimitSetter, u64)] = &[
        (FramedDecodeLimits::with_max_input_bytes, bytes.len() as u64),
        (FramedDecodeLimits::with_max_output_bytes, 6),
        (FramedDecodeLimits::with_max_decoded_bytes, 9),
        (FramedDecodeLimits::with_max_resource_bytes, 3),
        (FramedDecodeLimits::with_max_metadata_bytes, 3),
        (FramedDecodeLimits::with_max_metadata_fields, 1),
        (FramedDecodeLimits::with_max_resources, 2),
        (FramedDecodeLimits::with_max_chunks, 4),
    ];
    for &(set, threshold) in cases {
        for limit in [Some(0), Some(threshold - 1), Some(threshold)] {
            let config = FramedDecodeConfig::default().with_limits(set(Default::default(), limit));
            for (split, widths) in [SCHEDULES[0], SCHEDULES[3]] {
                let log = parity(config, Default::default(), &bytes, split, widths);
                assert_eq!(
                    finished(&log),
                    limit == Some(threshold),
                    "{threshold} {limit:?}"
                );
            }
        }
    }

    // An exact size beyond the output budget is refused at start.
    let config = FramedDecodeConfig::default()
        .with_limits(FramedDecodeLimits::default().with_max_output_bytes(Some(5)));
    let stream = FramedDecodeStreamConfig::from(OutputSize::Exact(6));
    let borrowed = format!("{:?}", decoder(config).start(stream).map(|_| ()));
    let owned = format!("{:?}", decoder(config).into_session(stream).map(|_| ()));
    assert_eq!(owned, borrowed);
    assert!(owned.starts_with("Err"));

    // And a mismatched exact size fails the same way.
    let log = parity(
        Default::default(),
        OutputSize::Exact(5).into(),
        &bytes,
        4,
        &[9],
    );
    assert!(!finished(&log));
}

#[test]
fn a_returned_framed_decompressor_is_reusable_after_every_ending() {
    let bytes = full(&[chunk(&[2, 0, 0], b"abc"), chunk(&[2, 0, 0], b"xyz")]);
    let reuse = |decoder: FramedDecompressor| {
        let mut session = decoder.into_session(Default::default()).expect("restart");
        assert!(finished(&drive(&mut session, &bytes, 4, &[3])));
        let mut decoder = session.into_framed_decompressor();
        assert_eq!(
            decoder.decompress(&bytes).expect("decode").resources.len(),
            2
        );
        let session = decoder.start(Default::default()).expect("borrowed start");
        drop(session);
        decoder
    };

    // Finished.
    let mut session = decoder(Default::default())
        .into_session(Default::default())
        .expect("start");
    assert!(finished(&drive(&mut session, &bytes, 100, &[100])));
    let d = reuse(session.into_framed_decompressor());

    // Waiting for input.
    let mut session = d.into_session(Default::default()).expect("start");
    let mut output = [0; 16];
    let p = session
        .process(&bytes[..3], &mut output, DecodeOperation::Process)
        .expect("partial");
    assert!(matches!(p.status, FramedDecoderStatus::NeedsInput));
    let d = reuse(session.into_framed_decompressor());

    // Waiting for output.
    let mut session = d.into_session(Default::default()).expect("start");
    let mut offset = 0;
    loop {
        let p = session
            .process(&bytes[offset..], &mut [], DecodeOperation::Process)
            .expect("zero-width");
        offset += p.consumed;
        if matches!(p.status, FramedDecoderStatus::NeedsOutput) {
            break;
        }
    }
    let d = reuse(session.into_framed_decompressor());

    // Just after a semantic event.
    let mut session = d.into_session(Default::default()).expect("start");
    let p = session
        .process(&bytes, &mut output, DecodeOperation::Process)
        .expect("event");
    assert!(matches!(p.status, FramedDecoderStatus::Event(_)));
    let d = reuse(session.into_framed_decompressor());

    // Failed.
    let mut session = d.into_session(Default::default()).expect("start");
    assert!(
        session
            .process(&[0x91, 10, 66, 0xff], &mut output, DecodeOperation::Finish)
            .is_err()
    );
    let d = reuse(session.into_framed_decompressor());

    // A leaked borrowed session still blocks the owned start.
    let mut d = d;
    std::mem::forget(d.start(Default::default()).expect("start"));
    assert!(matches!(
        d.into_session(Default::default()),
        Err(FramedDecodeError::AbandonedSession)
    ));
}

#[test]
fn owned_framed_sessions_move_between_threads() {
    const fn assert_send<T: Send>() {}
    assert_send::<FramedDecoderSessionOwned>();
    assert_send::<FramedDecoderSessionOwned<Arc<Resolver>>>();
    assert_send::<FramedDecoderSessionOwned<&'static Resolver>>();

    let bytes = full(&[chunk(&[2, 0, 0], b"threaded")]);
    let mut session = decoder(Default::default())
        .into_session(Default::default())
        .expect("start");
    let mut output = [0; 32];
    let p = session
        .process(&bytes[..4], &mut output, DecodeOperation::Process)
        .expect("signature");
    let offset = p.consumed;
    let rest = bytes[offset..].to_vec();
    let (log, decoder) = std::thread::spawn(move || {
        let log = drive(&mut session, &rest, usize::MAX, &[32]);
        (log, session.into_framed_decompressor())
    })
    .join()
    .expect("the worker finished");
    assert!(finished(&log));
    assert!(
        log.iter()
            .any(|line| line.contains("116, 104, 114, 101, 97, 100, 101, 100"))
    ); // "threaded"
    let mut decoder = decoder;
    assert!(decoder.decompress(&bytes).is_ok());
    assert!(
        format!("{:?}", decoder.into_session(Default::default()))
            .contains("FramedDecoderSessionOwned")
    );
}

fn fresh_borrowed(
    config: FramedDecodeConfig,
    stream: FramedDecodeStreamConfig,
    input: &[u8],
    split: usize,
    widths: &[usize],
) -> Vec<String> {
    let mut owner = decoder(config);
    drive(
        &mut owner.start(stream).expect("start"),
        input,
        split,
        widths,
    )
}

/// Leaves `session` in one of the states `reinit` must recover from.
fn end_first_object<R: DictionaryResolver + 'static>(
    session: &mut FramedDecoderSessionOwned<R>,
    ending: &str,
) {
    let first = full(&[chunk(&[2, 0, 0], b"first"), chunk(&[2, 0, 0], b"object")]);
    let mut output = [0; 64];
    let result = match ending {
        "finished" => {
            assert!(finished(&drive(&mut *session, &first, 1000, &[64])));
            return;
        }
        "needs input" => session
            .process(&first[..3], &mut output, DecodeOperation::Process)
            .map(|p| format!("{:?}", p.status)),
        "needs output" => {
            let mut offset = 0;
            loop {
                let p = session
                    .process(&first[offset..], &mut [], DecodeOperation::Process)
                    .expect("zero width");
                offset += p.consumed;
                if matches!(p.status, FramedDecoderStatus::NeedsOutput) {
                    break Ok("NeedsOutput".to_owned());
                }
            }
        }
        "event" => session
            .process(&first, &mut output, DecodeOperation::Process)
            .map(|p| format!("{:?}", p.status)),
        "failed" => session
            .process(&[0x91, 10, 66, 0xff], &mut output, DecodeOperation::Finish)
            .map(|p| format!("{:?}", p.status)),
        _ => unreachable!(),
    };
    match (ending, result) {
        ("needs input", Ok(status)) => assert_eq!(status, "NeedsInput"),
        ("event", Ok(status)) => assert!(status.starts_with("Event"), "{status}"),
        ("failed", Err(_)) => {}
        ("needs output", Ok(status)) => assert_eq!(status, "NeedsOutput"),
        (ending, other) => panic!("{ending}: {other:?}"),
    }
}

#[test]
fn reinit_starts_an_object_identical_to_a_fresh_borrowed_one() {
    let second = full(&[
        chunk(&[7, 0], b"AA\x00"),
        chunk(&[2, 0, 0], b"abc"),
        chunk(&[2, 0, 0], b"xyz"),
    ]);
    for ending in ["finished", "needs input", "needs output", "event", "failed"] {
        for stream in [
            FramedDecodeStreamConfig::default(),
            OutputSize::Exact(6).into(),
        ] {
            let mut session = decoder(Default::default())
                .into_session(Default::default())
                .expect("start");
            end_first_object(&mut session, ending);
            session.reinit(stream).expect("reinit");
            let mut fresh_owner = decoder(Default::default());
            let fresh = fresh_owner.start(stream).expect("start").counters();
            assert_eq!(session.counters(), fresh, "{ending}");
            for (split, widths) in SCHEDULES {
                let expected = fresh_borrowed(Default::default(), stream, &second, split, widths);
                let actual = drive(&mut session, &second, split, widths);
                assert_eq!(actual, expected, "{ending}, split {split}");
                assert!(finished(&actual));
                session.reinit(stream).expect("reinit again");
            }
        }
    }
}

#[test]
fn a_rejected_reinit_leaves_a_failed_object_that_a_later_reinit_recovers() {
    let bytes = full(&[chunk(&[2, 0, 0], b"abc")]);
    let config = FramedDecodeConfig::default()
        .with_limits(FramedDecodeLimits::default().with_max_output_bytes(Some(5)));
    let mut session = decoder(config)
        .into_session(Default::default())
        .expect("start");
    let rejected = session.reinit(OutputSize::Exact(6).into());
    let borrowed = decoder(config)
        .start(OutputSize::Exact(6).into())
        .map(|_| ());
    assert_eq!(format!("{rejected:?}"), format!("{borrowed:?}"));
    assert!(rejected.is_err());
    let failure = session
        .process(&bytes, &mut [0; 16], DecodeOperation::Finish)
        .unwrap_err();
    assert!(matches!(failure.error, FramedDecodeError::InvalidState));
    assert_eq!((failure.consumed, failure.produced), (0, 0));

    session.reinit(Default::default()).expect("reinit");
    assert_eq!(
        drive(&mut session, &bytes, 3, &[2]),
        fresh_borrowed(config, Default::default(), &bytes, 3, &[2])
    );
    session
        .reinit(OutputSize::Exact(6).into())
        .expect_err("rejected");
    let mut decoder = session.into_framed_decompressor();
    assert!(decoder.decompress(&bytes).is_ok());
}

#[test]
fn reinit_keeps_the_resolver_of_the_session() {
    let input = full(&[chunk(&external_header(2), &[0x3b])]);
    let resolver = Arc::new(Resolver {
        bytes: vec![0x91, 0],
    });
    let mut owner = decoder(Default::default());
    let expected = drive(
        &mut owner
            .start_with_dictionaries(&*resolver, Default::default())
            .expect("borrowed"),
        &input,
        5,
        &[1, 0, 3],
    );
    assert!(finished(&expected));
    let mut session = decoder(Default::default())
        .into_session_with_dictionaries(Arc::clone(&resolver), Default::default())
        .expect("owned");
    end_first_object(&mut session, "failed");
    session.reinit(Default::default()).expect("reinit");
    assert_eq!(drive(&mut session, &input, 5, &[1, 0, 3]), expected);
}

/// Accepts all of `input` with `Process`, then drains with the non-EOF call
/// and ends with the EOF call, through the shorthands or through `process`.
fn with_shorthands(session: &mut impl Facade, input: &[u8], shorthand: bool) -> Vec<String> {
    let mut log = Vec::new();
    let mut remaining = input;
    while !remaining.is_empty() {
        let (text, next) = session.record(remaining, &mut [0; 2], DecodeOperation::Process);
        log.push(text);
        let (consumed, _) = next.expect("the object decodes");
        remaining = &remaining[consumed..];
    }
    for operation in [DecodeOperation::Process, DecodeOperation::Finish] {
        loop {
            let mut output = [0; 2];
            let (text, next) = if shorthand {
                session.record_shorthand(&mut output, operation)
            } else {
                session.record(&[], &mut output, operation)
            };
            let stop = text.contains("NeedsInput") || text.contains("status: Finished");
            log.push(text);
            log.push(session.counters());
            let (_, finished) = next.expect("the object decodes");
            if finished || (stop && operation == DecodeOperation::Process) {
                break;
            }
        }
    }
    log
}

#[test]
fn flush_and_finish_are_process_without_input_in_both_session_shapes() {
    let footerless = {
        let mut b = vec![0x91, 10, 66, 82, 0];
        b.extend(chunk(&[2, 0, 0], b"footerless"));
        b
    };
    let mut inputs = vec![footerless];
    inputs.extend(
        corpus()
            .into_iter()
            .filter(|(name, _)| !name.starts_with("dictionary"))
            .map(|(_, bytes)| bytes),
    );
    for input in inputs {
        let mut owner = decoder(Default::default());
        let expected = with_shorthands(
            &mut owner.start(Default::default()).expect("start"),
            &input,
            false,
        );
        let borrowed = with_shorthands(
            &mut owner.start(Default::default()).expect("start"),
            &input,
            true,
        );
        let owned = with_shorthands(
            &mut decoder(Default::default())
                .into_session(Default::default())
                .expect("start"),
            &input,
            true,
        );
        assert_eq!(borrowed, expected);
        assert_eq!(owned, expected);
        assert!(finished(&owned));
    }
}

#[test]
fn the_framed_shorthands_keep_the_finish_contract() {
    let bytes = full(&[chunk(&[2, 0, 0], b"abcdef")]);
    let mut session = decoder(Default::default())
        .into_session(Default::default())
        .expect("start");
    session
        .process(&bytes[..7], &mut [0; 64], DecodeOperation::Process)
        .expect("partial");
    // EOF inside the object is truncation, not success.
    assert!(session.finish(&mut [0; 64]).is_err());

    session.reinit(Default::default()).expect("reinit");
    let mut output = [0; 1];
    session
        .process(&bytes, &mut output, DecodeOperation::Finish)
        .expect("first finish");
    let failure = session.flush(&mut [0; 8]).unwrap_err();
    assert!(matches!(failure.error, FramedDecodeError::InvalidState));
    assert_eq!((failure.consumed, failure.produced), (0, 0));
}
