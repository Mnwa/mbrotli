//! Native C-produced inputs; setup and validation are outside timing. C decoder
//! state is created/destroyed per operation, while warm Rust retains workspace.
#[path = "../tests/decode_support/c_decoder.rs"]
mod c_decoder;
#[path = "../tests/support/mod.rs"]
mod support;
#[path = "../tests/decode_support/wire.rs"]
pub mod wire;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use google_brotli_ffi as ffi;
use mbrotli::{DecodeOperation, DecodeStreamConfig, DecoderConfig, DecoderStatus, Decompressor};
use std::hint::black_box;

fn c_slice(input: &[u8], output: &mut [u8]) {
    let mut length = output.len();
    // SAFETY: C receives live, disjoint slice buffers and a valid length pointer.
    assert_eq!(
        unsafe {
            ffi::BrotliDecoderDecompress(
                input.len(),
                input.as_ptr(),
                &raw mut length,
                output.as_mut_ptr(),
            )
        },
        ffi::BROTLI_DECODER_RESULT_SUCCESS
    );
    assert_eq!(length, output.len());
}
fn streaming(decoder: &mut Decompressor, input: &[u8], output: &mut [u8]) {
    let mut session = decoder.start(DecodeStreamConfig::default()).unwrap();
    let mut read = 0;
    let mut written = 0;
    loop {
        let end = (read + 31).min(input.len());
        let output_end = (written + 127).min(output.len());
        let operation = if end == input.len() {
            DecodeOperation::Finish
        } else {
            DecodeOperation::Process
        };
        let progress = session
            .process(
                &input[read..end],
                &mut output[written..output_end],
                operation,
            )
            .unwrap();
        read += progress.consumed;
        written += progress.produced;
        if progress.status == DecoderStatus::Finished {
            break;
        }
    }
    assert_eq!((read, written), (input.len(), output.len()));
}
fn streaming_c(input: &[u8], output: &mut [u8]) {
    let mut decoder = c_decoder::Decoder::default();
    let mut read = 0;
    let mut written = 0;
    loop {
        let end = (read + 31).min(input.len());
        let output_end = (written + 127).min(output.len());
        let (consumed, produced, finished) =
            decoder.process(&input[read..end], &mut output[written..output_end]);
        read += consumed;
        written += produced;
        if finished {
            break;
        }
        assert!(consumed != 0 || produced != 0);
    }
    assert_eq!((read, written), (input.len(), output.len()));
}

fn dictionaries(c: &mut Criterion) {
    use mbrotli::dictionary::{DecodeDictionary, DecodeDictionaryLimits, DictionaryAttachment};
    let prefix = b"Prefix data: dictionary references shared by both codecs. ".repeat(100);
    let payload = &prefix[27..3500];
    let compressed =
        support::c_compress_with_prefixes(support::CParams::new(5, 22), &[&prefix], payload);
    let dictionary = DecodeDictionary::new(
        &[DictionaryAttachment::Raw(&prefix)],
        DecodeDictionaryLimits::default(),
    )
    .unwrap();
    let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
    assert_eq!(
        decoder
            .decompress_with_dictionary(&dictionary, &compressed)
            .unwrap(),
        payload
    );
    assert_eq!(
        support::c_decompress_with_prefixes(&[&prefix], &compressed, payload.len()).unwrap(),
        payload
    );
    eprintln!(
        "dictionary: payload={} compressed={} dictionary_retained={} decoder_retained={}",
        payload.len(),
        compressed.len(),
        dictionary.retained_bytes(),
        decoder.retained_bytes()
    );
    let mut group = c.benchmark_group("decompress/dictionary");
    group.throughput(Throughput::Bytes(payload.len() as u64));
    group.bench_function("rust-vec-warm-dictionary", |b| {
        b.iter(|| {
            decoder
                .decompress_with_dictionary(&dictionary, black_box(&compressed))
                .unwrap()
        })
    });
    group.bench_function("c-vec-create-attach-destroy", |b| {
        b.iter(|| {
            support::c_decompress_with_prefixes(&[&prefix], black_box(&compressed), payload.len())
                .unwrap()
        })
    });
    group.finish();
}

