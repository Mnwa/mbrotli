//! Shared helpers and engine-neutral bodies for the AFL fuzz targets.
//!
//! Every target consumes a raw byte string. The helpers here turn that byte
//! string into encoder parameters and provide the oracles the targets assert
//! against; [`targets`] holds the target bodies themselves.
//!
//! Nothing in this crate depends on AFL. Each binary under `src/bin/` is a
//! three line adapter around one [`targets`] function, so a minimised crash can
//! be replayed from an ordinary `cargo test` — see `tests/regressions.rs` — and
//! not only by piping bytes into an instrumented binary.

use fearless_simd::Level;
use google_brotli_ffi as ffi;
use mbrotli::Brotli;
use mbrotli::compressor::{CompressParams, Compressor, QualityLevel, WindowBits};
use std::ffi::c_int;
use std::mem::discriminant;

pub mod targets;

/// The two qualities the fast encoder implements.
pub const FAST_QUALITIES: [QualityLevel; 2] = [QualityLevel::Q0, QualityLevel::Q1];

/// Largest payload any target will compress.
///
/// AFL's own default input cap is a mebibyte, which lets a single iteration
/// spend milliseconds compressing one seed — and the multi-backend targets pay
/// that cost several times over. Capping the payload here trades input length,
/// which stops adding coverage quickly, for iteration count, which does not.
///
/// The value is chosen so that the vendored `backward65536` fixture survives
/// intact and so that every window below 2^17 still spans several encoder
/// blocks, the block size being `1 << lgwin`. Very large multi-fragment inputs
/// are covered by `tests/vendor_corpus.rs` instead, which is not throughput
/// bound.
pub const MAX_PAYLOAD: usize = 128 * 1024;

/// Prepared state shared by every iteration of a target.
///
/// SIMD detection and level enumeration happen once, when the context is
/// built, so no iteration repeats them. The encoder itself is stateless — a
/// [`Compressor`] is `Copy` and carries only a resolved [`Level`] — which is
/// why AFL's persistent mode needs no reset hook for these targets.
pub struct Context {
    /// Compressor pinned to the level this host detected.
    pub compressor: Compressor,
    /// Every distinct backend this host can run.
    pub levels: Vec<Level>,
}

impl Default for Context {
    /// Detects the host's instruction set and enumerates its backends.
    fn default() -> Self {
        Self {
            compressor: Brotli::default().compressor(),
            levels: host_levels(),
        }
    }
}

/// Parameters and payload decoded from one fuzz input.
pub struct Case<'a> {
    /// Encoder parameters.
    pub params: CompressParams,
    /// Chunk size for the streaming targets, always at least one.
    pub chunk: usize,
    /// The bytes to compress, truncated to [`MAX_PAYLOAD`].
    pub data: &'a [u8],
}

/// Splits a fuzz input into parameters and a payload.
///
/// The first three bytes choose the quality, the window size and the streaming
/// chunk size; anything shorter falls back to the defaults. The payload is
/// truncated to [`MAX_PAYLOAD`] rather than rejected, so an oversized input
/// still exercises whatever structure its prefix carries.
pub fn decode_case(input: &[u8]) -> Case<'_> {
    let (header, data) = input.split_at(input.len().min(3));
    let quality = FAST_QUALITIES[usize::from(header.first().copied().unwrap_or(0)) % 2];
    let lgwin_index = usize::from(header.get(1).copied().unwrap_or(12)) % 15;
    let lgwin = WindowBits::try_from(10 + lgwin_index).unwrap_or(WindowBits::DEFAULT);
    let chunk = 1usize << (usize::from(header.get(2).copied().unwrap_or(12)) % 18);
    Case {
        params: CompressParams::new(quality, lgwin),
        chunk,
        data: cap(data),
    }
}

/// Truncates a payload to [`MAX_PAYLOAD`].
pub fn cap(data: &[u8]) -> &[u8] {
    &data[..data.len().min(MAX_PAYLOAD)]
}

/// Returns every *distinct* SIMD backend the host can run, scalar fallback
/// included.
///
/// `Level::new()`, `Level::baseline()` and the architecture-specific token
/// accessors routinely resolve to the same backend — on aarch64 all three of
/// the first are Neon — so the list is deduplicated by variant. Without that,
/// the equivalence target would compress the same input through the same code
/// path several times per iteration and pay for it in throughput.
pub fn host_levels() -> Vec<Level> {
    let detected = Level::new();
    let mut candidates = vec![detected, Level::baseline(), Level::fallback()];

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if let Some(token) = detected.as_sse2() {
            candidates.push(Level::Sse2(token));
        }
        if let Some(token) = detected.as_sse4_2() {
            candidates.push(Level::Sse4_2(token));
        }
        if let Some(token) = detected.as_avx2() {
            candidates.push(Level::Avx2(token));
        }
        if let Some(token) = detected.as_avx512() {
            candidates.push(Level::Avx512(token));
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        if let Some(token) = detected.as_neon() {
            candidates.push(Level::Neon(token));
        }
    }

    let mut levels: Vec<Level> = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if !levels
            .iter()
            .any(|kept| discriminant(kept) == discriminant(&candidate))
        {
            levels.push(candidate);
        }
    }
    levels
}

/// Compresses `input` with the pinned C encoder.
///
/// # Panics
///
/// Panics when the C encoder reports failure; that would mean the harness, not
/// the encoder under test, is broken.
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

/// Decodes `input` with the pinned C decoder.
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

/// Asserts that `compressed` decodes back to `data`.
///
/// # Panics
///
/// Panics when the stream is rejected or decodes to different bytes.
pub fn assert_round_trip(data: &[u8], compressed: &[u8]) {
    let decoded = c_decompress(compressed, data.len())
        .unwrap_or_else(|| panic!("the decoder rejected a {} byte stream", compressed.len()));
    assert_eq!(decoded.len(), data.len(), "decoded length differs");
    assert_eq!(decoded, data, "decoded content differs");
}
