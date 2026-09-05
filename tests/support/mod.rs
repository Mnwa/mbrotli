//! Shared helpers for the integration tests.
//!
//! Wraps the pinned Google Brotli C library (v1.2.0, commit `028fb5a`) exposed
//! by the `google-brotli-ffi` workspace crate, and builds the corpora both the
//! differential and the round-trip tests run over.

#![allow(dead_code, reason = "each integration test uses a different subset")]

use google_brotli_ffi as ffi;
use mbrotli::{Compressor, EncoderConfig, Quality, Window};
use std::ffi::c_int;

/// Compresses `input` with the pinned C encoder.
///
/// # Panics
///
/// Panics when the C encoder reports failure, which would mean the harness is
/// misconfigured rather than the encoder under test being wrong.
pub fn c_compress(quality: c_int, lgwin: c_int, input: &[u8]) -> Vec<u8> {
    let mut params = CParams::new(quality, lgwin);
    params.size_hint = Some(input.len().min(u32::MAX as usize) as u32);
    c_compress_with(params, input)
}

/// Native C one-shot behavior, including its API-specific bitstream rewrites.
pub fn c_compress_native_one_shot(quality: c_int, lgwin: c_int, input: &[u8]) -> Vec<u8> {
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

/// Every encoder parameter the C harness can set.
#[derive(Copy, Clone, Debug)]
pub struct CParams {
    pub quality: c_int,
    pub lgwin: c_int,
    pub mode: ffi::BrotliEncoderMode,
    pub size_hint: Option<u32>,
    pub lgblock: Option<u32>,
    pub npostfix: u32,
    pub ndirect: u32,
    pub disable_literal_context_modeling: bool,
}

impl CParams {
    /// Returns the defaults, for `quality` and `lgwin`.
    pub fn new(quality: c_int, lgwin: c_int) -> Self {
        Self {
            quality,
            lgwin,
            mode: ffi::BROTLI_DEFAULT_MODE,
            size_hint: None,
            lgblock: None,
            npostfix: 0,
            ndirect: 0,
            disable_literal_context_modeling: false,
        }
    }
}

/// Compresses `input` with the pinned C encoder through its streaming API.
///
/// Sets every parameter explicitly and omits C's one-shot bitstream rewrites.
///
/// # Panics
///
/// Panics when the C encoder reports failure, which would mean the harness is
/// misconfigured rather than the encoder under test being wrong.
pub fn c_compress_with(params: CParams, input: &[u8]) -> Vec<u8> {
    // Native C's one-shot bound relies on a rewrite that streaming cannot use.
    let capacity = input
        .len()
        .checked_mul(2)
        .and_then(|size| size.checked_add(4096))
        .expect("bounded corpus");
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
        set(ffi::BROTLI_PARAM_QUALITY, params.quality as u32);
        set(ffi::BROTLI_PARAM_LGWIN, params.lgwin as u32);
        set(ffi::BROTLI_PARAM_MODE, params.mode as u32);
        set(ffi::BROTLI_PARAM_NPOSTFIX, params.npostfix);
        set(ffi::BROTLI_PARAM_NDIRECT, params.ndirect);
        set(
            ffi::BROTLI_PARAM_DISABLE_LITERAL_CONTEXT_MODELING,
            u32::from(params.disable_literal_context_modeling),
        );
        if let Some(size_hint) = params.size_hint {
            set(ffi::BROTLI_PARAM_SIZE_HINT, size_hint);
        }
        if let Some(lgblock) = params.lgblock {
            set(ffi::BROTLI_PARAM_LGBLOCK, lgblock);
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

/// Compresses `input` with the pinned C encoder, flushing at `flush_after`.
///
/// The chunks are fed one at a time; every chunk but the last is followed by
/// `BROTLI_OPERATION_FLUSH`, and the last by `BROTLI_OPERATION_FINISH`. This
/// is the reference behaviour `CompressorWriter::flush` has to reproduce byte
/// for byte.
///
/// # Panics
///
/// Panics when the C encoder reports failure, which would mean the harness is
/// misconfigured rather than the encoder under test being wrong.
pub fn c_compress_flushing(params: CParams, chunks: &[&[u8]]) -> Vec<u8> {
    let total: usize = chunks.iter().map(|chunk| chunk.len()).sum();
    // A flush costs a padding block per chunk on top of the bound, and an
    // incompressible chunk can round up; 64 bytes a chunk is far past either.
    let capacity =
        unsafe { ffi::BrotliEncoderMaxCompressedSize(total) }.max(64) + 4096 + 64 * chunks.len();
    let mut output = vec![0u8; capacity];
    let mut written = 0usize;
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
        set(ffi::BROTLI_PARAM_QUALITY, params.quality as u32);
        set(ffi::BROTLI_PARAM_LGWIN, params.lgwin as u32);
        set(ffi::BROTLI_PARAM_MODE, params.mode as u32);
        set(ffi::BROTLI_PARAM_NPOSTFIX, params.npostfix);
        set(ffi::BROTLI_PARAM_NDIRECT, params.ndirect);
        set(
            ffi::BROTLI_PARAM_DISABLE_LITERAL_CONTEXT_MODELING,
            u32::from(params.disable_literal_context_modeling),
        );
        if let Some(size_hint) = params.size_hint {
            set(ffi::BROTLI_PARAM_SIZE_HINT, size_hint);
        }
        if let Some(lgblock) = params.lgblock {
            set(ffi::BROTLI_PARAM_LGBLOCK, lgblock);
        }

        for (index, chunk) in chunks.iter().enumerate() {
            let operation = if index + 1 == chunks.len() {
                ffi::BROTLI_OPERATION_FINISH
            } else {
                ffi::BROTLI_OPERATION_FLUSH
            };
            let mut available_in = chunk.len();
            let mut next_in = chunk.as_ptr();
            // The reference only completes an operation once it stops asking
            // to be called again, which it signals through these two.
            loop {
                let mut available_out = output.len() - written;
                let mut next_out = output.as_mut_ptr().add(written);
                let mut total_out = 0usize;
                let ok = ffi::BrotliEncoderCompressStream(
                    state,
                    operation,
                    &raw mut available_in,
                    &raw mut next_in,
                    &raw mut available_out,
                    &raw mut next_out,
                    &raw mut total_out,
                );
                assert_eq!(ok, ffi::BROTLI_TRUE, "the C encoder failed");
                written = output.len() - available_out;
                if available_in == 0 && ffi::BrotliEncoderHasMoreOutput(state) != ffi::BROTLI_TRUE {
                    break;
                }
            }
        }
        assert_eq!(
            ffi::BrotliEncoderIsFinished(state),
            ffi::BROTLI_TRUE,
            "the C encoder did not finish"
        );
        ffi::BrotliEncoderDestroyInstance(state);
    }
    output.truncate(written);
    output
}

/// Compresses `input` with the C encoder, with `prefixes` attached.
///
/// Each prefix is prepared with `BrotliEncoderPrepareDictionary` and attached
/// in order, which is the reference's compound dictionary — the thing RFC 9841
/// calls an LZ77 prefix.
///
/// # Panics
///
/// Panics when the C encoder rejects a dictionary or reports failure, which
/// would mean the harness is misconfigured rather than the encoder under test
/// being wrong.
pub fn c_compress_with_prefixes(params: CParams, prefixes: &[&[u8]], input: &[u8]) -> Vec<u8> {
    let capacity = unsafe { ffi::BrotliEncoderMaxCompressedSize(input.len()) }.max(64) + 4096;
    let mut output = vec![0u8; capacity];
    let mut prepared = Vec::with_capacity(prefixes.len());
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
        set(ffi::BROTLI_PARAM_QUALITY, params.quality as u32);
        set(ffi::BROTLI_PARAM_LGWIN, params.lgwin as u32);
        set(ffi::BROTLI_PARAM_MODE, params.mode as u32);
        set(ffi::BROTLI_PARAM_NPOSTFIX, params.npostfix);
        set(ffi::BROTLI_PARAM_NDIRECT, params.ndirect);
        set(
            ffi::BROTLI_PARAM_DISABLE_LITERAL_CONTEXT_MODELING,
            u32::from(params.disable_literal_context_modeling),
        );
        if let Some(size_hint) = params.size_hint {
            set(ffi::BROTLI_PARAM_SIZE_HINT, size_hint);
        }
        if let Some(lgblock) = params.lgblock {
            set(ffi::BROTLI_PARAM_LGBLOCK, lgblock);
        }

        for prefix in prefixes {
            let dictionary = ffi::BrotliEncoderPrepareDictionary(
                ffi::BROTLI_SHARED_DICTIONARY_RAW,
                prefix.len(),
                prefix.as_ptr(),
                params.quality,
                None,
                None,
                std::ptr::null_mut(),
            );
            assert!(
                !dictionary.is_null(),
                "the C encoder could not prepare a dictionary"
            );
            assert_eq!(
                ffi::BrotliEncoderAttachPreparedDictionary(state, dictionary),
                ffi::BROTLI_TRUE,
                "the C encoder rejected a prepared dictionary"
            );
            prepared.push(dictionary);
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
        for dictionary in prepared {
            ffi::BrotliEncoderDestroyPreparedDictionary(dictionary);
        }
        output.truncate(total_out);
    }
    output
}

/// Decompresses `input` with the C decoder, with `prefixes` attached.
///
/// The decoder needs the same dictionaries the encoder had, or a stream that
/// references them cannot be read back. Returns [`None`] when it rejects the
/// stream.
///
/// # Panics
///
/// Panics when the decoder cannot be created or rejects a dictionary.
pub fn c_decompress_with_prefixes(
    prefixes: &[&[u8]],
    input: &[u8],
    expected_size: usize,
) -> Option<Vec<u8>> {
    let mut output = vec![0u8; expected_size.max(1)];
    unsafe {
        let state = ffi::BrotliDecoderCreateInstance(None, None, std::ptr::null_mut());
        assert!(!state.is_null(), "the C decoder could not be created");
        assert_eq!(
            ffi::BrotliDecoderSetParameter(state, ffi::BROTLI_DECODER_PARAM_LARGE_WINDOW, 1),
            ffi::BROTLI_TRUE,
            "the C decoder rejected the large window parameter"
        );
        for prefix in prefixes {
            assert_eq!(
                ffi::BrotliDecoderAttachDictionary(
                    state,
                    ffi::BROTLI_SHARED_DICTIONARY_RAW,
                    prefix.len(),
                    prefix.as_ptr(),
                ),
                ffi::BROTLI_TRUE,
                "the C decoder rejected a dictionary"
            );
        }

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

/// Decodes as much of an unterminated `input` as the C decoder will produce.
///
/// Returns what the decoder emitted once it asked for more input, which for a
/// stream that has just been flushed is everything the encoder has consumed.
/// Returns [`None`] when the decoder rejects the bytes outright.
///
/// # Panics
///
/// Panics when the decoder cannot be created, which would mean the harness is
/// misconfigured rather than the encoder under test being wrong.
pub fn c_decompress_partial(input: &[u8], capacity: usize) -> Option<Vec<u8>> {
    let mut output = vec![0u8; capacity.max(1)];
    unsafe {
        let state = ffi::BrotliDecoderCreateInstance(None, None, std::ptr::null_mut());
        assert!(!state.is_null(), "the C decoder could not be created");

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
        ffi::BrotliDecoderDestroyInstance(state);

        if result == ffi::BROTLI_DECODER_RESULT_ERROR {
            return None;
        }
        output.truncate(total_out);
    }
    Some(output)
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

/// Decompresses `input` with the pinned C decoder in large-window mode.
///
/// The RFC 9841 fourteen-bit window header is only accepted when the decoder
/// has been told to expect it, which the one-shot `BrotliDecoderDecompress`
/// entry point cannot do. Returns [`None`] when the decoder rejects the
/// stream.
///
/// # Panics
///
/// Panics when the decoder cannot be created or rejects the parameter, which
/// would mean the harness is misconfigured rather than the encoder under test
/// being wrong.
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

/// Returns the numeric quality of a level, for the C side.
pub fn quality_number(quality: Quality) -> c_int {
    c_int::from(quality.get())
}

/// Builds a configuration for `quality` and a window size of `lgwin` bits.
///
/// # Panics
///
/// Panics when `lgwin` is outside the range the Brotli format allows.
pub fn config(quality: Quality, lgwin: u8) -> EncoderConfig {
    EncoderConfig::default()
        .with_quality(quality)
        .with_window(Window::standard(lgwin).expect("window size out of range"))
}

/// Builds a compressor for `quality` and a window size of `lgwin` bits.
///
/// # Panics
///
/// Panics when the configuration is one no compressor can be built for.
pub fn encoder(quality: Quality, lgwin: u8) -> Compressor {
    Compressor::new(config(quality, lgwin)).expect("a legal configuration")
}

/// Builds a compressor pinned to `level`, so a backend can be exercised.
///
/// # Panics
///
/// Panics when the configuration is one no compressor can be built for.
pub fn encoder_on(level: mbrotli::Backend, quality: Quality, lgwin: u8) -> Compressor {
    Compressor::builder(config(quality, lgwin))
        .with_backend(level)
        .build()
        .expect("a legal configuration")
}

/// The two qualities the fast encoder implements.
pub const FAST_QUALITIES: [Quality; 2] = [Quality::Q0, Quality::Q1];

/// The eight qualities the greedy encoder implements.
pub const GREEDY_QUALITIES: [Quality; 8] = [
    Quality::Q2,
    Quality::Q3,
    Quality::Q4,
    Quality::Q5,
    Quality::Q6,
    Quality::Q7,
    Quality::Q8,
    Quality::Q9,
];

/// The two qualities the high-quality encoder implements.
pub const HQ_QUALITIES: [Quality; 2] = [Quality::Q10, Quality::Q11];

/// Largest input the high-quality qualities are exercised over by default.
pub const HQ_INPUT_CAP: usize = 1 << 16;

/// Returns the prefix of `data` that `quality` should be exercised over.
///
/// Qualities ten and eleven solve a dynamic program over every match at every
/// position. In the debug builds these tests run in, quality eleven costs about
/// a second per hundred and fifty kilobytes, against a hundredth of that at
/// quality nine — so a sweep that is seconds for the greedy qualities is tens of
/// minutes for these two.
///
/// Capping them keeps the *shapes* each sweep covers — literal runs,
/// back-references, periodic data, noise — while moving the large-input
/// coverage to the tests built for it: `vendor_corpus.rs`'s multi-fragment
/// case, and `streaming.rs`'s chunk-boundary cases, both of which still run
/// these qualities over inputs spanning several blocks.
pub fn prefix_for(quality: Quality, data: &[u8]) -> &[u8] {
    if quality >= Quality::Q10 {
        &data[..data.len().min(HQ_INPUT_CAP)]
    } else {
        data
    }
}

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
pub fn host_levels() -> Vec<(&'static str, mbrotli::Backend)> {
    mbrotli::Backend::available()
        .into_iter()
        .map(|backend| (backend.name(), backend))
        .collect()
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
