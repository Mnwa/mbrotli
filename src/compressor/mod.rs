//! Public compression API.
//!
//! [`BrotliCompressor`] pairs a resolved SIMD level with per-call
//! [`BrotliCompressParams`] and exposes one-shot and streaming entry points.
//! The algorithms themselves live in the private `core` tree.

mod core;
pub mod reader;
pub mod writer;

use crate::Brotli;
use crate::compressor::reader::BrotliCompressorReader;
use crate::compressor::writer::BrotliCompressorWriter;
use fearless_simd::Level;
use std::io::{Read, Write};
use thiserror::Error;

#[derive(Copy, Clone, Debug)]
pub struct BrotliCompressor {
    level: Level,
}

impl BrotliCompressor {
    /// Returns an upper bound on the compressed size of `input_size` bytes.
    ///
    /// # Errors
    ///
    /// Returns [`BrotliCompressError::BoundOverflow`] when the bound does not
    /// fit in a `usize`.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::{BrotliCompressParams, BrotliQualityLevel, BrotliWindowBits};
    ///
    /// let compressor = Brotli::default().compressor();
    /// let params = BrotliCompressParams::new(BrotliQualityLevel::Q0, BrotliWindowBits::DEFAULT);
    ///
    /// assert!(compressor.calculate_bound(&params, 4096)? >= 4096);
    /// assert!(compressor.calculate_bound(&params, usize::MAX).is_err());
    /// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
    /// ```
    pub const fn calculate_bound(
        &self,
        params: &BrotliCompressParams,
        input_size: usize,
    ) -> BrotliResult<usize> {
        core::bound::bound(params, input_size)
    }

    /// Compresses `src` into a freshly allocated Brotli stream.
    ///
    /// # Errors
    ///
    /// Returns [`BrotliCompressError::UnsupportedQuality`] when `params` asks
    /// for a quality this encoder does not implement yet.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::{BrotliCompressParams, BrotliQualityLevel, BrotliWindowBits};
    ///
    /// let compressor = Brotli::default().compressor();
    /// let params = BrotliCompressParams::new(BrotliQualityLevel::Q1, BrotliWindowBits::DEFAULT);
    /// let compressed = compressor.compress(params, b"hello hello hello hello")?;
    ///
    /// assert!(!compressed.is_empty());
    /// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
    /// ```
    pub fn compress(&self, params: BrotliCompressParams, src: &[u8]) -> BrotliResult<Vec<u8>> {
        let mut output = Vec::with_capacity(self.calculate_bound(&params, src.len())?);
        core::fast::compress_to_vec(self.level, &params, src, &mut output)?;
        Ok(output)
    }

    /// Compresses `src` into `dst` and returns the number of bytes written.
    ///
    /// Size `dst` with [`BrotliCompressor::calculate_bound`]; a shorter buffer
    /// is reported rather than truncated.
    ///
    /// # Errors
    ///
    /// Returns [`BrotliCompressError::OutputTooSmall`] when `dst` cannot hold
    /// the whole stream, and [`BrotliCompressError::UnsupportedQuality`] for an
    /// unimplemented quality.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::{BrotliCompressParams, BrotliQualityLevel, BrotliWindowBits};
    ///
    /// let compressor = Brotli::default().compressor();
    /// let params = BrotliCompressParams::new(BrotliQualityLevel::Q0, BrotliWindowBits::DEFAULT);
    /// let mut buffer = vec![0u8; compressor.calculate_bound(&params, 5)?];
    /// let written = compressor.compress_to_slice(params, b"aaaaa", &mut buffer)?;
    ///
    /// assert_eq!(&buffer[..written], compressor.compress(params, b"aaaaa")?.as_slice());
    /// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
    /// ```
    pub fn compress_to_slice(
        &self,
        params: BrotliCompressParams,
        src: &[u8],
        dst: &mut [u8],
    ) -> BrotliResult<usize> {
        core::fast::compress_to_slice(self.level, &params, src, dst)
    }

