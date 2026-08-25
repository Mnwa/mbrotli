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
use mbrotli::compressor::{CompressParams, CompressWorkspace, QualityLevel, WindowBits};
use std::hint::black_box;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

/// Sliding window size used by every benchmark.
const LGWIN: WindowBits = WindowBits::DEFAULT;

/// Applies the sampling policy a quality needs to finish in useful time.
///
/// Qualities ten and eleven solve a dynamic program over every match at every
/// position: quality eleven costs roughly seven hundred milliseconds per
/// mebibyte on an Apple M5 Pro, against six at quality nine. At Criterion's
/// default hundred samples one such case alone would run for over a minute and
/// the whole sweep for hours, so a plain `cargo bench` would be unusable.
///
/// The numbers are the ones every recorded run in `docs/benchmarks/` already
/// uses on the command line, so a default run and a recorded one agree rather
/// than producing two different sample counts for the same case.
///
/// Fewer samples widen the confidence interval; they do not bias the estimate,
/// and Criterion still reports the interval so a reader can see the cost. The
/// alternative — shortening the input — would change *what* is measured, since
/// the meta-block and block-splitting decisions depend on length.
fn configure<M: criterion::measurement::Measurement>(
    group: &mut criterion::BenchmarkGroup<'_, M>,
    quality: QualityLevel,
) {
    if usize::from(quality) >= 10 {
        group.sample_size(10);
        group.warm_up_time(Duration::from_secs(1));
        group.measurement_time(Duration::from_secs(3));
    }
}

