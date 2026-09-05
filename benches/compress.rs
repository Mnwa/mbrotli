//! Criterion benchmarks for the Brotli qualities this crate implements.
//!
//! Every case feeds identical bytes, quality, window size, and encoder mode to
//! this crate and to Google's C Brotli exposed by the `google-brotli-ffi`
//! workspace crate. Each group documents whether compressor construction,
//! output allocation, and streaming delivery are inside its timed operation.
//!
//! Corpora are generated deterministically at startup, or read from Google
//! Brotli's own test data in `brotli-ffi/vendor/brotli/tests/testdata`, and
//! validated before any timing: every compressed stream is decoded with the C
//! decoder and compared with the original input, and the compressed sizes are
//! printed so a speedup can be checked against the compression ratio it
//! produced.
//!
//! The shapes the acceptance gate names are measured separately, because a
//! stateful encoder makes them genuinely different work:
//!
//! * `cold` — build the compressor, allocate the output, compress once. This is
//!   what a caller who compresses one thing pays. Construction is also timed
//!   in the cold tiny-input cases.
//! * `reused` — repeated `compress_into` into a destination that is already big
//!   enough. Rust retains encoder workspace; C constructs state for each stream.
//! * `presized` — `compress_to_slice` into a caller-owned buffer.
//! * `writer`, `reader`, `session` — the three streaming shapes, in large
//!   chunks.
//! * `tiny` — per-call overhead on payloads where it dominates.
//!
//! Both sides reuse output storage in the reused and presized groups. C still
//! creates and destroys encoder state for every independent stream.
//!
//! A full corpus checkout provides 658 paired Rust/C cases.
//! Tiny payloads are validated at each quality before their timing begins.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use mbrotli::dictionary::DictionaryBuilder;
use mbrotli::io::FinishError;
use mbrotli::{
    Compressor, EncoderConfig, EncoderStatus, InputSize, Operation, Quality, StreamConfig, Window,
};
use std::hint::black_box;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

/// Sliding window size used by every benchmark.
const LGWIN: Window = Window::DEFAULT;

/// Returns the configuration a benchmark at `quality` runs under.
fn config(quality: Quality) -> EncoderConfig {
    EncoderConfig::default()
        .with_quality(quality)
        .with_window(LGWIN)
}

/// Builds a compressor for `quality`.
fn encoder(quality: Quality) -> Compressor {
    Compressor::new(config(quality)).expect("a legal configuration")
}

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
    quality: Quality,
) {
    if quality >= Quality::Q10 {
        group.sample_size(10);
        group.warm_up_time(Duration::from_secs(1));
        group.measurement_time(Duration::from_secs(3));
    }
}

/// Quality levels under measurement.
///
/// Every implemented quality is gated separately, so a gain at one may not be
/// used to cover a loss at another.
const QUALITIES: [Quality; 12] = [
    Quality::Q0,
    Quality::Q1,
    Quality::Q2,
    Quality::Q3,
    Quality::Q4,
    Quality::Q5,
    Quality::Q6,
    Quality::Q7,
    Quality::Q8,
    Quality::Q9,
    Quality::Q10,
    Quality::Q11,
];

/// Safe wrappers over the raw C Brotli bindings.
mod c_brotli {
    use google_brotli_ffi as ffi;
    use std::ffi::c_int;

    /// Conservative capacity for the C stream, without relying on one-shot rewrites.
    pub fn max_compressed_size(input_size: usize) -> usize {
        input_size
            .checked_mul(2)
            .and_then(|size| size.checked_add(4096))
            .expect("bounded benchmark corpus")
    }

