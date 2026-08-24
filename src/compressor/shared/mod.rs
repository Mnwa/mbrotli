//! RFC 9841 Shared Brotli.
//!
//! [RFC 9841] updates RFC 7932 with three separable features: Large Window
//! Brotli, shared dictionaries, and a framing container format. This module
//! owns the parts of that surface which are not per-call encoder parameters.
//!
//! Large Window Brotli is the exception: it is part of the window a call asks
//! for, so it lives beside the other parameters as
//! [`WindowBits::large`](super::WindowBits::large) rather than here.
//!
//! # Implemented today
//!
//! - [`SharedBrotliError`], the error type every RFC 9841 feature reports
//!   through.
//! - Large Window streams for qualities three and above, selected through
//!   [`CompressParams`](super::CompressParams).
//! - [`SharedContext`], the caller-owned mutable context that owns attached
//!   LZ77 prefix dictionaries and the indexes prepared over them, built by
//!   [`SharedContextBuilder`] under [`SharedContextLimits`].
//! - The prefix search those indexes exist for, reachable today through
//!   [`Compressor::longest_prefix_match`](super::Compressor::longest_prefix_match).
//!
//! # Not implemented yet
//!
//! No encoder consults an attached prefix dictionary yet, so a *non-empty*
//! context is refused by every compression entry point with
//! [`SharedBrotliError::UnsupportedSharedContextForQuality`] rather than
//! quietly ignored. An empty context compresses exactly as the ordinary entry
//! points do. The serialized dictionary format and the framing container are
//! not written either; see `architecture/shared-brotli.md` for the current
//! state and the order the remaining work lands in.
//!
//! [RFC 9841]: https://www.rfc-editor.org/rfc/rfc9841.html

mod context;
mod limits;

pub use context::{PrefixMatch, SharedContext, SharedContextBuilder};
pub use limits::SharedContextLimits;

use thiserror::Error;

/// Error reported by the RFC 9841 features of this compressor.
///
/// Every variant travels to the caller inside
/// [`BrotliCompressError::Shared`](super::BrotliCompressError::Shared), which
/// is the only way a shared-Brotli failure reaches the public API.
///
/// # Examples
///
/// ```
/// use mbrotli::Brotli;
/// use mbrotli::compressor::shared::SharedBrotliError;
/// use mbrotli::compressor::{
///     BrotliCompressError, CompressParams, QualityLevel, WindowBits,
/// };
///
/// let compressor = Brotli::default().compressor();
/// let params = CompressParams::new(QualityLevel::Q0, WindowBits::large(30)?);
///
/// assert!(matches!(
///     compressor.compress(params, b"payload"),
///     Err(BrotliCompressError::Shared(
///         SharedBrotliError::UnsupportedLargeWindow { quality: 0 }
///     ))
/// ));
/// # Ok::<(), mbrotli::compressor::ParseWindowBitsError>(())
/// ```
#[derive(Error, Debug, Copy, Clone, Eq, PartialEq)]
#[non_exhaustive]
pub enum SharedBrotliError {
    /// The requested quality has no Large Window implementation.
    ///
    /// Qualities zero and one write their distances through a static entropy
    /// model built for the RFC 7932 alphabet, so they cannot carry the wider
    /// one. The request is refused rather than quietly downgraded to an
    /// ordinary stream.
    #[error("Quality level {quality} does not implement large window Brotli")]
    UnsupportedLargeWindow {
        /// The numeric quality that was asked for.
        quality: usize,
    },
    /// More than fifteen prefix dictionaries were attached to one context.
    ///
    /// RFC 9841 gives a distance no way to say which of a sixteenth
    /// dictionary's bytes it meant, so the limit is the format's, not this
    /// implementation's.
    #[error("A shared context holds at most {limit} prefix dictionaries, not {attached}")]
    TooManyPrefixDictionaries {
        /// How many dictionaries the builder was given.
        attached: usize,
        /// How many it may hold.
        limit: usize,
    },
    /// A dictionary, or the whole logical prefix, is larger than allowed.
    ///
    /// Either past a configured [`SharedContextLimits`] ceiling or past what a
    /// prepared index can address at all.
    #[error("A shared dictionary of {bytes} bytes exceeds the limit of {limit}")]
    DictionaryTooLarge {
        /// How many bytes were offered.
        bytes: u64,
        /// How many were allowed.
        limit: u64,
    },
    /// Preparing the context would allocate more than the limit allows.
    ///
    /// Reported from an upper bound computed before anything is built, so the
    /// allocation the limit refused is never actually made.
    #[error("Preparing a shared context would allocate {bytes} bytes, past the limit of {limit}")]
    SharedContextTooLarge {
        /// The predicted allocation.
        bytes: u64,
        /// How many bytes were allowed.
        limit: u64,
    },
    /// A call asked for a higher quality than the context was prepared for.
    ///
    /// A context prepared for one quality serves that quality and every lower
    /// one; the reverse needs indexes it was never asked to build.
    #[error("A shared context prepared for quality {prepared} cannot serve quality {requested}")]
    SharedContextQualityMismatch {
        /// The quality the call asked for.
        requested: usize,
        /// The quality the context was prepared for.
        prepared: usize,
    },
    /// This quality cannot compress against an attached dictionary yet.
    ///
    /// Raised instead of emitting a valid stream that simply failed to use the
    /// context it was given: a stream compressed without a dictionary decodes
    /// perfectly well, which is exactly what makes silently ignoring one so
    /// hard to notice.
    #[error("Quality level {quality} cannot compress against a shared dictionary yet")]
    UnsupportedSharedContextForQuality {
        /// The numeric quality that was asked for.
        quality: usize,
    },
}
