//! Quality 0 and quality 1 encoders and their runtime SIMD dispatch.
//!
//! This module owns the only dispatch point of the fast path: a
//! [`FastEncoder`] resolves the instruction set once per block and threads the
//! resulting token down into the match scan. Nothing below re-detects features,
//! and every backend produces byte-identical output.
//!
//! The block loop reproduces `BrotliEncoderCompressStreamFast` from
//! `c/enc/encode.c` of the pinned reference (`google/brotli` v1.2.0, commit
//! `028fb5a`): the input is cut into `1 << lgwin` fragments, each fragment gets
//! a freshly cleared hash table, and the trailing partial byte is carried into
//! the next fragment.

pub(crate) mod commands;
pub(crate) mod constants;
pub(crate) mod histogram;
pub(crate) mod q0;
pub(crate) mod q1;
pub(crate) mod tables;
pub(crate) mod workspace;

pub(crate) use crate::compressor::core::shared::{bits, huffman, match_len};

use fearless_simd::{Level, Simd, dispatch};

use self::bits::BitWriter;
use self::constants::{OUTPUT_RESERVE_CONST, OUTPUT_SLACK, WINDOW_BITS_FAST};
use self::q1::TwoPassState;
use self::workspace::OnePassArena;
use crate::compressor::{BrotliCompressError, BrotliResult, CompressParams, QualityLevel};

/// The two qualities this encoder implements.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum FastQuality {
    /// One-pass encoding.
    Q0,
    /// Two-pass encoding.
    Q1,
}

impl TryFrom<QualityLevel> for FastQuality {
    type Error = BrotliCompressError;

    /// Routes quality 0 and 1 to the fast path.
    ///
    /// # Errors
    ///
    /// Returns [`BrotliCompressError::UnsupportedQuality`] for every other
    /// quality, which has no implementation yet.
    fn try_from(value: QualityLevel) -> Result<Self, Self::Error> {
        match value {
            QualityLevel::Q0 => Ok(Self::Q0),
            QualityLevel::Q1 => Ok(Self::Q1),
            other => Err(BrotliCompressError::UnsupportedQuality(usize::from(other))),
        }
    }
}

/// Quality-specific scratch state.
enum FastCore {
    /// Quality 0 state.
    OnePass { arena: Box<OnePassArena> },
    /// Quality 1 state, including the pass-one command and literal buffers.
    TwoPass { state: Box<TwoPassState> },
}

impl FastCore {
    /// Creates the state for `quality`.
    fn new(quality: FastQuality) -> Self {
        match quality {
            FastQuality::Q0 => Self::OnePass {
                arena: Box::default(),
            },
            FastQuality::Q1 => Self::TwoPass {
                state: Box::default(),
            },
        }
    }

    /// Returns the hash table size this core needs for a fragment.
    fn table_entries(&self, input_size: usize) -> usize {
        match self {
            Self::OnePass { .. } => q0::TableBits::for_input(input_size).entries(),
            Self::TwoPass { .. } => q1::TableBits::for_input(input_size).entries(),
        }
    }
}

/// Compresses one fragment with a resolved SIMD token.
///
/// This is the only place the fast path branches on the instruction set; the
/// token is passed by value into every leaf that uses it.
#[inline(always)]
fn encode_fragment<S: Simd>(
    simd: S,
    core: &mut FastCore,
    input: &[u8],
    is_last: bool,
    table: &mut [i32],
    w: &mut BitWriter,
) {
    match core {
        FastCore::OnePass { arena } => q0::compress_fragment(
            simd,
            arena,
            input,
            is_last,
            q0::TableBits::for_input(input.len()),
            table,
            w,
        ),
        FastCore::TwoPass { state } => q1::compress_fragment(
            simd,
            state,
            input,
            is_last,
            q1::TableBits::for_input(input.len()),
            table,
            w,
        ),
    }
}

/// Encodes the stream header for an effective window size.
///
/// The fast path never uses the large-window extension, so the header is at
/// most seven bits wide.
const fn encode_window_bits(lgwin: usize) -> (u16, u32) {
    if lgwin == 16 {
        (0, 1)
    } else if lgwin == 17 {
        (1, 7)
    } else if lgwin > 17 {
        ((((lgwin - 17) << 1) | 0x01) as u16, 4)
    } else {
        ((((lgwin - 8) << 4) | 0x01) as u16, 7)
    }
}

