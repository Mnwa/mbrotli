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
    let mut encoder = mbrotli::Compressor::new(Default::default()).unwrap();
    let mut writer = encoder
        .framed_writer(
            Vec::new(),
            FramingConfig {
                chunk_bytes: 31,
                repeat_metadata: true,
                ..Default::default()
            },
        )
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
