//! Criterion benchmarks for the native decoder against Google's C Brotli.
//!
//! The inputs are the compressor benchmark's corpora, shared through
//! `benches/support/corpora.rs`: the deterministic text, binary, compressible
//! and incompressible inputs plus the same six vendored Google Brotli test
//! files. Each corpus is encoded once by the reference encoder's one-shot entry
//! point at every quality from 0 to 11 with the default 22-bit window, so a
//! decoder is measured on exactly the streams the compressor benchmark
//! produces and a slowdown at one quality cannot hide behind a gain at another.
//!
//! Every stream is validated before any timing: both decoders must reproduce
//! the original payload, and the compressed sizes are printed so a throughput
//! figure can be read against the ratio of the stream it decoded.
//!
//! The shapes mirror the compressor benchmark, because a stateful decoder
//! makes them genuinely different work:
//!
//! * `cold` — build the decoder, allocate the output, decode once. Both sides
//!   create and destroy their decoder state inside the timed region.
//! * `reused` — repeated `decompress_into` into a destination that is already
//!   big enough. Rust retains its workspace; C constructs state for each call.
//! * `presized` — `decompress_to_slice` into a caller-owned buffer, with the
//!   default backend and separate `mbrotli-<backend>` entries for every host level.
//! * `tiny` — per-call overhead on payloads where it dominates.
//! * `streaming` — the `Write`, `Read` and session shapes fed 64 KiB chunks of
//!   compressed input and drained in 64 KiB windows, against the reference's
//!   streaming API.
//! * `small-chunks` — sessions fed 31-byte input and 127-byte output windows,
//!   which is what the per-call resumption cost looks like on its own.
//! * `dictionary`, `universal`, `metadata` — attached raw dictionaries, the
//!   canonical empty and 16 KiB incompressible streams, and a metadata-only
//!   stream, which are the decoder's own boundary cases.
//!
//! C decoder state is created and destroyed per operation in every group,
//! which is what `BrotliDecoderDecompress` does on each call anyway. A full
//! corpus checkout yields eleven corpora per quality.
#[path = "../tests/decode_support/c_decoder.rs"]
mod c_decoder;
#[path = "support/corpora.rs"]
mod corpora;
#[path = "../tests/support/mod.rs"]
mod support;
#[path = "../tests/decode_support/wire.rs"]
pub mod wire;

use corpora::{Corpus, TINY_SIZES, corpora, incompressible, text, vendor_corpora};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use google_brotli_ffi as ffi;
use mbrotli::dictionary::{DecodeDictionary, DecodeDictionaryLimits, DictionaryAttachment};
use mbrotli::{
    DecodeOperation, DecodeStreamConfig, DecoderConfig, DecoderStatus, Decompressor, Window,
};
use std::ffi::c_int;
use std::hint::black_box;
use std::sync::OnceLock;

/// Sliding window size every stream is encoded with.
const LGWIN: Window = Window::DEFAULT;

