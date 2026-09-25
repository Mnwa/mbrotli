#![cfg(feature = "decompression")]
//! `DecoderSessionOwned` against the borrowed `DecoderSession` it mirrors.
//!
//! Compressed inputs come from the pinned C encoder, so this file needs only
//! the decompression codec. Every scenario is driven through both session
//! shapes by the same schedule, and the owned one must reproduce the borrowed
//! one's output, per-call progress, failure and totals exactly.

mod support;

use mbrotli::dictionary::{DecodeDictionary, DictionaryAttachment};
use mbrotli::{
    DecodeError, DecodeFailure, DecodeLimits, DecodeOperation, DecodeProgress, DecodeStreamConfig,
    DecoderConfig, DecoderSession, DecoderSessionOwned, DecoderStatus, Decompressor, MemberMode,
    OutputSize, RetentionPolicy, Window, WindowLimit,
};
use std::sync::Arc;
use support::{CParams, Rng, c_compress, c_compress_with_prefixes};

/// The calls both session shapes answer, so a schedule can drive either.
trait Session {
    fn step(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        operation: DecodeOperation,
    ) -> Result<DecodeProgress, DecodeFailure>;
    fn totals(&self) -> Totals;
}

/// Everything a session reports about itself between calls.
#[derive(Debug, PartialEq)]
struct Totals {
    finished: bool,
    total_in: u64,
    total_out: u64,
    members: u64,
    window: Option<Window>,
}

impl Session for DecoderSession<'_, '_> {
    fn step(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        operation: DecodeOperation,
    ) -> Result<DecodeProgress, DecodeFailure> {
        self.process(input, output, operation)
    }
    fn totals(&self) -> Totals {
        Totals {
            finished: self.is_finished(),
            total_in: self.total_in(),
            total_out: self.total_out(),
            members: self.members_decoded(),
            window: self.window(),
        }
    }
}

impl<D: AsRef<DecodeDictionary> + 'static> Session for DecoderSessionOwned<D> {
    fn step(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        operation: DecodeOperation,
    ) -> Result<DecodeProgress, DecodeFailure> {
        self.process(input, output, operation)
    }
    fn totals(&self) -> Totals {
        Totals {
            finished: self.is_finished(),
            total_in: self.total_in(),
            total_out: self.total_out(),
            members: self.members_decoded(),
            window: self.window(),
        }
    }
}

/// What one call did: its progress, or its failure with exact progress.
#[derive(Debug, PartialEq)]
enum Call {
    Progress(DecodeProgress),
    Failure {
        error: String,
        consumed: usize,
        produced: usize,
    },
}

#[derive(Debug, Default, PartialEq)]
struct Trace {
    output: Vec<u8>,
    calls: Vec<Call>,
    totals: Vec<Totals>,
}

impl Trace {
    fn failed(&self) -> bool {
        matches!(self.calls.last(), Some(Call::Failure { .. }))
    }
}

fn call(
    session: &mut impl Session,
    trace: &mut Trace,
    input: &[u8],
    output: usize,
    operation: DecodeOperation,
) -> Option<DecodeProgress> {
    let mut buffer = vec![0; output];
    let result = session.step(input, &mut buffer, operation);
    let (call, progress) = match result {
        Ok(progress) => {
            trace.output.extend_from_slice(&buffer[..progress.produced]);
            (Call::Progress(progress), Some(progress))
        }
        Err(failure) => {
            trace.output.extend_from_slice(&buffer[..failure.produced]);
            let call = Call::Failure {
                consumed: failure.consumed,
                produced: failure.produced,
                error: format!("{:?}", failure.into_error()),
            };
            (call, None)
        }
    };
    trace.calls.push(call);
    trace.totals.push(session.totals());
    progress
}

/// Feeds `input` in `chunk`-byte pieces through `output`-byte buffers,
/// declaring EOF with `Finish` once the last piece is offered.
fn drive(session: &mut impl Session, input: &[u8], chunk: usize, output: usize) -> Trace {
    let mut trace = Trace::default();
    let mut offset = 0;
    loop {
        let end = input.len().min(offset + chunk);
        let operation = if end == input.len() {
            DecodeOperation::Finish
        } else {
            DecodeOperation::Process
        };
        let Some(progress) = call(session, &mut trace, &input[offset..end], output, operation)
        else {
            break;
        };
        offset += progress.consumed;
        if progress.status == DecoderStatus::Finished {
            break;
        }
    }
    trace
}