    /// Wraps `writer` in an adapter that compresses everything written to it.
    ///
    /// The stream is only terminated by
    /// [`BrotliCompressorWriter::finish`]; dropping the adapter discards any
    /// buffered input.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::{BrotliCompressParams, BrotliQualityLevel, BrotliWindowBits};
    /// use std::io::Write;
    ///
    /// let compressor = Brotli::default().compressor();
    /// let params = BrotliCompressParams::new(BrotliQualityLevel::Q0, BrotliWindowBits::DEFAULT);
    /// let mut sink = compressor.compress_writer(params, Vec::new());
    /// sink.write_all(b"streamed payload")?;
    /// let compressed = sink.finish()?;
    ///
    /// assert!(!compressed.is_empty());
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn compress_writer<T: Write>(
        &self,
        params: BrotliCompressParams,
        writer: T,
    ) -> BrotliCompressorWriter<T> {
        BrotliCompressorWriter::new(writer, self.level, params)
    }

    /// Wraps `reader` in an adapter that yields the compressed stream.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::{BrotliCompressParams, BrotliQualityLevel, BrotliWindowBits};
    /// use std::io::Read;
    ///
    /// let compressor = Brotli::default().compressor();
    /// let params = BrotliCompressParams::new(BrotliQualityLevel::Q1, BrotliWindowBits::DEFAULT);
    /// let mut source = compressor.compress_reader(params, &b"streamed payload"[..]);
    /// let mut compressed = Vec::new();
    /// source.read_to_end(&mut compressed)?;
    ///
    /// assert!(!compressed.is_empty());
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn compress_reader<T: Read>(
        &self,
        params: BrotliCompressParams,
        reader: T,
    ) -> BrotliCompressorReader<T> {
        BrotliCompressorReader::new(reader, self.level, params)
    }
}

impl From<Level> for BrotliCompressor {
    fn from(value: Level) -> Self {
        Self { level: value }
    }
}

impl From<Brotli> for BrotliCompressor {
    fn from(value: Brotli) -> Self {
        Self::from(value.level)
    }
}

#[derive(Copy, Clone, Debug)]
pub struct BrotliCompressParams {
    quality: BrotliQualityLevel,
    lgwin: BrotliWindowBits,
}

impl BrotliCompressParams {
    /// Creates compression parameters from a quality level and a window size.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::{BrotliCompressParams, BrotliQualityLevel, BrotliWindowBits};
    ///
    /// let params = BrotliCompressParams::new(BrotliQualityLevel::Q0, BrotliWindowBits::DEFAULT);
    ///
    /// assert_eq!(params.lgwin(), BrotliWindowBits::DEFAULT);
    /// ```
    pub const fn new(quality: BrotliQualityLevel, lgwin: BrotliWindowBits) -> Self {
        Self { quality, lgwin }
    }

    /// Returns the configured quality level.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::{BrotliCompressParams, BrotliQualityLevel, BrotliWindowBits};
    ///
    /// let params = BrotliCompressParams::new(BrotliQualityLevel::Q1, BrotliWindowBits::DEFAULT);
    ///
    /// assert_eq!(usize::from(params.quality()), 1);
    /// ```
    pub const fn quality(&self) -> BrotliQualityLevel {
        self.quality
    }

    /// Returns the configured sliding window size.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::{BrotliCompressParams, BrotliQualityLevel, BrotliWindowBits};
    ///
    /// let lgwin = BrotliWindowBits::try_from(18)?;
    /// let params = BrotliCompressParams::new(BrotliQualityLevel::Q1, lgwin);
    ///
    /// assert_eq!(usize::from(params.lgwin()), 18);
    /// # Ok::<(), mbrotli::compressor::ParseWindowBitsError>(())
    /// ```
    pub const fn lgwin(&self) -> BrotliWindowBits {
        self.lgwin
    }
}

