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
            let mut encoder = Compressor::new(Default::default()).unwrap();
            let mut writer = encoder
                .framed_writer(Vec::new(), FramingConfig::default())
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
            let mut buffer = [0; 4096];
            assert_eq!(stream(&mut decoder, &bytes, &mut buffer), text.len());
            group.bench_function(format!("stream-{name}-{policy:?}"), |b| {
                b.iter(|| stream(&mut decoder, black_box(&bytes), black_box(&mut buffer)))
            });
        }
        let mut encoder = Compressor::new(Default::default()).unwrap();
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
        let mut writer = encoder
            .framed_writer(Vec::new(), FramingConfig::default())
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
}
#[cfg(not(feature = "no_std"))]
criterion::criterion_group!(benches, benchmarks);
#[cfg(not(feature = "no_std"))]
criterion::criterion_main!(benches);
#[cfg(feature = "no_std")]
fn main() {}