fn decoder(config: DecoderConfig) -> Decompressor {
    Decompressor::new(config).expect("config")
}

/// Runs one schedule through a borrowed and an owned session and requires
/// identical traces, returning the owned one.
fn parity(
    config: DecoderConfig,
    stream: DecodeStreamConfig,
    input: &[u8],
    chunk: usize,
    output: usize,
) -> Trace {
    let mut borrowed_owner = decoder(config);
    let expected = drive(
        &mut borrowed_owner.start(stream).expect("borrowed start"),
        input,
        chunk,
        output,
    );
    let mut session = decoder(config).into_session(stream).expect("owned start");
    let actual = drive(&mut session, input, chunk, output);
    assert_eq!(actual, expected, "chunk {chunk}, output {output}");
    actual
}

fn payloads() -> Vec<(&'static str, Vec<u8>)> {
    let text = b"The owned decoder session decodes what the borrowed one does. ".repeat(100);
    let mut rng = Rng::new(0xdec0de);
    let mut large = Vec::with_capacity(1 << 20);
    while large.len() < 1 << 20 {
        large.extend_from_slice(&text[..usize::from(rng.next_u8()) + 1]);
        large.extend_from_slice(&rng.bytes(64, 16));
    }
    vec![
        ("empty", Vec::new()),
        ("tiny", b"x".to_vec()),
        ("several KiB", text),
        ("large", large),
    ]
}

#[test]
fn owned_and_borrowed_sessions_decode_identically_in_chunks() {
    for (name, payload) in payloads() {
        for quality in [0, 5, 11] {
            let compressed = c_compress(quality, 22, &payload);
            for (chunk, output) in [(1, 1), (7, 3), (1024, 4096), (1 << 16, 1 << 16)] {
                if chunk * output < 64 && payload.len() > 1 << 14 {
                    continue;
                }
                let trace = parity(
                    DecoderConfig::default(),
                    OutputSize::Exact(payload.len() as u64).into(),
                    &compressed,
                    chunk,
                    output,
                );
                assert!(!trace.failed(), "{name}, q{quality}");
                assert_eq!(trace.output, payload, "{name}, q{quality}");
                assert!(trace.totals.last().expect("calls").finished);
            }
        }
    }
}

#[test]
fn an_owned_session_decodes_chunked_input() {
    let payload = b"owned chunked decode ".repeat(500);
    let compressed = c_compress(9, 22, &payload);
    let mut session = decoder(DecoderConfig::default())
        .into_session(DecodeStreamConfig::default())
        .expect("start");
    let trace = drive(&mut session, &compressed, 113, 500);
    assert_eq!(trace.output, payload);
    assert!(session.is_finished());
    assert_eq!(session.total_in(), compressed.len() as u64);
    assert_eq!(session.total_out(), payload.len() as u64);
    assert_eq!(session.members_decoded(), 1);
    assert_eq!(session.window().map(|window| window.bits()), Some(22));
}

/// Offers the whole input with `Finish` and zero-, one- and two-byte buffers.
fn finish_through_tiny_buffers(session: &mut impl Session, input: &[u8]) -> Trace {
    let mut trace = Trace::default();
    let mut remaining = input;
    for size in [0, 1, 2].into_iter().cycle() {
        let progress = call(
            session,
            &mut trace,
            remaining,
            size,
            DecodeOperation::Finish,
        )
        .expect("a complete member decodes");
        remaining = &remaining[progress.consumed..];
        if progress.status == DecoderStatus::Finished {
            break;
        }
        assert_eq!(progress.status, DecoderStatus::NeedsOutput);
        if size == 0 {
            assert_eq!(progress.produced, 0);
        }
    }
    assert!(remaining.is_empty());
    let again = call(session, &mut trace, &[], 16, DecodeOperation::Finish);
    assert_eq!(
        again,
        Some(DecodeProgress {
            consumed: 0,
            produced: 0,
            status: DecoderStatus::Finished,
        })
    );
    trace
}

