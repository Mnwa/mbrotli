//! Shared helpers for the integration tests.
//!
//! Wraps the pinned Google Brotli C library (v1.2.0, commit `028fb5a`) exposed
//! by the `google-brotli-ffi` workspace crate, and builds the corpora both the
//! differential and the round-trip tests run over.

#![allow(dead_code, reason = "each integration test uses a different subset")]

use google_brotli_ffi as ffi;
use mbrotli::compressor::{BrotliCompressParams, BrotliQualityLevel, BrotliWindowBits};
use std::ffi::c_int;

/// Compresses `input` with the pinned C encoder.
///
/// # Panics
///
/// Panics when the C encoder reports failure, which would mean the harness is
/// misconfigured rather than the encoder under test being wrong.
pub fn c_compress(quality: c_int, lgwin: c_int, input: &[u8]) -> Vec<u8> {
    let capacity = unsafe { ffi::BrotliEncoderMaxCompressedSize(input.len()) }.max(64) + 1024;
    let mut output = vec![0u8; capacity];
    let mut size = output.len();
    let ok = unsafe {
        ffi::BrotliEncoderCompress(
            quality,
            lgwin,
            ffi::BROTLI_DEFAULT_MODE,
            input.len(),
            input.as_ptr(),
            &raw mut size,
            output.as_mut_ptr(),
        )
    };
    assert_eq!(ok, ffi::BROTLI_TRUE, "the C encoder failed");
    output.truncate(size);
    output
}

/// Decompresses `input` with the pinned C decoder.
pub fn c_decompress(input: &[u8], expected_size: usize) -> Option<Vec<u8>> {
    let mut output = vec![0u8; expected_size.max(1)];
    let mut size = output.len();
    let result = unsafe {
        ffi::BrotliDecoderDecompress(
            input.len(),
            input.as_ptr(),
            &raw mut size,
            output.as_mut_ptr(),
        )
    };
    if result != ffi::BROTLI_DECODER_RESULT_SUCCESS {
        return None;
    }
    output.truncate(size);
    Some(output)
}

/// Returns the numeric quality of a level, for the C side.
pub fn quality_number(quality: BrotliQualityLevel) -> c_int {
    usize::from(quality) as c_int
}

/// Builds parameters for `quality` and a window size of `lgwin` bits.
///
/// # Panics
///
/// Panics when `lgwin` is outside the range the Brotli format allows.
pub fn params(quality: BrotliQualityLevel, lgwin: usize) -> BrotliCompressParams {
    let lgwin = BrotliWindowBits::try_from(lgwin).expect("window size out of range");
    BrotliCompressParams::new(quality, lgwin)
}

/// The two qualities the fast encoder implements.
pub const FAST_QUALITIES: [BrotliQualityLevel; 2] =
    [BrotliQualityLevel::Q0, BrotliQualityLevel::Q1];

/// Deterministic xorshift generator, so corpora are reproducible.
pub struct Rng(u64);

impl Rng {
    /// Creates a generator seeded with `seed`.
    pub const fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    /// Returns the next pseudo-random word.
    pub const fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Returns the next pseudo-random byte.
    pub const fn next_u8(&mut self) -> u8 {
        (self.next_u64() >> 24) as u8
    }

    /// Returns a vector of `len` bytes drawn from `0..alphabet`.
    pub fn bytes(&mut self, len: usize, alphabet: u16) -> Vec<u8> {
        (0..len)
            .map(|_| (u16::from(self.next_u8()) % alphabet) as u8)
            .collect()
    }
}

/// A named test input.
pub struct Corpus {
    /// Human readable label used in assertion messages.
    pub name: String,
    /// The bytes to compress.
    pub data: Vec<u8>,
}

impl Corpus {
    fn new(name: impl Into<String>, data: Vec<u8>) -> Self {
        Self {
            name: name.into(),
            data,
        }
    }
}

/// Lengths that sit on every interesting encoder boundary.
pub fn boundary_lengths() -> Vec<usize> {
    let mut lengths: Vec<usize> = (0..=64).collect();
    lengths.extend([
        127, 128, 129, 255, 256, 257, 511, 512, 513, 2047, 2048, 2049, 4095, 4096, 4097, 8191,
        8192, 8193, 32_767, 32_768, 32_769, 65_535, 65_536, 65_537, 98_303, 98_304, 98_305,
        131_071, 131_072, 131_073,
    ]);
    lengths
}

