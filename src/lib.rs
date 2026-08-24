//! Brotli compression, in safe Rust.
//!
//! `mbrotli` implements every Brotli quality but 2 as a port of Google's
//! reference encoder, and emits bytes that are identical to it. There is no
//! `unsafe` in this crate, and the SIMD instruction set is resolved once per
//! compressed block rather than inside any loop.
//!
//! Quality 2 is the one quality the format defines that has no encoder here;
//! it is reported as [`BrotliCompressError::UnsupportedQuality`]. There is no
//! decoder.
//!
//! [`BrotliCompressError::UnsupportedQuality`]: compressor::BrotliCompressError::UnsupportedQuality
//!
//! # Choosing a quality
//!
//! | Quality | What it does | Typical use |
//! | --- | --- | --- |
//! | 0 | One pass, static entropy codes | Fastest, largest output |
//! | 1 | Two passes, per-block entropy codes | Fast |
//! | 3 | Greedy matching, one prefix code per stream | Balanced |
//! | 4 | Adds block splitting and histogram optimisation | Balanced, denser |
//! | 5 | Adds an extensive search and literal context modelling | Densest of these |
//! | 6 to 9 | Wider match search, more cached distances, richer context models | Denser, slower |
//! | 10, 11 | Binary-tree matching and a Zopfli dynamic program | Densest, slowest |
//!
//! # Large Window Brotli
//!
//! [RFC 9841] widens the sliding window past what RFC 7932 can express. Which
//! header a stream carries is part of the window itself: build one with
//! [`WindowBits::standard`] or [`WindowBits::large`], never by widening a
//! number.
//!
//! [`WindowBits::standard`]: compressor::WindowBits::standard
//! [`WindowBits::large`]: compressor::WindowBits::large
//!
//! ```
//! use mbrotli::Brotli;
//! use mbrotli::compressor::{CompressParams, QualityLevel, WindowBits};
//!
//! let compressor = Brotli::default().compressor();
//! let params = CompressParams::new(QualityLevel::Q5, WindowBits::large(30)?);
//!
//! let payload = "large window ".repeat(1000);
//! let compressed = compressor.compress(params, payload.as_bytes())?;
//!
//! // The stream carries the RFC 9841 header, so it needs a decoder that
//! // expects one.
//! assert_eq!(compressed[0], 0b0001_0001);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Qualities 0 and 1 report
//! [`SharedBrotliError::UnsupportedLargeWindow`] rather than dropping the
//! request.
//!
//! # Shared dictionaries
//!
//! RFC 9841 also lets a caller attach up to fifteen LZ77 prefix dictionaries in
//! front of a stream. [`SharedContext`] is the caller-owned object that holds
//! them and the indexes prepared over them: build it once, keep it, and hand it
//! to a compression call by exclusive borrow. It contains no `Arc`, no lock and
//! no interior mutability, so it is an ordinary owned value that happens to be
//! expensive to build.
//!
//! ```
//! use mbrotli::Brotli;
//! use mbrotli::compressor::QualityLevel;
//!
//! let compressor = Brotli::default().compressor();
//! let context = compressor
//!     .shared_context_builder(QualityLevel::Q5)
//!     .add_prefix_dictionary(b"HTTP/1.1 200 OK\r\nContent-Type: ".to_vec())
//!     .prepare()?;
//!
//! // How much of an input the dictionary actually covers.
//! let found = compressor
//!     .longest_prefix_match(&context, b"Content-Type: text/html")
//!     .expect("the header is in the dictionary");
//! assert_eq!(found.length(), 14);
//! # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
//! ```
//!
//! **No encoder consults an attached dictionary yet.** Until one does,
//! [`Compressor::compress_shared`] refuses a non-empty context with
//! [`SharedBrotliError::UnsupportedSharedContextForQuality`] rather than
//! emitting a stream that quietly ignored it; an *empty* context produces
//! exactly the bytes [`Compressor::compress`] does. Serialized shared
//! dictionaries and the framing container are not implemented.
//!
//! [RFC 9841]: https://www.rfc-editor.org/rfc/rfc9841.html
//! [`SharedBrotliError::UnsupportedLargeWindow`]: compressor::shared::SharedBrotliError::UnsupportedLargeWindow
//! [`SharedBrotliError::UnsupportedSharedContextForQuality`]: compressor::shared::SharedBrotliError::UnsupportedSharedContextForQuality
//! [`SharedContext`]: compressor::shared::SharedContext
//! [`Compressor::compress_shared`]: compressor::Compressor::compress_shared
//! [`Compressor::compress`]: compressor::Compressor::compress
//!
//! Qualities 4 and 5 pick a different match finder for inputs of a mebibyte or
//! more. The one-shot entry points know the input length and pass it on; the
//! streaming adapters do not, so set
//! [`CompressParams::with_size_hint`](compressor::CompressParams::with_size_hint)
//! when a stream should compress exactly like the same bytes in one shot.
//!
//! # Examples
//!
//! One-shot compression into a fresh buffer:
//!
//! ```
//! use mbrotli::Brotli;
//! use mbrotli::compressor::{CompressParams, QualityLevel, WindowBits};
//!
//! let compressor = Brotli::default().compressor();
//! let params = CompressParams::new(QualityLevel::Q1, WindowBits::DEFAULT);
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
//! A denser quality, with the encoder parameters spelled out:
//!
//! ```
//! use mbrotli::Brotli;
//! use mbrotli::compressor::{CompressMode, CompressParams, QualityLevel, WindowBits};
//!
//! let compressor = Brotli::default().compressor();
//! let payload = "the quick brown fox ".repeat(500);
//! let params = CompressParams::new(QualityLevel::Q5, WindowBits::DEFAULT)
//!     .with_mode(CompressMode::Text)
//!     .with_size_hint(Some(payload.len()));
//!
//! let compressed = compressor.compress(params, payload.as_bytes())?;
//!
//! assert!(compressed.len() < payload.len() / 100);
//! # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
//! ```
//!
//! Streaming into a writer, which has to be closed with
//! [`finish`](compressor::writer::CompressorWriter::finish):
//!
//! ```
//! use mbrotli::Brotli;
//! use mbrotli::compressor::{CompressParams, QualityLevel, WindowBits};
//! use std::io::Write;
//!
//! let compressor = Brotli::default().compressor();
//! let params = CompressParams::new(QualityLevel::Q0, WindowBits::DEFAULT);
//!
//! let mut sink = compressor.compress_writer(params, Vec::new());
//! sink.write_all(b"chunk one ")?;
//! sink.write_all(b"chunk two ")?;
//! let compressed = sink.finish()?;
//!
//! assert_eq!(compressed, compressor.compress(params, b"chunk one chunk two ")?);
//! # Ok::<(), std::io::Error>(())
//! ```

use crate::compressor::Compressor;
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
    /// use mbrotli::compressor::{CompressParams, QualityLevel, WindowBits};
    ///
    /// let params = CompressParams::new(QualityLevel::Q0, WindowBits::DEFAULT);
    /// let compressed = Brotli::default().compressor().compress(params, b"payload payload")?;
    ///
    /// assert!(!compressed.is_empty());
    /// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
    /// ```
    pub fn compressor(&self) -> Compressor {
        Compressor::from(*self)
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
