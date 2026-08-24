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
use mbrotli::compressor::{
    BlockBits, CompressMode, CompressParams, Compressor, DistanceCodes, QualityLevel, WindowBits,
};
use std::ffi::c_int;
use std::mem::discriminant;

pub mod targets;

/// Every quality this crate implements.
pub const IMPLEMENTED_QUALITIES: [QualityLevel; 5] = [
    QualityLevel::Q0,
    QualityLevel::Q1,
    QualityLevel::Q3,
    QualityLevel::Q4,
    QualityLevel::Q5,
];

/// The three modes the encoder accepts.
const MODES: [CompressMode; 3] = [
    CompressMode::Generic,
    CompressMode::Text,
    CompressMode::Font,
];

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

/// Bytes of a fuzz input that configure the encoder rather than feed it.
pub const HEADER_LEN: usize = 6;

/// Splits a fuzz input into parameters and a payload.
///
/// The first [`HEADER_LEN`] bytes choose, in order: the quality, the window
/// size, the streaming chunk size, the mode and whether literal context
/// modelling is on, the block size, and the distance code layout. A shorter
/// input falls back to the defaults. The payload is truncated to
/// [`MAX_PAYLOAD`] rather than rejected, so an oversized input still exercises
/// whatever structure its prefix carries.
///
/// The size hint is always pinned to the payload length, which is what the
/// one-shot entry points would substitute anyway, so the streaming and
/// one-shot targets can be compared against each other and against the C
/// reference without the hint drifting between them.
pub fn decode_case(input: &[u8]) -> Case<'_> {
    let (header, data) = input.split_at(input.len().min(HEADER_LEN));
    let byte =
        |index: usize, fallback: u8| usize::from(header.get(index).copied().unwrap_or(fallback));

    let quality = IMPLEMENTED_QUALITIES[byte(0, 0) % IMPLEMENTED_QUALITIES.len()];
    let lgwin = WindowBits::try_from(10 + byte(1, 12) % 15).unwrap_or(WindowBits::DEFAULT);
    let chunk = 1usize << (byte(2, 12) % 18);

    let flags = byte(3, 0);
    let mode = MODES[flags % MODES.len()];
    let literal_context_modeling = (flags >> 2) & 1 == 0;

    // Byte 4 either leaves the block size to the encoder or pins it.
    let lgblock = match byte(4, 0) {
        0 => None,
        value => BlockBits::try_from(16 + value % 9).ok(),
    };

    // Byte 5 picks a layout the format can express; anything else keeps the
    // default, because the public type refuses to build an invalid one.
    let layout = byte(5, 0);
    let postfix = (layout % 4) as u32;
    let groups = ((layout / 4) % 16) as u32;
    let direct = groups << postfix;
    let distance_codes =
        DistanceCodes::try_from((postfix, direct)).unwrap_or(DistanceCodes::DEFAULT);

    let data = cap(data);
    Case {
        params: CompressParams::new(quality, lgwin)
            .with_mode(mode)
            .with_size_hint(Some(data.len()))
            .with_block_bits(lgblock)
            .with_distance_codes(distance_codes)
            .with_literal_context_modeling(literal_context_modeling),
        chunk,
        data,
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

/// Compresses `input` with the pinned C encoder, configured like `params`.
///
/// The streaming entry point is the only one that accepts a block size, a size
/// hint or a distance layout, so the differential target goes through it.
///
/// # Panics
///
/// Panics when the C encoder reports failure; that would mean the harness, not
/// the encoder under test, is broken.
pub fn c_compress_with(params: &CompressParams, input: &[u8]) -> Vec<u8> {
    let capacity = unsafe { ffi::BrotliEncoderMaxCompressedSize(input.len()) }.max(64) + 4096;
    let mut output = vec![0u8; capacity];
    unsafe {
        let state = ffi::BrotliEncoderCreateInstance(None, None, std::ptr::null_mut());
        assert!(!state.is_null(), "the C encoder could not be created");
        let set = |parameter, value| {
            assert_eq!(
                ffi::BrotliEncoderSetParameter(state, parameter, value),
                ffi::BROTLI_TRUE,
                "the C encoder rejected a parameter"
            );
        };
        set(
            ffi::BROTLI_PARAM_QUALITY,
            usize::from(params.quality()) as u32,
        );
        set(ffi::BROTLI_PARAM_LGWIN, usize::from(params.lgwin()) as u32);
        set(
            ffi::BROTLI_PARAM_MODE,
            match params.mode() {
                CompressMode::Generic => ffi::BROTLI_MODE_GENERIC,
                CompressMode::Text => ffi::BROTLI_MODE_TEXT,
                CompressMode::Font => ffi::BROTLI_MODE_FONT,
            } as u32,
        );
        set(
            ffi::BROTLI_PARAM_NPOSTFIX,
            params.distance_codes().postfix_bits(),
        );
        set(
            ffi::BROTLI_PARAM_NDIRECT,
            params.distance_codes().direct_codes(),
        );
        set(
            ffi::BROTLI_PARAM_DISABLE_LITERAL_CONTEXT_MODELING,
            u32::from(!params.literal_context_modeling()),
        );
        set(
            ffi::BROTLI_PARAM_SIZE_HINT,
            params.size_hint().unwrap_or(input.len()) as u32,
        );
        if let Some(lgblock) = params.lgblock() {
            set(ffi::BROTLI_PARAM_LGBLOCK, usize::from(lgblock) as u32);
        }

        let mut available_in = input.len();
        let mut next_in = input.as_ptr();
        let mut available_out = output.len();
        let mut next_out = output.as_mut_ptr();
        let mut total_out = 0usize;
        let ok = ffi::BrotliEncoderCompressStream(
            state,
            ffi::BROTLI_OPERATION_FINISH,
            &raw mut available_in,
            &raw mut next_in,
            &raw mut available_out,
            &raw mut next_out,
            &raw mut total_out,
        );
        assert_eq!(ok, ffi::BROTLI_TRUE, "the C encoder failed");
        assert_eq!(
            ffi::BrotliEncoderIsFinished(state),
            ffi::BROTLI_TRUE,
            "the C encoder did not finish"
        );
        ffi::BrotliEncoderDestroyInstance(state);
        output.truncate(total_out);
    }
    output
}

/// Compresses `input` with the pinned C encoder's one-shot entry point.
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