/// Quality levels whose streams are decoded.
///
/// The decoder does not know the quality, but the streams differ: qualities
/// zero and one emit many small meta-blocks with simple codes, the greedy
/// qualities emit context-modelled Huffman codes, and the two highest emit
/// long block splits with dictionary references. Every level is gated
/// separately, so a gain at one may not be used to cover a loss at another.
const QUALITIES: [c_int; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

/// Chunk size the streaming group feeds and drains in.
const STREAM_CHUNK: usize = 64 << 10;

/// Input and output window sizes of the small-chunk session group.
const SMALL_INPUT: usize = 31;
const SMALL_OUTPUT: usize = 127;

/// One payload and its reference encoding at one quality.
struct Stream {
    name: String,
    payload: Vec<u8>,
    compressed: Vec<u8>,
}

impl Stream {
    /// Encodes `corpus` at `quality` with the reference one-shot entry point
    /// and checks that both decoders reproduce it.
    fn new(quality: c_int, corpus: &Corpus) -> Self {
        let compressed =
            support::c_compress_native_one_shot(quality, c_int::from(LGWIN.bits()), &corpus.data);
        let stream = Self {
            name: corpus.name.clone(),
            payload: corpus.data.clone(),
            compressed,
        };
        stream.validate(quality);
        stream
    }

    /// Verifies both decoders against the payload and reports the sizes.
    fn validate(&self, quality: c_int) {
        let Self {
            name,
            payload,
            compressed,
        } = self;
        assert_eq!(
            support::c_decompress(compressed, payload.len()).as_deref(),
            Some(payload.as_slice()),
            "C Brotli does not round-trip {name} at q{quality}",
        );
        assert_eq!(
            decoder()
                .decompress(compressed)
                .expect("mbrotli failed to decode the stream"),
            *payload,
            "mbrotli does not round-trip {name} at q{quality}",
        );
        println!(
            "q{quality} {name:<26} payload {payload_size:>9} bytes  compressed {compressed_size:>9} bytes",
            payload_size = payload.len(),
            compressed_size = compressed.len(),
        );
    }

    /// Throughput in payload bytes, which is what a decoder produces.
    fn throughput(&self) -> Throughput {
        Throughput::Bytes(self.payload.len() as u64)
    }
}

/// Builds a decoder with the default budgets.
fn decoder() -> Decompressor {
    Decompressor::new(DecoderConfig::default()).expect("a legal configuration")
}

/// The corpora encoded at every quality, built once and shared by every group.
fn streams(quality: c_int) -> &'static [Stream] {
    static STREAMS: OnceLock<Vec<Vec<Stream>>> = OnceLock::new();
    let all = STREAMS.get_or_init(|| {
        let corpora = corpora();
        println!("corpus validation (lgwin {})", LGWIN.bits());
        QUALITIES
            .iter()
            .map(|&quality| {
                corpora
                    .iter()
                    .map(|corpus| Stream::new(quality, corpus))
                    .collect()
            })
            .collect()
    });
    &all[quality as usize]
}

/// Safe wrappers over the raw C Brotli decoder bindings.
mod c_brotli {
    use super::{c_decoder, ffi};

    /// Decodes `input` into the caller's exactly sized `output` in one call.
    ///
    /// The reference creates and destroys its decoder state inside this call.
    pub fn decompress_to_slice(input: &[u8], output: &mut [u8]) {
        let mut length = output.len();
        // SAFETY: C receives live, disjoint slice buffers and a valid length
        // pointer, and writes at most `length` bytes.
        let result = unsafe {
            ffi::BrotliDecoderDecompress(
                input.len(),
                input.as_ptr(),
                &raw mut length,
                output.as_mut_ptr(),
            )
        };
        assert_eq!(result, ffi::BROTLI_DECODER_RESULT_SUCCESS);
        assert_eq!(length, output.len());
    }

    /// Decodes `input` fed in `input_chunk` pieces and drained in
    /// `output_chunk` windows of `output`, creating and destroying the
    /// decoder state around the stream.
    pub fn decompress_streaming(
        input: &[u8],
        output: &mut [u8],
        input_chunk: usize,
        output_chunk: usize,
    ) {
        let mut decoder = c_decoder::Decoder::default();
        let mut read = 0;
        let mut written = 0;
        loop {
            let end = (read + input_chunk).min(input.len());
            let output_end = (written + output_chunk).min(output.len());
            let (consumed, produced, finished) =
                decoder.process(&input[read..end], &mut output[written..output_end]);
            read += consumed;
            written += produced;
            if finished {
                break;
            }
            assert!(consumed != 0 || produced != 0, "the C decoder stalled");
        }
        assert_eq!((read, written), (input.len(), output.len()));
    }
}

/// Decodes `input` through a session fed in `input_chunk` pieces and drained
/// in `output_chunk` windows of `output`, on a warm decoder.
fn session(
    decoder: &mut Decompressor,
    input: &[u8],
    output: &mut [u8],
    input_chunk: usize,
    output_chunk: usize,
) {
    let mut session = decoder
        .start(DecodeStreamConfig::default())
        .expect("a legal stream");
    let mut read = 0;
    let mut written = 0;
    loop {
        let end = (read + input_chunk).min(input.len());
        let output_end = (written + output_chunk).min(output.len());
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
            .expect("decoding failed");
        read += progress.consumed;
        written += progress.produced;
        if progress.status == DecoderStatus::Finished {
            break;
        }
        assert!(
            progress.consumed != 0 || progress.produced != 0,
            "the decoder stalled"
        );
    }
    assert_eq!((read, written), (input.len(), output.len()));
}

