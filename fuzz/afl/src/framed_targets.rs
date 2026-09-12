//! Experimental framing parser and structured round-trip oracles.
use crate::Context;
use mbrotli::DecodeOperation;
use mbrotli::framing::*;
use std::io::Write;

fn config() -> FramedDecodeConfig {
    FramedDecodeConfig::default()
        .with_input_mode(InputMode::Auto)
        .with_limits(
            FramedDecodeLimits::default()
                .with_max_output_bytes(Some(1 << 20))
                .with_max_decoded_bytes(Some(2 << 20))
                .with_max_chunks(Some(4096))
                .with_max_resources(Some(1024)),
        )
}
/// Arbitrary bytes: bounded parsing, progress, backend equivalence and Vec rollback.
pub fn framed_decode(_ctx: &Context, data: &[u8]) {
    let data = &data[..data.len().min(65536)];
    let mut decoder = FramedDecompressor::new(config()).unwrap();
    let expected = decoder.decompress(data);
    if let Ok(output) = &expected
        && matches!(output.structure, OutputStructure::Raw)
    {
        let mut raw = mbrotli::Decompressor::new(Default::default()).unwrap();
        assert_eq!(raw.decompress(data).unwrap(), output.resources[0].data);
    }
    // Opening is structural only: malformed untouched payload can remain accepted.
    // For sequentially valid objects, every indexed resource must match exactly.
    if let Ok(mut indexed) = decoder.framed_seek_reader(std::io::Cursor::new(data)) {
        use std::io::Read;
        for i in (0..indexed.resources().len().min(16)).rev() {
            let index = ResourceIndex(i as u64);
            if let Ok(mut resource) = indexed.resource(index) {
                let _ = resource.read(&mut [0; 1]);
            }
            if let Ok(mut resource) = indexed.resource(index) {
                let mut payload = Vec::new();
                let decoded = resource.read_to_end(&mut payload);
                if let Ok(expected) = &expected {
                    decoded.unwrap();
                    assert_eq!(payload, expected.resources[i].data);
                }
            } else {
                assert!(expected.is_err());
            }
            let metadata = indexed.resource_metadata(index);
            if let Ok(expected) = &expected {
                assert_eq!(metadata.unwrap(), expected.resources[i].metadata.as_ref());
            }
            let metadata = indexed.resource_footer_metadata(index);
            if let Ok(expected) = &expected {
                assert_eq!(
                    metadata.unwrap(),
                    expected.resources[i].footer_metadata.as_ref()
                );
            }
        }
    }
    let mut prefix = b"preserved prefix".to_vec();
    let appended = decoder.decompress_into(data, &mut prefix);
    assert_eq!(expected.is_ok(), appended.is_ok());
    if appended.is_err() {
        assert_eq!(prefix, b"preserved prefix");
    }
    for backend in mbrotli::Backend::available() {
        let mut decoder = FramedDecompressor::builder(config())
            .with_backend(backend)
            .build()
            .unwrap();
        let mut session = decoder.start(Default::default()).unwrap();
        let mut cursor = 0;
        let mut payload = Vec::new();
        let mut done = false;
        let stride = usize::from(data.first().copied().unwrap_or(0) % 31) + 1;
        let mut end = stride.min(data.len());
        for step in 0..2_200_000 {
            let mut output = [0; 32];
            let capacity = if step % 17 == 0 { 0 } else { 1 + step % 32 };
            let operation = if end == data.len() {
                DecodeOperation::Finish
            } else {
                DecodeOperation::Process
            };
            match session.process(&data[cursor..end], &mut output[..capacity], operation) {
                Err(failure) => {
                    assert!(failure.consumed <= end - cursor);
                    assert!(failure.produced <= capacity);
                    assert!(
                        expected.is_err(),
                        "valid one-shot input failed incrementally: {failure:?}"
                    );
                    assert_eq!(failure.last_output.is_some(), failure.produced != 0);
                    done = true;
                    break;
                }
                Ok(progress) => {
                    assert!(progress.consumed <= end - cursor);
                    assert!(progress.produced <= capacity);
                    cursor += progress.consumed;
                    match progress.status {
                        FramedDecoderStatus::Event(FramedEvent::ResourceData(fragment)) => {
                            assert_eq!(fragment.bytes.len(), progress.produced);
                            payload.extend_from_slice(fragment.bytes);
                        }
                        FramedDecoderStatus::Finished => {
                            assert_eq!((progress.consumed, progress.produced), (0, 0));
                            if cursor == data.len()
                                && let Ok(expected) = &expected
                            {
                                assert!(
                                    expected
                                        .resources
                                        .iter()
                                        .flat_map(|r| r.data.iter())
                                        .copied()
                                        .eq(payload)
                                );
                            }
                            done = true;
                            break;
                        }
                        FramedDecoderStatus::NeedsInput => {
                            assert_eq!(cursor, end);
                            end = (end + stride).min(data.len());
                        }
                        _ => assert_eq!(progress.produced, 0),
                    }
                }
            }
        }
        assert!(done, "framing zero-progress loop");
    }
}
/// Structured writer inputs exercise valid compressed chunks and repeated metadata.
pub fn framed_roundtrip(_ctx: &Context, input: &[u8]) {
    let data = &input[..input.len().min(4096)];
    let mut framed_encoder = mbrotli::framing::FramedCompressor::new(
        mbrotli::framing::FramedEncodeConfig::default().with_framing_config(FramingConfig {
            chunk_bytes: 31,
            repeat_metadata: true,
            ..Default::default()
        }),
    )
    .expect("framed configuration");
    let mut writer = framed_encoder
        .framed_writer(Vec::new(), Default::default())
        .unwrap();
    writer
        .metadata(
            MetadataKind::Resource,
            &[MetadataField {
                code: *b"id",
                value: b"fuzz",
            }],
        )
        .unwrap();
    {
        let mut resource = writer
            .resource(Default::default(), Default::default())
            .unwrap();
        resource.write_all(data).unwrap();
        resource.try_finish().unwrap();
    }
    let bytes = writer.finish().unwrap();
    let mut decoder = FramedDecompressor::new(config()).unwrap();
    let result = decoder.decompress(&bytes).unwrap();
    assert_eq!(result.resources.len(), 1);
    assert_eq!(result.resources[0].data, data);
    framed_decode(_ctx, &bytes);
}

