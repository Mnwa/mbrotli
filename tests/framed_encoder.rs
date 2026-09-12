#![cfg(all(feature = "compression", feature = "experimental"))]
use mbrotli::framing::*;
use mbrotli::{Backend, EncoderConfig, InputSize, Operation, Quality, RetentionPolicy};

fn config() -> FramedEncodeConfig {
    FramedEncodeConfig::default()
        .with_encoder_config(EncoderConfig::default().with_quality(Quality::Q5))
        .with_framing_config(FramingConfig {
            chunk_bytes: 7,
            ..Default::default()
        })
}
fn native(
    owner: &mut FramedCompressor,
    data: &[u8],
    width: usize,
    split: usize,
    stored: bool,
) -> Vec<u8> {
    let mut session = owner
        .start(InputSize::Exact(data.len() as u64).into())
        .unwrap();
    let mut output = vec![0; width];
    let mut wire = Vec::new();
    loop {
        let p = session
            .process(&mut output, FramedEncodeOperation::Process)
            .unwrap();
        wire.extend_from_slice(&output[..p.produced]);
        if p.status == FramedEncoderStatus::NeedsInput {
            break;
        }
    }
    {
        let mut resource = if stored {
            session.uncompressed_resource(Default::default()).unwrap()
        } else {
            session
                .resource(
                    Default::default(),
                    InputSize::Exact(data.len() as u64).into(),
                )
                .unwrap()
        };
        let mut start = 0;
        loop {
            let end = if start < split { split } else { data.len() };
            let op = if end == data.len() {
                Operation::Finish
            } else {
                Operation::Process
            };
            let p = resource
                .process(&data[start..end], &mut output, op)
                .unwrap();
            start += p.consumed;
            wire.extend_from_slice(&output[..p.produced]);
            if p.status == FramedEncoderStatus::Finished {
                break;
            }
        }
        assert!(resource.is_finished());
        assert_eq!(resource.total_in(), data.len() as u64);
        assert!(resource.total_out() > 0);
    }
    assert_eq!(session.resources_encoded(), 1);
    loop {
        let p = session
            .process(&mut output, FramedEncodeOperation::Finish)
            .unwrap();
        wire.extend_from_slice(&output[..p.produced]);
        if p.status == FramedEncoderStatus::Finished {
            break;
        }
    }
    assert!(session.is_finished());
    assert_eq!(session.total_in(), data.len() as u64);
    assert_eq!(session.total_out(), wire.len() as u64);
    assert_eq!(session.next_chunk_offset(), wire.len() as u64);
    assert_eq!(
        session
            .process(&mut [], FramedEncodeOperation::Finish)
            .unwrap()
            .produced,
        0
    );
    wire
}
#[test]
fn all_destinations_preserve_canonical_bytes_across_chunk_and_output_boundaries() {
    for backend in Backend::available() {
        let mut owner = FramedCompressor::builder(config())
            .with_backend(backend)
            .build()
            .unwrap();
        for stored in [false, true] {
            for length in [0, 1, 6, 7, 8, 14, 21, 32] {
                let data: Vec<u8> = (0..length).map(|i| b'a' + (i % 4) as u8).collect();
                let mut r = FramedResource::from(data.as_slice());
                if stored {
                    r.encoding = ResourceEncoding::Uncompressed;
                }
                let items = [FramedItem::Resource(r)];
                let input = FramedInput::from(items.as_slice());
                let expected = owner.compress(input).unwrap();
                for width in [1, 2, 7, 31, 4096] {
                    for split in 0..=length {
                        assert_eq!(native(&mut owner, &data, width, split, stored), expected);
                    }
                }
                let mut dst = vec![0xa5; expected.len() + 3];
                assert_eq!(
                    owner.compress_to_slice(input, &mut dst).unwrap(),
                    expected.len()
                );
                assert_eq!(&dst[..expected.len()], expected);
                assert_eq!(&dst[expected.len()..], [0xa5; 3]);
                for capacity in 0..expected.len() {
                    let mut dst = vec![0xa5; capacity];
                    let failure = owner.compress_to_slice(input, &mut dst).unwrap_err();
                    assert!(matches!(failure.error, FramedEncodeError::OutputTooSmall));
                    assert_eq!(failure.produced, capacity);
                    assert_eq!(dst, expected[..capacity]);
                }
                let mut appended = b"prefix".to_vec();
                let range = owner.compress_into(input, &mut appended).unwrap();
                assert_eq!(range, 6..6 + expected.len());
                assert_eq!(&appended[..6], b"prefix");
                assert_eq!(&appended[range], expected);
                #[cfg(not(feature = "no_std"))]
                {
                    use std::io::{Read, Write};
                    let mut reader = owner
                        .framed_reader(input, InputSize::Exact(length as u64).into())
                        .unwrap();
                    assert_eq!(reader.read(&mut []).unwrap(), 0);
                    let mut actual = Vec::new();
                    let mut byte = [0; 1];
                    while reader.read(&mut byte).unwrap() != 0 {
                        actual.push(byte[0]);
                    }
                    assert_eq!(actual, expected);
                    drop(reader);
                    let mut writer = owner.framed_writer(Vec::new(), Default::default()).unwrap();
                    {
                        let mut resource = if stored {
                            writer.uncompressed_resource(Default::default()).unwrap()
                        } else {
                            writer.resource(Default::default(), r.stream).unwrap()
                        };
                        resource.write_all(&data).unwrap();
                        resource.try_finish().unwrap();
                    }
                    assert_eq!(writer.finish().unwrap(), expected);
                }
                #[cfg(feature = "decompression")]
                assert_eq!(
                    FramedDecompressor::new(Default::default())
                        .unwrap()
                        .decompress(&expected)
                        .unwrap()
                        .resources[0]
                        .data,
                    data
                );
            }
        }
    }
}
#[test]
fn command_admission_zero_output_and_final_boundaries_are_explicit() {
    let mut owner = FramedCompressor::new(config()).unwrap();
    let mut s = owner.start(Default::default()).unwrap();
    assert!(matches!(
        s.padding(1),
        Err(FramedEncodeError::OutputPending)
    ));
    assert!(matches!(
        s.metadata(MetadataKind::Global, &[]),
        Err(FramedEncodeError::OutputPending)
    ));
    assert!(matches!(
        s.resource(Default::default(), Default::default()),
        Err(FramedEncodeError::OutputPending)
    ));
    assert_eq!(
        s.process(&mut [], FramedEncodeOperation::Process)
            .unwrap()
            .status,
        FramedEncoderStatus::NeedsOutput
    );
    assert_eq!(
        s.process(&mut [0; 5], FramedEncodeOperation::Process)
            .unwrap()
            .produced,
        5
    );
    assert!(
        s.resource(
            Default::default(),
            mbrotli::StreamConfig::default().with_stream_offset(1)
        )
        .is_err()
    );
    assert_eq!(s.next_chunk_offset(), 5);
    let mut r = s.uncompressed_resource(Default::default()).unwrap();
    let p = r.process(b"1234567", &mut [], Operation::Process).unwrap();
    assert_eq!(
        (p.consumed, p.produced, p.status),
        (7, 0, FramedEncoderStatus::NeedsInput)
    );
    let p = r.process(&[], &mut [], Operation::Finish).unwrap();
    assert_eq!(p.status, FramedEncoderStatus::NeedsOutput);
    let failure = r
        .process(b"x", &mut [0; 32], Operation::Finish)
        .unwrap_err();
    assert_eq!((failure.consumed, failure.produced), (0, 0));
    assert!(matches!(
        failure.into_error(),
        FramedEncodeError::InvalidState
    ));
    assert!(r.process(&[], &mut [0; 32], Operation::Finish).is_err());
}
#[test]
fn exact_input_contracts_distinguish_resource_and_container_payload() {
    let mut owner = FramedCompressor::new(config()).unwrap();
    for (expected, payload, scope) in [(2, &b"abc"[..], "container"), (4, &b"abc"[..], "container")]
    {
        let mut s = owner.start(InputSize::Exact(expected).into()).unwrap();
        s.process(&mut [0; 5], FramedEncodeOperation::Process)
            .unwrap();
        let mut r = s
            .uncompressed_resource(ResourceOptions {
                hidden: true,
                ..Default::default()
            })
            .unwrap();
        let result = r.process(payload, &mut [0; 128], Operation::Finish);
        if expected == 2 {
            let e = result.unwrap_err();
            assert_eq!(e.consumed, 0);
            assert!(
                matches!(e.error,FramedEncodeError::InputSizeMismatch { scope:s, .. } if s == scope)
            );
        } else {
            result.unwrap();
            drop(r);
            let e = s
                .process(&mut [0; 128], FramedEncodeOperation::Finish)
                .unwrap_err();
            assert!(
                matches!(e.error,FramedEncodeError::InputSizeMismatch { scope:s, .. } if s == scope)
            );
        }
    }
    let mut r = FramedResource::from(&b"abc"[..]);
    r.stream = InputSize::Exact(4).into();
    let items = [FramedItem::Resource(r)];
    assert!(matches!(
        owner.compress(items.as_slice().into()),
        Err(FramedEncodeError::InputSizeMismatch {
            scope: "resource",
            ..
        })
    ));
}
#[test]
fn forgotten_and_dropped_guards_obey_recovery_and_retention() {
    for policy in [
        RetentionPolicy::Aggressive,
        RetentionPolicy::CurrentConfig,
        RetentionPolicy::Bounded { max_bytes: 1 },
        RetentionPolicy::ReleaseAll,
    ] {
        let mut owner = FramedCompressor::builder(config())
            .with_retention(policy)
            .build()
            .unwrap();
        assert_eq!(owner.retention(), policy);
        assert_eq!(owner.retained_bytes(), 0);
        {
            let mut s = owner.start(Default::default()).unwrap();
            s.process(&mut [0; 5], FramedEncodeOperation::Process)
                .unwrap();
            let r = s.resource(Default::default(), Default::default()).unwrap();
            std::mem::forget(r);
            assert!(matches!(
                s.padding(1),
                Err(FramedEncodeError::AbandonedResource)
            ));
            assert!(matches!(
                s.process(&mut [0; 128], FramedEncodeOperation::Finish)
                    .unwrap_err()
                    .error,
                FramedEncodeError::AbandonedResource
            ));
        }
        drop(owner.start(Default::default()).unwrap());
        std::mem::forget(owner.start(Default::default()).unwrap());
        owner.trim(RetentionPolicy::ReleaseAll);
        assert!(matches!(
            owner.start(Default::default()),
            Err(FramedEncodeError::AbandonedSession)
        ));
        let bad = config().with_framing_config(FramingConfig {
            chunk_bytes: 0,
            ..Default::default()
        });
        assert!(owner.reconfigure(bad).is_err());
        assert_eq!(*owner.config(), config());
        assert!(matches!(
            owner.start(Default::default()),
            Err(FramedEncodeError::AbandonedSession)
        ));
        owner.reconfigure(config()).unwrap();
        drop(owner.start(Default::default()).unwrap());
        std::mem::forget(owner.start(Default::default()).unwrap());
        owner.recover();
        assert_eq!(owner.retained_bytes(), 0);
        let items = [FramedItem::Resource(FramedResource::from(&b"abc"[..]))];
        let bytes = owner.compress(items.as_slice().into()).unwrap();
        let mut fork = owner.fork_empty();
        assert_eq!(fork.config(), owner.config());
        assert_eq!(fork.retained_bytes(), 0);
        assert_eq!(fork.compress(items.as_slice().into()).unwrap(), bytes);
        if matches!(
            policy,
            RetentionPolicy::ReleaseAll | RetentionPolicy::Bounded { .. }
        ) {
            assert_eq!(owner.retained_bytes(), 0);
        }
        owner.recover();
        assert_eq!(owner.retained_bytes(), 0);
    }
}
#[test]
fn late_metadata_failure_rolls_back_append_and_preserves_slice_prefix() {
    let items = [
        FramedItem::Resource(FramedResource::from(&b"abc"[..])),
        FramedItem::Metadata {
            kind: MetadataKind::Global,
            fields: &[MetadataField {
                code: *b"xx",
                value: b"bad",
            }],
            options: Default::default(),
        },
    ];
    let input = items.as_slice().into();
    let mut owner = FramedCompressor::new(config()).unwrap();
    let mut dst = b"prefix".to_vec();
    assert!(owner.compress_into(input, &mut dst).is_err());
    assert_eq!(dst, b"prefix");
    let mut dst = [0xa5; 128];
    let e = owner.compress_to_slice(input, &mut dst).unwrap_err();
    assert_eq!(e.consumed, 3);
    assert_eq!(e.location.item_index, Some(1));
    assert_eq!(e.location.wire_offset, e.produced as u64);
    assert!(e.produced > 5);
    assert!(dst[e.produced..].iter().all(|b| *b == 0xa5));
    #[cfg(not(feature = "no_std"))]
    {
        use std::io::Read;
        let mut reader = owner.framed_reader(input, Default::default()).unwrap();
        let mut dst = [0; 128];
        assert!(reader.read(&mut dst).unwrap() > 0);
        assert!(reader.read(&mut dst).is_err());
        assert!(reader.read(&mut dst).is_err());
        assert_eq!(reader.into_inner().items.len(), 2);
    }
}
#[test]
fn explicit_flush_is_repeatable_without_duplicate_empty_chunks() {
    let mut owner = FramedCompressor::new(config()).unwrap();
    let mut s = owner.start(Default::default()).unwrap();
    s.process(&mut [0; 5], FramedEncodeOperation::Process)
        .unwrap();
    let mut r = s.uncompressed_resource(Default::default()).unwrap();
    let p = r.process(b"abc", &mut [], Operation::Flush).unwrap();
    assert_eq!(p.consumed, 3);
    assert_eq!(p.status, FramedEncoderStatus::NeedsOutput);
    let p = r.process(&[], &mut [0; 128], Operation::Flush).unwrap();
    assert_eq!(p.status, FramedEncoderStatus::NeedsInput);
    let p = r.process(&[], &mut [0; 128], Operation::Flush).unwrap();
    assert_eq!(p.produced, 0);
    assert_eq!(
        r.process(&[], &mut [0; 128], Operation::Finish)
            .unwrap()
            .status,
        FramedEncoderStatus::Finished
    );
    assert_eq!(
        r.process(&[], &mut [0; 128], Operation::Finish)
            .unwrap()
            .produced,
        0
    );
}