/// Registers the cold comparison: build the decoder, allocate, decode once.
///
/// This is what a caller who decodes one thing pays. Both sides create and
/// destroy their decoder state inside the timed region, and both allocate
/// the destination there too; the reference's one-shot API is handed the
/// exact output size, which is all it offers, while `decompress` grows a
/// `Vec` as it goes.
fn bench_cold(criterion: &mut Criterion) {
    for quality in QUALITIES {
        let mut group = criterion.benchmark_group(format!("decompress/cold/q{quality}"));

        for stream in streams(quality) {
            group.throughput(stream.throughput());
            let payload_len = stream.payload.len();

            group.bench_with_input(
                BenchmarkId::new("c-brotli", &stream.name),
                &stream.compressed,
                |bencher, compressed| {
                    bencher.iter(|| {
                        support::c_decompress(black_box(compressed), payload_len)
                            .expect("C Brotli failed to decode the stream")
                    });
                },
            );

            group.bench_with_input(
                BenchmarkId::new("mbrotli", &stream.name),
                &stream.compressed,
                |bencher, compressed| {
                    bencher.iter(|| {
                        decoder()
                            .decompress(black_box(compressed))
                            .expect("mbrotli failed to decode the stream")
                    });
                },
            );
        }

        group.finish();
    }
}

/// Registers the reused comparison: one decoder, one destination, many calls.
///
/// The reference's one-shot entry point builds a whole decoder per call and
/// has no reuse to compare against, so the `c-brotli` arm decodes into the
/// same caller-owned buffer every time. That difference is the point of the
/// shape.
fn bench_reused(criterion: &mut Criterion) {
    for quality in QUALITIES {
        let mut group = criterion.benchmark_group(format!("decompress/reused/q{quality}"));

        for stream in streams(quality) {
            group.throughput(stream.throughput());

            let mut c_output = vec![0u8; stream.payload.len()];
            group.bench_with_input(
                BenchmarkId::new("c-brotli", &stream.name),
                &stream.compressed,
                |bencher, compressed| {
                    bencher.iter(|| {
                        c_brotli::decompress_to_slice(black_box(compressed), &mut c_output);
                    });
                },
            );

            group.bench_with_input(
                BenchmarkId::new("mbrotli", &stream.name),
                &stream.compressed,
                |bencher, compressed| {
                    let mut decoder = decoder();
                    let mut output = Vec::new();
                    // Warm the workspace and the destination, so the measured
                    // calls allocate nothing at all.
                    decoder
                        .decompress_into(compressed, &mut output)
                        .expect("mbrotli failed to decode the stream");
                    bencher.iter(|| {
                        output.clear();
                        decoder
                            .decompress_into(black_box(compressed), &mut output)
                            .expect("mbrotli failed to decode the stream")
                    });
                },
            );
        }

        group.finish();
    }
}

/// Registers the comparison into a caller-owned, exactly sized output buffer.
fn bench_presized(criterion: &mut Criterion) {
    for quality in QUALITIES {
        let mut group = criterion.benchmark_group(format!("decompress/presized/q{quality}"));

        for stream in streams(quality) {
            group.throughput(stream.throughput());

            let mut c_output = vec![0u8; stream.payload.len()];
            group.bench_with_input(
                BenchmarkId::new("c-brotli", &stream.name),
                &stream.compressed,
                |bencher, compressed| {
                    bencher.iter(|| {
                        c_brotli::decompress_to_slice(black_box(compressed), &mut c_output);
                    });
                },
            );

            // Keep each supported backend separately measurable. Construction,
            // warmup, and validation stay outside the timed slice operation.
            for backend in mbrotli::Backend::available() {
                let mut decoder = Decompressor::builder(DecoderConfig::default())
                    .with_backend(backend)
                    .build()
                    .expect("a host-validated backend");
                let mut output = vec![0u8; stream.payload.len()];
                decoder
                    .decompress_to_slice(&stream.compressed, &mut output)
                    .expect("the selected backend decodes the stream");
                assert_eq!(output, stream.payload, "{backend} differs");
                group.bench_with_input(
                    BenchmarkId::new(format!("mbrotli-{backend}"), &stream.name),
                    &stream.compressed,
                    |bencher, compressed| {
                        bencher.iter(|| {
                            decoder
                                .decompress_to_slice(black_box(compressed), &mut output)
                                .expect("the selected backend decodes the stream")
                        });
                    },
                );
            }

            let mut rust_output = vec![0u8; stream.payload.len()];
            group.bench_with_input(
                BenchmarkId::new("mbrotli", &stream.name),
                &stream.compressed,
                |bencher, compressed| {
                    let mut decoder = decoder();
                    bencher.iter(|| {
                        decoder
                            .decompress_to_slice(black_box(compressed), &mut rust_output)
                            .expect("mbrotli failed to decode the stream")
                    });
                },
            );
        }

        group.finish();
    }
}

