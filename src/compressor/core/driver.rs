//! Quality routing and the one-shot compression entry points.
//!
//! Ports `BrotliEncoderCompress`, `BrotliEncoderMaxCompressedSize` and
//! `MakeUncompressedStream` from `c/enc/encode.c` of the pinned reference
//! (`google/brotli` v1.2.0, commit `028fb5a`).
//!
//! Three encoder families live below this module: the fast one for qualities
//! zero and one, the greedy one for qualities three to nine, and the
//! high-quality one for qualities ten and eleven. Everything they share — the
//! empty-input shortcut, the final fallback to an uncompressed stream —
//! belongs here rather than in any of them.

use fearless_simd::Level;

use super::fast::FastEncoder;
use super::greedy::encoder::GreedyEncoder;
use super::greedy::params::GreedyParams;
use super::hq::encoder::HqEncoder;
use super::hq::params::HqParams;
use super::rfc9841::context::SharedContextInner;
use crate::compressor::shared::SharedBrotliError;
use crate::compressor::{BrotliCompressError, BrotliResult, CompressParams, QualityLevel};

/// The encoder a quality routes to.
pub(crate) enum Encoder {
    /// Quality 0 and 1: one fragment at a time, static or per-fragment codes.
    Fast(FastEncoder),
    /// Qualities 3 to 9: greedy references over a sliding window.
    Greedy(Box<GreedyEncoder>),
    /// Qualities 10 and 11: Zopfli references and high-quality meta-blocks.
    Hq(Box<HqEncoder>),
}

impl Encoder {
    /// Creates the encoder `params` asks for.
    ///
    /// `size_hint` is the total input size when it is known; qualities four
    /// and above choose a different match finder above one mebibyte.
    ///
    /// # Errors
    ///
    /// Returns [`BrotliCompressError::UnsupportedQuality`] for a quality no
    /// encoder implements yet.
    pub(crate) fn new(
        level: Level,
        params: &CompressParams,
        size_hint: usize,
    ) -> BrotliResult<Self> {
        match FastEncoder::new(level, params) {
            Ok(encoder) => return Ok(Self::Fast(encoder)),
            Err(BrotliCompressError::UnsupportedQuality(_)) => {}
            Err(error) => return Err(error),
        }
        // An explicit hint always wins; the fallback is what the one-shot entry
        // points know and the streaming ones do not.
        let size_hint = params.size_hint().unwrap_or(size_hint);
        match GreedyEncoder::new(level, params, size_hint) {
            Ok(encoder) => return Ok(Self::Greedy(Box::new(encoder))),
            Err(BrotliCompressError::UnsupportedQuality(_)) => {}
            Err(error) => return Err(error),
        }
        Ok(Self::Hq(Box::new(HqEncoder::new(level, params)?)))
    }

    /// Returns the largest input one [`Encoder::encode_block`] call accepts.
    pub(crate) const fn block_size_limit(&self) -> usize {
        match self {
            Self::Fast(encoder) => encoder.block_size_limit(),
            Self::Greedy(encoder) => encoder.block_size_limit(),
            Self::Hq(encoder) => encoder.block_size_limit(),
        }
    }

    /// Restores the encoder for another stream with the same parameters.
    ///
    /// Returns `false` when `params` and `size_hint` would not resolve to this
    /// encoder's shape, in which case nothing is touched and the caller has to
    /// build a new one. A `true` return leaves the encoder exactly as
    /// [`Encoder::new`] would have: every allocation is kept, but no state
    /// from the previous stream survives.
    pub(crate) fn reset_for(&mut self, params: &CompressParams, size_hint: usize) -> bool {
        let size_hint = params.size_hint().unwrap_or(size_hint);
        match self {
            Self::Fast(encoder) => {
                if !encoder.matches(params) {
                    return false;
                }
                encoder.reset();
                true
            }
            Self::Greedy(encoder) => {
                let Ok(fresh) = GreedyParams::new(params, size_hint) else {
                    return false;
                };
                if fresh != *encoder.params() {
                    return false;
                }
                encoder.reset();
                true
            }
            Self::Hq(encoder) => {
                let Ok(fresh) = HqParams::new(params) else {
                    return false;
                };
                if fresh != *encoder.params() {
                    return false;
                }
                encoder.reset();
                true
            }
        }
    }