fn structured_native(
    owner: &mut FramedCompressor,
    input: FramedInput<'_>,
    width: usize,
) -> Vec<u8> {
    fn drain(
        s: &mut FramedEncoderSession<'_>,
        wire: &mut Vec<u8>,
        width: usize,
        op: FramedEncodeOperation,
    ) {
        let mut buffer = vec![0; width];
        loop {
            let p = s.process(&mut buffer, op).unwrap();
            wire.extend_from_slice(&buffer[..p.produced]);
            if p.status != FramedEncoderStatus::NeedsOutput {
                break;
            }
        }
    }
    let mut wire = Vec::new();
    let mut s = owner.start(Default::default()).unwrap();
    drain(&mut s, &mut wire, width, FramedEncodeOperation::Process);
    if let Some(codes) = input.repeat_metadata_fields {
        s.repeat_metadata_fields(codes).unwrap();
    }
    for item in input.items {
        match *item {
            FramedItem::Metadata {
                kind,
                fields,
                options,
            } => s.metadata_with_options(kind, fields, options).unwrap(),
            FramedItem::Padding { bytes } => s.padding(bytes).unwrap(),
            FramedItem::Resource(resource) => {
                let mut r = match resource.encoding {
                    ResourceEncoding::Uncompressed => {
                        s.uncompressed_resource(resource.options).unwrap()
                    }
                    ResourceEncoding::Brotli => {
                        s.resource(resource.options, resource.stream).unwrap()
                    }
                    ResourceEncoding::Shared {
                        dictionary,
                        references,
                    } => s
                        .resource_with_dictionary(
                            resource.options,
                            resource.stream,
                            dictionary,
                            references,
                        )
                        .unwrap(),
                };
                let mut consumed = 0;
                let mut output = vec![0; width];
                loop {
                    let p = r
                        .process(&resource.data[consumed..], &mut output, Operation::Finish)
                        .unwrap();
                    consumed += p.consumed;
                    wire.extend_from_slice(&output[..p.produced]);
                    if p.status == FramedEncoderStatus::Finished {
                        break;
                    }
                }
            }
        }
        drain(&mut s, &mut wire, width, FramedEncodeOperation::Process);
    }
    drain(&mut s, &mut wire, width, FramedEncodeOperation::Finish);
    wire
}
#[test]
fn structured_drivers_match_legacy_all_chunk_types_fixture() {
    let payload = b"framed resource contents ".repeat(9);
    let items = [
        FramedItem::Metadata {
            kind: MetadataKind::Global,
            fields: &[MetadataField {
                code: *b"XX",
                value: b"global",
            }],
            options: Default::default(),
        },
        FramedItem::Metadata {
            kind: MetadataKind::Resource,
            fields: &[MetadataField {
                code: *b"id",
                value: b"example.txt",
            }],
            options: Default::default(),
        },
        FramedItem::Padding { bytes: 2 },
        FramedItem::Resource(FramedResource {
            data: &payload,
            options: Default::default(),
            stream: Default::default(),
            encoding: ResourceEncoding::Brotli,
        }),
        FramedItem::Metadata {
            kind: MetadataKind::Footer,
            fields: &[MetadataField {
                code: *b"YY",
                value: b"footer",
            }],
            options: Default::default(),
        },
        FramedItem::Resource(FramedResource {
            data: b"raw",
            options: Default::default(),
            stream: Default::default(),
            encoding: ResourceEncoding::Uncompressed,
        }),
    ];
    let config = config().with_framing_config(FramingConfig {
        chunk_bytes: 32,
        repeat_metadata: true,
        ..Default::default()
    });
    let mut owner = FramedCompressor::new(config).unwrap();
    let input = items.as_slice().into();
    let bytes = owner.compress(input).unwrap();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    assert!(include_str!("../testdata/framing-legacy/all_chunk_types_have_rfc_headers_and_the_compressed_resource_interoperates.hex").lines().any(|line|line==hex));
    for width in [1, 2, 7, 31, 4096] {
        assert_eq!(structured_native(&mut owner, input, width), bytes);
    }
    #[cfg(not(feature = "no_std"))]
    {
        use std::io::Read;
        let mut actual = Vec::new();
        owner
            .framed_reader(input, Default::default())
            .unwrap()
            .read_to_end(&mut actual)
            .unwrap();
        assert_eq!(actual, bytes);
    }
}
#[test]
fn dictionaries_metadata_selection_and_large_window_use_the_shared_driver() {
    let dictionary = mbrotli::dictionary::DictionaryBuilder::new()
        .add_prefix(&b"some dictionary words and payload"[..])
        .build()
        .unwrap();
    let references = [DictionaryReference::PrefixId(DictionaryId([7; 32]))];
    let fields = [
        MetadataField {
            code: *b"id",
            value: b"file",
        },
        MetadataField {
            code: *b"AB",
            value: b"some dictionary words and payload",
        },
    ];
    for large in [false, true] {
        let mut config = config().with_framing_config(FramingConfig {
            chunk_bytes: 7,
            repeat_metadata: true,
            ..Default::default()
        });
        if large {
            config = config.with_encoder_config(
                config
                    .encoder_config()
                    .with_window(mbrotli::Window::large(25).unwrap()),
            );
        }
        let mut owner = FramedCompressor::new(config).unwrap();
        for encoding in [
            MetadataEncoding::Uncompressed,
            MetadataEncoding::Brotli,
            MetadataEncoding::Shared {
                dictionary: &dictionary,
                references: &references,
            },
        ] {
            let items = [
                FramedItem::Metadata {
                    kind: MetadataKind::Resource,
                    fields: &fields,
                    options: MetadataOptions {
                        encoding,
                        repeated_encoding: encoding,
                    },
                },
                FramedItem::Resource(FramedResource {
                    data: b"some dictionary words and payload",
                    options: ResourceOptions {
                        hidden: true,
                        id: Some(DictionaryId([9; 32])),
                    },
                    stream: Default::default(),
                    encoding: ResourceEncoding::Shared {
                        dictionary: &dictionary,
                        references: &references,
                    },
                }),
                FramedItem::Metadata {
                    kind: MetadataKind::Footer,
                    fields: &fields[1..],
                    options: MetadataOptions {
                        encoding,
                        repeated_encoding: encoding,
                    },
                },
            ];
            for codes in [None, Some(&[][..]), Some(&[*b"id"][..])] {
                let input = FramedInput {
                    items: &items,
                    repeat_metadata_fields: codes,
                };
                let bytes = owner.compress(input).unwrap();
                assert_eq!(structured_native(&mut owner, input, 1), bytes);
                #[cfg(not(feature = "no_std"))]
                {
                    use std::io::Read;
                    let mut actual = Vec::new();
                    owner
                        .framed_reader(input, Default::default())
                        .unwrap()
                        .read_to_end(&mut actual)
                        .unwrap();
                    assert_eq!(actual, bytes);
                }
            }
        }
    }
}
#[test]
fn dictionary_borrow_ends_with_resource_and_forgotten_guard_retains_no_borrow() {
    let mut owner = FramedCompressor::new(config()).unwrap();
    let mut s = owner.start(Default::default()).unwrap();
    s.process(&mut [0; 5], FramedEncodeOperation::Process)
        .unwrap();
    {
        let dictionary = mbrotli::dictionary::DictionaryBuilder::new()
            .add_prefix(&b"dictionary"[..])
            .build()
            .unwrap();
        let mut r = s
            .resource_with_dictionary(
                Default::default(),
                Default::default(),
                &dictionary,
                &[DictionaryReference::PrefixId(DictionaryId([1; 32]))],
            )
            .unwrap();
        r.process(b"abc", &mut [0; 512], Operation::Finish).unwrap();
    }
    s.metadata(MetadataKind::Global, &[]).unwrap();
    assert_eq!(
        s.process(&mut [0; 512], FramedEncodeOperation::Finish)
            .unwrap()
            .status,
        FramedEncoderStatus::Finished
    );
    drop(s);
    {
        let mut s = owner.start(Default::default()).unwrap();
        s.process(&mut [0; 5], FramedEncodeOperation::Process)
            .unwrap();
        {
            let dictionary = mbrotli::dictionary::DictionaryBuilder::new()
                .add_prefix(&b"dictionary"[..])
                .build()
                .unwrap();
            let mut r = s
                .resource_with_dictionary(
                    Default::default(),
                    Default::default(),
                    &dictionary,
                    &[DictionaryReference::PrefixId(DictionaryId([1; 32]))],
                )
                .unwrap();
            r.process(b"dictionary dictionary", &mut [0; 512], Operation::Process)
                .unwrap();
            std::mem::forget(r);
        }
        std::mem::forget(s);
    }
    owner.recover();
    assert_eq!(owner.retained_bytes(), 0);
    assert!(
        owner
            .compress(
                [FramedItem::Resource(FramedResource::from(&b"next"[..]))]
                    .as_slice()
                    .into()
            )
            .is_ok()
    );
}
#[test]
fn empty_and_footerless_profiles_preserve_structure_and_selection_validation() {
    let mut owner = FramedCompressor::new(config()).unwrap();
    assert!(owner.compress([].as_slice().into()).is_ok());
    assert!(
        owner
            .compress(FramedInput {
                items: &[],
                repeat_metadata_fields: Some(&[])
            })
            .is_err()
    );
    owner
        .reconfigure(config().with_framing_config(FramingConfig {
            container: false,
            central_directory: false,
            ..Default::default()
        }))
        .unwrap();
    assert!(owner.compress([].as_slice().into()).is_err());
    let r = FramedItem::Resource(FramedResource {
        encoding: ResourceEncoding::Uncompressed,
        ..FramedResource::from(&b""[..])
    });
    let items = [r];
    assert_eq!(
        owner.compress(items.as_slice().into()).unwrap(),
        [0x91, 10, 66, 82, 0, 3, 2, 0, 0]
    );
    assert!(owner.compress([r, r].as_slice().into()).is_err());
}

