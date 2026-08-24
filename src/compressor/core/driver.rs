//! Quality routing and the one-shot compression entry points.
//!
//! Ports `BrotliEncoderCompress`, `BrotliEncoderMaxCompressedSize` and
//! `MakeUncompressedStream` from `c/enc/encode.c` of the pinned reference
//! (`google/brotli` v1.2.0, commit `028fb5a`).
//!
//! Two encoder families live below this module: the fast one for qualities
//! zero and one, and the greedy one for qualities three to five. Everything
//! they share — the empty-input shortcut, the final fallback to an
//! uncompressed stream — belongs here rather than in either of them.

use fearless_simd::Level;

use super::fast::FastEncoder;
use super::greedy::encoder::GreedyEncoder;
use crate::compressor::{BrotliCompressError, BrotliResult, CompressParams};

/// The encoder a quality routes to.
pub(crate) enum Encoder {
    /// Quality 0 and 1: one fragment at a time, static or per-fragment codes.
    Fast(FastEncoder),
    /// Quality 3, 4 and 5: greedy references over a sliding window.
    Greedy(Box<GreedyEncoder>),
}

impl Encoder {
    /// Creates the encoder `params` asks for.
    ///
    /// `size_hint` is the total input size when it is known; qualities four and
    /// five choose a different match finder above one mebibyte.
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
            Ok(encoder) => Ok(Self::Fast(encoder)),
            Err(BrotliCompressError::UnsupportedQuality(_)) => {
                // An explicit hint always wins; the fallback is what the
                // one-shot entry points know and the streaming ones do not.
                let size_hint = params.size_hint().unwrap_or(size_hint);
                Ok(Self::Greedy(Box::new(GreedyEncoder::new(
                    level, params, size_hint,
                )?)))
            }
            Err(error) => Err(error),
        }
    }

    /// Returns the largest input one [`Encoder::encode_block`] call accepts.
    pub(crate) const fn block_size_limit(&self) -> usize {
        match self {
            Self::Fast(encoder) => encoder.block_size_limit(),
            Self::Greedy(encoder) => encoder.block_size_limit(),
        }
    }

    /// Returns whether the final meta-block has already been written.
    pub(crate) const fn is_finished(&self) -> bool {
        match self {
            Self::Fast(encoder) => encoder.is_finished(),
            Self::Greedy(encoder) => encoder.is_finished(),
        }
    }

    /// Compresses one block and returns the bytes it completed.
    ///
    /// The result may be empty: the greedy encoder buffers input until a
    /// meta-block is worth emitting.
    ///
    /// # Errors
    ///
    /// Propagates [`BrotliCompressError::BufferOverflow`] from the encoders.
    pub(crate) fn encode_block(&mut self, input: &[u8], is_last: bool) -> BrotliResult<&[u8]> {
        match self {
            Self::Fast(encoder) => encoder.encode_block(input, is_last),
            Self::Greedy(encoder) => encoder.encode_block(input, is_last),
        }
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

    fn params(quality: QualityLevel, lgwin: usize) -> CompressParams {
        let lgwin = WindowBits::try_from(lgwin).unwrap_or(WindowBits::DEFAULT);
        CompressParams::new(quality, lgwin)
    }

    /// Every quality this crate implements.
    const IMPLEMENTED: [QualityLevel; 5] = [
        QualityLevel::Q0,
        QualityLevel::Q1,
        QualityLevel::Q3,
        QualityLevel::Q4,
        QualityLevel::Q5,
    ];

    #[test]
    fn quality_routing_reaches_both_encoders() {
        let level = Level::new();
        for quality in IMPLEMENTED {
            let encoder = Encoder::new(level, &params(quality, 22), 0).expect("routed");
            match (quality, encoder) {
                (QualityLevel::Q0 | QualityLevel::Q1, Encoder::Fast(_)) => {}
                (QualityLevel::Q3 | QualityLevel::Q4 | QualityLevel::Q5, Encoder::Greedy(_)) => {}
                (quality, _) => panic!("quality {quality:?} routed to the wrong encoder"),
            }
        }
    }

    #[test]
    fn unsupported_qualities_are_rejected_before_any_output() {
        let level = Level::new();
        let mut out = Vec::new();
        for quality in [QualityLevel::Q2, QualityLevel::Q6, QualityLevel::Q11] {
            assert!(matches!(
                compress_to_vec(level, &params(quality, 22), b"data", &mut out),
                Err(BrotliCompressError::UnsupportedQuality(_))
            ));
            assert!(out.is_empty());
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
}