    /// Compresses `src` into `dst` with C streaming FINISH, without outer rewrites.
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
        let mut available_in = src.len();
        let mut next_in = src.as_ptr();
        let mut available_out = dst.capacity();
        let mut next_out = dst.as_mut_ptr();
        let mut encoded_size = 0;
        // SAFETY: source and destination are disjoint live allocations. The C
        // API writes at most available_out bytes and reports its initialized
        // prefix in encoded_size. State is destroyed on every constructed path.
        let complete = unsafe {
            let state = ffi::BrotliEncoderCreateInstance(None, None, std::ptr::null_mut());
            if state.is_null() {
                return None;
            }
            for (parameter, value) in [
                (ffi::BROTLI_PARAM_QUALITY, quality as u32),
                (ffi::BROTLI_PARAM_LGWIN, lgwin as u32),
                (ffi::BROTLI_PARAM_SIZE_HINT, src.len() as u32),
            ] {
                if ffi::BrotliEncoderSetParameter(state, parameter, value) != ffi::BROTLI_TRUE {
                    ffi::BrotliEncoderDestroyInstance(state);
                    return None;
                }
            }
            let status = ffi::BrotliEncoderCompressStream(
                state,
                ffi::BROTLI_OPERATION_FINISH,
                &raw mut available_in,
                &raw mut next_in,
                &raw mut available_out,
                &raw mut next_out,
                &raw mut encoded_size,
            );
            let finished = ffi::BrotliEncoderIsFinished(state);
            ffi::BrotliEncoderDestroyInstance(state);
            status == ffi::BROTLI_TRUE && finished == ffi::BROTLI_TRUE
        };
        if !complete {
            return None;
        }
        // SAFETY: successful finalization initialized exactly this prefix.
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

    /// Compresses `chunks` one at a time, without flushing between them.
    ///
    /// Every chunk but the last is fed with `BROTLI_OPERATION_PROCESS` and the
    /// last with `BROTLI_OPERATION_FINISH`, which is what a `Write` adapter that
    /// is never flushed does. `dst` is replaced with the stream.
    ///
    /// Returns the compressed length, or `None` when the encoder fails.
    pub fn compress_streaming(
        quality: usize,
        lgwin: usize,
        chunks: &[&[u8]],
        dst: &mut Vec<u8>,
    ) -> Option<usize> {
        // C's fast streaming API encodes each PROCESS chunk immediately. Rust
        // stages up to the configured window for chunk-independent output.
        // Normalize C to those same fragment boundaries, including the staging
        // allocation/copy in the timed end-to-end operation on both sides.
        if quality < 2 {
            let staged = chunks.concat();
            let fragments: Vec<_> = if staged.is_empty() {
                vec![staged.as_slice()]
            } else {
                staged.chunks(1usize << lgwin).collect()
            };
            compress_streaming_fragments(quality, lgwin, &fragments, dst)
        } else {
            compress_streaming_fragments(quality, lgwin, chunks, dst)
        }
    }