/// Registers the per-call overhead measurement.
fn bench_tiny(criterion: &mut Criterion) {
    let payload = text(*TINY_SIZES.iter().max().unwrap_or(&1024));

    for quality in QUALITIES {
        let mut group = criterion.benchmark_group(format!("decompress/tiny/q{quality}"));

        for size in TINY_SIZES {
            let stream = Stream::new(
                quality,
                &Corpus::new(format!("tiny-{size}"), payload[..size].to_vec()),
            );
            group.throughput(stream.throughput());

            group.bench_with_input(
                BenchmarkId::new("c-brotli", size),
                &stream.compressed,
                |bencher, compressed| {
                    bencher.iter(|| {
                        support::c_decompress(black_box(compressed), size)
                            .expect("C Brotli failed to decode the stream")
                    });
                },
            );

            // Cold, which is what the reference's own entry point does.
            group.bench_with_input(
                BenchmarkId::new("mbrotli", size),
                &stream.compressed,
                |bencher, compressed| {
                    bencher.iter(|| {
                        decoder()
                            .decompress(black_box(compressed))
                            .expect("mbrotli failed to decode the stream")
                    });
                },
            );

            // And warm, which is what a server reusing one decoder pays.
            group.bench_with_input(
                BenchmarkId::new("mbrotli-reused", size),
                &stream.compressed,
                |bencher, compressed| {
                    let mut decoder = decoder();
                    let mut output = Vec::new();
                    decoder
                        .decompress_into(compressed, &mut output)
                        .expect("mbrotli failed to decode the stream");
                    bencher.iter(|| {
                        output.clear();
                        decoder
                            .decompress_into(black_box(compressed), &mut output)
                            .expect("mbrotli failed to decode the stream")
                    });
                },
            );
        }

        group.finish();
    }
}

/// Registers the three streaming shapes against the reference's streaming API.
///
/// Compressed input arrives in 64 KiB chunks and payload leaves in 64 KiB
/// windows, which is what a network reader or a bounded sink does. Corpora
/// smaller than one chunk are skipped, since they would stream in one call.
fn bench_streaming(criterion: &mut Criterion) {
    for quality in QUALITIES {
        let mut group = criterion.benchmark_group(format!("decompress/streaming/q{quality}"));

        for stream in streams(quality) {
            if stream.payload.len() < STREAM_CHUNK {
                continue;
            }
            group.throughput(stream.throughput());
            let compressed = stream.compressed.as_slice();
            let mut output = vec![0u8; stream.payload.len()];
            let mut decoder = decoder();

            // Every shape has to reach the same bytes before any of them is
            // timed, or the comparison is between two different jobs.
            c_brotli::decompress_streaming(compressed, &mut output, STREAM_CHUNK, STREAM_CHUNK);
            assert_eq!(output, stream.payload, "the streamed reference differs");
            output.fill(0);
            session(
                &mut decoder,
                compressed,
                &mut output,
                STREAM_CHUNK,
                STREAM_CHUNK,
            );
            assert_eq!(output, stream.payload, "the streamed session differs");

            group.bench_with_input(
                BenchmarkId::new("c-brotli", &stream.name),
                &compressed,
                |bencher, compressed| {
                    bencher.iter(|| {
                        c_brotli::decompress_streaming(
                            black_box(compressed),
                            &mut output,
                            STREAM_CHUNK,
                            STREAM_CHUNK,
                        );
                    });
                },
            );

            #[cfg(not(feature = "no_std"))]
            {
                use std::io::{Read, Write};

                group.bench_with_input(
                    BenchmarkId::new("mbrotli-writer", &stream.name),
                    &compressed,
                    |bencher, compressed| {
                        bencher.iter(|| {
                            let mut sink = decoder
                                .writer(output.as_mut_slice(), DecodeStreamConfig::default())
                                .expect("a legal stream");
                            for chunk in black_box(compressed).chunks(STREAM_CHUNK) {
                                sink.write_all(chunk).expect("write failed");
                            }
                            sink.finish().expect("finish failed");
                        });
                    },
                );

                group.bench_with_input(
                    BenchmarkId::new("mbrotli-reader", &stream.name),
                    &compressed,
                    |bencher, compressed| {
                        bencher.iter(|| {
                            let mut source = decoder
                                .reader(black_box(*compressed), DecodeStreamConfig::default())
                                .expect("a legal stream");
                            for window in output.chunks_mut(STREAM_CHUNK) {
                                source.read_exact(window).expect("read failed");
                            }
                            assert_eq!(source.read(&mut [0]).expect("read failed"), 0);
                        });
                    },
                );
                assert_eq!(output, stream.payload, "the streamed adapters differ");
            }

            group.bench_with_input(
                BenchmarkId::new("mbrotli-session", &stream.name),
                &compressed,
                |bencher, compressed| {
                    bencher.iter(|| {
                        session(
                            &mut decoder,
                            black_box(compressed),
                            &mut output,
                            STREAM_CHUNK,
                            STREAM_CHUNK,
                        );
                    });
                },
            );
        }

        group.finish();
    }
}

