//! RFC framing wire fixtures and recovery under programmable sink faults.
#![cfg(feature = "experimental")]
mod support;

use mbrotli::framing::{FramingConfig, FramingError, MetadataField, MetadataKind, ResourceOptions};
use mbrotli::{Compressor, EncoderConfig, Quality};
use std::io::{self, Write};

fn number(bytes: &[u8], cursor: &mut usize) -> u64 {
    let mut value = 0;
    for shift in (0..63).step_by(7) {
        let byte = bytes[*cursor];
        *cursor += 1;
        value |= u64::from(byte & 127) << shift;
        if byte < 128 {
            return value;
        }
    }
    panic!("overlong varint")
}

fn chunks(bytes: &[u8]) -> Vec<(usize, &[u8])> {
    assert_eq!(&bytes[..4], &[0x91, 10, 66, 82]);
    let mut cursor = 5;
    let mut result = Vec::new();
    while cursor < bytes.len() {
        let start = cursor;
        let length = number(bytes, &mut cursor) as usize;
        let end = cursor.checked_add(length).expect("bounded");
        result.push((start, &bytes[cursor..end]));
        cursor = end;
    }
    assert_eq!(cursor, bytes.len());
    result
}

fn config() -> FramingConfig {
    FramingConfig {
        chunk_bytes: 32,
        repeat_metadata: true,
        ..FramingConfig::default()
    }
}

fn build<W: Write + std::fmt::Debug>(writer: W) -> W {
    let mut compressor =
        Compressor::new(EncoderConfig::default().with_quality(Quality::Q5)).expect("config");
    let mut container = compressor
        .framed_writer(writer, config())
        .expect("container");
    container
        .metadata(
            MetadataKind::Global,
            &[MetadataField {
                code: *b"XX",
                value: b"global",
            }],
        )
        .expect("global");
    container
        .metadata(
            MetadataKind::Resource,
            &[MetadataField {
                code: *b"id",
                value: b"example.txt",
            }],
        )
        .expect("metadata");
    container.padding(2).expect("padding");
    {
        let mut resource = container
            .resource(ResourceOptions::default(), Default::default())
            .expect("resource");
        resource
            .write_all(&b"framed resource contents ".repeat(9))
            .expect("write");
        resource.try_finish().expect("finish resource");
    }
    container
        .metadata(
            MetadataKind::Footer,
            &[MetadataField {
                code: *b"YY",
                value: b"footer",
            }],
        )
        .expect("footer");
    {
        let mut resource = container
            .uncompressed_resource(ResourceOptions::default())
            .expect("resource");
        resource.write_all(b"raw").expect("write");
        resource.try_finish().expect("finish");
    }
    container.finish().expect("finish container")
}

#[test]
fn all_chunk_types_have_rfc_headers_and_the_compressed_resource_interoperates() {
    let bytes = build(Vec::new());
    assert_eq!(bytes[4], 4); // Main header/footer interpretation of RFC bit 2.
    let parsed = chunks(&bytes);
    for kind in 0..=10 {
        assert!(parsed.iter().any(|(_, c)| c[0] == kind), "type {kind}");
    }
    let mut compressed = Vec::new();
    let mut size = 0;
    for (_, chunk) in &parsed {
        if matches!(chunk[0], 3..=5) {
            assert_eq!(chunk[1], if chunk[0] == 3 { 2 } else { 1 });
            let mut cursor = 2;
            size += number(chunk, &mut cursor);
            assert_eq!(chunk[cursor], 0);
            cursor += 1;
            compressed.extend_from_slice(&chunk[cursor..]);
        }
    }
    let expected = b"framed resource contents ".repeat(9);
    assert_eq!(size as usize, expected.len());
    assert_eq!(
        support::c_decompress(&compressed, expected.len()),
        Some(expected)
    );
    let (directory_offset, directory) = parsed.iter().find(|(_, c)| c[0] == 9).expect("directory");
    let mut cursor = 1;
    let repeat = number(directory, &mut cursor) as usize;
    assert_eq!(
        parsed
            .iter()
            .find(|(offset, _)| *offset == repeat)
            .expect("repeat pointer")
            .1[0],
        8
    );
    let mut entries = 0;
    while cursor < directory.len() {
        let offset = number(directory, &mut cursor) as usize;
        let length = number(directory, &mut cursor) as usize;
        assert_eq!(
            &directory[cursor..cursor + length],
            &bytes[offset..offset + length]
        );
        cursor += length;
        entries += 1;
    }
    assert_eq!(
        entries,
        parsed.iter().filter(|(_, c)| matches!(c[0], 1..=7)).count()
    );
    let footer = parsed.last().expect("footer").1;
    let reversed: Vec<_> = footer[1..].iter().rev().copied().collect();
    let mut cursor = 0;
    assert_eq!(number(&reversed, &mut cursor), *directory_offset as u64);
    assert_eq!(number(&reversed, &mut cursor), bytes.len() as u64);
}

