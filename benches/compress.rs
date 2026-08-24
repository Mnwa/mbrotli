//! Criterion benchmarks for the Brotli qualities this crate implements.
//!
//! Every case feeds identical bytes, quality, window size, and encoder mode to
//! this crate and to Google's C Brotli exposed by the `google-brotli-ffi`
//! workspace crate, so the two measurements are directly comparable. Both
//! sides measure the same end-to-end shape: allocate an output buffer sized by
//! the compressed-size bound, then compress the whole input in one shot.
//!
//! Corpora are generated deterministically at startup, or read from Google
//! Brotli's own test data in `brotli-ffi/vendor/brotli/tests/testdata`, and
//! validated before any timing: every compressed stream is decoded with the C
//! decoder and compared with the original input, and the compressed sizes are
//! printed so a speedup can be checked against the compression ratio it
//! produced.
//!
//! Two shapes are measured, as required by the acceptance gate:
//!
//! * `oneshot` — the full end-to-end API, including output allocation and
//!   growth on both sides.
//! * `presized` — the same work into a caller-owned buffer sized by the
//!   compressed-size bound, so output allocation leaves the timed region.
//!
//! A separate `tiny` group measures per-call overhead, including the single
//! runtime SIMD dispatch, on payloads where it dominates.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use mbrotli::Brotli;
use mbrotli::compressor::{CompressParams, QualityLevel, WindowBits};
use std::hint::black_box;
use std::path::Path;

/// Sliding window size used by every benchmark.
const LGWIN: WindowBits = WindowBits::DEFAULT;

/// Quality levels under measurement.
///
/// Every implemented quality is gated separately, so a gain at one may not be
/// used to cover a loss at another.
const QUALITIES: [QualityLevel; 11] = [
    QualityLevel::Q0,
    QualityLevel::Q1,
    QualityLevel::Q3,
    QualityLevel::Q4,
    QualityLevel::Q5,
    QualityLevel::Q6,
    QualityLevel::Q7,
    QualityLevel::Q8,
    QualityLevel::Q9,
    QualityLevel::Q10,
    QualityLevel::Q11,
];

/// Safe wrappers over the raw C Brotli bindings.
mod c_brotli {
    use google_brotli_ffi as ffi;
    use std::ffi::c_int;

    /// Returns the upper bound on the compressed size of `input_size` bytes.
    pub fn max_compressed_size(input_size: usize) -> usize {
        // SAFETY: `BrotliEncoderMaxCompressedSize` is a pure arithmetic helper
        // that touches no memory.
        unsafe { ffi::BrotliEncoderMaxCompressedSize(input_size) }
    }

    /// Compresses `src` into `dst`, replacing its contents.
    ///
    /// Returns the compressed length, or `None` when the encoder fails.
    pub fn compress_into(
        quality: usize,
        lgwin: usize,
        src: &[u8],
        dst: &mut Vec<u8>,
    ) -> Option<usize> {
        dst.clear();
        dst.reserve(max_compressed_size(src.len()));

        let mut encoded_size = dst.capacity();
        // SAFETY: `src` is a valid slice of `src.len()` readable bytes, and
        // `dst` owns at least `encoded_size` bytes of allocated capacity. The
        // encoder writes no more than `encoded_size` bytes into that buffer and
        // reports how many it wrote through `encoded_size`. The two buffers
        // cannot alias because `dst` was freshly reserved.
        let status = unsafe {
            ffi::BrotliEncoderCompress(
                quality as c_int,
                lgwin as c_int,
                ffi::BROTLI_DEFAULT_MODE,
                src.len(),
                src.as_ptr(),
                &mut encoded_size,
                dst.as_mut_ptr(),
            )
        };

        if status != ffi::BROTLI_TRUE {
            return None;
        }

        // SAFETY: the encoder reported success, so the first `encoded_size`
        // bytes of `dst` are initialized and `encoded_size` is within the
        // capacity reserved above.
        unsafe { dst.set_len(encoded_size) };
        Some(encoded_size)
    }

    /// Decompresses `src`, expecting exactly `decoded_size` bytes of output.
    ///
    /// Returns `None` when the stream is invalid or its size does not match.
    pub fn decompress(src: &[u8], decoded_size: usize) -> Option<Vec<u8>> {
        let mut decoded = vec![0; decoded_size];
        let mut written = decoded.len();

        // SAFETY: `src` is a valid slice of `src.len()` readable bytes and
        // `decoded` owns `written` writable, initialized bytes. The decoder
        // writes at most `written` bytes and reports the actual count back.
        let result = unsafe {
            ffi::BrotliDecoderDecompress(
                src.len(),
                src.as_ptr(),
                &mut written,
                decoded.as_mut_ptr(),
            )
        };

        if result != ffi::BROTLI_DECODER_RESULT_SUCCESS || written != decoded_size {
            return None;
        }

        Some(decoded)
    }
}

