//! Brotli compression, in safe Rust.
//!
//! `mbrotli` implements the two fast Brotli qualities — 0 and 1 — as a port of
//! Google's reference encoder, and emits bytes that are identical to it. There
//! is no `unsafe` in this crate, and the SIMD instruction set is resolved once
//! per compression rather than inside any loop.
//!
//! Qualities 2 through 11 are not implemented yet and are reported as
//! [`BrotliCompressError::UnsupportedQuality`]; there is no decoder.
//!
//! [`BrotliCompressError::UnsupportedQuality`]: compressor::BrotliCompressError::UnsupportedQuality
//!
//! # Examples
//!
//! One-shot compression into a fresh buffer:
//!
//! ```
//! use mbrotli::Brotli;
//! use mbrotli::compressor::{BrotliCompressParams, BrotliQualityLevel, BrotliWindowBits};
//!
//! let compressor = Brotli::default().compressor();
//! let params = BrotliCompressParams::new(BrotliQualityLevel::Q1, BrotliWindowBits::DEFAULT);
//!
//! let payload = "brotli ".repeat(1000);
//! let compressed = compressor.compress(params, payload.as_bytes())?;
//!
//! // Deterministic, and byte-identical to the reference encoder.
//! assert_eq!(payload.len(), 7000);
//! assert_eq!(compressed.len(), 41);
//! # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
//! ```
//!
//! Streaming into a writer, which has to be closed with
//! [`finish`](compressor::writer::BrotliCompressorWriter::finish):
//!
//! ```
//! use mbrotli::Brotli;
//! use mbrotli::compressor::{BrotliCompressParams, BrotliQualityLevel, BrotliWindowBits};
//! use std::io::Write;
//!
//! let compressor = Brotli::default().compressor();
//! let params = BrotliCompressParams::new(BrotliQualityLevel::Q0, BrotliWindowBits::DEFAULT);
//!
//! let mut sink = compressor.compress_writer(params, Vec::new());
//! sink.write_all(b"chunk one ")?;
//! sink.write_all(b"chunk two ")?;
//! let compressed = sink.finish()?;
//!
//! assert_eq!(compressed, compressor.compress(params, b"chunk one chunk two ")?);
//! # Ok::<(), std::io::Error>(())
//! ```

use crate::compressor::BrotliCompressor;
use fearless_simd::Level;

pub mod compressor;

/// Entry point that owns the resolved SIMD instruction set.
///
/// Feature detection happens once, when the value is created, and the result
/// is carried by value from there on. Nothing below this type re-detects
/// anything, so a `Brotli` is cheap to copy and to keep around.
///
/// # Examples
///
/// ```
/// use mbrotli::Brotli;
///
/// let brotli = Brotli::default();
/// let also = brotli; // `Copy`
///
/// assert_eq!(format!("{brotli:?}"), format!("{also:?}"));
/// ```
#[derive(Copy, Clone, Debug)]
pub struct Brotli {
    level: Level,
}

impl Default for Brotli {
    /// Detects the best instruction set this machine supports.
    ///
    /// Falls back to the compile-time baseline when runtime detection is
    /// unavailable on the target.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    ///
    /// let compressor = Brotli::default().compressor();
    /// # let _ = compressor;
    /// ```
    fn default() -> Self {
        Self::from(Level::try_detect().unwrap_or_else(Level::baseline))
    }
}

impl Brotli {
    /// Returns a compressor bound to this instruction set.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::{BrotliCompressParams, BrotliQualityLevel, BrotliWindowBits};
    ///
    /// let params = BrotliCompressParams::new(BrotliQualityLevel::Q0, BrotliWindowBits::DEFAULT);
    /// let compressed = Brotli::default().compressor().compress(params, b"payload payload")?;
    ///
    /// assert!(!compressed.is_empty());
    /// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
    /// ```
    pub fn compressor(&self) -> BrotliCompressor {
        BrotliCompressor::from(*self)
    }
}

impl From<Level> for Brotli {
    /// Pins the encoder to a specific instruction set.
    ///
    /// Every level produces the same bytes; this is useful for testing a
    /// particular backend, or for reusing a level that was detected elsewhere.
    ///
    /// # Examples
    ///
    /// ```
    /// use fearless_simd::Level;
    /// use mbrotli::Brotli;
    ///
    /// let brotli = Brotli::from(Level::baseline());
    /// # let _ = brotli.compressor();
    /// ```
    fn from(value: Level) -> Self {
        Self { level: value }
    }
}