#[test]
fn stream_configuration_and_slice_abandonment_report_without_progress() {
    let stream = FramedEncodeStreamConfig::default().with_input_size(InputSize::Exact(3));
    assert_eq!(stream.input_size(), InputSize::Exact(3));
    let mut owner = FramedCompressor::new(config()).unwrap();
    std::mem::forget(owner.start(stream).unwrap());
    let failure = owner
        .compress_to_slice([].as_slice().into(), &mut [0; 128])
        .unwrap_err();
    assert!(matches!(failure.error, FramedEncodeError::AbandonedSession));
    assert_eq!((failure.consumed, failure.produced), (0, 0));
}

#[test]
fn late_chunk_limit_reports_exact_partial_input_and_output() {
    let mut owner = FramedCompressor::new(config().with_framing_config(FramingConfig {
        chunk_bytes: 7,
        max_chunks: 1,
        ..Default::default()
    }))
    .unwrap();
    let mut s = owner.start(Default::default()).unwrap();
    s.process(&mut [0; 5], FramedEncodeOperation::Process)
        .unwrap();
    let mut r = s.uncompressed_resource(Default::default()).unwrap();
    let mut output = [0xa5; 128];
    let e = r
        .process(&[b'x'; 21], &mut output, Operation::Finish)
        .unwrap_err();
    assert_eq!(e.consumed, 14);
    assert_eq!(e.produced, 11);
    assert_eq!(e.location.resource_input_offset, Some(14));
    assert!(output[e.produced..].iter().all(|b| *b == 0xa5));
    assert!(matches!(
        e.error,
        FramedEncodeError::Limit {
            kind: "chunk count",
            limit: 1
        }
    ));
    assert!(r.process(&[], &mut output, Operation::Finish).is_err());
}
#[cfg(not(feature = "no_std"))]
#[test]
fn writer_defers_failure_after_acceptance_and_recovers_oversized_sink_counts() {
    use std::io::{self, Write};
    let mut owner = FramedCompressor::new(config().with_framing_config(FramingConfig {
        chunk_bytes: 7,
        max_chunks: 1,
        ..Default::default()
    }))
    .unwrap();
    {
        let mut writer = owner.framed_writer(Vec::new(), Default::default()).unwrap();
        let mut r = writer.uncompressed_resource(Default::default()).unwrap();
        assert_eq!(r.write(&[b'x'; 21]).unwrap(), 14);
        assert!(r.write(b"x").is_err());
    }
    #[derive(Debug, Default)]
    struct Oversized {
        bad: bool,
        bytes: Vec<u8>,
    }
    impl Write for Oversized {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.bad {
                Ok(bytes.len() + 1)
            } else {
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    owner.reconfigure(config()).unwrap();
    let expected = owner.compress([].as_slice().into()).unwrap();
    let mut writer = owner
        .framed_writer(
            Oversized {
                bad: true,
                ..Default::default()
            },
            Default::default(),
        )
        .unwrap();
    assert!(matches!(writer.try_finish(), Err(FramedEncodeError::Io(_))));
    writer.get_mut().bad = false;
    assert_eq!(writer.finish().unwrap().bytes, expected);
}

#[test]
fn every_reference_form_keeps_container_relative_offsets_in_structured_apis() {
    let dictionary = mbrotli::dictionary::DictionaryBuilder::new()
        .add_prefix(&b"dictionary"[..])
        .build()
        .unwrap();
    let refs = [
        [DictionaryReference::PrefixId(DictionaryId([1; 32]))],
        [DictionaryReference::SerializedId(DictionaryId([2; 32]))],
        [DictionaryReference::PrefixResource(5)],
        [DictionaryReference::SerializedResource(5)],
        [DictionaryReference::PrefixChunk(5)],
    ];
    let mut items = vec![FramedItem::Resource(FramedResource {
        data: b"dictionary",
        options: ResourceOptions {
            hidden: true,
            ..Default::default()
        },
        stream: Default::default(),
        encoding: ResourceEncoding::Uncompressed,
    })];
    for references in &refs {
        items.push(FramedItem::Resource(FramedResource {
            data: b"dictionary dictionary",
            options: Default::default(),
            stream: Default::default(),
            encoding: ResourceEncoding::Shared {
                dictionary: &dictionary,
                references,
            },
        }));
    }
    let mut owner = FramedCompressor::new(config().with_framing_config(FramingConfig {
        chunk_bytes: 32,
        ..Default::default()
    }))
    .unwrap();
    let input = items.as_slice().into();
    let expected = owner.compress(input).unwrap();
    assert_eq!(structured_native(&mut owner, input, 1), expected);
    let mut appended = b"caller prefix".to_vec();
    let range = owner.compress_into(input, &mut appended).unwrap();
    assert_eq!(&appended[range], expected);
    let mut dst = vec![0; expected.len()];
    assert_eq!(
        owner.compress_to_slice(input, &mut dst).unwrap(),
        expected.len()
    );
    assert_eq!(dst, expected);
    #[cfg(not(feature = "no_std"))]
    {
        use std::io::Read;
        let mut actual = Vec::new();
        owner
            .framed_reader(input, Default::default())
            .unwrap()
            .read_to_end(&mut actual)
            .unwrap();
        assert_eq!(actual, expected);
    }
}

#[test]
fn typed_configuration_and_codec_failures_preserve_source_chains() {
    use std::error::Error;
    let invalid = FramedEncodeConfig::default().with_encoder_config(
        EncoderConfig::default()
            .with_quality(Quality::Q0)
            .with_window(mbrotli::Window::large(25).unwrap()),
    );
    let error = FramedCompressor::new(invalid).unwrap_err();
    assert!(matches!(error, FramedEncodeError::Config(_)));
    assert!(error.source().is_some());
    let dictionary = mbrotli::dictionary::DictionaryBuilder::new()
        .add_prefix(&b"dictionary"[..])
        .build()
        .unwrap();
    let mut owner = FramedCompressor::new(
        config().with_encoder_config(EncoderConfig::default().with_quality(Quality::Q0)),
    )
    .unwrap();
    let items = [FramedItem::Resource(FramedResource {
        data: b"dictionary",
        options: Default::default(),
        stream: Default::default(),
        encoding: ResourceEncoding::Shared {
            dictionary: &dictionary,
            references: &[DictionaryReference::PrefixId(DictionaryId([7; 32]))],
        },
    })];
    let failure = owner
        .compress_to_slice(items.as_slice().into(), &mut [0; 512])
        .unwrap_err();
    assert_eq!(failure.location.resource_index, Some(0));
    assert_eq!(failure.location.resource_input_offset, Some(0));
    assert!(matches!(failure.error, FramedEncodeError::Encode(_)));
    assert!(failure.source().unwrap().source().is_some());
    assert!(owner.compress([].as_slice().into()).is_ok());
    #[cfg(not(feature = "no_std"))]
    {
        let error = FramedEncodeError::Io(std::io::ErrorKind::WouldBlock.into());
        assert_eq!(
            std::io::Error::from(error).kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[test]
fn small_repeated_metadata_headers_fit_the_existing_framing_budget() {
    let config = config().with_framing_config(FramingConfig {
        chunk_bytes: 31,
        repeat_metadata: true,
        max_buffer_bytes: 128 << 10,
        ..Default::default()
    });
    let mut items = Vec::new();
    for _ in 0..150 {
        items.push(FramedItem::Metadata {
            kind: MetadataKind::Resource,
            fields: &[],
            options: Default::default(),
        });
        items.push(FramedItem::Resource(FramedResource {
            encoding: ResourceEncoding::Uncompressed,
            ..FramedResource::from(&b""[..])
        }));
    }
    assert!(
        FramedCompressor::new(config)
            .unwrap()
            .compress(items.as_slice().into())
            .is_ok()
    );
}

#[test]
fn explicit_unknown_resource_hint_is_preserved_independently_of_aggregate_size() {
    let data = (0..40000)
        .map(|i| {
            format!(
                "resource entry {} has dictionary words {}\n",
                i % 500,
                (i * 13) % 97
            )
        })
        .collect::<String>()
        .into_bytes();
    assert!(data.len() >= 1 << 20);
    let config = FramedEncodeConfig::default()
        .with_encoder_config(EncoderConfig::default().with_quality(Quality::Q4));
    let mut owner = FramedCompressor::new(config).unwrap();
    let exact = [FramedItem::Resource(FramedResource::from(data.as_slice()))];
    let unknown = [FramedItem::Resource(FramedResource {
        stream: Default::default(),
        ..FramedResource::from(data.as_slice())
    })];
    let exact_bytes = owner.compress(exact.as_slice().into()).unwrap();
    let unknown_bytes = owner.compress(unknown.as_slice().into()).unwrap();
    assert_ne!(
        unknown_bytes, exact_bytes,
        "corpus must exercise different size-hint decisions"
    );
    assert_eq!(
        structured_native(&mut owner, unknown.as_slice().into(), 31),
        unknown_bytes
    );
}

#[test]
fn large_resources_preserve_bytes_under_deterministic_random_backpressure() {
    let mut rng = 0xa076_1d64_78bd_642fu64;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng as usize
    };
    let data: Vec<u8> = (0..65_553).map(|_| next() as u8).collect();
    let framing = FramingConfig {
        chunk_bytes: 4096,
        ..Default::default()
    };
    let mut owner = FramedCompressor::new(config().with_framing_config(framing)).unwrap();
    for stored in [false, true] {
        let mut description = FramedResource::from(data.as_slice());
        if stored {
            description.encoding = ResourceEncoding::Uncompressed;
        }
        let items = [FramedItem::Resource(description)];
        let expected = owner.compress(items.as_slice().into()).unwrap();
        for _ in 0..4 {
            let mut session = owner.start(Default::default()).unwrap();
            let mut output = [0; 4096];
            let mut actual = Vec::new();
            loop {
                let width = [0, 1, 7, 31, 4096][next() % 5];
                let p = session
                    .process(&mut output[..width], FramedEncodeOperation::Process)
                    .unwrap();
                actual.extend_from_slice(&output[..p.produced]);
                if p.status == FramedEncoderStatus::NeedsInput {
                    break;
                }
            }
            {
                let mut resource = if stored {
                    session.uncompressed_resource(Default::default()).unwrap()
                } else {
                    session
                        .resource(Default::default(), description.stream)
                        .unwrap()
                };
                let mut cursor = 0;
                let mut end = 0;
                loop {
                    if cursor == end && end < data.len() {
                        end = (end + 1 + next() % 8192).min(data.len());
                    }
                    let width = [0, 1, 7, 31, 4096][next() % 5];
                    let p = resource
                        .process(
                            &data[cursor..end],
                            &mut output[..width],
                            if end == data.len() {
                                Operation::Finish
                            } else {
                                Operation::Process
                            },
                        )
                        .unwrap();
                    cursor += p.consumed;
                    actual.extend_from_slice(&output[..p.produced]);
                    if p.status == FramedEncoderStatus::Finished {
                        assert_eq!(cursor, data.len());
                        break;
                    }
                }
            }
            loop {
                let width = [0, 1, 7, 31, 4096][next() % 5];
                let p = session
                    .process(&mut output[..width], FramedEncodeOperation::Finish)
                    .unwrap();
                actual.extend_from_slice(&output[..p.produced]);
                if p.status == FramedEncoderStatus::Finished {
                    break;
                }
            }
            assert_eq!(actual, expected);
        }
    }
}