/// Streaming quality 0 / quality 1 encoder.
///
/// One instance owns every buffer the encoder needs and reuses them across
/// blocks, so after construction no allocation happens in the match scan or the
/// command replay.
pub(crate) struct FastEncoder {
    level: Level,
    core: FastCore,
    block_size_limit: usize,
    last_bytes: u16,
    last_bytes_bits: u32,
    table: Vec<i32>,
    storage: Vec<u8>,
    finished: bool,
}

impl FastEncoder {
    /// Creates an encoder for `params` running at SIMD `level`.
    ///
    /// # Errors
    ///
    /// Returns [`BrotliCompressError::UnsupportedQuality`] when the quality is
    /// outside the range this encoder implements.
    pub(crate) fn new(level: Level, params: &CompressParams) -> BrotliResult<Self> {
        let quality = FastQuality::try_from(params.quality())?;
        let lgwin = usize::from(params.lgwin());
        // The reference fast path always advertises at least eighteen window
        // bits, while still cutting the input at the requested window size.
        let (last_bytes, last_bytes_bits) = encode_window_bits(lgwin.max(WINDOW_BITS_FAST));
        Ok(Self {
            level,
            core: FastCore::new(quality),
            block_size_limit: 1usize << lgwin,
            last_bytes,
            last_bytes_bits,
            table: Vec::new(),
            storage: Vec::new(),
            finished: false,
        })
    }

    /// Returns the largest fragment this encoder compresses in one go.
    pub(crate) const fn block_size_limit(&self) -> usize {
        self.block_size_limit
    }

    /// Returns whether the final meta-block has already been written.
    pub(crate) const fn is_finished(&self) -> bool {
        self.finished
    }

    /// Returns the scratch capacity one fragment of `input_len` bytes needs.
    ///
    /// This is the reference's `2 * block_size + 503`, plus the headroom the
    /// bit writer's whole-word stores reach past the last bit.
    ///
    /// # Errors
    ///
    /// Returns [`BrotliCompressError::BufferOverflow`] when the arithmetic
    /// does not fit in a `usize`.
    pub(crate) const fn fragment_reserve(input_len: usize) -> BrotliResult<usize> {
        let Some(doubled) = input_len.checked_mul(2) else {
            return Err(BrotliCompressError::BufferOverflow);
        };
        let Some(reserve) = doubled.checked_add(OUTPUT_RESERVE_CONST) else {
            return Err(BrotliCompressError::BufferOverflow);
        };
        match reserve.checked_add(OUTPUT_SLACK) {
            Some(reserve) => Ok(reserve),
            None => Err(BrotliCompressError::BufferOverflow),
        }
    }

    /// Clears the hash table for a fragment of `input_len` bytes.
    fn prepare_table(&mut self, input_len: usize) -> usize {
        let entries = self.core.table_entries(input_len);
        if self.table.len() < entries {
            // A fresh zeroed allocation, rather than growing in place: the
            // allocator hands out zero pages, while `resize` would memset a
            // buffer the encoder is about to overwrite anyway. It also arrives
            // already cleared.
            self.table = vec![0i32; entries];
        } else {
            // Only the active range is cleared; unused capacity stays untouched.
            self.table[..entries].fill(0);
        }
        entries
    }

    /// Compresses one fragment into `storage`, returning the completed bytes.
    ///
    /// `storage` must hold at least [`FastEncoder::fragment_reserve`] bytes.
    fn run_fragment(
        &mut self,
        input: &[u8],
        is_last: bool,
        entries: usize,
        storage: &mut [u8],
    ) -> BrotliResult<usize> {
        storage[0] = self.last_bytes as u8;
        storage[1] = (self.last_bytes >> 8) as u8;

        let Self {
            level,
            core,
            table,
            last_bytes_bits,
            ..
        } = self;
        let mut w = BitWriter::new(storage, *last_bytes_bits as usize);
        let table = &mut table[..entries];
        dispatch!(*level, simd => encode_fragment(simd, core, input, is_last, table, &mut w));

        if w.overflowed() {
            return Err(BrotliCompressError::BufferOverflow);
        }
        let position = w.position();
        let complete = position >> 3;
        self.last_bytes = u16::from(w.byte(complete));
        self.last_bytes_bits = (position & 7) as u32;
        self.finished = is_last;
        Ok(complete)
    }