/// Structural corpora that exercise the match finder and the fallbacks.
pub fn structural_corpora() -> Vec<Corpus> {
    let mut corpora = Vec::new();
    let mut rng = Rng::new(0x5EED_1234_ABCD_0001);

    corpora.push(Corpus::new("empty", Vec::new()));
    corpora.push(Corpus::new("single-byte", vec![b'x']));
    corpora.push(Corpus::new("one-symbol-alphabet", vec![b'a'; 200_000]));
    corpora.push(Corpus::new("all-byte-values", (0..=255u8).collect()));
    corpora.push(Corpus::new(
        "all-byte-values-repeated",
        (0..300_000u32).map(|i| (i % 256) as u8).collect(),
    ));
    corpora.push(Corpus::new("long-zero-run", vec![0u8; 250_000]));
    corpora.push(Corpus::new(
        "repeated-word",
        b"the quick brown fox "
            .iter()
            .copied()
            .cycle()
            .take(180_000)
            .collect(),
    ));
    corpora.push(Corpus::new(
        "alternating",
        (0..120_000u32).map(|i| (i % 2) as u8).collect(),
    ));
    for period in [3usize, 4, 5, 6, 7, 8, 16, 32] {
        corpora.push(Corpus::new(
            format!("periodic-{period}"),
            (0..90_000usize).map(|i| (i % period) as u8).collect(),
        ));
    }
    corpora.push(Corpus::new("uniform-random", rng.bytes(200_000, 256)));
    corpora.push(Corpus::new("low-entropy-random", rng.bytes(200_000, 4)));
    corpora.push(Corpus::new("two-symbol-random", rng.bytes(200_000, 2)));

    // Matches that end exactly at the input limit and the window gap.
    let mut window_edge = vec![0u8; 300_000];
    for (index, byte) in window_edge.iter_mut().enumerate() {
        *byte = u8::from(index % 262_128 < 64);
    }
    corpora.push(Corpus::new("max-backward-distance", window_edge));

    // Hash collisions: identical five byte prefixes with different tails.
    let mut collisions = Vec::with_capacity(200_000);
    while collisions.len() < 200_000 {
        collisions.extend_from_slice(b"AAAAA");
        collisions.push(rng.next_u8());
    }
    corpora.push(Corpus::new("hash-collisions", collisions));

    // Text-like data with long literal runs between matches.
    let mut text = Vec::new();
    let words = [
        "brotli",
        "compression",
        "window",
        "literal",
        "distance",
        "meta",
        "block",
        "prefix",
    ];
    let mut index = 0usize;
    while text.len() < 300_000 {
        text.extend_from_slice(words[index % words.len()].as_bytes());
        text.push(b' ');
        if index.is_multiple_of(17) {
            text.extend(rng.bytes(64, 256));
        }
        index += 1;
    }
    corpora.push(Corpus::new("mixed-text", text));

    // Incompressible payload framed by compressible headers.
    let mut framed = b"HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\n\r\n".to_vec();
    framed.extend(rng.bytes(150_000, 256));
    framed.extend_from_slice(b"\r\n--boundary--\r\n");
    corpora.push(Corpus::new("framed-incompressible", framed));

    corpora
}

/// Boundary-length corpora built from a compressible and a random source.
pub fn boundary_corpora() -> Vec<Corpus> {
    let mut rng = Rng::new(0x0BAD_C0DE_1234_5678);
    let random = rng.bytes(200_000, 256);
    let mut corpora = Vec::new();
    for length in boundary_lengths() {
        corpora.push(Corpus::new(
            format!("compressible-{length}"),
            (0..length).map(|i| (i % 7) as u8 + b'a').collect(),
        ));
        corpora.push(Corpus::new(
            format!("random-{length}"),
            random[..length.min(random.len())].to_vec(),
        ));
    }
    corpora
}

/// Every SIMD backend the host can actually run.
///
/// Higher tokens are downgraded through the `as_*` accessors, so an AVX2
/// machine also exercises SSE4.2 and SSE2 without any unsafe code.
pub fn host_levels() -> Vec<(&'static str, fearless_simd::Level)> {
    use fearless_simd::Level;

    let detected = Level::new();
    let mut levels: Vec<(&'static str, Level)> = vec![("detected", detected)];
    levels.push(("baseline", Level::baseline()));
    levels.push(("fallback", Level::fallback()));

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if let Some(token) = detected.as_sse2() {
            levels.push(("sse2", Level::Sse2(token)));
        }
        if let Some(token) = detected.as_sse4_2() {
            levels.push(("sse4.2", Level::Sse4_2(token)));
        }
        if let Some(token) = detected.as_avx2() {
            levels.push(("avx2", Level::Avx2(token)));
        }
        if let Some(token) = detected.as_avx512() {
            levels.push(("avx512", Level::Avx512(token)));
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        if let Some(token) = detected.as_neon() {
            levels.push(("neon", Level::Neon(token)));
        }
    }
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    {
        if let Some(token) = detected.as_wasm_simd128() {
            levels.push(("wasm-simd128", Level::WasmSimd128(token)));
        }
    }
    levels
}

/// Path of Google Brotli's own test data, from the vendored submodule.
pub fn vendor_testdata_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("brotli-ffi/vendor/brotli/tests/testdata")
}

/// Loads Google Brotli's test corpus, truncating anything above `max_bytes`.
///
/// The directory also holds `.compressed` decoder fixtures, which are not
/// encoder inputs and are skipped.
///
/// # Panics
///
/// Panics when the vendored submodule is missing; the C library the tests
/// compare against comes from the same submodule, so it is always required.
pub fn vendor_corpora(max_bytes: usize) -> Vec<Corpus> {
    let directory = vendor_testdata_dir();
    let entries = std::fs::read_dir(&directory).unwrap_or_else(|error| {
        panic!(
            "missing {}: {error}; run `git submodule update --init --recursive`",
            directory.display()
        )
    });

    let mut corpora: Vec<Corpus> = entries
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let name = path.file_name()?.to_str()?.to_owned();
            if name.contains(".compressed") || !path.is_file() {
                return None;
            }
            let mut data = std::fs::read(&path).ok()?;
            data.truncate(max_bytes);
            Some(Corpus::new(name, data))
        })
        .collect();
    corpora.sort_by(|left, right| left.name.cmp(&right.name));
    assert!(!corpora.is_empty(), "the vendored test corpus is empty");
    corpora
}

/// Loads one file from the vendored test corpus in full.
///
/// # Panics
///
/// Panics when the file cannot be read.
pub fn vendor_file(name: &str) -> Vec<u8> {
    let path = vendor_testdata_dir().join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}
