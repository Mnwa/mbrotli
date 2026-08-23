//! Criterion benchmarks for Brotli quality 0 and 1.
//!
//! Every case feeds identical bytes, quality, window size, and encoder mode to
//! this crate and to Google's C Brotli exposed by the `google-brotli-ffi`
//! workspace crate, so the two measurements are directly comparable. Both
//! sides measure the same end-to-end shape: allocate an output buffer sized by
//! the compressed-size bound, then compress the whole input in one shot.
//!
//! Corpora are generated deterministically at startup and validated before any
//! timing: every compressed stream is decoded with the C decoder and compared
//! with the original input, and the compressed sizes are printed so a speedup
//! can be checked against the compression ratio it produced.
//!
//! This crate's compressor is still unimplemented. Until it is, the benchmark
//! probe below detects the panic, prints a notice, and registers only the C
//! benchmarks; the comparison turns itself on as soon as the core lands.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use mbrotli::Brotli;
use mbrotli::compressor::{BrotliCompressParams, BrotliQualityLevel, BrotliWindowBits};
use std::hint::black_box;
use std::panic::{self, AssertUnwindSafe};

/// Sliding window size used by every benchmark.
const LGWIN: BrotliWindowBits = BrotliWindowBits::DEFAULT;

/// Quality levels under measurement, paired with their numeric value.
const QUALITIES: [BrotliQualityLevel; 2] = [BrotliQualityLevel::Q0, BrotliQualityLevel::Q1];

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
    name: &'static str,
    data: Vec<u8>,
}

/// Builds the deterministic corpora: text, binary, compressible,
/// incompressible, small, and large inputs.
fn corpora() -> Vec<Corpus> {
    vec![
        Corpus {
            name: "text-1KiB",
            data: text(1 << 10),
        },
        Corpus {
            name: "text-1MiB",
            data: text(1 << 20),
        },
        Corpus {
            name: "binary-256KiB",
            data: binary(1 << 18),
        },
        Corpus {
            name: "compressible-256KiB",
            data: compressible(1 << 18),
        },
        Corpus {
            name: "incompressible-256KiB",
            data: incompressible(1 << 18),
        },
    ]
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

/// Compresses `data` with this crate, returning `None` when the compressor is
/// not implemented yet.
fn mbrotli_compress(params: BrotliCompressParams, data: &[u8]) -> Option<Vec<u8>> {
    let compressor = Brotli::default().compressor();

    let previous_hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| compressor.compress(params, data)));
    panic::set_hook(previous_hook);

    match outcome {
        Ok(Ok(compressed)) => Some(compressed),
        Ok(Err(error)) => panic!("mbrotli failed to compress {} bytes: {error}", data.len()),
        Err(_) => None,
    }
}

/// Verifies both implementations against the C decoder and reports the
/// compressed sizes they produced.
///
/// Returns whether this crate's compressor took part.
fn validate(params: BrotliCompressParams, corpus: &Corpus) -> bool {
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

    let Some(rust_output) = mbrotli_compress(params, input) else {
        println!(
            "q{quality} {name:<22} c-brotli {c_size:>9} bytes  mbrotli unimplemented",
            name = corpus.name,
        );
        return false;
    };

    assert_eq!(
        c_brotli::decompress(&rust_output, input.len()).as_deref(),
        Some(input),
        "mbrotli output does not round-trip for {} at q{quality}",
        corpus.name,
    );
    println!(
        "q{quality} {name:<22} c-brotli {c_size:>9} bytes  mbrotli {rust_size:>9} bytes",
        name = corpus.name,
        rust_size = rust_output.len(),
    );
    true
}

/// Registers the quality 0 and quality 1 comparison benchmarks.
fn bench_compress(criterion: &mut Criterion) {
    let corpora = corpora();

    println!(
        "corpus validation (lgwin {}, {:?})",
        usize::from(LGWIN),
        Brotli::default()
    );

    for quality in QUALITIES {
        let params = BrotliCompressParams::new(quality, LGWIN);
        let numeric_quality = usize::from(quality);
        let mut group = criterion.benchmark_group(format!("compress/q{numeric_quality}"));

        for corpus in &corpora {
            let bench_mbrotli = validate(params, corpus);
            let data = corpus.data.as_slice();
            group.throughput(Throughput::Bytes(data.len() as u64));

            group.bench_with_input(
                BenchmarkId::new("c-brotli", corpus.name),
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

            if !bench_mbrotli {
                continue;
            }

            let compressor = Brotli::default().compressor();
            group.bench_with_input(
                BenchmarkId::new("mbrotli", corpus.name),
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

criterion_group!(benches, bench_compress);
criterion_main!(benches);