    /// Compresses one block, consulting `attached` for matches.
    ///
    /// `None` is exactly [`Encoder::encode_block`]. A non-empty context only
    /// reaches an encoder that can consult one, which
    /// [`check_shared_context`] has already established.
    ///
    /// # Errors
    ///
    /// Propagates [`BrotliCompressError::BufferOverflow`] from the encoders.
    pub(crate) fn encode_block_with(
        &mut self,
        input: &[u8],
        is_last: bool,
        attached: Option<&SharedContextInner>,
    ) -> BrotliResult<&[u8]> {
        match self {
            Self::Fast(encoder) => encoder.encode_block(input, is_last),
            Self::Greedy(encoder) => encoder.encode_block_with(input, is_last, attached),
            Self::Hq(encoder) => encoder.encode_block_with(input, is_last, attached),
        }
    }

    /// Compresses one block, closes the meta-block and realigns the stream.
    ///
    /// Mirrors `BROTLI_OPERATION_FLUSH`. Unlike [`Encoder::encode_block`] this
    /// never leaves a meta-block half-gathered, so every byte returned by the
    /// encoder so far decodes to every byte fed into it so far. The stream
    /// stays open: a later [`Encoder::encode_block`] with `is_last` still
    /// terminates it.
    ///
    /// The result is empty only when nothing was buffered and the stream was
    /// already byte-aligned.
    ///
    /// # Errors
    ///
    /// Propagates [`BrotliCompressError::BufferOverflow`] from the encoders.
    pub(crate) fn flush_block(
        &mut self,
        input: &[u8],
        attached: Option<&SharedContextInner>,
    ) -> BrotliResult<&[u8]> {
        match self {
            Self::Fast(encoder) => encoder.flush_block(input),
            Self::Greedy(encoder) => encoder.flush_block(input, attached),
            Self::Hq(encoder) => encoder.flush_block(input, attached),
        }
    }

    /// Returns the bytes this encoder keeps allocated between blocks.
    ///
    /// Counts the buffers that dominate an encoder's footprint — the window,
    /// the match finder, the command and output buffers — so a caller can see
    /// what reusing a compressor is holding on to.
    pub(crate) fn retained_bytes(&self) -> usize {
        match self {
            Self::Fast(encoder) => encoder.retained_bytes(),
            Self::Greedy(encoder) => encoder.retained_bytes(),
            Self::Hq(encoder) => encoder.retained_bytes(),
        }
    }
}

/// A retained encoder, reused when the next call resolves to the same shape.
///
/// This is what backs the public `CompressWorkspace`. It holds one encoder and
/// the SIMD level it was built for; a call whose parameters resolve to that
/// same shape resets it instead of building a new one, and a call that does
/// not replaces it.
#[derive(Default)]
pub(crate) struct EncoderCache {
    /// The retained encoder, absent until the first call fills it.
    encoder: Option<Encoder>,
    /// The level the retained encoder was built for.
    ///
    /// `Level` carries a proof that a target feature is available, so it has
    /// no equality of its own; the discriminant is what actually selects the
    /// kernels, and it is what a reuse has to agree on.
    level: Option<core::mem::Discriminant<Level>>,
}

impl core::fmt::Debug for EncoderCache {
    /// Reports whether an encoder is retained, without naming its type.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EncoderCache")
            .field("retained", &self.encoder.is_some())
            .finish_non_exhaustive()
    }
}

impl EncoderCache {
    /// Returns an encoder for `params`, reusing the retained one if it fits.
    ///
    /// # Errors
    ///
    /// Propagates whatever [`Encoder::new`] reports when a new encoder has to
    /// be built.
    pub(crate) fn acquire(
        &mut self,
        level: Level,
        params: &CompressParams,
        size_hint: usize,
    ) -> BrotliResult<&mut Encoder> {
        let level_key = core::mem::discriminant(&level);
        let reusable = self.level == Some(level_key)
            && self
                .encoder
                .as_mut()
                .is_some_and(|encoder| encoder.reset_for(params, size_hint));
        if !reusable {
            self.encoder = Some(Encoder::new(level, params, size_hint)?);
            self.level = Some(level_key);
        }
        match self.encoder.as_mut() {
            Some(encoder) => Ok(encoder),
            // Unreachable: the branch above assigns `Some` on every path that
            // did not already hold one.
            None => Err(BrotliCompressError::BufferOverflow),
        }
    }