/// Base-2 logarithm of the Brotli sliding window size.
///
/// The Brotli format restricts this value to the inclusive range
/// `10..=24`; every way of building a `BrotliWindowBits` enforces that range,
/// so a value of this type is always usable as a window size.
///
/// # Examples
///
/// ```
/// use mbrotli::compressor::BrotliWindowBits;
///
/// let lgwin = BrotliWindowBits::try_from(16)?;
///
/// assert_eq!(usize::from(lgwin), 16);
/// assert!(BrotliWindowBits::try_from(9).is_err());
/// assert!(BrotliWindowBits::try_from(25).is_err());
/// # Ok::<(), mbrotli::compressor::ParseWindowBitsError>(())
/// ```
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct BrotliWindowBits(usize);

impl BrotliWindowBits {
    /// Smallest window size allowed by the Brotli format: 2^10 bytes.
    pub const MIN: Self = Self(10);

    /// Largest window size allowed by the Brotli format: 2^24 bytes.
    pub const MAX: Self = Self(24);

    /// Window size used when no other is requested: 2^22 bytes.
    pub const DEFAULT: Self = Self(22);
}

impl Default for BrotliWindowBits {
    /// Returns [`BrotliWindowBits::DEFAULT`].
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::BrotliWindowBits;
    ///
    /// assert_eq!(BrotliWindowBits::default(), BrotliWindowBits::DEFAULT);
    /// ```
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl TryFrom<usize> for BrotliWindowBits {
    type Error = ParseWindowBitsError;

    /// Creates a window size from its base-2 logarithm.
    ///
    /// # Errors
    ///
    /// Returns [`ParseWindowBitsError::LowerBound`] when `value` is below
    /// [`BrotliWindowBits::MIN`] and [`ParseWindowBitsError::UpperBound`] when
    /// it is above [`BrotliWindowBits::MAX`].
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::{BrotliWindowBits, ParseWindowBitsError};
    ///
    /// assert_eq!(BrotliWindowBits::try_from(10)?, BrotliWindowBits::MIN);
    /// assert_eq!(BrotliWindowBits::try_from(24)?, BrotliWindowBits::MAX);
    /// assert!(matches!(
    ///     BrotliWindowBits::try_from(9),
    ///     Err(ParseWindowBitsError::LowerBound)
    /// ));
    /// assert!(matches!(
    ///     BrotliWindowBits::try_from(25),
    ///     Err(ParseWindowBitsError::UpperBound)
    /// ));
    /// # Ok::<(), ParseWindowBitsError>(())
    /// ```
    fn try_from(value: usize) -> Result<Self, Self::Error> {
        if value < Self::MIN.0 {
            return Err(ParseWindowBitsError::LowerBound);
        }
        if value > Self::MAX.0 {
            return Err(ParseWindowBitsError::UpperBound);
        }
        Ok(Self(value))
    }
}

impl From<BrotliWindowBits> for usize {
    /// Returns the base-2 logarithm of the window size.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::BrotliWindowBits;
    ///
    /// assert_eq!(usize::from(BrotliWindowBits::MIN), 10);
    /// assert_eq!(usize::from(BrotliWindowBits::MAX), 24);
    /// assert_eq!(usize::from(BrotliWindowBits::DEFAULT), 22);
    /// ```
    fn from(value: BrotliWindowBits) -> Self {
        value.0
    }
}

/// Error returned when a window size falls outside the range the Brotli format
/// allows.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum ParseWindowBitsError {
    #[error("Window bits should be greater than or equal to 10")]
    LowerBound,
    #[error("Window bits should be less than or equal to 24")]
    UpperBound,
}

#[derive(Copy, Clone, Debug)]
pub enum BrotliQualityLevel {
    Q0,
    Q1,
    Q2,
    Q3,
    Q4,
    Q5,
    Q6,
    Q7,
    Q8,
    Q9,
    Q11,
}