#[test]
fn repeated_needs_output_through_tiny_buffers_matches_the_borrowed_session() {
    let payload = b"one or two bytes at a time ".repeat(40);
    for quality in [0, 5, 11] {
        let compressed = c_compress(quality, 22, &payload);
        let mut owner = decoder(DecoderConfig::default());
        let expected = finish_through_tiny_buffers(
            &mut owner.start(Default::default()).expect("start"),
            &compressed,
        );
        let actual = finish_through_tiny_buffers(
            &mut decoder(DecoderConfig::default())
                .into_session(Default::default())
                .expect("start"),
            &compressed,
        );
        assert_eq!(actual, expected, "q{quality}");
        assert_eq!(actual.output, payload);
    }
}

#[test]
fn truncated_and_empty_input_fail_with_the_same_exact_progress() {
    let payload = b"a member cut short ".repeat(200);
    let compressed = c_compress(5, 22, &payload);
    for cut in [0, 1, compressed.len() / 2, compressed.len() - 1] {
        for (chunk, output) in [(1, 1), (64, 7), (compressed.len(), 1 << 16)] {
            let trace = parity(
                DecoderConfig::default(),
                DecodeStreamConfig::default(),
                &compressed[..cut],
                chunk,
                output,
            );
            match trace.calls.last() {
                Some(Call::Failure { error, .. }) => {
                    assert!(error.contains("UnexpectedEndOfInput"), "{cut}: {error}");
                }
                other => panic!("cut {cut}: {other:?}"),
            }
            assert!(payload.starts_with(&trace.output));
        }
    }

    let mut session = decoder(DecoderConfig::default())
        .into_session(Default::default())
        .expect("start");
    let failure = session
        .process(&[], &mut [], DecodeOperation::Finish)
        .unwrap_err();
    assert_eq!((failure.consumed, failure.produced), (0, 0));
    assert!(matches!(
        failure.into_error(),
        DecodeError::UnexpectedEndOfInput
    ));
    assert!(matches!(
        session
            .process(&[], &mut [], DecodeOperation::Finish)
            .unwrap_err()
            .into_error(),
        DecodeError::InvalidState
    ));
}

/// Starts a `Finish`, then offers a shorter suffix than the one it declared.
fn shorten_the_final_suffix(session: &mut impl Session, compressed: &[u8]) -> Trace {
    let mut trace = Trace::default();
    let first = call(session, &mut trace, compressed, 8, DecodeOperation::Finish)
        .expect("the first call decodes");
    assert_eq!(first.status, DecoderStatus::NeedsOutput);
    let rest = &compressed[first.consumed..];
    assert!(
        call(
            session,
            &mut trace,
            &rest[..rest.len() - 1],
            8,
            DecodeOperation::Finish
        )
        .is_none()
    );
    trace
}

#[test]
fn a_changed_final_suffix_is_rejected_the_same_way() {
    let compressed = c_compress(5, 22, &b"suffix contract ".repeat(100));
    let mut owner = decoder(DecoderConfig::default());
    let expected = shorten_the_final_suffix(
        &mut owner.start(Default::default()).expect("start"),
        &compressed,
    );
    let actual = shorten_the_final_suffix(
        &mut decoder(DecoderConfig::default())
            .into_session(Default::default())
            .expect("start"),
        &compressed,
    );
    assert_eq!(actual, expected);
    assert!(matches!(
        actual.calls.last(),
        Some(Call::Failure { error, consumed: 0, produced: 0 }) if error == "InvalidState"
    ));
}

#[test]
fn a_returned_decoder_is_reusable_after_every_ending() {
    let payload = b"reuse the decoder ".repeat(300);
    let compressed = c_compress(6, 22, &payload);

    // Finished.
    let mut session = decoder(DecoderConfig::default())
        .into_session(Default::default())
        .expect("start");
    assert_eq!(drive(&mut session, &compressed, 100, 100).output, payload);
    let decoder = session.into_decompressor();

    // Unfinished, mid-member.
    let mut session = decoder.into_session(Default::default()).expect("start");
    session
        .process(
            &compressed[..compressed.len() / 2],
            &mut [0; 32],
            DecodeOperation::Process,
        )
        .expect("partial");
    assert!(!session.is_finished());
    let mut decoder = session.into_decompressor();
    assert_eq!(decoder.decompress(&compressed).expect("decode"), payload);

    // Failed.
    let mut session = decoder.into_session(Default::default()).expect("start");
    let mut corrupt = compressed.clone();
    corrupt[0] = 0xff;
    corrupt[1] = 0xff;
    assert!(
        session
            .process(
                &corrupt,
                &mut vec![0; payload.len()],
                DecodeOperation::Finish
            )
            .is_err()
    );
    let mut decoder = session.into_decompressor();
    assert_eq!(decoder.decompress(&compressed).expect("decode"), payload);

    // And back into a fresh owned session.
    let mut session = decoder.into_session(Default::default()).expect("restart");
    assert_eq!(drive(&mut session, &compressed, 4096, 4096).output, payload);
}

