//! End-to-end structured decoding; fixture construction and validation are untimed.
#[cfg(not(feature = "no_std"))]
#[path = "../tests/decode_support/c_decoder.rs"]
mod c_decoder;

#[cfg(not(feature = "no_std"))]
fn stream(
    decoder: &mut mbrotli::framing::FramedDecompressor,
    bytes: &[u8],
    output: &mut [u8],
) -> usize {
    use mbrotli::{DecodeOperation, framing::FramedDecoderStatus};
    let mut session = decoder.start(Default::default()).unwrap();
    let mut cursor = 0;
    let mut total = 0;
    loop {
        let end = (cursor + 31).min(bytes.len());
        let operation = if end == bytes.len() {
            DecodeOperation::Finish
        } else {
            DecodeOperation::Process
        };
        let progress = session
            .process(&bytes[cursor..end], output, operation)
            .unwrap();
        cursor += progress.consumed;
        total += progress.produced;
        if matches!(progress.status, FramedDecoderStatus::Finished) {
            assert_eq!(cursor, bytes.len());
            return total;
        }
    }
}

#[cfg(not(feature = "no_std"))]
fn benchmarks(c: &mut criterion::Criterion) {
    use mbrotli::framing::*;
    use mbrotli::{Compressor, Decompressor};
    use std::{hint::black_box, io::Write};
    let text = b"framed resources with shared words and repeated payload bytes. ".repeat(1024);
    let raw = Compressor::new(Default::default())
        .unwrap()
        .compress(&text)
        .unwrap();
    let mut raw_decoder = Decompressor::new(Default::default()).unwrap();
    let mut destination = vec![0; text.len()];
    raw_decoder
        .decompress_to_slice(&raw, &mut destination)
        .unwrap();
    assert_eq!(destination, text);
    let mut reference = c_decoder::Decoder::default();
    let (consumed, produced, finished) = reference.process(&raw, &mut destination);
    assert!(finished);
    assert_eq!(consumed, raw.len());
    assert_eq!(produced, text.len());
    assert_eq!(destination, text);
    let mut group = c.benchmark_group("framed-decode");
    group.throughput(criterion::Throughput::Bytes(text.len() as u64));
    eprintln!("raw-text: input={} compressed={}", text.len(), raw.len());
    group.bench_function("raw-presized", |b| {
        b.iter(|| {
            raw_decoder
                .decompress_to_slice(black_box(&raw), black_box(&mut destination))
                .unwrap()
        })
    });
    group.bench_function("c-raw-presized", |b| {
        b.iter(|| {
            let mut decoder = c_decoder::Decoder::default();
            black_box(decoder.process(black_box(&raw), black_box(&mut destination)));
        })
    });
    for policy in [
        InternalDictionaryPolicy::Retain,
        InternalDictionaryPolicy::Reject,
    ] {
        let mut decoder = FramedDecompressor::new(
            FramedDecodeConfig::default()
                .with_input_mode(InputMode::Auto)
                .with_internal_dictionaries(policy),
        )
        .unwrap();
        assert_eq!(decoder.decompress(&raw).unwrap().resources[0].data, text);
        group.bench_function(format!("auto-{policy:?}"), |b| {
            b.iter(|| decoder.decompress(black_box(&raw)).unwrap())
        });
        for (name, compressed, resources, metadata) in [
            ("stored", false, 1, false),
            ("compressed", true, 1, false),
            ("many-small", false, 256, false),
            ("metadata-heavy", false, 256, true),
        ] {
            let mut framed_encoder = mbrotli::framing::FramedCompressor::new(Default::default())
                .expect("framed configuration");
            let mut writer = framed_encoder
                .framed_writer(Vec::new(), Default::default())
                .unwrap();
            for i in 0..resources {
                if metadata {
                    writer
                        .metadata(
                            MetadataKind::Resource,
                            &[
                                MetadataField {
                                    code: *b"id",
                                    value: b"resource",
                                },
                                MetadataField {
                                    code: *b"AA",
                                    value: b"custom metadata",
                                },
                            ],
                        )
                        .unwrap();
                }
                let payload = &text[text.len() * i / resources..text.len() * (i + 1) / resources];
                let mut r = if compressed {
                    writer
                        .resource(Default::default(), Default::default())
                        .unwrap()
                } else {
                    writer.uncompressed_resource(Default::default()).unwrap()
                };
                r.write_all(payload).unwrap();
                r.try_finish().unwrap();
            }
            let bytes = writer.finish().unwrap();
            let output = decoder.decompress(&bytes).unwrap();
            assert!(
                output
                    .resources
                    .iter()
                    .flat_map(|r| r.data.iter())
                    .copied()
                    .eq(text.iter().copied())
            );
            eprintln!(
                "{name}-{policy:?}: payload={} wire={}",
                text.len(),
                bytes.len()
            );
            group.bench_function(format!("{name}-{policy:?}"), |b| {
                b.iter(|| decoder.decompress(black_box(&bytes)).unwrap())
            });
            {
                use std::io::{Cursor, Read};
                let mut indexed = decoder.framed_seek_reader(Cursor::new(&bytes)).unwrap();
                for i in (0..resources).rev() {
                    let mut decoded = Vec::new();
                    indexed
                        .resource(ResourceIndex(i as u64))
                        .unwrap()
                        .read_to_end(&mut decoded)
                        .unwrap();
                    assert_eq!(decoded, output.resources[i].data);
                }
                group.bench_function(format!("seek-all-{name}-{policy:?}"), |b| {
                    b.iter(|| {
                        let mut count = 0;
                        for i in (0..resources).rev() {
                            let mut resource = indexed.resource(ResourceIndex(i as u64)).unwrap();
                            count += std::io::copy(&mut resource, &mut std::io::sink()).unwrap();
                        }
                        black_box(count)
                    })
                });
            }
            group.bench_function(format!("seek-open-{name}-{policy:?}"), |b| {
                b.iter(|| {
                    let indexed = decoder
                        .framed_seek_reader(std::io::Cursor::new(black_box(&bytes)))
                        .unwrap();
                    black_box(indexed.resources().len())
                })
            });
            let mut buffer = [0; 4096];
            assert_eq!(stream(&mut decoder, &bytes, &mut buffer), text.len());
            group.bench_function(format!("stream-{name}-{policy:?}"), |b| {
                b.iter(|| stream(&mut decoder, black_box(&bytes), black_box(&mut buffer)))
            });
        }
        let prefix = b"dictionary content ".repeat(64);
        let dictionary = mbrotli::dictionary::DictionaryBuilder::new()
            .add_prefix(prefix.as_slice())
            .build()
            .unwrap();
        #[derive(Debug)]
        struct Resolver(Vec<u8>);
        impl DictionaryResolver for Resolver {
            fn resolve(&self, _: ExternalDictionaryRequest) -> Option<&[u8]> {
                Some(&self.0)
            }
        }
        let resolver = Resolver(prefix.clone());
        let mut framed_encoder = mbrotli::framing::FramedCompressor::new(Default::default())
            .expect("framed configuration");
        let mut writer = framed_encoder
            .framed_writer(Vec::new(), Default::default())
            .unwrap();
        for _ in 0..32 {
            let mut r = writer
                .resource_with_dictionary(
                    Default::default(),
                    Default::default(),
                    &dictionary,
                    &[DictionaryReference::PrefixId(DictionaryId([7; 32]))],
                )
                .unwrap();
            r.write_all(&prefix).unwrap();
            r.try_finish().unwrap();
        }
        let bytes = writer.finish().unwrap();
        let output = decoder
            .decompress_with_dictionaries(&resolver, &bytes)
            .unwrap();
        assert!(output.resources.iter().all(|r| r.data == prefix));
        group.throughput(criterion::Throughput::Bytes((prefix.len() * 32) as u64));
        eprintln!(
            "dictionary-heavy-{policy:?}: payload={} wire={}",
            prefix.len() * 32,
            bytes.len()
        );
        group.bench_function(format!("dictionary-heavy-{policy:?}"), |b| {
            b.iter(|| {
                decoder
                    .decompress_with_dictionaries(black_box(&resolver), black_box(&bytes))
                    .unwrap()
            })
        });
        group.throughput(criterion::Throughput::Bytes(text.len() as u64));
    }
    group.finish();

    // C has no container API. Use exactly the enclosed raw bitstream as its
    // control and report indexed/sequential container overhead explicitly.
    let mut random = vec![0; 65536];
    let mut state = 0x1234_5678_u32;
    for byte in &mut random {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        *byte = state as u8;
    }
    for (name, payload) in [
        ("small", b"one small logical resource".to_vec()),
        ("text", text),
        ("binary", (0..65536).map(|n| (n % 256) as u8).collect()),
        ("incompressible", random),
        ("large-compressible", vec![0; 1 << 20]),
    ] {
        use std::io::{Cursor, Read};
        let mut encoder = FramedCompressor::new(Default::default()).unwrap();
        let mut writer = encoder
            .framed_writer(Vec::new(), Default::default())
            .unwrap();
        {
            let mut resource = writer
                .resource(Default::default(), Default::default())
                .unwrap();
            resource.write_all(&payload).unwrap();
            resource.try_finish().unwrap();
        }
        let bytes = writer.finish().unwrap();
        let mut decoder = FramedDecompressor::new(
            FramedDecodeConfig::default()
                .with_internal_dictionaries(InternalDictionaryPolicy::Reject),
        )
        .unwrap();
        let output = decoder.decompress(&bytes).unwrap();
        assert_eq!(output.resources[0].data, payload);
        let OutputStructure::Framed { layout, .. } = output.structure else {
            unreachable!()
        };
        // Writer partials carry contiguous portions of one raw member.
        let raw: Vec<u8> = layout
            .chunks
            .iter()
            .filter(|c| {
                matches!(
                    c.kind,
                    ChunkType::Data
                        | ChunkType::FirstPartial
                        | ChunkType::MiddlePartial
                        | ChunkType::LastPartial
                )
            })
            .flat_map(|c| {
                &bytes[c.offset.0 as usize + c.header_bytes.len()..(c.offset.0 + c.length) as usize]
            })
            .copied()
            .collect();
        let mut destination = vec![0; payload.len()];
        let mut reference = c_decoder::Decoder::default();
        let (consumed, produced, finished) = reference.process(&raw, &mut destination);
        assert!(finished);
        assert_eq!((consumed, produced), (raw.len(), payload.len()));
        assert_eq!(destination, payload);
        {
            let mut indexed = decoder.framed_seek_reader(Cursor::new(&bytes)).unwrap();
            let mut resource = indexed.resource(ResourceIndex(0)).unwrap();
            resource.read_exact(&mut destination).unwrap();
            assert_eq!(resource.read(&mut [0]).unwrap(), 0);
            assert_eq!(destination, payload);
        }
        eprintln!(
            "seek-corpus-{name}: payload={} raw={} framed={}",
            payload.len(),
            raw.len(),
            bytes.len()
        );
        let mut group = c.benchmark_group(format!("seek-corpus/{name}"));
        group.throughput(criterion::Throughput::Bytes(payload.len() as u64));
        group.bench_function("indexed", |b| {
            b.iter(|| {
                let mut indexed = decoder
                    .framed_seek_reader(Cursor::new(black_box(&bytes)))
                    .unwrap();
                let mut resource = indexed.resource(ResourceIndex(0)).unwrap();
                resource.read_exact(black_box(&mut destination)).unwrap();
                assert_eq!(resource.read(&mut [0]).unwrap(), 0);
            })
        });
        group.bench_function("sequential", |b| {
            b.iter(|| {
                decoder
                    .decompress_to_slice(black_box(&bytes), black_box(&mut destination))
                    .unwrap()
            })
        });
        group.bench_function("c-raw", |b| {
            b.iter(|| {
                let mut decoder = c_decoder::Decoder::default();
                black_box(decoder.process(black_box(&raw), black_box(&mut destination)))
            })
        });
        group.finish();
    }
}
#[cfg(not(feature = "no_std"))]
criterion::criterion_group!(benches, benchmarks);
#[cfg(not(feature = "no_std"))]
criterion::criterion_main!(benches);
#[cfg(feature = "no_std")]
fn main() {}