/// A named benchmark input.
struct Corpus {
    name: String,
    data: Vec<u8>,
}

impl Corpus {
    fn new(name: impl Into<String>, data: Vec<u8>) -> Self {
        Self {
            name: name.into(),
            data,
        }
    }
}

/// Builds the deterministic corpora: text, binary, compressible,
/// incompressible, small, and large inputs.
fn corpora() -> Vec<Corpus> {
    let mut corpora = vec![
        Corpus::new("text-1KiB", text(1 << 10)),
        Corpus::new("text-1MiB", text(1 << 20)),
        Corpus::new("binary-256KiB", binary(1 << 18)),
        Corpus::new("compressible-256KiB", compressible(1 << 18)),
        Corpus::new("incompressible-256KiB", incompressible(1 << 18)),
    ];
    corpora.extend(vendor_corpora());
    corpora
}

/// Payload sizes used to measure the fixed per-call cost.
const TINY_SIZES: [usize; 4] = [16, 64, 256, 1024];

/// Files of Google Brotli's own corpus used as real-world inputs.
const VENDOR_FILES: [&str; 6] = [
    "alice29.txt",
    "lcet10.txt",
    "plrabn12.txt",
    "mapsdatazrh",
    "random_org_10k.bin",
    "quickfox_repeated",
];

/// Reads the vendored reference corpus, skipping files that are not present.
fn vendor_corpora() -> Vec<Corpus> {
    let directory =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("brotli-ffi/vendor/brotli/tests/testdata");
    VENDOR_FILES
        .iter()
        .filter_map(|name| {
            let data = std::fs::read(directory.join(name)).ok()?;
            Some(Corpus::new(format!("vendor-{name}"), data))
        })
        .collect()
}

/// Generates `len` bytes of English-like text.
fn text(len: usize) -> Vec<u8> {
    const PARAGRAPH: &str = concat!(
        "Brotli is a generic-purpose lossless compression algorithm that ",
        "compresses data using a combination of a modern variant of the LZ77 ",
        "algorithm, Huffman coding and second order context modeling. ",
    );

    let mut out = String::with_capacity(len + PARAGRAPH.len());
    let mut line = 0_usize;
    while out.len() < len {
        out.push_str(&format!("{line}. {PARAGRAPH}\n"));
        line += 1;
    }

    let mut bytes = out.into_bytes();
    bytes.truncate(len);
    bytes
}

/// Generates `len` bytes of structured binary data: fixed-size records holding
/// a counter, a derived tag, and bytes drawn from a small pool.
fn binary(len: usize) -> Vec<u8> {
    const POOL: [u8; 8] = [0x00, 0xff, 0x7f, 0x80, 0x01, 0xfe, 0x10, 0xef];

    let mut bytes = Vec::with_capacity(len + 16);
    let mut record = 0_u32;
    while bytes.len() < len {
        bytes.extend_from_slice(&record.to_le_bytes());
        bytes.extend_from_slice(&record.wrapping_mul(2_654_435_761).to_le_bytes());
        for offset in 0..8 {
            bytes.push(POOL[(record as usize + offset) % POOL.len()]);
        }
        record += 1;
    }

    bytes.truncate(len);
    bytes
}

/// Generates `len` bytes of highly compressible data: long runs broken up by a
/// short repeating marker.
fn compressible(len: usize) -> Vec<u8> {
    let mut bytes = vec![0; len];
    for (index, byte) in bytes.iter_mut().enumerate() {
        if index % 1024 == 0 {
            *byte = b'#';
        }
    }
    bytes
}

/// Generates `len` bytes of incompressible data from a deterministic PRNG.
fn incompressible(len: usize) -> Vec<u8> {
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as u8
        })
        .collect()
}

/// Verifies both implementations against the C decoder and reports the
/// compressed sizes they produced.
fn validate(params: CompressParams, corpus: &Corpus) {
    let quality = usize::from(params.quality());
    let input = corpus.data.as_slice();

    let mut c_output = Vec::new();
    let c_size =
        c_brotli::compress_into(quality, usize::from(params.lgwin()), input, &mut c_output)
            .expect("C Brotli failed to compress the corpus");
    assert_eq!(
        c_brotli::decompress(&c_output, input.len()).as_deref(),
        Some(input),
        "C Brotli output does not round-trip for {} at q{quality}",
        corpus.name,
    );

    let compressor = Brotli::default().compressor();
    let rust_output = compressor
        .compress(params, input)
        .expect("mbrotli failed to compress the corpus");
    assert_eq!(
        c_brotli::decompress(&rust_output, input.len()).as_deref(),
        Some(input),
        "mbrotli output does not round-trip for {} at q{quality}",
        corpus.name,
    );
    // Parity with the reference is a test-suite guarantee; reasserting it here
    // keeps a benchmark run from reporting a speedup that changed the output.
    assert_eq!(
        rust_output, c_output,
        "mbrotli and C Brotli disagree for {} at q{quality}",
        corpus.name,
    );
    println!(
        "q{quality} {name:<26} c-brotli {c_size:>9} bytes  mbrotli {rust_size:>9} bytes",
        name = corpus.name,
        rust_size = rust_output.len(),
    );
}