#[test]
fn a_returned_decoder_applies_its_retention_policy() {
    let payload = b"retention ".repeat(1000);
    let compressed = c_compress(5, 22, &payload);
    for (policy, releases) in [
        (RetentionPolicy::ReleaseAll, true),
        (RetentionPolicy::Aggressive, false),
    ] {
        let decoder = Decompressor::builder(DecoderConfig::default())
            .with_retention(policy)
            .build()
            .expect("config");
        let mut session = decoder.into_session(Default::default()).expect("start");
        drive(&mut session, &compressed, 1 << 16, 1 << 16);
        let decoder = session.into_decompressor();
        assert_eq!(decoder.retained_bytes() == 0, releases, "{policy:?}");
    }
}

#[test]
fn owned_sessions_keep_the_output_size_contract() {
    let payload = b"exact output size ".repeat(50);
    let compressed = c_compress(5, 22, &payload);
    for expected in [payload.len() as u64 - 1, payload.len() as u64 + 1] {
        let trace = parity(
            DecoderConfig::default(),
            OutputSize::Exact(expected).into(),
            &compressed,
            97,
            64,
        );
        assert!(trace.failed(), "{expected}");
    }
}

#[test]
fn owned_sessions_keep_input_output_and_workspace_limits() {
    let payload = b"resource limits ".repeat(400);
    let compressed = c_compress(5, 22, &payload);

    let limited = |limits: DecodeLimits| DecoderConfig::default().with_limits(limits);
    let input = limited(DecodeLimits::default().with_max_input_bytes(Some(10)));
    let trace = parity(input, Default::default(), &compressed, 64, 64);
    assert!(trace.failed());
    let output = limited(DecodeLimits::default().with_max_output_bytes(Some(100)));
    let trace = parity(output, Default::default(), &compressed, 64, 64);
    assert!(trace.failed());
    assert!(trace.output.len() <= 100);

    // An exact size beyond the output budget is refused at start.
    let stream = DecodeStreamConfig::from(OutputSize::Exact(101));
    assert!(matches!(
        decoder(output).start(stream),
        Err(DecodeError::OutputLimitExceeded { limit: 100 })
    ));
    assert!(matches!(
        decoder(output).into_session(stream),
        Err(DecodeError::OutputLimitExceeded { limit: 100 })
    ));

    // A decoder over its workspace budget is released before it starts.
    let workspace = limited(DecodeLimits::default().with_max_workspace_bytes(Some(1)));
    let over_budget = || {
        let mut owner = decoder(DecoderConfig::default());
        assert_eq!(owner.decompress(&compressed).expect("decode"), payload);
        owner.reconfigure(workspace).expect("reconfigure");
        assert!(owner.retained_bytes() > 1);
        owner
    };
    let mut owner = over_budget();
    drop(owner.start(Default::default()).expect("borrowed start"));
    assert_eq!(owner.retained_bytes(), 0);
    let owner = over_budget()
        .into_session(Default::default())
        .expect("owned start")
        .into_decompressor();
    assert_eq!(owner.retained_bytes(), 0);
}

#[test]
fn owned_sessions_keep_the_window_limit() {
    let payload = b"window limit ".repeat(100);
    let compressed = c_compress(5, 22, &payload);
    let config =
        DecoderConfig::default().with_window_limit(WindowLimit::standard(16).expect("window"));
    let trace = parity(config, Default::default(), &compressed, 16, 16);
    assert!(trace.failed());
    assert!(trace.output.is_empty());
}