    /// Returns the bytes the retained encoder keeps allocated.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.encoder.as_ref().map_or(0, Encoder::retained_bytes)
    }

    /// Returns the retained encoder, if a call has already built one.
    pub(crate) const fn encoder(&mut self) -> Option<&mut Encoder> {
        self.encoder.as_mut()
    }

    /// Drops the retained encoder, so a failed call cannot leak state.
    ///
    /// An encoder is reset on the way in rather than on the way out, so a
    /// mid-stream error would otherwise leave a half-written stream behind for
    /// the next call to reset. Resetting is cheap; forgetting is cheaper and
    /// removes the question.
    pub(crate) fn invalidate(&mut self) {
        self.encoder = None;
        self.level = None;
    }
}

/// Rejects a large window at a quality that cannot carry one.
///
/// Runs before the empty-input shortcut, so an explicit RFC 9841 request is
/// never quietly dropped on the way to a one-byte stream. It only inspects the
/// field this extension added, which is why no previously constructible
/// parameter set changes behaviour: without a large window the check is a
/// predictable branch that returns immediately.
const fn check_large_window(params: &CompressParams) -> BrotliResult<()> {
    if !params.lgwin().is_large() {
        return Ok(());
    }
    match params.quality() {
        // The fast qualities write distances through a static entropy model
        // built for the RFC 7932 alphabet and cannot carry the wider one.
        QualityLevel::Q0 => Err(BrotliCompressError::Shared(
            SharedBrotliError::UnsupportedLargeWindow { quality: 0 },
        )),
        QualityLevel::Q1 => Err(BrotliCompressError::Shared(
            SharedBrotliError::UnsupportedLargeWindow { quality: 1 },
        )),
        // `SanitizeParams` forces `large_window` off at or below quality two,
        // because the fixed distance code these qualities may fall back to is
        // built for the RFC 7932 alphabet. Refusing rather than silently
        // dropping the request is this crate's contract.
        QualityLevel::Q2 => Err(BrotliCompressError::Shared(
            SharedBrotliError::UnsupportedLargeWindow { quality: 2 },
        )),
        _ => Ok(()),
    }
}

/// Returns whether `quality` has a match finder that consults a prefix.
///
/// The reference compiles its compound-dictionary search only for the match
/// finders qualities five and above select — `H5`, `H6`, `H40`, `H41`, `H42`,
/// `H55`, `H65` and the binary tree — so a lower quality has nowhere to put a
/// prefix match. Where the reference then silently ignores the dictionary,
/// this crate refuses: a stream compressed without the dictionary it was given
/// decodes perfectly well, so the mistake would stay invisible until a decoder
/// that does attach it produced the wrong bytes.
pub(crate) const fn quality_reads_a_prefix(quality: QualityLevel) -> bool {
    matches!(
        quality,
        QualityLevel::Q5
            | QualityLevel::Q6
            | QualityLevel::Q7
            | QualityLevel::Q8
            | QualityLevel::Q9
            | QualityLevel::Q10
            | QualityLevel::Q11
    )
}

/// Upper bound the reference one-shot API enforces on its own output.
///
/// Mirrors `BrotliEncoderMaxCompressedSize`; `None` marks the overflow the
/// reference reports as a zero bound, which disables the check entirely.
const fn max_compressed_size(input_size: usize) -> Option<usize> {
    if input_size == 0 {
        return Some(2);
    }
    let large_blocks = input_size >> 14;
    let overhead = 2 + 4 * large_blocks + 3 + 1;
    input_size.checked_add(overhead)
}

/// Wraps `src` in a stream of uncompressed meta-blocks with a minimal window.
///
/// Mirrors `MakeUncompressedStream`; the reference falls back to this whenever
/// compressing produced more bytes than [`max_compressed_size`] allows.
fn make_uncompressed_stream(src: &[u8], out: &mut Vec<u8>) {
    if src.is_empty() {
        out.push(6);
        return;
    }
    out.push(0x21); // window bits = 10, is_last = false
    out.push(0x03); // empty metadata, padding
    let mut offset = 0usize;
    while offset < src.len() {
        let chunk = (src.len() - offset).min(1 << 24);
        let nibbles: u32 = if chunk > 1 << 20 {
            2
        } else if chunk > 1 << 16 {
            1
        } else {
            0
        };
        let bits = (nibbles << 1) | (((chunk - 1) as u32) << 3) | (1u32 << (19 + 4 * nibbles));
        out.extend_from_slice(&bits.to_le_bytes()[..if nibbles == 2 { 4 } else { 3 }]);
        out.extend_from_slice(&src[offset..offset + chunk]);
        offset += chunk;
    }
    out.push(3);
}