/// Registers the end-to-end one-shot comparison, allocation included.
fn bench_oneshot(criterion: &mut Criterion) {
    let corpora = corpora();

    println!(
        "corpus validation (lgwin {}, {:?})",
        usize::from(LGWIN),
        Brotli::default()
    );

    for quality in QUALITIES {
        let params = CompressParams::new(quality, LGWIN);
        let numeric_quality = usize::from(quality);
        let mut group = criterion.benchmark_group(format!("oneshot/q{numeric_quality}"));
        let compressor = Brotli::default().compressor();

        for corpus in &corpora {
            validate(params, corpus);
            let data = corpus.data.as_slice();
            group.throughput(Throughput::Bytes(data.len() as u64));

            group.bench_with_input(
                BenchmarkId::new("c-brotli", &corpus.name),
                &data,
                |bencher, data| {
                    bencher.iter(|| {
                        let mut output = Vec::new();
                        c_brotli::compress_into(
                            numeric_quality,
                            LGWIN.into(),
                            black_box(data),
                            &mut output,
                        )
                        .expect("C Brotli failed to compress the corpus");
                        output
                    });
                },
            );

            group.bench_with_input(
                BenchmarkId::new("mbrotli", &corpus.name),
                &data,
                |bencher, data| {
                    bencher.iter(|| {
                        compressor
                            .compress(params, black_box(data))
                            .expect("mbrotli failed to compress the corpus")
                    });
                },
            );
        }

        group.finish();
    }
}

/// Registers the comparison into a caller-owned, pre-sized output buffer.
///
/// Output allocation and growth leave the timed region on both sides; the
/// encoder workspaces are still built per call, exactly as the public API of
/// each implementation does it.
fn bench_presized(criterion: &mut Criterion) {
    let corpora = corpora();

    for quality in QUALITIES {
        let params = CompressParams::new(quality, LGWIN);
        let numeric_quality = usize::from(quality);
        let mut group = criterion.benchmark_group(format!("presized/q{numeric_quality}"));
        let compressor = Brotli::default().compressor();

        for corpus in &corpora {
            let data = corpus.data.as_slice();
            group.throughput(Throughput::Bytes(data.len() as u64));

            let mut c_output = Vec::with_capacity(c_brotli::max_compressed_size(data.len()) + 1024);
            group.bench_with_input(
                BenchmarkId::new("c-brotli", &corpus.name),
                &data,
                |bencher, data| {
                    bencher.iter(|| {
                        c_brotli::compress_into(
                            numeric_quality,
                            LGWIN.into(),
                            black_box(data),
                            &mut c_output,
                        )
                        .expect("C Brotli failed to compress the corpus");
                    });
                },
            );

            let bound = compressor
                .calculate_bound(&params, data.len())
                .expect("the compressed-size bound overflowed");
            let mut rust_output = vec![0u8; bound];
            group.bench_with_input(
                BenchmarkId::new("mbrotli", &corpus.name),
                &data,
                |bencher, data| {
                    bencher.iter(|| {
                        compressor
                            .compress_to_slice(params, black_box(data), &mut rust_output)
                            .expect("mbrotli failed to compress the corpus")
                    });
                },
            );
        }

        group.finish();
    }
}

/// Registers the per-call overhead measurement, dispatch included.
fn bench_tiny(criterion: &mut Criterion) {
    let payload = text(*TINY_SIZES.iter().max().unwrap_or(&1024));

    for quality in QUALITIES {
        let params = CompressParams::new(quality, LGWIN);
        let numeric_quality = usize::from(quality);
        let mut group = criterion.benchmark_group(format!("tiny/q{numeric_quality}"));
        let compressor = Brotli::default().compressor();

        for size in TINY_SIZES {
            let data = &payload[..size];
            group.throughput(Throughput::Bytes(size as u64));

            group.bench_with_input(
                BenchmarkId::new("c-brotli", size),
                &data,
                |bencher, data| {
                    bencher.iter(|| {
                        let mut output = Vec::new();
                        c_brotli::compress_into(
                            numeric_quality,
                            LGWIN.into(),
                            black_box(data),
                            &mut output,
                        )
                        .expect("C Brotli failed to compress the corpus");
                        output
                    });
                },
            );

            group.bench_with_input(BenchmarkId::new("mbrotli", size), &data, |bencher, data| {
                bencher.iter(|| {
                    compressor
                        .compress(params, black_box(data))
                        .expect("mbrotli failed to compress the corpus")
                });
            });
        }

        group.finish();
    }
}

criterion_group!(benches, bench_oneshot, bench_presized, bench_tiny);
criterion_main!(benches);