    /// Compresses one fragment and returns the bytes it completed.
    ///
    /// A fragment shorter than [`FastEncoder::block_size_limit`] is allowed
    /// only when `is_last` is set. The trailing partial byte is retained and
    /// emitted with the next fragment, or by the final one.
    ///
    /// # Errors
    ///
    /// Returns [`BrotliCompressError::BufferOverflow`] if the internal scratch
    /// buffer proved too small, which would indicate a bug in the size bound.
    #[hotpath::measure]
    pub(crate) fn encode_block(&mut self, input: &[u8], is_last: bool) -> BrotliResult<&[u8]> {
        debug_assert!(!self.finished);
        debug_assert!(input.len() <= self.block_size_limit);

        let reserve = Self::fragment_reserve(input.len())?;
        if self.storage.len() < reserve {
            self.storage = vec![0u8; reserve];
        }
        let entries = self.prepare_table(input.len());

        let mut storage = core::mem::take(&mut self.storage);
        let outcome = self.run_fragment(input, is_last, entries, &mut storage);
        self.storage = storage;
        let complete = outcome?;
        Ok(&self.storage[..complete])
    }

    /// Compresses one fragment straight into `dst`, returning its length.
    ///
    /// This is the in-place path: nothing is copied afterwards. `dst` has to
    /// hold at least [`FastEncoder::fragment_reserve`] bytes, which a buffer
    /// sized by the public compressed-size bound always does.
    ///
    /// # Errors
    ///
    /// Returns [`BrotliCompressError::OutputTooSmall`] when `dst` is shorter
    /// than the reservation, and [`BrotliCompressError::BufferOverflow`] if
    /// the encoder still ran out of room.
    #[hotpath::measure]
    pub(crate) fn encode_block_into(
        &mut self,
        input: &[u8],
        is_last: bool,
        dst: &mut [u8],
    ) -> BrotliResult<usize> {
        debug_assert!(!self.finished);
        debug_assert!(input.len() <= self.block_size_limit);

        if dst.len() < Self::fragment_reserve(input.len())? {
            return Err(BrotliCompressError::OutputTooSmall);
        }
        let entries = self.prepare_table(input.len());
        self.run_fragment(input, is_last, entries, dst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compressor::WindowBits;

    #[test]
    fn window_header_matches_the_reference_encoding() {
        assert_eq!(encode_window_bits(16), (0, 1));
        assert_eq!(encode_window_bits(17), (1, 7));
        assert_eq!(encode_window_bits(18), (3, 4));
        assert_eq!(encode_window_bits(22), (11, 4));
        assert_eq!(encode_window_bits(24), (15, 4));
        assert_eq!(encode_window_bits(10), (0x21, 7));
    }

    #[test]
    fn quality_routing_accepts_only_the_fast_path() {
        assert_eq!(
            FastQuality::try_from(QualityLevel::Q0).ok(),
            Some(FastQuality::Q0)
        );
        assert_eq!(
            FastQuality::try_from(QualityLevel::Q1).ok(),
            Some(FastQuality::Q1)
        );
        assert!(matches!(
            FastQuality::try_from(QualityLevel::Q5),
            Err(BrotliCompressError::UnsupportedQuality(5))
        ));
    }

    #[test]
    fn block_size_limit_follows_the_requested_window() -> Result<(), BrotliCompressError> {
        let level = Level::new();
        for lgwin in [10usize, 16, 18, 22, 24] {
            let lgwin_bits = WindowBits::try_from(lgwin).unwrap_or(WindowBits::DEFAULT);
            let params = CompressParams::new(QualityLevel::Q0, lgwin_bits);
            let encoder = FastEncoder::new(level, &params)?;
            assert_eq!(encoder.block_size_limit(), 1 << usize::from(lgwin_bits));
        }
        Ok(())
    }

    #[test]
    fn table_sizing_depends_on_the_quality() {
        assert_eq!(FastCore::new(FastQuality::Q0).table_entries(10), 512);
        assert_eq!(FastCore::new(FastQuality::Q1).table_entries(10), 256);
        assert_eq!(
            FastCore::new(FastQuality::Q0).table_entries(1 << 20),
            32_768
        );
        assert_eq!(
            FastCore::new(FastQuality::Q1).table_entries(1 << 20),
            131_072
        );
    }
}