/// Registers the small-window session shape, where resumption cost dominates.
fn bench_small_chunks(criterion: &mut Criterion) {
    for quality in QUALITIES {
        let mut group = criterion.benchmark_group(format!("decompress/small-chunks/q{quality}"));

        for stream in streams(quality) {
            group.throughput(stream.throughput());
            let compressed = stream.compressed.as_slice();
            let mut output = vec![0u8; stream.payload.len()];
            let mut decoder = decoder();

            c_brotli::decompress_streaming(compressed, &mut output, SMALL_INPUT, SMALL_OUTPUT);
            assert_eq!(output, stream.payload, "the chunked reference differs");
            output.fill(0);
            session(
                &mut decoder,
                compressed,
                &mut output,
                SMALL_INPUT,
                SMALL_OUTPUT,
            );
            assert_eq!(output, stream.payload, "the chunked session differs");

            group.bench_with_input(
                BenchmarkId::new("c-brotli", &stream.name),
                &compressed,
                |bencher, compressed| {
                    bencher.iter(|| {
                        c_brotli::decompress_streaming(
                            black_box(compressed),
                            &mut output,
                            SMALL_INPUT,
                            SMALL_OUTPUT,
                        );
                    });
                },
            );

            group.bench_with_input(
                BenchmarkId::new("mbrotli-session", &stream.name),
                &compressed,
                |bencher, compressed| {
                    bencher.iter(|| {
                        session(
                            &mut decoder,
                            black_box(compressed),
                            &mut output,
                            SMALL_INPUT,
                            SMALL_OUTPUT,
                        );
                    });
                },
            );
        }

        group.finish();
    }
}

/// Qualities whose reference encoder consults an attached dictionary.
///
/// Below quality five the C encoder ignores the prefix, so the streams would
/// be the plain ones the other groups already measure.
const PREFIX_QUALITIES: [c_int; 3] = [5, 9, 11];