    fn compress_streaming_fragments(
        quality: usize,
        lgwin: usize,
        chunks: &[&[u8]],
        dst: &mut Vec<u8>,
    ) -> Option<usize> {
        let total: usize = chunks.iter().map(|chunk| chunk.len()).sum();
        dst.clear();
        dst.resize(max_compressed_size(total) + 1024, 0);
        let mut written = 0usize;

        // SAFETY: as `compress_flushing` below; every pointer is derived from a
        // live slice or from `dst`'s own allocation, `written` stays at or below
        // `dst.len()` because `available_out` is what the encoder decrements,
        // and the state never escapes this block.
        unsafe {
            let state = ffi::BrotliEncoderCreateInstance(None, None, std::ptr::null_mut());
            if state.is_null() {
                return None;
            }
            ffi::BrotliEncoderSetParameter(state, ffi::BROTLI_PARAM_QUALITY, quality as u32);
            ffi::BrotliEncoderSetParameter(state, ffi::BROTLI_PARAM_LGWIN, lgwin as u32);
            ffi::BrotliEncoderSetParameter(state, ffi::BROTLI_PARAM_SIZE_HINT, total as u32);

            for (index, chunk) in chunks.iter().enumerate() {
                let operation = if index + 1 == chunks.len() {
                    ffi::BROTLI_OPERATION_FINISH
                } else {
                    ffi::BROTLI_OPERATION_PROCESS
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
fn validate(quality: Quality, corpus: &Corpus) {
    let numeric = usize::from(quality.get());
    let input = corpus.data.as_slice();

    let mut c_output = Vec::new();
    let c_size = c_brotli::compress_into(numeric, usize::from(LGWIN.bits()), input, &mut c_output)
        .expect("C Brotli failed to compress the corpus");
    assert_eq!(
        c_brotli::decompress(&c_output, input.len()).as_deref(),
        Some(input),
        "C Brotli output does not round-trip for {} at q{numeric}",
        corpus.name,
    );

    let rust_output = encoder(quality)
        .compress(input)
        .expect("mbrotli failed to compress the corpus");
    assert_eq!(
        c_brotli::decompress(&rust_output, input.len()).as_deref(),
        Some(input),
        "mbrotli output does not round-trip for {} at q{numeric}",
        corpus.name,
    );
    // Parity with the reference is a test-suite guarantee; reasserting it here
    // keeps a benchmark run from reporting a speedup that changed the output.
    assert_eq!(
        rust_output, c_output,
        "mbrotli and C Brotli disagree for {} at q{numeric}",
        corpus.name,
    );
    println!(
        "q{numeric} {name:<26} c-brotli {c_size:>9} bytes  mbrotli {rust_size:>9} bytes",
        name = corpus.name,
        rust_size = rust_output.len(),
    );
}

/// Registers the cold comparison: build the encoder, allocate, compress once.
///
/// This is what a caller who compresses one thing pays. Both sides create and
/// destroy their encoder state inside the timed region, which is what
/// `BrotliEncoderCompress` does on every call anyway.
fn bench_cold(criterion: &mut Criterion) {
    let corpora = corpora();

    println!("corpus validation (lgwin {})", LGWIN.bits());

    for quality in QUALITIES {
        let numeric = usize::from(quality.get());
        let mut group = criterion.benchmark_group(format!("cold/q{numeric}"));
        configure(&mut group, quality);

        for corpus in &corpora {
            validate(quality, corpus);
            let data = corpus.data.as_slice();
            group.throughput(Throughput::Bytes(data.len() as u64));

            group.bench_with_input(
                BenchmarkId::new("c-brotli", &corpus.name),
                &data,
                |bencher, data| {
                    bencher.iter(|| {
                        let mut output = Vec::new();
                        c_brotli::compress_into(
                            numeric,
                            usize::from(LGWIN.bits()),
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
                        encoder(quality)
                            .compress(black_box(data))
                            .expect("mbrotli failed to compress the corpus")
                    });
                },
            );
        }

        group.finish();
    }
}

/// Registers the reused comparison: one encoder, one destination, many calls.
///
/// The reference's one-shot entry point builds a whole encoder per call and has
/// no reuse to compare against, so the `c-brotli` arm is the same work it always
/// does. That difference is the point of the shape.
fn bench_reused(criterion: &mut Criterion) {
    let corpora = corpora();

    for quality in QUALITIES {
        let numeric = usize::from(quality.get());
        let mut group = criterion.benchmark_group(format!("reused/q{numeric}"));
        configure(&mut group, quality);

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
                            numeric,
                            usize::from(LGWIN.bits()),
                            black_box(data),
                            &mut c_output,
                        )
                        .expect("C Brotli failed to compress the corpus");
                    });
                },
            );

            group.bench_with_input(
                BenchmarkId::new("mbrotli", &corpus.name),
                &data,
                |bencher, data| {
                    let mut compressor = encoder(quality);
                    let mut output = Vec::new();
                    // Warm the workspace and the destination, so the measured
                    // calls allocate nothing at all.
                    compressor
                        .compress_into(data, &mut output)
                        .expect("mbrotli failed to compress the corpus");
                    bencher.iter(|| {
                        output.clear();
                        compressor
                            .compress_into(black_box(data), &mut output)
                            .expect("mbrotli failed to compress the corpus")
                    });
                },
            );
        }

        group.finish();
    }
}

