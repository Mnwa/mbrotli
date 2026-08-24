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
//!
//! # Not implemented yet
//!
//! Shared dictionaries (`SharedContext` and the serialized dictionary format)
//! and the framing container are not written yet; see
//! `architecture/shared-brotli.md` for the current state and the order the
//! remaining work lands in.
//!
//! [RFC 9841]: https://www.rfc-editor.org/rfc/rfc9841.html

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
}
