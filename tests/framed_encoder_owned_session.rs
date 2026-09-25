#![cfg(all(feature = "compression", feature = "experimental"))]
//! `FramedEncoderSessionOwned` against the borrowed `FramedEncoderSession`.
//!
//! One driver runs the same structured input, output widths, payload chunks
//! and flush points through both facades; the wire bytes and the debug record
//! of every call must match exactly.

use mbrotli::dictionary::{DictionaryBuilder, PreparedDictionary};
use mbrotli::framing::*;
use mbrotli::{EncoderConfig, InputSize, Operation, Quality, StreamConfig};

/// The calls both facades answer, so one driver can run either.
trait Facade {
    fn process(
        &mut self,
        output: &mut [u8],
        operation: FramedEncodeOperation,
    ) -> Result<FramedEncodeProgress, FramedEncodeFailure>;
    fn metadata(
        &mut self,
        kind: MetadataKind,
        fields: &[MetadataField<'_>],
    ) -> Result<(), FramedEncodeError>;
    fn metadata_with_options(
        &mut self,
        kind: MetadataKind,
        fields: &[MetadataField<'_>],
        options: MetadataOptions<'_>,
    ) -> Result<(), FramedEncodeError>;
    fn repeat_metadata_fields(&mut self, codes: &[[u8; 2]]) -> Result<(), FramedEncodeError>;
    fn padding(&mut self, bytes: usize) -> Result<(), FramedEncodeError>;
    fn resource(
        &mut self,
        options: ResourceOptions,
        stream: StreamConfig,
    ) -> Result<FramedResourceSession<'_, 'static>, FramedEncodeError>;
    fn resource_with_dictionary<'s, 'dict>(
        &'s mut self,
        options: ResourceOptions,
        stream: StreamConfig,
        dictionary: &'dict PreparedDictionary,
        references: &[DictionaryReference],
    ) -> Result<FramedResourceSession<'s, 'dict>, FramedEncodeError>;
    fn uncompressed_resource(
        &mut self,
        options: ResourceOptions,
    ) -> Result<FramedResourceSession<'_, 'static>, FramedEncodeError>;
    fn counters(&self) -> (u64, u64, u64, bool, u64);
    fn flush_now(&mut self, output: &mut [u8])
    -> Result<FramedEncodeProgress, FramedEncodeFailure>;
    fn finish_now(
        &mut self,
        output: &mut [u8],
    ) -> Result<FramedEncodeProgress, FramedEncodeFailure>;
}

macro_rules! facade {
    ($type:ty) => {
        impl Facade for $type {
            fn process(
                &mut self,
                output: &mut [u8],
                operation: FramedEncodeOperation,
            ) -> Result<FramedEncodeProgress, FramedEncodeFailure> {
                <$type>::process(self, output, operation)
            }
            fn metadata(
                &mut self,
                kind: MetadataKind,
                fields: &[MetadataField<'_>],
            ) -> Result<(), FramedEncodeError> {
                <$type>::metadata(self, kind, fields)
            }
            fn metadata_with_options(
                &mut self,
                kind: MetadataKind,
                fields: &[MetadataField<'_>],
                options: MetadataOptions<'_>,
            ) -> Result<(), FramedEncodeError> {
                <$type>::metadata_with_options(self, kind, fields, options)
            }
            fn repeat_metadata_fields(
                &mut self,
                codes: &[[u8; 2]],
            ) -> Result<(), FramedEncodeError> {
                <$type>::repeat_metadata_fields(self, codes)
            }
            fn padding(&mut self, bytes: usize) -> Result<(), FramedEncodeError> {
                <$type>::padding(self, bytes)
            }
            fn resource(
                &mut self,
                options: ResourceOptions,
                stream: StreamConfig,
            ) -> Result<FramedResourceSession<'_, 'static>, FramedEncodeError> {
                <$type>::resource(self, options, stream)
            }
            fn resource_with_dictionary<'s, 'dict>(
                &'s mut self,
                options: ResourceOptions,
                stream: StreamConfig,
                dictionary: &'dict PreparedDictionary,
                references: &[DictionaryReference],
            ) -> Result<FramedResourceSession<'s, 'dict>, FramedEncodeError> {
                <$type>::resource_with_dictionary(self, options, stream, dictionary, references)
            }
            fn uncompressed_resource(
                &mut self,
                options: ResourceOptions,
            ) -> Result<FramedResourceSession<'_, 'static>, FramedEncodeError> {
                <$type>::uncompressed_resource(self, options)
            }
            fn counters(&self) -> (u64, u64, u64, bool, u64) {
                (
                    self.total_in(),
                    self.total_out(),
                    self.resources_encoded(),
                    self.is_finished(),
                    self.next_chunk_offset(),
                )
            }
            fn flush_now(
                &mut self,
                output: &mut [u8],
            ) -> Result<FramedEncodeProgress, FramedEncodeFailure> {
                <$type>::flush(self, output)
            }
            fn finish_now(
                &mut self,
                output: &mut [u8],
            ) -> Result<FramedEncodeProgress, FramedEncodeFailure> {
                <$type>::finish(self, output)
            }
        }
    };
}
facade!(FramedEncoderSession<'_>);
facade!(FramedEncoderSessionOwned);

/// How the driver feeds one container.
#[derive(Clone, Copy, Debug)]
struct Schedule {
    /// Destination sizes, used in turn by every output-producing call.
    widths: &'static [usize],
    /// Payload chunk offered per resource call.
    chunk: usize,
    /// Flush every resource after its first chunk.
    flush: bool,
}

/// The wire bytes and a debug record of every call and counter snapshot.
#[derive(Debug, Default, PartialEq)]
struct Trace {
    wire: Vec<u8>,
    log: Vec<String>,
}

struct Driver<'w> {
    widths: &'w [usize],
    turn: usize,
    trace: Trace,
    /// Use `flush`/`finish` wherever a call would pass no input.
    shorthand: bool,
}

impl Driver<'_> {
    fn width(&mut self) -> usize {
        let width = self.widths[self.turn % self.widths.len()];
        self.turn += 1;
        width
    }
    fn drain(&mut self, session: &mut impl Facade, operation: FramedEncodeOperation) {
        loop {
            let mut output = vec![0; self.width()];
            let result = match (self.shorthand, operation) {
                (true, FramedEncodeOperation::Process) => session.flush_now(&mut output),
                (true, FramedEncodeOperation::Finish) => session.finish_now(&mut output),
                (false, operation) => session.process(&mut output, operation),
            };
            self.trace.log.push(format!("{result:?}"));
            let progress = result.expect("the container encodes");
            self.trace
                .wire
                .extend_from_slice(&output[..progress.produced]);
            self.trace.log.push(format!("{:?}", session.counters()));
            if progress.status != FramedEncoderStatus::NeedsOutput {
                return;
            }
        }
    }
    /// Offers `input` with `operation` until it is taken and nothing is pending.
    fn feed(
        &mut self,
        resource: &mut FramedResourceSession<'_, '_>,
        mut input: &[u8],
        operation: Operation,
        done: FramedEncoderStatus,
    ) {
        loop {
            let mut output = vec![0; self.width()];
            let result = match (self.shorthand && input.is_empty(), operation) {
                (true, Operation::Flush) => resource.flush(&mut output),
                (true, Operation::Finish) => resource.finish(&mut output),
                (_, operation) => resource.process(input, &mut output, operation),
            };
            self.trace.log.push(format!("{result:?}"));
            let progress = result.expect("the resource encodes");
            input = &input[progress.consumed..];
            self.trace
                .wire
                .extend_from_slice(&output[..progress.produced]);
            self.trace.log.push(format!(
                "{} {} {}",
                resource.total_in(),
                resource.total_out(),
                resource.is_finished()
            ));
            if progress.status == done && input.is_empty() {
                return;
            }
        }
    }
    fn resource(
        &mut self,
        resource: &mut FramedResourceSession<'_, '_>,
        item: &FramedResource<'_>,
        schedule: Schedule,
    ) {
        for (index, piece) in item.data.chunks(schedule.chunk).enumerate() {
            self.feed(
                resource,
                piece,
                Operation::Process,
                FramedEncoderStatus::NeedsInput,
            );
            if schedule.flush && index == 0 {
                self.feed(
                    resource,
                    &[],
                    Operation::Flush,
                    FramedEncoderStatus::NeedsInput,
                );
            }
        }
        self.feed(
            resource,
            &[],
            Operation::Finish,
            FramedEncoderStatus::Finished,
        );
    }
}

/// Encodes `input` item by item through `session`, the way `compress` would.
fn encode(session: &mut impl Facade, input: FramedInput<'_>, schedule: Schedule) -> Trace {
    encode_with(session, input, schedule, false)
}

/// As [`encode`], optionally through the input-free `flush`/`finish`.
fn encode_with(
    session: &mut impl Facade,
    input: FramedInput<'_>,
    schedule: Schedule,
    shorthand: bool,
) -> Trace {
    let mut driver = Driver {
        widths: schedule.widths,
        turn: 0,
        trace: Trace::default(),
        shorthand,
    };
    driver.drain(session, FramedEncodeOperation::Process);
    if let Some(codes) = input.repeat_metadata_fields {
        session.repeat_metadata_fields(codes).expect("repeat");
    }
    for item in input.items {
        match *item {
            // The plain entry point is exercised wherever defaults suffice.
            FramedItem::Metadata {
                kind: kind @ MetadataKind::Global,
                fields,
                options: _,
            } => session.metadata(kind, fields).expect("metadata"),
            FramedItem::Metadata {
                kind,
                fields,
                options,
            } => session
                .metadata_with_options(kind, fields, options)
                .expect("metadata"),
            FramedItem::Padding { bytes } => session.padding(bytes).expect("padding"),
            FramedItem::Resource(resource) => {
                let mut guard = match resource.encoding {
                    ResourceEncoding::Uncompressed => {
                        session.uncompressed_resource(resource.options)
                    }
                    ResourceEncoding::Brotli => session.resource(resource.options, resource.stream),
                    ResourceEncoding::Shared {
                        dictionary,
                        references,
                    } => session.resource_with_dictionary(
                        resource.options,
                        resource.stream,
                        dictionary,
                        references,
                    ),
                }
                .expect("the resource opens");
                driver.resource(&mut guard, &resource, schedule);
            }
        }
        driver.trace.log.push(format!("{:?}", session.counters()));
        driver.drain(session, FramedEncodeOperation::Process);
    }
    driver.drain(session, FramedEncodeOperation::Finish);
    let again = session.process(&mut [0; 8], FramedEncodeOperation::Finish);
    driver.trace.log.push(format!("{again:?}"));
    driver.trace
}

fn config(repeat_metadata: bool) -> FramedEncodeConfig {
    FramedEncodeConfig::default()
        .with_encoder_config(EncoderConfig::default().with_quality(Quality::Q5))
        .with_framing_config(FramingConfig {
            chunk_bytes: 64,
            repeat_metadata,
            ..Default::default()
        })
}

fn owner(repeat_metadata: bool) -> FramedCompressor {
    FramedCompressor::new(config(repeat_metadata)).expect("config")
}

const SCHEDULES: [Schedule; 4] = [
    Schedule {
        widths: &[0, 1, 2],
        chunk: 1,
        flush: false,
    },
    Schedule {
        widths: &[1],
        chunk: 5,
        flush: true,
    },
    Schedule {
        widths: &[7, 0, 31],
        chunk: 100,
        flush: false,
    },
    Schedule {
        widths: &[4096],
        chunk: 1 << 16,
        flush: true,
    },
];

/// Runs `input` through a borrowed and an owned session and requires
/// identical traces; without flushes, both must also equal `compress`.
fn parity(repeat_metadata: bool, input: FramedInput<'_>) -> Vec<u8> {
    let one_shot = owner(repeat_metadata).compress(input).expect("compress");
    for schedule in SCHEDULES {
        let mut borrowed_owner = owner(repeat_metadata);
        let expected = encode(
            &mut borrowed_owner.start(Default::default()).expect("start"),
            input,
            schedule,
        );
        let mut session = owner(repeat_metadata)
            .into_session(Default::default())
            .expect("start");
        let actual = encode(&mut session, input, schedule);
        assert_eq!(actual, expected, "{schedule:?}");
        if !schedule.flush {
            assert_eq!(actual.wire, one_shot, "{schedule:?}");
        }
        // The returned owner encodes the same container again.
        let mut returned = session.into_framed_compressor();
        assert_eq!(returned.compress(input).expect("compress"), one_shot);
    }
    one_shot
}

/// Visible resource payloads a decoder must recover, in order.
#[cfg(feature = "decompression")]
fn decodes_to(wire: &[u8], payloads: &[&[u8]]) {
    let output = FramedDecompressor::new(Default::default())
        .expect("config")
        .decompress(wire)
        .expect("the container decodes");
    let decoded: Vec<&[u8]> = output
        .resources
        .iter()
        .map(|resource| resource.data.as_slice())
        .collect();
    assert_eq!(decoded, payloads);
}
#[cfg(not(feature = "decompression"))]
fn decodes_to(_: &[u8], _: &[&[u8]]) {}

#[test]
fn an_empty_container_is_identical_through_both_facades() {
    let wire = parity(false, [].as_slice().into());
    decodes_to(&wire, &[]);
}

#[test]
fn one_chunked_brotli_resource_is_identical_through_both_facades() {
    let payload = b"one resource, fed a chunk at a time. ".repeat(20);
    let items = [FramedItem::Resource(FramedResource::from(&payload[..]))];
    let wire = parity(false, items.as_slice().into());
    decodes_to(&wire, &[&payload]);
}

#[test]
fn mixed_resources_are_identical_through_both_facades() {
    let first = b"first brotli resource ".repeat(12);
    let raw = b"verbatim bytes".to_vec();
    let last = b"last brotli resource ".repeat(9);
    let items = [
        FramedItem::Resource(FramedResource::from(&first[..])),
        FramedItem::Resource(FramedResource {
            data: &raw,
            options: Default::default(),
            stream: Default::default(),
            encoding: ResourceEncoding::Uncompressed,
        }),
        FramedItem::Resource(FramedResource {
            data: &last,
            options: Default::default(),
            stream: InputSize::Exact(last.len() as u64).into(),
            encoding: ResourceEncoding::Brotli,
        }),
    ];
    let wire = parity(false, items.as_slice().into());
    decodes_to(&wire, &[&first, &raw, &last]);
}

#[test]
fn metadata_padding_and_repeats_are_identical_through_both_facades() {
    let payload = b"described resource ".repeat(10);
    let global = [MetadataField {
        code: *b"XX",
        value: b"global",
    }];
    let resource = [
        MetadataField {
            code: *b"id",
            value: b"example.txt",
        },
        MetadataField {
            code: *b"AB",
            value: b"annotation",
        },
    ];
    let footer = [MetadataField {
        code: *b"YY",
        value: b"footer",
    }];
    let brotli = MetadataOptions {
        encoding: MetadataEncoding::Brotli,
        repeated_encoding: MetadataEncoding::Uncompressed,
    };
    let items = [
        FramedItem::Metadata {
            kind: MetadataKind::Global,
            fields: &global,
            options: Default::default(),
        },
        FramedItem::Metadata {
            kind: MetadataKind::Resource,
            fields: &resource,
            options: brotli,
        },
        FramedItem::Padding { bytes: 3 },
        FramedItem::Resource(FramedResource::from(&payload[..])),
        FramedItem::Metadata {
            kind: MetadataKind::Footer,
            fields: &footer,
            options: Default::default(),
        },
        FramedItem::Padding { bytes: 0 },
    ];
    for codes in [None, Some(&[*b"id"][..])] {
        let input = FramedInput {
            items: &items,
            repeat_metadata_fields: codes,
        };
        let wire = parity(true, input);
        decodes_to(&wire, &[&payload]);
    }
}

#[test]
fn a_dictionary_resource_is_identical_through_both_facades() {
    let dictionary = DictionaryBuilder::new()
        .add_prefix(&b"some dictionary words and payload"[..])
        .build()
        .expect("dictionary");
    let references = [DictionaryReference::PrefixId(DictionaryId([7; 32]))];
    let items = [
        FramedItem::Resource(FramedResource {
            data: b"some dictionary words and payload, then more payload",
            options: Default::default(),
            stream: Default::default(),
            encoding: ResourceEncoding::Shared {
                dictionary: &dictionary,
                references: &references,
            },
        }),
        FramedItem::Resource(FramedResource::from(&b"plain"[..])),
    ];
    parity(false, items.as_slice().into());
}

#[test]
fn a_resource_guard_borrows_the_owned_session_until_it_is_dropped() {
    let payload = b"guarded payload";
    let mut session = owner(false)
        .into_session(Default::default())
        .expect("start");
    let mut wire = Vec::new();
    let mut output = [0; 256];
    let p = session
        .process(&mut output, FramedEncodeOperation::Process)
        .expect("header");
    wire.extend_from_slice(&output[..p.produced]);
    {
        let mut resource = session
            .resource(Default::default(), Default::default())
            .expect("resource");
        let p = resource
            .process(payload, &mut output, Operation::Finish)
            .expect("resource");
        assert_eq!(p.status, FramedEncoderStatus::Finished);
        assert!(resource.is_finished());
        assert_eq!(resource.total_in(), payload.len() as u64);
        wire.extend_from_slice(&output[..p.produced]);
    }
    assert_eq!(session.resources_encoded(), 1);
    assert_eq!(session.total_in(), payload.len() as u64);
    let p = session
        .process(&mut output, FramedEncodeOperation::Finish)
        .expect("finish");
    wire.extend_from_slice(&output[..p.produced]);
    assert!(session.is_finished());
    assert_eq!(session.total_out(), wire.len() as u64);
    let items = [FramedItem::Resource(FramedResource::from(&payload[..]))];
    assert_eq!(
        owner(false)
            .compress(items.as_slice().into())
            .expect("compress"),
        wire
    );
}

#[test]
fn a_returned_framed_compressor_is_reusable_after_every_ending() {
    let items = [FramedItem::Resource(FramedResource::from(
        &b"next container"[..],
    ))];
    let expected = owner(false)
        .compress(items.as_slice().into())
        .expect("compress");
    let reuse = |session: FramedEncoderSessionOwned| {
        let mut encoder = session.into_framed_compressor();
        assert_eq!(
            encoder.compress(items.as_slice().into()).expect("reuse"),
            expected
        );
        let mut again = encoder.into_session(Default::default()).expect("restart");
        assert_eq!(again.total_out(), 0);
        let trace = encode(&mut again, items.as_slice().into(), SCHEDULES[2]);
        assert_eq!(trace.wire, expected);
        again.into_framed_compressor()
    };

    // Finished.
    let mut session = owner(false)
        .into_session(Default::default())
        .expect("start");
    encode(&mut session, items.as_slice().into(), SCHEDULES[3]);
    assert!(session.is_finished());
    let encoder = reuse(session);

    // Header still queued: nothing delivered yet.
    let mut session = encoder.into_session(Default::default()).expect("start");
    let p = session
        .process(&mut [], FramedEncodeOperation::Process)
        .expect("zero-width");
    assert_eq!(p.status, FramedEncoderStatus::NeedsOutput);
    let encoder = reuse(session);

    // A finished resource and metadata, but no container suffix.
    let mut session = encoder.into_session(Default::default()).expect("start");
    session
        .process(&mut [0; 256], FramedEncodeOperation::Process)
        .expect("header");
    {
        let mut resource = session
            .resource(Default::default(), Default::default())
            .expect("resource");
        resource
            .process(b"payload", &mut [0; 256], Operation::Finish)
            .expect("resource");
    }
    session
        .metadata(MetadataKind::Global, &[])
        .expect("metadata");
    assert!(!session.is_finished());
    let encoder = reuse(session);

    // A terminal failure: the resource breaks its declared size.
    let mut session = encoder.into_session(Default::default()).expect("start");
    session
        .process(&mut [0; 256], FramedEncodeOperation::Process)
        .expect("header");
    {
        let mut resource = session
            .resource(Default::default(), InputSize::Exact(10).into())
            .expect("resource");
        assert!(
            resource
                .process(b"abc", &mut [0; 256], Operation::Finish)
                .is_err()
        );
    }
    reuse(session);
}

#[test]
fn owned_starts_reject_what_borrowed_starts_reject() {
    let mut encoder = owner(false);
    std::mem::forget(encoder.start(Default::default()).expect("start"));
    assert!(matches!(
        encoder.into_session(Default::default()),
        Err(FramedEncodeError::AbandonedSession)
    ));
}

#[test]
fn an_owned_framed_session_moves_between_threads() {
    const fn assert_send<T: Send>() {}
    assert_send::<FramedEncoderSessionOwned>();

    let items = [FramedItem::Resource(FramedResource::from(&b"threaded"[..]))];
    let expected = owner(false)
        .compress(items.as_slice().into())
        .expect("compress");
    let mut session = owner(false)
        .into_session(Default::default())
        .expect("start");
    let mut head = vec![0; 256];
    let p = session
        .process(&mut head, FramedEncodeOperation::Process)
        .expect("header");
    head.truncate(p.produced);
    let (tail, _encoder) = std::thread::spawn(move || {
        let mut wire = Vec::new();
        let mut output = [0; 256];
        {
            let mut resource = session
                .resource(Default::default(), Default::default())
                .expect("resource");
            let p = resource
                .process(b"threaded", &mut output, Operation::Finish)
                .expect("resource");
            wire.extend_from_slice(&output[..p.produced]);
        }
        let p = session
            .process(&mut output, FramedEncodeOperation::Finish)
            .expect("finish");
        wire.extend_from_slice(&output[..p.produced]);
        (wire, session.into_framed_compressor())
    })
    .join()
    .expect("the worker finished");
    assert_eq!([head, tail].concat(), expected);
}

/// Leaves `session` in one of the states `reinit` must recover from.
fn end_first_container(session: &mut FramedEncoderSessionOwned, ending: &str) {
    let items = [FramedItem::Resource(FramedResource::from(
        &b"first container"[..],
    ))];
    match ending {
        "finished" => {
            encode(&mut *session, items.as_slice().into(), SCHEDULES[3]);
            assert!(session.is_finished());
        }
        "needs output" => {
            let p = session
                .process(&mut [0; 2], FramedEncodeOperation::Process)
                .expect("partial header");
            assert_eq!(p.status, FramedEncoderStatus::NeedsOutput);
        }
        "needs input" => {
            session
                .process(&mut [0; 256], FramedEncodeOperation::Process)
                .expect("header");
            let mut resource = session
                .resource(Default::default(), Default::default())
                .expect("resource");
            let p = resource
                .process(b"unfinished", &mut [0; 256], Operation::Process)
                .expect("resource");
            assert_eq!(p.status, FramedEncoderStatus::NeedsInput);
        }
        "failed" => {
            session
                .process(&mut [0; 256], FramedEncodeOperation::Process)
                .expect("header");
            {
                let mut resource = session
                    .resource(Default::default(), InputSize::Exact(10).into())
                    .expect("resource");
                assert!(
                    resource
                        .process(b"abc", &mut [0; 256], Operation::Finish)
                        .is_err()
                );
            }
            assert!(
                session
                    .process(&mut [0; 256], FramedEncodeOperation::Finish)
                    .is_err()
            );
        }
        _ => unreachable!(),
    }
}

#[test]
fn reinit_starts_a_container_identical_to_a_fresh_borrowed_one() {
    let payload = b"the second container ".repeat(15);
    let footer = [MetadataField {
        code: *b"YY",
        value: b"footer",
    }];
    let items = [
        FramedItem::Padding { bytes: 2 },
        FramedItem::Resource(FramedResource::from(&payload[..])),
        FramedItem::Metadata {
            kind: MetadataKind::Footer,
            fields: &footer,
            options: Default::default(),
        },
    ];
    let input = FramedInput::from(items.as_slice());
    let one_shot = owner(false).compress(input).expect("compress");
    for ending in ["finished", "needs output", "needs input", "failed"] {
        let mut session = owner(false)
            .into_session(Default::default())
            .expect("start");
        end_first_container(&mut session, ending);
        session.reinit(Default::default()).expect("reinit");
        let mut fresh = owner(false);
        let fresh = fresh.start(Default::default()).expect("start").counters();
        assert_eq!(session.counters(), fresh, "{ending}");
        for schedule in SCHEDULES {
            let mut borrowed_owner = owner(false);
            let expected = encode(
                &mut borrowed_owner.start(Default::default()).expect("start"),
                input,
                schedule,
            );
            let actual = encode(&mut session, input, schedule);
            assert_eq!(actual, expected, "{ending}, {schedule:?}");
            if !schedule.flush {
                assert_eq!(actual.wire, one_shot);
            }
            session.reinit(Default::default()).expect("reinit again");
        }
    }
}

#[test]
fn flush_and_finish_are_process_without_input_in_both_session_shapes() {
    let payload = b"flush and finish take no input ".repeat(12);
    let raw = b"verbatim".to_vec();
    let footer = [MetadataField {
        code: *b"YY",
        value: b"footer",
    }];
    let items = [
        FramedItem::Resource(FramedResource::from(&payload[..])),
        FramedItem::Metadata {
            kind: MetadataKind::Footer,
            fields: &footer,
            options: Default::default(),
        },
        FramedItem::Resource(FramedResource {
            data: &raw,
            options: Default::default(),
            stream: Default::default(),
            encoding: ResourceEncoding::Uncompressed,
        }),
    ];
    let input = FramedInput::from(items.as_slice());
    for schedule in SCHEDULES {
        let mut encoder = owner(false);
        let expected = encode_with(
            &mut encoder.start(Default::default()).expect("start"),
            input,
            schedule,
            false,
        );
        let borrowed = encode_with(
            &mut encoder.start(Default::default()).expect("start"),
            input,
            schedule,
            true,
        );
        let mut session = owner(false)
            .into_session(Default::default())
            .expect("start");
        let owned = encode_with(&mut session, input, schedule, true);
        assert_eq!(borrowed, expected, "{schedule:?}");
        assert_eq!(owned, expected, "{schedule:?}");
        let idle = session.finish(&mut [0; 4]).expect("finished");
        assert_eq!(idle.status, FramedEncoderStatus::Finished);
        assert_eq!((idle.consumed, idle.produced), (0, 0));
    }
}