/// Registers the comparison into a caller-owned, pre-sized output buffer.
fn bench_presized(criterion: &mut Criterion) {
    let corpora = corpora();

    for quality in QUALITIES {
        let numeric = usize::from(quality.get());
        let mut group = criterion.benchmark_group(format!("presized/q{numeric}"));
        configure(&mut group, quality);

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
                            numeric,
                            usize::from(LGWIN.bits()),
                            black_box(data),
                            &mut c_output,
                        )
                        .expect("C Brotli failed to compress the corpus");
                    });
                },
            );

            let bound = Compressor::max_compressed_size(data.len())
                .expect("the compressed-size bound overflowed");
            let mut rust_output = vec![0u8; bound];
            group.bench_with_input(
                BenchmarkId::new("mbrotli", &corpus.name),
                &data,
                |bencher, data| {
                    let mut compressor = encoder(quality);
                    bencher.iter(|| {
                        compressor
                            .compress_to_slice(black_box(data), &mut rust_output)
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
        let numeric = usize::from(quality.get());
        let mut group = criterion.benchmark_group(format!("tiny/q{numeric}"));
        configure(&mut group, quality);

        for size in TINY_SIZES {
            let data = &payload[..size];
            validate(quality, &Corpus::new(format!("tiny-{size}"), data.to_vec()));
            group.throughput(Throughput::Bytes(size as u64));

            group.bench_with_input(
                BenchmarkId::new("c-brotli", size),
                &data,
                |bencher, data| {
                    bencher.iter(|| {
                        let mut output = Vec::new();
                        c_brotli::compress_into(
                            numeric,
                            usize::from(LGWIN.bits()),
                            black_box(data),
                            &mut output,
                        )
                        .expect("C Brotli failed to compress the corpus");
                        output
                    });
                },
            );

            // Cold, which is what the reference's own entry point does.
            group.bench_with_input(BenchmarkId::new("mbrotli", size), &data, |bencher, data| {
                bencher.iter(|| {
                    encoder(quality)
                        .compress(black_box(data))
                        .expect("mbrotli failed to compress the corpus")
                });
            });

            // And warm, which is what a server reusing one compressor pays.
            group.bench_with_input(
                BenchmarkId::new("mbrotli-reused", size),
                &data,
                |bencher, data| {
                    let mut compressor = encoder(quality);
                    let mut output = Vec::new();
                    compressor
                        .compress_into(data, &mut output)
                        .expect("mbrotli failed to compress");
                    bencher.iter(|| {
                        output.clear();
                        compressor
                            .compress_into(black_box(data), &mut output)
                            .expect("mbrotli failed to compress")
                    });
                },
            );
        }

        group.finish();
    }
}

/// Chunk size the streaming groups feed and drain in.
const STREAM_CHUNK: usize = 64 << 10;

/// Qualities the streaming, flush and dictionary groups are measured at.
///
/// One from each encoder core, plus quality 5 as the cheapest quality that can
/// consult a dictionary. Sweeping all twelve would triple a run that already
/// takes tens of minutes without saying anything the three cores do not.
const REPRESENTATIVE: [Quality; 5] = [
    Quality::Q1,
    Quality::Q2,
    Quality::Q5,
    Quality::Q9,
    Quality::Q11,
];

/// Registers the three streaming shapes against the reference's streaming API.
fn bench_streaming(criterion: &mut Criterion) {
    let corpora = corpora();

    for quality in REPRESENTATIVE {
        let numeric = usize::from(quality.get());
        let mut group = criterion.benchmark_group(format!("streaming/q{numeric}"));
        configure(&mut group, quality);

        for corpus in &corpora {
            let data = corpus.data.as_slice();
            if data.len() < STREAM_CHUNK {
                continue;
            }
            let chunks: Vec<&[u8]> = data.chunks(STREAM_CHUNK).collect();
            let stream = StreamConfig::from(InputSize::Exact(data.len() as u64));
            group.throughput(Throughput::Bytes(data.len() as u64));

            // Every shape has to reach the same bytes before any of them is
            // timed, or the comparison is between two different jobs.
            let mut compressor = encoder(quality);
            let mut expected = Vec::new();
            compressor
                .reader(data, stream)
                .expect("reader")
                .read_to_end(&mut expected)
                .expect("read");
            let mut writer = compressor.writer(Vec::new(), stream).expect("writer");
            for chunk in &chunks {
                writer.write_all(chunk).expect("write");
            }
            assert_eq!(
                writer.finish().expect("finish"),
                expected,
                "adapter equivalence"
            );
            let mut theirs = Vec::new();
            c_brotli::compress_streaming(numeric, usize::from(LGWIN.bits()), &chunks, &mut theirs)
                .expect("C Brotli failed to compress");
            assert_eq!(
                expected, theirs,
                "q{numeric} {}: the streamed reference differs",
                corpus.name
            );

            group.bench_with_input(
                BenchmarkId::new("c-brotli", &corpus.name),
                &chunks,
                |bencher, chunks| {
                    bencher.iter(|| {
                        let mut output = Vec::new();
                        c_brotli::compress_streaming(
                            numeric,
                            usize::from(LGWIN.bits()),
                            black_box(chunks),
                            &mut output,
                        )
                        .expect("C Brotli failed to compress");
                        output
                    });
                },
            );

            group.bench_with_input(
                BenchmarkId::new("mbrotli-writer", &corpus.name),
                &chunks,
                |bencher, chunks| {
                    bencher.iter(|| {
                        let mut sink = compressor
                            .writer(Vec::new(), stream)
                            .expect("a legal stream");
                        for chunk in black_box(chunks) {
                            sink.write_all(chunk).expect("write failed");
                        }
                        sink.finish()
                            .map_err(FinishError::into_error)
                            .expect("finish failed")
                    });
                },
            );

            group.bench_with_input(
                BenchmarkId::new("mbrotli-reader", &corpus.name),
                &data,
                |bencher, data| {
                    bencher.iter(|| {
                        let mut source = compressor
                            .reader(black_box(*data), stream)
                            .expect("a legal stream");
                        let mut output = Vec::new();
                        source.read_to_end(&mut output).expect("read failed");
                        output
                    });
                },
            );

            group.bench_with_input(
                BenchmarkId::new("mbrotli-session", &corpus.name),
                &data,
                |bencher, data| {
                    let mut buffer = vec![0u8; STREAM_CHUNK];
                    bencher.iter(|| {
                        let data = black_box(*data);
                        let mut output = Vec::new();
                        let mut session = compressor.start(stream).expect("a legal stream");
                        let mut offset = 0usize;
                        loop {
                            let take = (data.len() - offset).min(STREAM_CHUNK);
                            let operation = if offset + take == data.len() {
                                Operation::Finish
                            } else {
                                Operation::Process
                            };
                            let progress = session
                                .process(&data[offset..offset + take], &mut buffer, operation)
                                .expect("the session failed");
                            offset += progress.consumed;
                            output.extend_from_slice(&buffer[..progress.produced]);
                            if progress.status == EncoderStatus::Finished {
                                break;
                            }
                        }
                        output
                    });
                },
            );
        }

        group.finish();
    }
}

/// Chunk counts the flush group splits its payload into.
///
/// One chunk is the no-flush baseline; the rest flush that many times minus one,
/// so the cost of a flush and the ratio it gives up are both visible.
const FLUSH_CHUNKS: [usize; 4] = [1, 4, 32, 256];

/// Splits `data` into `count` roughly equal chunks.
fn chunked(data: &[u8], count: usize) -> Vec<&[u8]> {
    let size = data.len().div_ceil(count.max(1));
    data.chunks(size.max(1)).collect()
}

/// Writes every chunk through the adapter, flushing after all but the last.
fn flush_with_writer(compressor: &mut Compressor, chunks: &[&[u8]]) -> Vec<u8> {
    let mut sink = compressor
        .writer(Vec::new(), StreamConfig::default())
        .expect("a legal stream");
    for (index, chunk) in chunks.iter().enumerate() {
        sink.write_all(chunk).expect("write failed");
        if index + 1 != chunks.len() {
            sink.flush().expect("flush failed");
        }
    }
    sink.finish()
        .map_err(FinishError::into_error)
        .expect("finish failed")
}

/// Registers the flushing-writer comparison.
///
/// Both sides compress the same chunks and flush between them: this crate
/// through `Write::flush`, the reference through `BROTLI_OPERATION_FLUSH`. The
/// compressed sizes are printed alongside, because a flush trades ratio for
/// latency and a timing without the size next to it would hide half the trade.
fn bench_flush(criterion: &mut Criterion) {
    let payload = text(1 << 18);

    for quality in REPRESENTATIVE {
        let numeric = usize::from(quality.get());
        let mut group = criterion.benchmark_group(format!("flush/q{numeric}"));
        configure(&mut group, quality);

        for count in FLUSH_CHUNKS {
            let chunks = chunked(&payload, count);
            group.throughput(Throughput::Bytes(payload.len() as u64));

            let mut compressor = encoder(quality);
            let ours = flush_with_writer(&mut compressor, &chunks);
            let mut theirs = Vec::new();
            c_brotli::compress_flushing(numeric, usize::from(LGWIN.bits()), &chunks, &mut theirs)
                .expect("C Brotli failed to compress");
            assert_eq!(
                ours, theirs,
                "q{numeric}: flushing every {count} chunks left the reference",
            );
            println!(
                "q{numeric} flush x{count:<4} {} bytes -> {} bytes",
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
                            numeric,
                            usize::from(LGWIN.bits()),
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
                    bencher.iter(|| flush_with_writer(&mut compressor, black_box(chunks)));
                },
            );
        }

        group.finish();
    }
}

/// Qualities whose match finders consult an attached dictionary.
const PREFIX_QUALITIES: [Quality; 3] = [Quality::Q5, Quality::Q9, Quality::Q11];

/// Registers the attached-dictionary comparison.
///
/// The dictionary is one half of a real text corpus and the payload the other,
/// which is the shape a shared dictionary is deployed in. Preparation happens
/// once, outside the timed region, on both sides: it is a per-connection cost,
/// not a per-request one, and timing it here would measure the wrong thing.
fn bench_dictionary(criterion: &mut Criterion) {
    let Some(corpus) = vendor_corpora()
        .into_iter()
        .find(|corpus| corpus.name.ends_with("alice29.txt"))
    else {
        return;
    };
    let (prefix, payload) = corpus.data.split_at(corpus.data.len() / 2);

    for quality in PREFIX_QUALITIES {
        let numeric = usize::from(quality.get());
        let mut group = criterion.benchmark_group(format!("dictionary/q{numeric}"));
        configure(&mut group, quality);
        group.throughput(Throughput::Bytes(payload.len() as u64));

        let prepared = c_brotli::Prepared::new(prefix, numeric)
            .expect("C Brotli failed to prepare the dictionary");
        let dictionary = DictionaryBuilder::new()
            .add_prefix(prefix)
            .build()
            .expect("mbrotli failed to prepare the dictionary");
        let mut compressor = encoder(quality);

        // Both sides declare a size hint of zero, which is what the reference's
        // streaming entry point leaves it at; the Rust one-shot path declares
        // the true length, so the comparison runs through a session.
        let stream = StreamConfig::default();
        let ours = {
            let mut sink = compressor
                .writer_with_dictionary(&dictionary, Vec::new(), stream)
                .expect("a legal stream");
            sink.write_all(payload).expect("write failed");
            sink.finish()
                .map_err(FinishError::into_error)
                .expect("finish failed")
        };
        let mut theirs = Vec::new();
        c_brotli::compress_with_prefix(
            numeric,
            usize::from(LGWIN.bits()),
            &prepared,
            payload,
            &mut theirs,
        )
        .expect("C Brotli failed to compress");
        assert_eq!(
            ours, theirs,
            "q{numeric}: an attached dictionary left the reference",
        );
        let without = compressor.compress(payload).expect("mbrotli failed");
        println!(
            "q{numeric} dictionary {:>8} bytes  with {:>8} bytes  without {:>8} bytes",
            payload.len(),
            ours.len(),
            without.len(),
        );

        group.bench_function(BenchmarkId::new("c-brotli", "alice29-half"), |bencher| {
            bencher.iter(|| {
                let mut output = Vec::new();
                c_brotli::compress_with_prefix(
                    numeric,
                    usize::from(LGWIN.bits()),
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
                let mut sink = compressor
                    .writer_with_dictionary(&dictionary, Vec::new(), stream)
                    .expect("a legal stream");
                sink.write_all(black_box(payload)).expect("write failed");
                sink.finish()
                    .map_err(FinishError::into_error)
                    .expect("finish failed")
            });
        });

        // The same payload with nothing attached, so the cost of consulting a
        // dictionary is separable from the cost of compressing at all.
        group.bench_function(
            BenchmarkId::new("mbrotli-no-dictionary", "alice29-half"),
            |bencher| {
                bencher.iter(|| {
                    compressor
                        .compress(black_box(payload))
                        .expect("mbrotli failed to compress")
                });
            },
        );

        group.finish();
    }
}

/// Measures the canonical empty and incompressible cases that used to be rewritten.
fn bench_universal(criterion: &mut Criterion) {
    let noise = incompressible(16 << 10);
    for quality in [Quality::Q0, Quality::Q1, Quality::Q5, Quality::Q11] {
        let config = config(quality).with_window(Window::standard(10).expect("window"));
        let mut group = criterion.benchmark_group(format!("universal/q{}", quality.get()));
        for (name, data) in [("empty", &[][..]), ("binary-16KiB", noise.as_slice())] {
            group.throughput(if data.is_empty() {
                Throughput::Elements(1)
            } else {
                Throughput::Bytes(data.len() as u64)
            });
            let expected = Compressor::new(config)
                .expect("config")
                .compress(data)
                .expect("compress");
            let mut reference = Vec::new();
            c_brotli::compress_into(quality.get().into(), 10, data, &mut reference)
                .expect("C stream");
            assert_eq!(expected, reference, "canonical q{} {name}", quality.get());
            assert_eq!(
                c_brotli::decompress(&expected, data.len()).as_deref(),
                Some(data)
            );
            println!(
                "universal q{} {name}: {} bytes on both implementations",
                quality.get(),
                expected.len()
            );
            group.bench_function(BenchmarkId::new("c-brotli", name), |bencher| {
                bencher.iter(|| {
                    let mut output = Vec::new();
                    c_brotli::compress_into(quality.get().into(), 10, black_box(data), &mut output)
                        .expect("C stream");
                    output
                });
            });
            group.bench_function(BenchmarkId::new("mbrotli", name), |bencher| {
                bencher.iter(|| {
                    Compressor::new(config)
                        .expect("config")
                        .compress(black_box(data))
                        .expect("compress")
                });
            });
        }
        group.finish();
    }
}

criterion_group!(
    benches,
    bench_cold,
    bench_reused,
    bench_presized,
    bench_tiny,
    bench_streaming,
    bench_flush,
    bench_dictionary,
    bench_universal
);
criterion_main!(benches);