/// Quality levels under measurement.
///
/// Every implemented quality is gated separately, so a gain at one may not be
/// used to cover a loss at another.
const QUALITIES: [QualityLevel; 12] = [
    QualityLevel::Q0,
    QualityLevel::Q1,
    QualityLevel::Q2,
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

    /// Compresses `chunks` one at a time, flushing between them.
    ///
    /// Every chunk but the last is followed by `BROTLI_OPERATION_FLUSH` and the
    /// last by `BROTLI_OPERATION_FINISH`, which is the reference behaviour
    /// `CompressorWriter::flush` reproduces. `dst` is replaced with the stream.
    ///
    /// Returns the compressed length, or `None` when the encoder fails.
    pub fn compress_flushing(
        quality: usize,
        lgwin: usize,
        chunks: &[&[u8]],
        dst: &mut Vec<u8>,
    ) -> Option<usize> {
        let total: usize = chunks.iter().map(|chunk| chunk.len()).sum();
        // A flush costs a padding block per chunk on top of the bound.
        dst.clear();
        dst.resize(max_compressed_size(total) + 64 * chunks.len() + 64, 0);
        let mut written = 0usize;

        // SAFETY: every pointer below is derived from a live slice or from
        // `dst`'s own allocation, and the loop keeps `written` at or below
        // `dst.len()` because `available_out` is what the encoder decrements.
        // The state is created and destroyed inside this block and never
        // escapes it.
        unsafe {
            let state = ffi::BrotliEncoderCreateInstance(None, None, std::ptr::null_mut());
            if state.is_null() {
                return None;
            }
            ffi::BrotliEncoderSetParameter(state, ffi::BROTLI_PARAM_QUALITY, quality as u32);
            ffi::BrotliEncoderSetParameter(state, ffi::BROTLI_PARAM_LGWIN, lgwin as u32);

            for (index, chunk) in chunks.iter().enumerate() {
                let operation = if index + 1 == chunks.len() {
                    ffi::BROTLI_OPERATION_FINISH
                } else {
                    ffi::BROTLI_OPERATION_FLUSH
                };
                let mut available_in = chunk.len();
                let mut next_in = chunk.as_ptr();
                loop {
                    let mut available_out = dst.len() - written;
                    let mut next_out = dst.as_mut_ptr().add(written);
                    let mut total_out = 0usize;
                    let ok = ffi::BrotliEncoderCompressStream(
                        state,
                        operation,
                        &mut available_in,
                        &mut next_in,
                        &mut available_out,
                        &mut next_out,
                        &mut total_out,
                    );
                    if ok != ffi::BROTLI_TRUE {
                        ffi::BrotliEncoderDestroyInstance(state);
                        return None;
                    }
                    written = dst.len() - available_out;
                    if available_in == 0
                        && ffi::BrotliEncoderHasMoreOutput(state) != ffi::BROTLI_TRUE
                    {
                        break;
                    }
                }
            }
            let finished = ffi::BrotliEncoderIsFinished(state);
            ffi::BrotliEncoderDestroyInstance(state);
            if finished != ffi::BROTLI_TRUE {
                return None;
            }
        }

        dst.truncate(written);
        Some(written)
    }

    /// One prepared dictionary, destroyed when it is dropped.
    pub struct Prepared(*mut ffi::BrotliEncoderPreparedDictionary);

    impl Prepared {
        /// Prepares `source` as an LZ77 prefix for `quality`.
        ///
        /// Returns `None` when the reference declines to prepare it.
        pub fn new(source: &[u8], quality: usize) -> Option<Self> {
            // SAFETY: `source` is a valid slice of `source.len()` readable
            // bytes; the reference copies what it needs and returns an owned
            // instance, which `Drop` below hands straight back to it.
            let prepared = unsafe {
                ffi::BrotliEncoderPrepareDictionary(
                    ffi::BROTLI_SHARED_DICTIONARY_RAW,
                    source.len(),
                    source.as_ptr(),
                    quality as c_int,
                    None,
                    None,
                    std::ptr::null_mut(),
                )
            };
            (!prepared.is_null()).then_some(Self(prepared))
        }
    }

    impl Drop for Prepared {
        fn drop(&mut self) {
            // SAFETY: `self.0` came from `BrotliEncoderPrepareDictionary` and
            // is destroyed exactly once, here.
            unsafe { ffi::BrotliEncoderDestroyPreparedDictionary(self.0) };
        }
    }

    /// Compresses `src` with `prefix` attached, replacing `dst`.
    ///
    /// Returns the compressed length, or `None` when the encoder fails.
    pub fn compress_with_prefix(
        quality: usize,
        lgwin: usize,
        prefix: &Prepared,
        src: &[u8],
        dst: &mut Vec<u8>,
    ) -> Option<usize> {
        dst.clear();
        dst.resize(max_compressed_size(src.len()) + 64, 0);
        let written;

        // SAFETY: as `compress_flushing` above; the prepared dictionary
        // outlives the state because `prefix` is borrowed for the call.
        unsafe {
            let state = ffi::BrotliEncoderCreateInstance(None, None, std::ptr::null_mut());
            if state.is_null() {
                return None;
            }
            ffi::BrotliEncoderSetParameter(state, ffi::BROTLI_PARAM_QUALITY, quality as u32);
            ffi::BrotliEncoderSetParameter(state, ffi::BROTLI_PARAM_LGWIN, lgwin as u32);
            ffi::BrotliEncoderSetParameter(state, ffi::BROTLI_PARAM_SIZE_HINT, 0);
            if ffi::BrotliEncoderAttachPreparedDictionary(state, prefix.0) != ffi::BROTLI_TRUE {
                ffi::BrotliEncoderDestroyInstance(state);
                return None;
            }

            let mut available_in = src.len();
            let mut next_in = src.as_ptr();
            let mut available_out = dst.len();
            let mut next_out = dst.as_mut_ptr();
            let mut total_out = 0usize;
            let ok = ffi::BrotliEncoderCompressStream(
                state,
                ffi::BROTLI_OPERATION_FINISH,
                &mut available_in,
                &mut next_in,
                &mut available_out,
                &mut next_out,
                &mut total_out,
            );
            let finished = ffi::BrotliEncoderIsFinished(state);
            ffi::BrotliEncoderDestroyInstance(state);
            if ok != ffi::BROTLI_TRUE || finished != ffi::BROTLI_TRUE {
                return None;
            }
            written = dst.len() - available_out;
        }

        dst.truncate(written);
        Some(written)
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
        configure(&mut group, quality);
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
        configure(&mut group, quality);
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
        configure(&mut group, quality);
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

/// Qualities the workspace, flush and prefix groups are measured at.
///
/// One from each encoder core, plus quality 5 as the cheapest quality that can
/// consult an attached prefix. Sweeping all twelve would triple a run that
/// already takes tens of minutes without saying anything the three cores do
/// not already say.
const REPRESENTATIVE: [QualityLevel; 5] = [
    QualityLevel::Q1,
    QualityLevel::Q2,
    QualityLevel::Q5,
    QualityLevel::Q9,
    QualityLevel::Q11,
];

/// Payload sizes the workspace group measures.
///
/// A retained workspace saves an allocation, not a comparison, so the win is
/// whatever the allocation was worth relative to the compression: large at a
/// small payload, negligible at a big one. Both ends are measured.
const WORKSPACE_SIZES: [usize; 4] = [256, 4 << 10, 64 << 10, 1 << 20];

/// Registers the reuse comparison for [`CompressWorkspace`].
///
/// Three arms per case. `c-brotli` and `mbrotli` are the ordinary one-shot
/// calls, both of which build a whole encoder per call — that is what the
/// reference's own one-shot entry point does, and it has no reuse to compare
/// against. `mbrotli-reused` is the same call through a retained workspace.
/// The size hint is pinned so every payload resolves to the same encoder shape
/// and the workspace stays on its reuse path.
fn bench_workspace(criterion: &mut Criterion) {
    let compressor = Brotli::default().compressor();

    for quality in REPRESENTATIVE {
        let numeric_quality = usize::from(quality);
        let params = CompressParams::new(quality, LGWIN).with_size_hint(Some(0));
        let mut group = criterion.benchmark_group(format!("workspace/q{numeric_quality}"));
        configure(&mut group, quality);

        for size in WORKSPACE_SIZES {
            let data = text(size);
            let data = data.as_slice();
            group.throughput(Throughput::Bytes(data.len() as u64));

            // Reuse must not change a byte; a benchmark that let it would be
            // measuring two different things.
            let mut workspace = CompressWorkspace::default();
            assert_eq!(
                compressor
                    .compress_with(&mut workspace, params, data)
                    .expect("reused"),
                compressor.compress(params, data).expect("fresh"),
                "q{numeric_quality}: a reused workspace changed the stream",
            );

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
                        .expect("C Brotli failed to compress");
                        output
                    });
                },
            );

            group.bench_with_input(BenchmarkId::new("mbrotli", size), &data, |bencher, data| {
                bencher.iter(|| {
                    compressor
                        .compress(params, black_box(data))
                        .expect("mbrotli failed to compress")
                });
            });

            group.bench_with_input(
                BenchmarkId::new("mbrotli-reused", size),
                &data,
                |bencher, data| {
                    let mut workspace = CompressWorkspace::default();
                    bencher.iter(|| {
                        compressor
                            .compress_with(&mut workspace, params, black_box(data))
                            .expect("mbrotli failed to compress")
                    });
                },
            );
        }

        group.finish();
    }
}

