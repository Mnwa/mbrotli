//! Shared helpers and engine-neutral bodies for the AFL fuzz targets.
//!
//! Every target consumes a raw byte string. The helpers here turn that byte
//! string into encoder parameters and provide the oracles the targets assert
//! against; [`targets`] holds the target bodies themselves.
//!
//! Nothing in this crate depends on AFL. Each binary under `src/bin/` is a
//! three line adapter around one [`targets`] function, so a minimised crash can
//! be replayed through `cargo afl test` — see `tests/regressions.rs` — and
//! not only by piping bytes into an instrumented binary.

use google_brotli_ffi as ffi;
use mbrotli::Backend;
use mbrotli::{
    BlockBits, BlockSize, CompressionMode, Compressor, DistanceParams, EncoderConfig, InputSize,
    LiteralContextMode, Quality, StreamConfig, Window,
};
use std::ffi::c_int;

pub mod targets;

/// Every quality this crate implements.
pub const IMPLEMENTED_QUALITIES: [Quality; 12] = [
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

/// The three modes the encoder accepts.
const MODES: [CompressionMode; 3] = [
    CompressionMode::Generic,
    CompressionMode::Text,
    CompressionMode::Font,
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
/// Backend enumeration happens once, when the context is built, so no iteration
/// repeats it. A [`Compressor`] is stateful now, so each target builds the one
/// it needs from the case's configuration; that construction is itself part of
/// what the targets exercise, and it allocates nothing large.
pub struct Context {
    /// The level this host detected, which every ordinary target encodes on.
    pub level: Backend,
    /// Every distinct backend this host can run.
    pub levels: Vec<Backend>,
}

impl Default for Context {
    /// Detects the host's instruction set and enumerates its backends.
    fn default() -> Self {
        Self {
            level: Backend::default(),
            levels: host_levels(),
        }
    }
}

impl Context {
    /// Builds a compressor for `config` on this host's detected backend.
    ///
    /// # Panics
    ///
    /// Panics when the configuration is one no compressor can be built for,
    /// which [`decode_case`] never produces.
    pub fn encoder(&self, config: EncoderConfig) -> Compressor {
        Compressor::builder(config)
            .with_backend(self.level)
            .build()
            .expect("decode_case only builds legal configurations")
    }

    /// Builds a compressor for `config` on a chosen backend.
    ///
    /// # Panics
    ///
    /// Panics when the configuration is one no compressor can be built for.
    pub fn encoder_on(&self, level: Backend, config: EncoderConfig) -> Compressor {
        Compressor::builder(config)
            .with_backend(level)
            .build()
            .expect("decode_case only builds legal configurations")
    }
}

/// Configuration and payload decoded from one fuzz input.
pub struct Case<'a> {
    /// The encoder configuration.
    pub config: EncoderConfig,
    /// The stream configuration, which declares the payload's length.
    pub stream: StreamConfig,
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
/// The declared stream size is always the payload's true length, which is what
/// the one-shot entry points declare for themselves, so the streaming and
/// one-shot targets can be compared against each other and against the C
/// reference without the declaration drifting between them.
pub fn decode_case(input: &[u8]) -> Case<'_> {
    let (header, data) = input.split_at(input.len().min(HEADER_LEN));
    let byte =
        |index: usize, fallback: u8| usize::from(header.get(index).copied().unwrap_or(fallback));

    let quality = IMPLEMENTED_QUALITIES[byte(0, 0) % IMPLEMENTED_QUALITIES.len()];
    let window = Window::standard(10 + (byte(1, 12) % 15) as u8).unwrap_or(Window::DEFAULT);
    let chunk = 1usize << (byte(2, 12) % 18);

    let flags = byte(3, 0);
    let mode = MODES[flags % MODES.len()];
    let literal_context = if (flags >> 2) & 1 == 0 {
        LiteralContextMode::Auto
    } else {
        LiteralContextMode::Disabled
    };

    // Byte 4 either leaves the block size to the encoder or pins it.
    let block_size = match byte(4, 0) {
        0 => BlockSize::Auto,
        value => {
            BlockBits::try_from((16 + value % 9) as u8).map_or(BlockSize::Auto, BlockSize::Bits)
        }
    };

    // Byte 5 picks a layout the format can express; anything else keeps the
    // default, because the public type refuses to build an invalid one.
    let layout = byte(5, 0);
    let postfix = (layout % 4) as u8;
    let groups = ((layout / 4) % 16) as u16;
    let direct = groups << postfix;
    let distance = DistanceParams::explicit(postfix, direct).unwrap_or(DistanceParams::Auto);

    let data = cap(data);
    Case {
        config: EncoderConfig::default()
            .with_quality(quality)
            .with_window(window)
            .with_mode(mode)
            .with_block_size(block_size)
            .with_distance(distance)
            .with_literal_context(literal_context),
        stream: StreamConfig::from(InputSize::Exact(data.len() as u64)),
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
/// The public enumeration already validates host support and returns each
/// backend exactly once. No implementation-specific SIMD type crosses the API.
pub fn host_levels() -> Vec<Backend> {
    Backend::available()
}

/// Compresses `input` with the pinned C encoder, configured like `config`.
///
/// The streaming entry point is the only one that accepts a block size, a size
/// hint or a distance layout, so the differential target goes through it. The
/// size hint is the input's true length, which is what both the Rust one-shot
/// path and [`decode_case`]'s stream configuration declare.
///
/// # Panics
///
/// Panics when the C encoder reports failure; that would mean the harness, not
/// the encoder under test, is broken.
pub fn c_compress_with(config: &EncoderConfig, input: &[u8]) -> Vec<u8> {
    // The native one-shot bound assumes a rewrite that streaming does not use.
    let capacity = input
        .len()
        .checked_mul(2)
        .and_then(|size| size.checked_add(4096))
        .expect("bounded fuzz payload");
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
        set(ffi::BROTLI_PARAM_QUALITY, u32::from(config.quality().get()));
        set(ffi::BROTLI_PARAM_LGWIN, u32::from(config.window().bits()));
        set(
            ffi::BROTLI_PARAM_MODE,
            match config.mode() {
                CompressionMode::Generic => ffi::BROTLI_MODE_GENERIC,
                CompressionMode::Text => ffi::BROTLI_MODE_TEXT,
                CompressionMode::Font => ffi::BROTLI_MODE_FONT,
            } as u32,
        );
        let (postfix, direct) = match config.distance() {
            DistanceParams::Auto => (0, 0),
            DistanceParams::Explicit {
                postfix_bits,
                direct_codes,
            } => (u32::from(postfix_bits), u32::from(direct_codes)),
        };
        set(ffi::BROTLI_PARAM_NPOSTFIX, postfix);
        set(ffi::BROTLI_PARAM_NDIRECT, direct);
        set(
            ffi::BROTLI_PARAM_DISABLE_LITERAL_CONTEXT_MODELING,
            u32::from(config.literal_context() == LiteralContextMode::Disabled),
        );
        set(ffi::BROTLI_PARAM_SIZE_HINT, input.len() as u32);
        if let BlockSize::Bits(lgblock) = config.block_size() {
            set(ffi::BROTLI_PARAM_LGBLOCK, u32::from(lgblock.get()));
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

/// Decodes `input` with the pinned C decoder in large-window mode.
///
/// The RFC 9841 fourteen-bit window header is only recognised when the decoder
/// has been told to expect it, which the one-shot entry point cannot do.
///
/// # Panics
///
/// Panics when the decoder cannot be created or refuses the parameter, which
/// means the harness is broken rather than the encoder.
pub fn c_decompress_large_window(input: &[u8], expected_size: usize) -> Option<Vec<u8>> {
    let mut output = vec![0u8; expected_size.max(1)];
    unsafe {
        let state = ffi::BrotliDecoderCreateInstance(None, None, std::ptr::null_mut());
        assert!(!state.is_null(), "the C decoder could not be created");
        assert_eq!(
            ffi::BrotliDecoderSetParameter(state, ffi::BROTLI_DECODER_PARAM_LARGE_WINDOW, 1),
            ffi::BROTLI_TRUE,
            "the C decoder rejected the large window parameter"
        );

        let mut available_in = input.len();
        let mut next_in = input.as_ptr();
        let mut available_out = output.len();
        let mut next_out = output.as_mut_ptr();
        let mut total_out = 0usize;
        let result = ffi::BrotliDecoderDecompressStream(
            state,
            &raw mut available_in,
            &raw mut next_in,
            &raw mut available_out,
            &raw mut next_out,
            &raw mut total_out,
        );
        let finished = ffi::BrotliDecoderIsFinished(state);
        ffi::BrotliDecoderDestroyInstance(state);

        if result != ffi::BROTLI_DECODER_RESULT_SUCCESS
            || finished != ffi::BROTLI_TRUE
            || available_in != 0
        {
            return None;
        }
        output.truncate(total_out);
    }
    Some(output)
}

/// Decodes a bounded custom-dictionary stream using the independent C decoder.
pub fn c_decompress_serialized(
    dictionary: &[u8],
    input: &[u8],
    expected_size: usize,
) -> Option<Vec<u8>> {
    let mut output = vec![0; expected_size.max(1)];
    // SAFETY: buffers remain alive for the decoder's lifetime; the output
    // capacity matches its writable length. State is freed on both paths.
    unsafe {
        let state = ffi::BrotliDecoderCreateInstance(None, None, std::ptr::null_mut());
        assert!(!state.is_null());
        if ffi::BrotliDecoderAttachDictionary(
            state,
            ffi::BROTLI_SHARED_DICTIONARY_SERIALIZED,
            dictionary.len(),
            dictionary.as_ptr(),
        ) != ffi::BROTLI_TRUE
        {
            ffi::BrotliDecoderDestroyInstance(state);
            return None;
        }
        let mut available_in = input.len();
        let mut next_in = input.as_ptr();
        let mut available_out = output.len();
        let mut next_out = output.as_mut_ptr();
        let mut total = 0;
        let result = ffi::BrotliDecoderDecompressStream(
            state,
            &raw mut available_in,
            &raw mut next_in,
            &raw mut available_out,
            &raw mut next_out,
            &raw mut total,
        );
        ffi::BrotliDecoderDestroyInstance(state);
        if result != ffi::BROTLI_DECODER_RESULT_SUCCESS || available_in != 0 {
            return None;
        }
        output.truncate(total);
    }
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

/// Returns whether the pinned C parser accepts `bytes` as a shared dictionary.
///
/// Wraps `BrotliSharedDictionaryAttach` with the serialized type through this
/// repository's shim, which is compiled only when the vendored library is built
/// with `BROTLI_EXPERIMENTAL`.
pub fn c_parse_shared_dictionary(bytes: &[u8]) -> bool {
    let mut info = google_brotli_ffi::MbrotliSharedDictInfo::default();
    // SAFETY: `bytes` is a live slice readable for its own length, and `info`
    // is a live, correctly typed local the shim fully writes. The shim owns and
    // frees the dictionary it builds internally.
    unsafe {
        google_brotli_ffi::mbrotli_shim_parse_shared_dictionary(
            bytes.as_ptr(),
            bytes.len(),
            &raw mut info,
        );
    }
    info.ok == 1
}