/// Drives a whole input through a fresh encoder, appending to `out`.
///
/// Returns the number of bytes appended.
fn drive_appending(
    encoder: &mut Encoder,
    attached: Option<&SharedContextInner>,
    src: &[u8],
    out: &mut Vec<u8>,
) -> BrotliResult<usize> {
    let limit = encoder.block_size_limit();
    let mut offset = 0usize;
    let mut written = 0usize;
    loop {
        let block = (src.len() - offset).min(limit);
        let is_last = offset + block == src.len();
        let bytes = encoder.encode_block_with(&src[offset..offset + block], is_last, attached)?;
        out.extend_from_slice(bytes);
        written += bytes.len();
        offset += block;
        if is_last {
            return Ok(written);
        }
    }
}

/// Drives a whole input through a fresh encoder, writing straight into `dst`.
///
/// Returns the number of bytes written. A fast-path fragment is encoded in
/// place when `dst` still has room for its reservation, which removes a full
/// copy of the output; otherwise the encoder's own scratch buffer is used and
/// the result is copied, so a caller-sized buffer still works when it is
/// merely tight.
fn drive_into(
    encoder: &mut Encoder,
    attached: Option<&SharedContextInner>,
    src: &[u8],
    dst: &mut [u8],
) -> BrotliResult<usize> {
    let limit = encoder.block_size_limit();
    let mut offset = 0usize;
    let mut written = 0usize;
    loop {
        let block = (src.len() - offset).min(limit);
        let is_last = offset + block == src.len();
        let input = &src[offset..offset + block];

        let tail = dst
            .get_mut(written..)
            .ok_or(BrotliCompressError::OutputTooSmall)?;
        let complete = match &mut *encoder {
            Encoder::Fast(fast) if tail.len() >= FastEncoder::fragment_reserve(block)? => {
                fast.encode_block_into(input, is_last, tail)?
            }
            other => {
                let bytes = other.encode_block_with(input, is_last, attached)?;
                let target = tail
                    .get_mut(..bytes.len())
                    .ok_or(BrotliCompressError::OutputTooSmall)?;
                target.copy_from_slice(bytes);
                bytes.len()
            }
        };

        written += complete;
        offset += block;
        if is_last {
            return Ok(written);
        }
    }
}

/// Compresses `src` into `out`, reusing `cache` and consulting `attached`.
///
/// # Errors
///
/// Propagates [`BrotliCompressError::UnsupportedQuality`] for qualities no
/// encoder implements.
pub(crate) fn compress_to_vec_attached(
    cache: &mut EncoderCache,
    level: Level,
    params: &CompressParams,
    attached: Option<&SharedContextInner>,
    src: &[u8],
    out: &mut Vec<u8>,
) -> BrotliResult<()> {
    check_large_window(params)?;
    if src.is_empty() {
        out.push(6);
        return Ok(());
    }
    let start = out.len();
    let encoder = match cache.acquire(level, params, src.len()) {
        Ok(encoder) => encoder,
        Err(error) => {
            cache.invalidate();
            return Err(error);
        }
    };
    let written = match drive_appending(encoder, attached, src, out) {
        Ok(written) => written,
        Err(error) => {
            // Leave the caller's vector exactly as it was found.
            out.truncate(start);
            cache.invalidate();
            return Err(error);
        }
    };

    if max_compressed_size(src.len()).is_some_and(|max| written > max) {
        out.truncate(start);
        make_uncompressed_stream(src, out);
    }
    Ok(())
}