/// Chunk counts the flush group splits its payload into.
///
/// One chunk is the no-flush baseline; the rest flush that many times minus
/// one, so the cost of a flush and the ratio it gives up are both visible.
const FLUSH_CHUNKS: [usize; 4] = [1, 4, 32, 256];

/// Splits `data` into `count` roughly equal chunks.
fn chunked(data: &[u8], count: usize) -> Vec<&[u8]> {
    let size = data.len().div_ceil(count.max(1));
    data.chunks(size.max(1)).collect()
}

/// Registers the flushing-writer comparison.
///
/// Both sides compress the same chunks and flush between them: this crate
/// through `CompressorWriter::flush`, the reference through
/// `BROTLI_OPERATION_FLUSH`. The compressed sizes are printed alongside,
/// because a flush trades ratio for latency and a timing without the size next
/// to it would hide half the trade.
fn bench_flush(criterion: &mut Criterion) {
    let compressor = Brotli::default().compressor();
    let payload = text(1 << 18);

    for quality in REPRESENTATIVE {
        let numeric_quality = usize::from(quality);
        let params = CompressParams::new(quality, LGWIN);
        let mut group = criterion.benchmark_group(format!("flush/q{numeric_quality}"));
        configure(&mut group, quality);

        for count in FLUSH_CHUNKS {
            let chunks = chunked(&payload, count);
            group.throughput(Throughput::Bytes(payload.len() as u64));

            let ours = flush_with_writer(&compressor, params, &chunks);
            let mut theirs = Vec::new();
            c_brotli::compress_flushing(numeric_quality, LGWIN.into(), &chunks, &mut theirs)
                .expect("C Brotli failed to compress");
            assert_eq!(
                ours, theirs,
                "q{numeric_quality}: flushing every {count} chunks left the reference",
            );
            println!(
                "q{numeric_quality} flush x{count:<4} {} bytes -> {} bytes",
                payload.len(),
                ours.len(),
            );

            group.bench_with_input(
                BenchmarkId::new("c-brotli", count),
                &chunks,
                |bencher, chunks| {
                    bencher.iter(|| {
                        let mut output = Vec::new();
                        c_brotli::compress_flushing(
                            numeric_quality,
                            LGWIN.into(),
                            black_box(chunks),
                            &mut output,
                        )
                        .expect("C Brotli failed to compress");
                        output
                    });
                },
            );

            group.bench_with_input(
                BenchmarkId::new("mbrotli", count),
                &chunks,
                |bencher, chunks| {
                    bencher.iter(|| flush_with_writer(&compressor, params, black_box(chunks)));
                },
            );
        }

        group.finish();
    }
}