#[test]
fn owned_sessions_keep_member_modes() {
    let first = b"first member ".repeat(20);
    let second = b"second member ".repeat(30);
    let mut joined = c_compress(5, 22, &first);
    let first_len = joined.len();
    joined.extend_from_slice(&c_compress(9, 18, &second));

    // Single mode stops at the first member and leaves the rest unconsumed.
    let trace = parity(
        DecoderConfig::default(),
        Default::default(),
        &joined,
        50,
        50,
    );
    assert_eq!(trace.output, first);
    let last = trace.totals.last().expect("calls");
    assert_eq!((last.total_in, last.members), (first_len as u64, 1));

    let concatenated = DecoderConfig::default().with_member_mode(MemberMode::Concatenated);
    for (chunk, output) in [(1, 1), (50, 50), (joined.len(), 1 << 16)] {
        let trace = parity(concatenated, Default::default(), &joined, chunk, output);
        assert_eq!(trace.output, [&first[..], &second[..]].concat());
        let last = trace.totals.last().expect("calls");
        assert!(last.finished);
        assert_eq!(last.members, 2);
        assert_eq!(last.window.map(|window| window.bits()), Some(18));
    }
}

#[test]
fn an_owned_dictionary_session_round_trips_like_the_borrowed_one() {
    let prefix = b"a prefix both sides share, repeated enough to matter. ".repeat(10);
    let payload = [&prefix[..120], b" plus a novel tail"].concat();
    let compressed = c_compress_with_prefixes(CParams::new(5, 22), &[&prefix], &payload);
    let build = || {
        DecodeDictionary::new(&[DictionaryAttachment::Raw(&prefix)], Default::default())
            .expect("dictionary")
    };
    let shared = Arc::new(build());
    let leaked: &'static DecodeDictionary = Box::leak(Box::new(build()));

    let mut owner = decoder(DecoderConfig::default());
    let expected = drive(
        &mut owner
            .start_with_dictionary(&*shared, Default::default())
            .expect("borrowed"),
        &compressed,
        11,
        13,
    );
    assert_eq!(expected.output, payload);
    let mut by_arc = decoder(DecoderConfig::default())
        .into_session_with_dictionary(Arc::clone(&shared), Default::default())
        .expect("owned");
    assert_eq!(drive(&mut by_arc, &compressed, 11, 13), expected);
    let mut by_value = decoder(DecoderConfig::default())
        .into_session_with_dictionary(build(), Default::default())
        .expect("owned");
    assert_eq!(drive(&mut by_value, &compressed, 11, 13), expected);
    let mut by_static = decoder(DecoderConfig::default())
        .into_session_with_dictionary(leaked, Default::default())
        .expect("owned");
    assert_eq!(drive(&mut by_static, &compressed, 11, 13), expected);

    // The returned decoder keeps no dictionary: without one it cannot
    // reproduce the payload, and the shared dictionary is released.
    let mut decoder = by_arc.into_decompressor();
    assert_eq!(Arc::strong_count(&shared), 1);
    assert_ne!(decoder.decompress(&compressed).ok(), Some(payload));
}

#[test]
fn an_owned_session_moves_between_threads() {
    const fn assert_send<T: Send>() {}
    assert_send::<DecoderSessionOwned>();
    assert_send::<DecoderSessionOwned<Arc<DecodeDictionary>>>();
    assert_send::<DecoderSessionOwned<&'static DecodeDictionary>>();

    let payload = b"decoded on another thread ".repeat(100);
    let compressed = c_compress(5, 22, &payload);
    let mut session = decoder(DecoderConfig::default())
        .into_session(Default::default())
        .expect("start");
    let mut head = vec![0; 16];
    let first = session
        .process(&compressed, &mut head, DecodeOperation::Finish)
        .expect("first call");
    head.truncate(first.produced);
    let rest = compressed[first.consumed..].to_vec();
    let (tail, decoder) = std::thread::spawn(move || {
        let trace = drive(&mut session, &rest, rest.len(), 64);
        (trace.output, session.into_decompressor())
    })
    .join()
    .expect("the worker finished");
    assert_eq!([head, tail].concat(), payload);
    let mut decoder = decoder;
    assert_eq!(decoder.decompress(&compressed).expect("decode"), payload);
}