/// Compresses `src` into `dst`, reusing `cache` and consulting `attached`.
///
/// # Errors
///
/// Returns [`BrotliCompressError::OutputTooSmall`] when `dst` cannot hold the
/// whole stream, and propagates quality routing errors.
pub(crate) fn compress_to_slice_attached(
    cache: &mut EncoderCache,
    level: Level,
    params: &CompressParams,
    attached: Option<&SharedContextInner>,
    src: &[u8],
    dst: &mut [u8],
) -> BrotliResult<usize> {
    check_large_window(params)?;
    if src.is_empty() {
        let target = dst.first_mut().ok_or(BrotliCompressError::OutputTooSmall)?;
        *target = 6;
        return Ok(1);
    }

    let encoder = match cache.acquire(level, params, src.len()) {
        Ok(encoder) => encoder,
        Err(error) => {
            cache.invalidate();
            return Err(error);
        }
    };
    // The uncompressed fallback can still shrink a stream that did not fit, so
    // a short buffer is only reported once the fallback has been ruled out.
    let outcome = drive_into(encoder, attached, src, dst);
    let written = match outcome {
        Ok(written) => written,
        Err(BrotliCompressError::OutputTooSmall) => {
            // The stream was abandoned part-written, so the retained encoder
            // is dropped rather than reset: the fallback below may still
            // succeed, and the next call must not inherit anything from here.
            cache.invalidate();
            usize::MAX
        }
        Err(error) => {
            cache.invalidate();
            return Err(error);
        }
    };

    if max_compressed_size(src.len()).is_some_and(|max| written > max) {
        let mut fallback = Vec::new();
        make_uncompressed_stream(src, &mut fallback);
        let target = dst
            .get_mut(..fallback.len())
            .ok_or(BrotliCompressError::OutputTooSmall)?;
        target.copy_from_slice(&fallback);
        return Ok(fallback.len());
    }
    if written == usize::MAX {
        return Err(BrotliCompressError::OutputTooSmall);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compressor::{QualityLevel, WindowBits};

    /// Compresses `src` through a throwaway workspace, as the tests need.
    fn compress_to_vec(
        level: Level,
        params: &CompressParams,
        src: &[u8],
        out: &mut Vec<u8>,
    ) -> BrotliResult<()> {
        compress_to_vec_attached(&mut EncoderCache::default(), level, params, None, src, out)
    }

    /// Compresses `src` into `dst` through a throwaway workspace.
    fn compress_to_slice(
        level: Level,
        params: &CompressParams,
        src: &[u8],
        dst: &mut [u8],
    ) -> BrotliResult<usize> {
        compress_to_slice_attached(&mut EncoderCache::default(), level, params, None, src, dst)
    }

    fn params(quality: QualityLevel, lgwin: u8) -> CompressParams {
        let lgwin = WindowBits::standard(lgwin).unwrap_or(WindowBits::DEFAULT);
        CompressParams::new(quality, lgwin)
    }

    /// Every quality this crate implements.
    const IMPLEMENTED: [QualityLevel; 12] = [
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

    #[test]
    fn quality_routing_reaches_both_encoders() {
        let level = Level::new();
        for quality in IMPLEMENTED {
            let encoder = Encoder::new(level, &params(quality, 22), 0).expect("routed");
            match (quality, encoder) {
                (QualityLevel::Q0 | QualityLevel::Q1, Encoder::Fast(_)) => {}
                (
                    QualityLevel::Q2
                    | QualityLevel::Q3
                    | QualityLevel::Q4
                    | QualityLevel::Q5
                    | QualityLevel::Q6
                    | QualityLevel::Q7
                    | QualityLevel::Q8
                    | QualityLevel::Q9,
                    Encoder::Greedy(_),
                ) => {}
                (QualityLevel::Q10 | QualityLevel::Q11, Encoder::Hq(_)) => {}
                (quality, _) => panic!("quality {quality:?} routed to the wrong encoder"),
            }
        }
    }

    #[test]
    fn every_quality_the_format_defines_now_compresses() {
        let level = Level::new();
        for quality in IMPLEMENTED {
            let mut out = Vec::new();
            compress_to_vec(level, &params(quality, 22), b"data data data", &mut out)
                .unwrap_or_else(|error| panic!("quality {quality:?} failed: {error}"));
            assert!(!out.is_empty(), "quality {quality:?} produced nothing");
        }
    }

    #[test]
    fn slice_output_reports_a_too_small_buffer() {
        let level = Level::new();
        let mut dst = [0u8; 1];
        for quality in IMPLEMENTED {
            assert!(matches!(
                compress_to_slice(
                    level,
                    &params(quality, 22),
                    b"hello world hello world",
                    &mut dst
                ),
                Err(BrotliCompressError::OutputTooSmall)
            ));
        }
    }

    #[test]
    fn vector_and_slice_outputs_agree() {
        let level = Level::new();
        for quality in IMPLEMENTED {
            let params = params(quality, 22);
            let input: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
            let mut expected = Vec::new();
            compress_to_vec(level, &params, &input, &mut expected).expect("vector output");
            let mut actual = vec![0u8; expected.len()];
            let written =
                compress_to_slice(level, &params, &input, &mut actual).expect("slice output");
            assert_eq!(written, expected.len(), "quality {quality:?}");
            assert_eq!(actual, expected, "quality {quality:?}");
        }
    }

    #[test]
    fn an_empty_input_is_one_byte() {
        let level = Level::new();
        for quality in IMPLEMENTED {
            let mut out = Vec::new();
            compress_to_vec(level, &params(quality, 22), b"", &mut out).expect("empty input");
            assert_eq!(out, vec![6]);

            let mut dst = [0u8; 4];
            assert_eq!(
                compress_to_slice(level, &params(quality, 22), b"", &mut dst).ok(),
                Some(1)
            );
            assert_eq!(dst[0], 6);
        }
    }

    #[test]
    fn the_maximum_compressed_size_matches_the_reference() {
        assert_eq!(max_compressed_size(0), Some(2));
        assert_eq!(max_compressed_size(1), Some(1 + 6));
        assert_eq!(max_compressed_size(1 << 14), Some((1 << 14) + 10));
        assert_eq!(max_compressed_size(usize::MAX), None);
    }

    #[test]
    fn the_uncompressed_fallback_is_a_valid_stream_shape() {
        let mut out = Vec::new();
        make_uncompressed_stream(b"", &mut out);
        assert_eq!(out, vec![6]);

        let payload: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
        let mut out = Vec::new();
        make_uncompressed_stream(&payload, &mut out);
        assert_eq!(out[0], 0x21);
        assert_eq!(out[1], 0x03);
        assert_eq!(out[out.len() - 1], 3);
        assert!(out.len() > payload.len());
        assert!(
            out.windows(payload.len())
                .any(|slice| slice == payload.as_slice())
        );
    }

    /// Address of the retained encoder, so reuse can be observed directly.
    ///
    /// Two calls that reuse hand back the same encoder; a rebuild almost
    /// always moves it, but the identity check below is only used to prove
    /// reuse happened, never to prove it did not.
    fn retained(cache: &mut EncoderCache) -> usize {
        match cache.encoder.as_mut() {
            Some(encoder) => core::ptr::from_mut(encoder) as usize,
            None => 0,
        }
    }

    #[test]
    fn the_cache_reuses_an_encoder_of_the_same_shape() {
        let level = Level::new();
        for quality in IMPLEMENTED {
            // The hint is pinned, so two different input lengths still resolve
            // to the same match finder and the same block sizes.
            let params = params(quality, 22).with_size_hint(Some(1 << 20));
            let mut cache = EncoderCache::default();

            cache.acquire(level, &params, 100).expect("first acquire");
            let first = retained(&mut cache);
            assert_ne!(first, 0, "quality {quality:?}: nothing was retained");

            cache.acquire(level, &params, 5000).expect("second acquire");
            assert_eq!(
                retained(&mut cache),
                first,
                "quality {quality:?}: an identically shaped call rebuilt the encoder"
            );
        }
    }

    #[test]
    fn the_cache_rebuilds_when_the_shape_changes() {
        let level = Level::new();
        let mut cache = EncoderCache::default();
        // Quality 0 and quality 11 do not even share an encoder core, so a
        // reset could not possibly serve both.
        cache
            .acquire(level, &params(QualityLevel::Q0, 22), 1000)
            .expect("first acquire");
        assert!(matches!(cache.encoder, Some(Encoder::Fast(_))));

        cache
            .acquire(level, &params(QualityLevel::Q11, 22), 1000)
            .expect("second acquire");
        assert!(
            matches!(cache.encoder, Some(Encoder::Hq(_))),
            "the cache reused a fast encoder for quality 11"
        );
    }

    #[test]
    fn the_cache_rebuilds_when_the_size_hint_moves_the_matcher() {
        let level = Level::new();
        // Quality 5 picks a different match finder above one mebibyte, so the
        // resolved parameters differ and the encoder must be rebuilt.
        let small = params(QualityLevel::Q5, 22).with_size_hint(Some(1024));
        let large = params(QualityLevel::Q5, 22).with_size_hint(Some(8 << 20));
        let mut cache = EncoderCache::default();

        cache.acquire(level, &small, 1024).expect("first acquire");
        assert!(cache.acquire(level, &large, 1024).is_ok());
        assert!(
            !cache
                .encoder
                .as_mut()
                .expect("retained")
                .reset_for(&small, 1024),
            "the encoder built for a large hint accepted a small one"
        );
    }

    #[test]
    fn invalidating_drops_the_retained_encoder() {
        let level = Level::new();
        let mut cache = EncoderCache::default();
        cache
            .acquire(level, &params(QualityLevel::Q5, 22), 1000)
            .expect("acquire");
        assert_ne!(retained(&mut cache), 0);

        cache.invalidate();
        assert_eq!(retained(&mut cache), 0, "invalidate kept the encoder");
        assert!(cache.level.is_none(), "invalidate kept the level");
    }
}