/// Writes every chunk through the adapter, flushing after all but the last.
fn flush_with_writer(
    compressor: &mbrotli::compressor::Compressor,
    params: CompressParams,
    chunks: &[&[u8]],
) -> Vec<u8> {
    let mut sink = compressor.compress_writer(params, Vec::new());
    for (index, chunk) in chunks.iter().enumerate() {
        sink.write_all(chunk).expect("write failed");
        if index + 1 != chunks.len() {
            sink.flush().expect("flush failed");
        }
    }
    sink.finish().expect("finish failed")
}

/// Qualities whose match finders consult an attached prefix.
const PREFIX_QUALITIES: [QualityLevel; 3] = [QualityLevel::Q5, QualityLevel::Q9, QualityLevel::Q11];

/// Registers the attached-prefix comparison.
///
/// The dictionary is one half of a real text corpus and the payload the other,
/// which is the shape a shared dictionary is deployed in. Preparation happens
/// once, outside the timed region, on both sides: it is a per-connection cost,
/// not a per-request one, and timing it here would measure the wrong thing.
fn bench_shared(criterion: &mut Criterion) {
    let compressor = Brotli::default().compressor();
    let Some(corpus) = vendor_corpora()
        .into_iter()
        .find(|corpus| corpus.name.ends_with("alice29.txt"))
    else {
        return;
    };
    let (prefix, payload) = corpus.data.split_at(corpus.data.len() / 2);

    for quality in PREFIX_QUALITIES {
        let numeric_quality = usize::from(quality);
        let params = CompressParams::new(quality, LGWIN).with_size_hint(Some(0));
        let mut group = criterion.benchmark_group(format!("shared/q{numeric_quality}"));
        configure(&mut group, quality);
        group.throughput(Throughput::Bytes(payload.len() as u64));

        let prepared = c_brotli::Prepared::new(prefix, numeric_quality)
            .expect("C Brotli failed to prepare the dictionary");
        let mut context = compressor
            .shared_context_builder(quality)
            .add_prefix_dictionary(prefix)
            .prepare()
            .expect("mbrotli failed to prepare the dictionary");

        let ours = compressor
            .compress_shared(params, &mut context, payload)
            .expect("mbrotli failed to compress");
        let mut theirs = Vec::new();
        c_brotli::compress_with_prefix(
            numeric_quality,
            LGWIN.into(),
            &prepared,
            payload,
            &mut theirs,
        )
        .expect("C Brotli failed to compress");
        assert_eq!(
            ours, theirs,
            "q{numeric_quality}: an attached prefix left the reference",
        );
        let without = compressor
            .compress(params, payload)
            .expect("mbrotli failed to compress");
        println!(
            "q{numeric_quality} prefix {:>8} bytes  with {:>8} bytes  without {:>8} bytes",
            payload.len(),
            ours.len(),
            without.len(),
        );

        group.bench_function(BenchmarkId::new("c-brotli", "alice29-half"), |bencher| {
            bencher.iter(|| {
                let mut output = Vec::new();
                c_brotli::compress_with_prefix(
                    numeric_quality,
                    LGWIN.into(),
                    &prepared,
                    black_box(payload),
                    &mut output,
                )
                .expect("C Brotli failed to compress");
                output
            });
        });

        group.bench_function(BenchmarkId::new("mbrotli", "alice29-half"), |bencher| {
            bencher.iter(|| {
                compressor
                    .compress_shared(params, &mut context, black_box(payload))
                    .expect("mbrotli failed to compress")
            });
        });

        // The same payload with nothing attached, so the cost of consulting a
        // dictionary is separable from the cost of compressing at all.
        group.bench_function(
            BenchmarkId::new("mbrotli-no-prefix", "alice29-half"),
            |bencher| {
                bencher.iter(|| {
                    compressor
                        .compress(params, black_box(payload))
                        .expect("mbrotli failed to compress")
                });
            },
        );

        group.finish();
    }
}

criterion_group!(
    benches,
    bench_oneshot,
    bench_presized,
    bench_tiny,
    bench_workspace,
    bench_flush,
    bench_shared
);
criterion_main!(benches);
