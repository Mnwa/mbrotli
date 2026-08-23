//! Shared helpers for the AFL fuzz targets.
//!
//! Every target consumes a raw byte string from AFL, so the helpers here turn
//! that byte string into encoder parameters and provide the oracles the targets
//! assert against.

use fearless_simd::Level;
use google_brotli_ffi as ffi;
use mbrotli::compressor::{CompressParams, QualityLevel, WindowBits};
use std::ffi::c_int;

/// The two qualities the fast encoder implements.
pub const FAST_QUALITIES: [QualityLevel; 2] = [QualityLevel::Q0, QualityLevel::Q1];

/// Parameters and payload decoded from one fuzz input.
pub struct Case<'a> {
    /// Encoder parameters.
    pub params: CompressParams,
    /// Chunk size for the streaming targets, always at least one.
    pub chunk: usize,
    /// The bytes to compress.
    pub data: &'a [u8],
}

/// Splits a fuzz input into parameters and a payload.
///
/// The first three bytes choose the quality, the window size and the streaming
/// chunk size; anything shorter falls back to the defaults.
pub fn decode_case(input: &[u8]) -> Case<'_> {
    let (header, data) = input.split_at(input.len().min(3));
    let quality = FAST_QUALITIES[usize::from(header.first().copied().unwrap_or(0)) % 2];
    let lgwin_index = usize::from(header.get(1).copied().unwrap_or(12)) % 15;
    let lgwin = WindowBits::try_from(10 + lgwin_index).unwrap_or(WindowBits::DEFAULT);
    let chunk = 1usize << (usize::from(header.get(2).copied().unwrap_or(12)) % 18);
    Case {
        params: CompressParams::new(quality, lgwin),
        chunk,
        data,
    }
}

/// Returns every SIMD backend the host can run, scalar fallback included.
pub fn host_levels() -> Vec<Level> {
    let detected = Level::new();
    let mut levels = vec![detected, Level::baseline(), Level::fallback()];

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if let Some(token) = detected.as_sse2() {
            levels.push(Level::Sse2(token));
        }
        if let Some(token) = detected.as_sse4_2() {
            levels.push(Level::Sse4_2(token));
        }
        if let Some(token) = detected.as_avx2() {
            levels.push(Level::Avx2(token));
        }
        if let Some(token) = detected.as_avx512() {
            levels.push(Level::Avx512(token));
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        if let Some(token) = detected.as_neon() {
            levels.push(Level::Neon(token));
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