/// Registers decoding with an attached raw dictionary.
///
/// The first half of `alice29.txt` is the prefix and the second half the
/// payload, as in the compressor benchmark. Rust decodes with a warm decoder
/// and a prepared dictionary; C prepares, attaches, decodes and destroys per
/// call, which is what its one-shot API offers.
fn bench_dictionary(criterion: &mut Criterion) {
    let Some(corpus) = vendor_corpora()
        .into_iter()
        .find(|corpus| corpus.name.ends_with("alice29.txt"))
    else {
        return;
    };
    let (prefix, payload) = corpus.data.split_at(corpus.data.len() / 2);
    let dictionary = DecodeDictionary::new(
        &[DictionaryAttachment::Raw(prefix)],
        DecodeDictionaryLimits::default(),
    )
    .expect("a legal dictionary");

    for quality in PREFIX_QUALITIES {
        let mut group = criterion.benchmark_group(format!("decompress/dictionary/q{quality}"));
        group.throughput(Throughput::Bytes(payload.len() as u64));

        let compressed = support::c_compress_with_prefixes(
            support::CParams::new(quality, c_int::from(LGWIN.bits())),
            &[prefix],
            payload,
        );
        let mut decoder = decoder();
        assert_eq!(
            decoder
                .decompress_with_dictionary(&dictionary, &compressed)
                .expect("mbrotli failed to decode the stream"),
            payload
        );
        assert_eq!(
            support::c_decompress_with_prefixes(&[prefix], &compressed, payload.len()).as_deref(),
            Some(payload)
        );
        println!(
            "q{quality} dictionary {:>8} bytes  compressed {:>8} bytes  dictionary retained {:>8} bytes",
            payload.len(),
            compressed.len(),
            dictionary.retained_bytes(),
        );

        group.bench_function(BenchmarkId::new("c-brotli", "alice29-half"), |bencher| {
            bencher.iter(|| {
                support::c_decompress_with_prefixes(
                    &[prefix],
                    black_box(&compressed),
                    payload.len(),
                )
                .expect("C Brotli failed to decode the stream")
            });
        });

        group.bench_function(BenchmarkId::new("mbrotli", "alice29-half"), |bencher| {
            bencher.iter(|| {
                decoder
                    .decompress_with_dictionary(&dictionary, black_box(&compressed))
                    .expect("mbrotli failed to decode the stream")
            });
        });

        let mut output = vec![0u8; payload.len()];
        group.bench_function(
            BenchmarkId::new("mbrotli-presized", "alice29-half"),
            |bencher| {
                bencher.iter(|| {
                    decoder
                        .decompress_with_dictionary_to_slice(
                            &dictionary,
                            black_box(&compressed),
                            &mut output,
                        )
                        .expect("mbrotli failed to decode the stream")
                });
            },
        );

        group.finish();
    }
}

/// Measures the canonical empty and incompressible streams at a 10-bit window.
fn bench_universal(criterion: &mut Criterion) {
    let noise = incompressible(16 << 10);
    for quality in [0, 1, 5, 11] {
        let mut group = criterion.benchmark_group(format!("decompress/universal/q{quality}"));
        for (name, data) in [("empty", &[][..]), ("binary-16KiB", noise.as_slice())] {
            group.throughput(if data.is_empty() {
                Throughput::Elements(1)
            } else {
                Throughput::Bytes(data.len() as u64)
            });
            let stream = Stream::new(quality, &Corpus::new(name, data.to_vec()));
            let compressed = stream.compressed.as_slice();

            group.bench_function(BenchmarkId::new("c-brotli", name), |bencher| {
                bencher.iter(|| {
                    support::c_decompress(black_box(compressed), data.len())
                        .expect("C Brotli failed to decode the stream")
                });
            });
            group.bench_function(BenchmarkId::new("mbrotli", name), |bencher| {
                bencher.iter(|| {
                    decoder()
                        .decompress(black_box(compressed))
                        .expect("mbrotli failed to decode the stream")
                });
            });
        }
        group.finish();
    }
}

/// Measures skipping a 64 KiB metadata block that carries no payload.
///
/// Throughput is in encoded bytes, since nothing is produced.
fn bench_metadata(criterion: &mut Criterion) {
    let mut wire = wire::Wire::window(22, false);
    wire.metadata(&vec![0x5a; 65536]);
    let compressed = wire.finish();
    let mut decoder = decoder();
    assert!(
        decoder
            .decompress(&compressed)
            .expect("mbrotli failed to decode the stream")
            .is_empty()
    );
    assert!(
        support::c_decompress(&compressed, 0)
            .expect("C Brotli failed to decode the stream")
            .is_empty()
    );
    println!(
        "metadata-only payload 0 bytes  compressed {} bytes (encoded throughput)",
        compressed.len()
    );

    let mut group = criterion.benchmark_group("decompress/metadata");
    group.throughput(Throughput::Bytes(compressed.len() as u64));
    group.bench_function(BenchmarkId::new("c-brotli", "metadata-64KiB"), |bencher| {
        bencher.iter(|| {
            support::c_decompress(black_box(&compressed), 0)
                .expect("C Brotli failed to decode the stream")
        });
    });
    group.bench_function(BenchmarkId::new("mbrotli", "metadata-64KiB"), |bencher| {
        bencher.iter(|| {
            decoder
                .decompress_to_slice(black_box(&compressed), &mut [])
                .expect("mbrotli failed to decode the stream")
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_cold,
    bench_reused,
    bench_presized,
    bench_tiny,
    bench_streaming,
    bench_small_chunks,
    bench_dictionary,
    bench_universal,
    bench_metadata
);
criterion_main!(benches);
