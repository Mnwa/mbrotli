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
    pub const fn calculate_bound(&self, params: &BrotliCompressParams, input_size: usize) -> usize {
        core::bound::bound(params, input_size)
    }
    pub fn compress(&self, params: BrotliCompressParams, src: &[u8]) -> BrotliResult<Vec<u8>> {
        let mut output = Vec::with_capacity(self.calculate_bound(&params, src.len()));
        self.compress_to_slice(params, src, &mut output)?;
        Ok(output)
    }

    pub fn compress_to_slice(
        &self,
        params: BrotliCompressParams,
        src: &[u8],
        dst: &mut [u8],
    ) -> BrotliResult<()> {
        self.compress_reader(params, src)
            .read_exact(dst)
            .map_err(From::from)
    }

    pub fn compress_writer<T: Write>(
        &self,
        params: BrotliCompressParams,
        writer: T,
    ) -> BrotliCompressorWriter<T> {
        BrotliCompressorWriter {
            writer,
            level: self.level,
            params,
        }
    }

    pub fn compress_reader<T: Read>(
        &self,
        params: BrotliCompressParams,
        reader: T,
    ) -> BrotliCompressorReader<T> {
        BrotliCompressorReader {
            reader,
            level: self.level,
            params,
        }
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

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        todo!()
    }
}

#[derive(Error, Debug)]
#[non_exhaustive]
pub enum ParseQualityLevelError {
    #[error("Quality level should be positive")]
    LowerBound,
    #[error("Quality level should be less than or equal to 11")]
    UpperBound,
}

#[derive(Error, Debug)]
#[non_exhaustive]
pub enum BrotliCompressError {
    #[error("IO error: {0}")]
    IOError(#[from] std::io::Error),
}

pub type BrotliResult<T> = Result<T, BrotliCompressError>;
