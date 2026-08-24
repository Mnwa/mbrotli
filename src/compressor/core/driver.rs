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
use super::hq::encoder::HqEncoder;
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

    /// Returns whether the final meta-block has already been written.
    pub(crate) const fn is_finished(&self) -> bool {
        match self {
            Self::Fast(encoder) => encoder.is_finished(),
            Self::Greedy(encoder) => encoder.is_finished(),
            Self::Hq(encoder) => encoder.is_finished(),
        }
    }

    /// Compresses one block and returns the bytes it completed.
    ///
    /// The result may be empty: the greedy and high-quality encoders buffer
    /// input until a meta-block is worth emitting.
    ///
    /// # Errors
    ///
    /// Propagates [`BrotliCompressError::BufferOverflow`] from the encoders.
    pub(crate) fn encode_block(&mut self, input: &[u8], is_last: bool) -> BrotliResult<&[u8]> {
        match self {
            Self::Fast(encoder) => encoder.encode_block(input, is_last),
            Self::Greedy(encoder) => encoder.encode_block(input, is_last),
            Self::Hq(encoder) => encoder.encode_block(input, is_last),
        }
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
        // Quality two has no encoder at all, which is the more useful thing to
        // report.
        QualityLevel::Q2 => Err(BrotliCompressError::UnsupportedQuality(2)),
        _ => Ok(()),
    }
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
    level: Level,
    params: &CompressParams,
    src: &[u8],
    out: &mut Vec<u8>,
) -> BrotliResult<usize> {
    let mut encoder = Encoder::new(level, params, src.len())?;
    let limit = encoder.block_size_limit();
    let mut offset = 0usize;
    let mut written = 0usize;
    loop {
        let block = (src.len() - offset).min(limit);
        let is_last = offset + block == src.len();
        let bytes = encoder.encode_block(&src[offset..offset + block], is_last)?;
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
    level: Level,
    params: &CompressParams,
    src: &[u8],
    dst: &mut [u8],
) -> BrotliResult<usize> {
    let mut encoder = Encoder::new(level, params, src.len())?;
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
        let complete = match &mut encoder {
            Encoder::Fast(fast) if tail.len() >= FastEncoder::fragment_reserve(block)? => {
                fast.encode_block_into(input, is_last, tail)?
            }
            other => {
                let bytes = other.encode_block(input, is_last)?;
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

/// Compresses `src` and appends the stream to `out`.
///
/// Reproduces the one-shot entry point of the reference, including its empty
/// input shortcut and its uncompressed fallback for payloads that grew.
///
/// # Errors
///
/// Propagates [`BrotliCompressError::UnsupportedQuality`] for qualities no
/// encoder implements.
pub(crate) fn compress_to_vec(
    level: Level,
    params: &CompressParams,
    src: &[u8],
    out: &mut Vec<u8>,
) -> BrotliResult<()> {
    check_large_window(params)?;
    if src.is_empty() {
        out.push(6);
        return Ok(());
    }
    let start = out.len();
    let written = match drive_appending(level, params, src, out) {
        Ok(written) => written,
        Err(error) => {
            // Leave the caller's vector exactly as it was found.
            out.truncate(start);
            return Err(error);
        }
    };

    if max_compressed_size(src.len()).is_some_and(|max| written > max) {
        out.truncate(start);
        make_uncompressed_stream(src, out);
    }
    Ok(())
}

/// Compresses `src` into `dst`, returning the number of bytes written.
///
/// # Errors
///
/// Returns [`BrotliCompressError::OutputTooSmall`] when `dst` cannot hold the
/// whole stream, and propagates quality routing errors.
pub(crate) fn compress_to_slice(
    level: Level,
    params: &CompressParams,
    src: &[u8],
    dst: &mut [u8],
) -> BrotliResult<usize> {
    check_large_window(params)?;
    if src.is_empty() {
        let target = dst.first_mut().ok_or(BrotliCompressError::OutputTooSmall)?;
        *target = 6;
        return Ok(1);
    }

    // The uncompressed fallback can still shrink a stream that did not fit, so
    // a short buffer is only reported once the fallback has been ruled out.
    let outcome = drive_into(level, params, src, dst);
    let written = match outcome {
        Ok(written) => written,
        Err(BrotliCompressError::OutputTooSmall) => usize::MAX,
        Err(error) => return Err(error),
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

    fn params(quality: QualityLevel, lgwin: u8) -> CompressParams {
        let lgwin = WindowBits::standard(lgwin).unwrap_or(WindowBits::DEFAULT);
        CompressParams::new(quality, lgwin)
    }

    /// Every quality this crate implements.
    const IMPLEMENTED: [QualityLevel; 11] = [
        QualityLevel::Q0,
        QualityLevel::Q1,
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
                    QualityLevel::Q3
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
    fn unsupported_qualities_are_rejected_before_any_output() {
        let level = Level::new();
        let mut out = Vec::new();
        // Quality two is the only one the format defines that no encoder here
        // implements.
        assert!(matches!(
            compress_to_vec(level, &params(QualityLevel::Q2, 22), b"data", &mut out),
            Err(BrotliCompressError::UnsupportedQuality(2))
        ));
        assert!(out.is_empty());
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
}