impl From<BrotliQualityLevel> for usize {
    /// Returns the numeric quality understood by the Brotli format.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::BrotliQualityLevel;
    ///
    /// assert_eq!(usize::from(BrotliQualityLevel::Q0), 0);
    /// assert_eq!(usize::from(BrotliQualityLevel::Q11), 11);
    /// ```
    fn from(value: BrotliQualityLevel) -> Self {
        match value {
            BrotliQualityLevel::Q0 => 0,
            BrotliQualityLevel::Q1 => 1,
            BrotliQualityLevel::Q2 => 2,
            BrotliQualityLevel::Q3 => 3,
            BrotliQualityLevel::Q4 => 4,
            BrotliQualityLevel::Q5 => 5,
            BrotliQualityLevel::Q6 => 6,
            BrotliQualityLevel::Q7 => 7,
            BrotliQualityLevel::Q8 => 8,
            BrotliQualityLevel::Q9 => 9,
            BrotliQualityLevel::Q11 => 11,
        }
    }
}

impl TryFrom<usize> for BrotliQualityLevel {
    type Error = ParseQualityLevelError;

    /// Creates a quality level from its numeric value.
    ///
    /// # Errors
    ///
    /// Returns [`ParseQualityLevelError::UpperBound`] above 11 and
    /// [`ParseQualityLevelError::Unrepresentable`] for 10, which this API does
    /// not model.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::{BrotliQualityLevel, ParseQualityLevelError};
    ///
    /// assert_eq!(usize::from(BrotliQualityLevel::try_from(0)?), 0);
    /// assert_eq!(usize::from(BrotliQualityLevel::try_from(11)?), 11);
    /// assert!(matches!(
    ///     BrotliQualityLevel::try_from(12),
    ///     Err(ParseQualityLevelError::UpperBound)
    /// ));
    /// assert!(matches!(
    ///     BrotliQualityLevel::try_from(10),
    ///     Err(ParseQualityLevelError::Unrepresentable)
    /// ));
    /// # Ok::<(), ParseQualityLevelError>(())
    /// ```
    fn try_from(value: usize) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Q0),
            1 => Ok(Self::Q1),
            2 => Ok(Self::Q2),
            3 => Ok(Self::Q3),
            4 => Ok(Self::Q4),
            5 => Ok(Self::Q5),
            6 => Ok(Self::Q6),
            7 => Ok(Self::Q7),
            8 => Ok(Self::Q8),
            9 => Ok(Self::Q9),
            10 => Err(ParseQualityLevelError::Unrepresentable),
            11 => Ok(Self::Q11),
            _ => Err(ParseQualityLevelError::UpperBound),
        }
    }
}

/// Error returned when a numeric quality cannot be represented.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum ParseQualityLevelError {
    #[error("Quality level should be positive")]
    LowerBound,
    #[error("Quality level should be less than or equal to 11")]
    UpperBound,
    #[error("Quality level 10 is not represented by this API")]
    Unrepresentable,
}

/// Error returned by the compression entry points.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum BrotliCompressError {
    #[error("IO error: {0}")]
    IOError(#[from] std::io::Error),
    /// The requested quality has no implementation yet.
    #[error("Quality level {0} is not implemented")]
    UnsupportedQuality(usize),
    /// The caller-provided output buffer cannot hold the stream.
    #[error("The output buffer is too small for the compressed stream")]
    OutputTooSmall,
    /// The internal scratch buffer proved too small; this indicates a bug.
    #[error("The encoder output buffer overflowed")]
    BufferOverflow,
    /// The compressed-size bound does not fit in a `usize`.
    #[error("The compressed-size bound overflows the address space")]
    BoundOverflow,
}

impl From<BrotliCompressError> for std::io::Error {
    /// Wraps a compression error so it can travel through [`std::io`] adapters.
    ///
    /// An IO error that entered the encoder is unwrapped again rather than
    /// nested.
    fn from(value: BrotliCompressError) -> Self {
        match value {
            BrotliCompressError::IOError(error) => error,
            other => Self::other(other),
        }
    }
}

/// Result alias used throughout the compressor API.
pub type BrotliResult<T> = Result<T, BrotliCompressError>;