/// Native resource schedules agree with Write and, without explicit Flush, one-shot/Read.
pub fn framed_encode(ctx: &Context, input: &[u8]) {
    use mbrotli::framing::*;
    use mbrotli::{EncoderConfig, InputSize, Operation, Quality};
    use std::io::{Read, Write};
    let selector = input.first().copied().unwrap_or(0);
    let data = &input[input.len().min(1)..input.len().min(1025)];
    let config = FramedEncodeConfig::default()
        .with_encoder_config(EncoderConfig::default().with_quality(Quality::Q5))
        .with_framing_config(FramingConfig {
            chunk_bytes: 1 + usize::from(selector % 31),
            ..Default::default()
        });
    let mut owner = FramedCompressor::builder(config)
        .with_backend(ctx.level)
        .build()
        .unwrap();
    let dictionary = mbrotli::dictionary::DictionaryBuilder::new()
        .add_prefix(&b"dictionary payload words"[..])
        .build()
        .unwrap();
    let references = [DictionaryReference::PrefixId(DictionaryId([7; 32]))];
    let encoding = if selector & 8 != 0 {
        ResourceEncoding::Shared {
            dictionary: &dictionary,
            references: &references,
        }
    } else if selector & 4 != 0 {
        ResourceEncoding::Uncompressed
    } else {
        ResourceEncoding::Brotli
    };
    let options = ResourceOptions {
        hidden: selector & 16 != 0,
        id: (selector & 32 != 0).then_some(DictionaryId([9; 32])),
    };
    let stream = InputSize::Exact(data.len() as u64).into();
    let split = data.len() / 2;
    let flush = selector & 1 != 0;
    let width = [1, 2, 7, 31][usize::from(selector >> 6)];
    let mut buffer = vec![0; width];
    let mut native = Vec::new();
    {
        let mut session = owner
            .start(InputSize::Exact(data.len() as u64).into())
            .unwrap();
        assert!(matches!(
            session.padding(0),
            Err(FramedEncodeError::OutputPending)
        ));
        assert_eq!(
            session
                .process(&mut [], FramedEncodeOperation::Process)
                .unwrap()
                .status,
            FramedEncoderStatus::NeedsOutput
        );
        loop {
            let p = session
                .process(&mut buffer, FramedEncodeOperation::Process)
                .unwrap();
            native.extend_from_slice(&buffer[..p.produced]);
            if p.status != FramedEncoderStatus::NeedsOutput {
                break;
            }
        }
        assert!(
            session
                .metadata(
                    MetadataKind::Global,
                    &[MetadataField {
                        code: *b"xx",
                        value: data
                    }]
                )
                .is_err()
        );
        assert!(
            session
                .resource_with_dictionary(
                    Default::default(),
                    stream,
                    &dictionary,
                    &[DictionaryReference::PrefixResource(u64::from(selector))]
                )
                .is_err()
        );
        {
            let mut resource = match encoding {
                ResourceEncoding::Uncompressed => session.uncompressed_resource(options).unwrap(),
                ResourceEncoding::Brotli => session.resource(options, stream).unwrap(),
                ResourceEncoding::Shared {
                    dictionary,
                    references,
                } => session
                    .resource_with_dictionary(options, stream, dictionary, references)
                    .unwrap(),
            };
            if flush {
                let mut p = 0;
                loop {
                    let progress = resource
                        .process(&data[p..split], &mut buffer, Operation::Flush)
                        .unwrap();
                    p += progress.consumed;
                    native.extend_from_slice(&buffer[..progress.produced]);
                    if progress.status == FramedEncoderStatus::NeedsInput {
                        break;
                    }
                }
            }
            let mut p = if flush { split } else { 0 };
            loop {
                let progress = resource
                    .process(&data[p..], &mut buffer, Operation::Finish)
                    .unwrap();
                p += progress.consumed;
                native.extend_from_slice(&buffer[..progress.produced]);
                if progress.status == FramedEncoderStatus::Finished {
                    break;
                }
            }
        }
        loop {
            let p = session
                .process(&mut buffer, FramedEncodeOperation::Finish)
                .unwrap();
            native.extend_from_slice(&buffer[..p.produced]);
            if p.status == FramedEncoderStatus::Finished {
                break;
            }
        }
    }
    {
        let mut writer = owner.framed_writer(Vec::new(), Default::default()).unwrap();
        {
            let mut resource = match encoding {
                ResourceEncoding::Uncompressed => writer.uncompressed_resource(options).unwrap(),
                ResourceEncoding::Brotli => writer.resource(options, stream).unwrap(),
                ResourceEncoding::Shared {
                    dictionary,
                    references,
                } => writer
                    .resource_with_dictionary(options, stream, dictionary, references)
                    .unwrap(),
            };
            resource.write_all(&data[..split]).unwrap();
            if flush {
                resource.flush().unwrap();
            }
            resource.write_all(&data[split..]).unwrap();
            resource.try_finish().unwrap();
        }
        assert_eq!(writer.finish().unwrap(), native);
    }
    if !flush {
        let items = [FramedItem::Resource(FramedResource {
            data,
            options,
            stream,
            encoding,
        })];
        let input = items.as_slice().into();
        assert_eq!(owner.compress(input).unwrap(), native);
        let mut actual = Vec::new();
        owner
            .framed_reader(input, Default::default())
            .unwrap()
            .read_to_end(&mut actual)
            .unwrap();
        assert_eq!(actual, native);
    }
    if !matches!(encoding, ResourceEncoding::Shared { .. }) {
        assert_eq!(
            FramedDecompressor::new(Default::default())
                .unwrap()
                .decompress(&native)
                .unwrap()
                .resources[0]
                .data,
            data
        );
    }
}