fn empty_and_metadata(c: &mut Criterion) {
    for (name, metadata) in [("empty", Vec::new()), ("metadata-only", vec![0x5a; 65536])] {
        let mut wire = wire::Wire::window(22, false);
        wire.metadata(&metadata);
        let compressed = wire.finish();
        let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
        assert!(decoder.decompress(&compressed).unwrap().is_empty());
        assert!(support::c_decompress(&compressed, 0).unwrap().is_empty());
        let mut group = c.benchmark_group(format!("decompress/{name}"));
        group.throughput(Throughput::Bytes(compressed.len() as u64));
        eprintln!(
            "{name}: payload=0 compressed={} (encoded throughput)",
            compressed.len()
        );
        group.bench_function("rust-slice-warm", |b| {
            b.iter(|| {
                decoder
                    .decompress_to_slice(black_box(&compressed), &mut [])
                    .unwrap()
            })
        });
        group.bench_function("c-vec-create-destroy", |b| {
            b.iter(|| support::c_decompress(black_box(&compressed), 0).unwrap())
        });
        group.finish();
    }
}

fn benchmarks(c: &mut Criterion) {
    let mut rng = support::Rng::new(0xdec0de);
    let cases = [
        ("tiny", b"a small payload".to_vec()),
        (
            "text",
            b"Brotli dictionaries and incremental decoding of structured text.\n".repeat(1024),
        ),
        ("binary", (0..65536).map(|i| (i % 251) as u8).collect()),
        ("noise", rng.bytes(65536, 256)),
        ("repeated", vec![b'x'; 65536]),
        (
            "large",
            b"Large repeated text including dictionary transforms.\n".repeat(20000),
        ),
    ];
    for (name, payload) in cases {
        let compressed = support::c_compress_native_one_shot(5, 22, &payload);
        let mut decoder = Decompressor::new(DecoderConfig::default()).unwrap();
        let mut output = vec![0; payload.len()];
        c_slice(&compressed, &mut output);
        assert_eq!(output, payload);
        decoder
            .decompress_to_slice(&compressed, &mut output)
            .unwrap();
        assert_eq!(output, payload);
        streaming_c(&compressed, &mut output);
        assert_eq!(output, payload);
        streaming(&mut decoder, &compressed, &mut output);
        assert_eq!(output, payload);
        eprintln!(
            "{name}: payload={} compressed={} retained={} backend=host/scalar",
            payload.len(),
            compressed.len(),
            decoder.retained_bytes()
        );
        let mut group = c.benchmark_group(format!("decompress/{name}"));
        group.throughput(Throughput::Bytes(payload.len() as u64));
        group.bench_function("c-slice-create-destroy", |b| {
            b.iter(|| c_slice(black_box(&compressed), black_box(&mut output)))
        });
        group.bench_function("rust-slice-warm", |b| {
            b.iter(|| {
                decoder
                    .decompress_to_slice(black_box(&compressed), black_box(&mut output))
                    .unwrap()
            })
        });
        group.bench_function("rust-vec-cold", |b| {
            b.iter(|| {
                Decompressor::new(DecoderConfig::default())
                    .unwrap()
                    .decompress(black_box(&compressed))
                    .unwrap()
            })
        });
        group.bench_function("c-vec-cold", |b| {
            b.iter(|| support::c_decompress(black_box(&compressed), payload.len()).unwrap())
        });
        let mut vector = Vec::with_capacity(payload.len());
        group.bench_function("rust-vec-warm", |b| {
            b.iter(|| {
                vector.clear();
                decoder
                    .decompress_into(black_box(&compressed), black_box(&mut vector))
                    .unwrap()
            })
        });
        group.bench_function("rust-session-small-chunks", |b| {
            b.iter(|| streaming(&mut decoder, black_box(&compressed), black_box(&mut output)))
        });
        group.bench_function("c-session-small-chunks-create-destroy", |b| {
            b.iter(|| streaming_c(black_box(&compressed), black_box(&mut output)))
        });
        #[cfg(not(feature = "no_std"))]
        {
            use std::io::{Read, Write};
            group.bench_function("rust-reader", |b| {
                b.iter(|| {
                    let mut reader = decoder
                        .reader(
                            black_box(compressed.as_slice()),
                            DecodeStreamConfig::default(),
                        )
                        .unwrap();
                    reader.read_exact(black_box(&mut output)).unwrap();
                    assert_eq!(reader.read(&mut [0]).unwrap(), 0);
                })
            });
            group.bench_function("rust-writer", |b| {
                b.iter(|| {
                    let mut writer = decoder
                        .writer(
                            black_box(output.as_mut_slice()),
                            DecodeStreamConfig::default(),
                        )
                        .unwrap();
                    writer.write_all(black_box(&compressed)).unwrap();
                    writer.finish().unwrap();
                })
            });
        }
        group.finish();
    }
    dictionaries(c);
    empty_and_metadata(c);
}
criterion_group!(benches, benchmarks);
criterion_main!(benches);