#[derive(Debug, Default)]
struct FaultSink {
    bytes: Vec<u8>,
    budget: usize,
    blocked: bool,
    zero: bool,
    interrupted: bool,
}
impl Write for FaultSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.interrupted {
            self.interrupted = false;
            return Err(io::ErrorKind::Interrupted.into());
        }
        if self.blocked || self.budget == 0 {
            return if self.zero {
                Ok(0)
            } else {
                Err(io::ErrorKind::WouldBlock.into())
            };
        }
        let count = bytes.len().min(self.budget).min(3);
        self.bytes.extend_from_slice(&bytes[..count]);
        self.budget -= count;
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.blocked {
            Err(io::ErrorKind::WouldBlock.into())
        } else {
            Ok(())
        }
    }
}

#[test]
fn every_partial_sink_failure_can_be_retried_without_losing_or_duplicating_bytes() {
    let payload = b"writer fault schedule contents".repeat(4);
    let mut expected = None;
    for fail_at in 0..150 {
        for zero in [false, true] {
            let mut compressor =
                Compressor::new(EncoderConfig::default().with_quality(Quality::Q5))
                    .expect("config");
            let sink = FaultSink {
                budget: fail_at,
                zero,
                interrupted: true,
                ..Default::default()
            };
            let mut container = compressor.framed_writer(sink, config()).expect("container");
            if container.flush().is_err() {
                container.get_mut().budget = usize::MAX;
                container.flush().expect("retry header");
            }
            {
                let mut resource = container
                    .resource(ResourceOptions::default(), Default::default())
                    .expect("resource");
                let mut consumed = 0;
                while consumed < payload.len() {
                    match resource.write(&payload[consumed..]) {
                        Ok(count) => {
                            assert!(count > 0);
                            consumed += count;
                        }
                        Err(error) => {
                            assert!(matches!(
                                error.kind(),
                                io::ErrorKind::WouldBlock | io::ErrorKind::WriteZero
                            ));
                            resource.get_mut().budget = usize::MAX;
                        }
                    }
                }
                if resource.try_finish().is_err() {
                    resource.get_mut().budget = usize::MAX;
                    resource.try_finish().expect("retry last chunk");
                }
                resource.try_finish().expect("idempotent resource finish");
            }
            if container.try_finish().is_err() {
                container.get_mut().budget = usize::MAX;
                container.try_finish().expect("retry footer");
            }
            container.try_finish().expect("idempotent container finish");
            let bytes = container.finish().expect("finish").bytes;
            if let Some(expected) = &expected {
                assert_eq!(&bytes, expected, "offset {fail_at}, zero {zero}");
            } else {
                expected = Some(bytes);
            }
        }
    }
}

#[test]
fn single_resource_empty_wire_fixture_is_canonical() {
    let mut compressor = Compressor::new(Default::default()).expect("config");
    let config = FramingConfig {
        container: false,
        central_directory: false,
        ..Default::default()
    };
    let mut writer = compressor
        .framed_writer(Vec::new(), config)
        .expect("container");
    writer
        .uncompressed_resource(ResourceOptions::default())
        .expect("resource")
        .try_finish()
        .expect("empty");
    assert_eq!(
        writer.finish().expect("finish"),
        [0x91, 10, 66, 82, 0, 3, 2, 0, 0]
    );
}

#[test]
fn abandoned_resources_and_invalid_metadata_cannot_produce_a_finished_container() {
    let mut compressor = Compressor::new(Default::default()).expect("config");
    let mut writer = compressor
        .framed_writer(Vec::new(), config())
        .expect("container");
    assert!(
        writer
            .metadata(
                MetadataKind::Global,
                &[MetadataField {
                    code: *b"id",
                    value: b"invalid"
                }]
            )
            .is_err()
    );
    assert!(writer.metadata(MetadataKind::Footer, &[]).is_err());
    drop(
        writer
            .resource(ResourceOptions::default(), Default::default())
            .expect("resource"),
    );
    assert!(matches!(writer.try_finish(), Err(FramingError::Invalid(_))));
    drop(writer);
    assert!(compressor.compress(b"fresh stream").is_ok());
}

#[test]
fn dictionary_references_and_explicit_ids_have_their_rfc_wire_forms() {
    use mbrotli::dictionary::DictionaryBuilder;
    use mbrotli::framing::{DictionaryId, DictionaryReference};
    let source = b"a prefix dictionary with useful repeated words";
    let dictionary = DictionaryBuilder::default()
        .add_prefix(&source[..])
        .build()
        .expect("dictionary");
    let mut compressor =
        Compressor::new(EncoderConfig::default().with_quality(Quality::Q5)).expect("config");
    let mut container = compressor
        .framed_writer(Vec::new(), config())
        .expect("container");
    let offset = container.next_chunk_offset();
    {
        let mut resource = container
            .uncompressed_resource(ResourceOptions {
                hidden: true,
                id: None,
            })
            .expect("dictionary resource");
        resource.write_all(source).expect("write");
        resource.try_finish().expect("finish");
    }
    for reference in [
        DictionaryReference::PrefixId(DictionaryId([7; 32])),
        DictionaryReference::SerializedId(DictionaryId([8; 32])),
        DictionaryReference::PrefixResource(offset),
        DictionaryReference::SerializedResource(offset),
        DictionaryReference::PrefixChunk(offset),
    ] {
        let mut resource = container
            .resource_with_dictionary(
                ResourceOptions {
                    hidden: false,
                    id: Some(DictionaryId([9; 32])),
                },
                Default::default(),
                &dictionary,
                &[reference],
            )
            .expect("reference");
        resource.write_all(b"useful repeated words").expect("write");
        resource.flush().expect("flush");
        resource.try_finish().expect("finish");
    }
    assert!(
        container
            .resource_with_dictionary(
                Default::default(),
                Default::default(),
                &dictionary,
                &[DictionaryReference::PrefixResource(u64::MAX)]
            )
            .is_err()
    );
    assert!(
        container
            .resource_with_dictionary(Default::default(), Default::default(), &dictionary, &[])
            .is_err()
    );
    assert!(
        container
            .resource_with_dictionary(
                Default::default(),
                Default::default(),
                &dictionary,
                &[DictionaryReference::SerializedId(DictionaryId([0; 32])); 2]
            )
            .is_err()
    );
    let output = container.finish().expect("container");
    let mut codes = Vec::new();
    for (_, chunk) in chunks(&output) {
        if matches!(chunk[0], 2..=3) && chunk[1] == 3 {
            let mut cursor = 2;
            number(chunk, &mut cursor);
            assert_eq!(chunk[cursor], 1);
            codes.push(chunk[cursor + 1]);
        }
    }
    assert_eq!(codes, [2, 6, 0, 4, 1]);
}

#[test]
fn limits_and_finish_errors_preserve_the_sink_for_recovery() {
    let mut compressor = Compressor::new(Default::default()).expect("config");
    assert!(
        compressor
            .framed_writer(
                Vec::new(),
                FramingConfig {
                    chunk_bytes: 0,
                    ..config()
                }
            )
            .is_err()
    );
    assert!(
        compressor
            .framed_writer(
                Vec::new(),
                FramingConfig {
                    container: false,
                    ..config()
                }
            )
            .is_err()
    );
    let mut container = compressor
        .framed_writer(
            Vec::new(),
            FramingConfig {
                max_metadata_bytes: 1,
                max_resources: 0,
                ..config()
            },
        )
        .expect("container");
    assert!(
        container
            .metadata(
                MetadataKind::Global,
                &[MetadataField {
                    code: *b"AA",
                    value: b"x"
                }]
            )
            .is_err()
    );
    assert!(container.uncompressed_resource(Default::default()).is_err());
    assert!(container.padding(usize::MAX).is_err());
    assert!(container.get_ref().is_empty());
    assert!(container.into_inner().is_empty());
    let sink = FaultSink {
        budget: usize::MAX,
        blocked: true,
        ..Default::default()
    };
    let container = compressor.framed_writer(sink, config()).expect("container");
    let failure = container.finish().expect_err("blocked sink");
    assert!(std::error::Error::source(&failure.error).is_some());
    let mut container = failure.writer;
    container.get_mut().blocked = false;
    assert!(!container.finish().expect("retry").bytes.is_empty());
}

#[test]
fn large_window_resources_are_marked_as_shared_brotli() {
    let mut compressor = Compressor::new(
        EncoderConfig::default()
            .with_quality(Quality::Q5)
            .with_window(mbrotli::Window::large(30).expect("window")),
    )
    .expect("config");
    let mut container = compressor
        .framed_writer(Vec::new(), config())
        .expect("container");
    assert!(
        container
            .resource(
                Default::default(),
                mbrotli::StreamConfig::default().with_stream_offset(1)
            )
            .is_err()
    );
    let payload = b"large window";
    {
        let mut resource = container
            .resource(Default::default(), Default::default())
            .expect("resource");
        resource.write_all(payload).expect("write");
        resource.try_finish().expect("finish");
    }
    let output = container.finish().expect("container");
    let parsed = chunks(&output);
    let chunk = parsed[0].1;
    assert_eq!(&chunk[..2], &[2, 3]);
    let mut cursor = 2;
    assert_eq!(number(chunk, &mut cursor), payload.len() as u64);
    assert_eq!(&chunk[cursor..cursor + 2], &[0, 0]);
    cursor += 2;
    assert_eq!(
        support::c_decompress_large_window(&chunk[cursor..], payload.len()).as_deref(),
        Some(&payload[..])
    );
}
