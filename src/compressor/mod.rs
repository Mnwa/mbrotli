//! Public compression API.
//!
//! [`Compressor`] pairs a resolved SIMD level with per-call
//! [`CompressParams`] and exposes one-shot and streaming entry points.
//! The algorithms themselves live in the private `core` tree.

mod core;
pub mod reader;
pub mod writer;

use crate::Brotli;
use crate::compressor::reader::CompressorReader;
use crate::compressor::writer::CompressorWriter;
use fearless_simd::Level;
use std::io::{Read, Write};
use thiserror::Error;

#[derive(Copy, Clone, Debug)]
pub struct Compressor {
    level: Level,
}

impl Compressor {
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
    /// use mbrotli::compressor::{CompressParams, QualityLevel, WindowBits};
    ///
    /// let compressor = Brotli::default().compressor();
    /// let params = CompressParams::new(QualityLevel::Q0, WindowBits::DEFAULT);
    ///
    /// assert!(compressor.calculate_bound(&params, 4096)? >= 4096);
    /// assert!(compressor.calculate_bound(&params, usize::MAX).is_err());
    /// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
    /// ```
    pub const fn calculate_bound(
        &self,
        params: &CompressParams,
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
    /// use mbrotli::compressor::{CompressParams, QualityLevel, WindowBits};
    ///
    /// let compressor = Brotli::default().compressor();
    /// let params = CompressParams::new(QualityLevel::Q1, WindowBits::DEFAULT);
    /// let compressed = compressor.compress(params, b"hello hello hello hello")?;
    ///
    /// assert!(!compressed.is_empty());
    /// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
    /// ```
    pub fn compress(&self, params: CompressParams, src: &[u8]) -> BrotliResult<Vec<u8>> {
        let mut output = Vec::with_capacity(self.calculate_bound(&params, src.len())?);
        core::driver::compress_to_vec(self.level, &params, src, &mut output)?;
        Ok(output)
    }

    /// Compresses `src` into `dst` and returns the number of bytes written.
    ///
    /// Size `dst` with [`Compressor::calculate_bound`]; a shorter buffer
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
    /// use mbrotli::compressor::{CompressParams, QualityLevel, WindowBits};
    ///
    /// let compressor = Brotli::default().compressor();
    /// let params = CompressParams::new(QualityLevel::Q0, WindowBits::DEFAULT);
    /// let mut buffer = vec![0u8; compressor.calculate_bound(&params, 5)?];
    /// let written = compressor.compress_to_slice(params, b"aaaaa", &mut buffer)?;
    ///
    /// assert_eq!(&buffer[..written], compressor.compress(params, b"aaaaa")?.as_slice());
    /// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
    /// ```
    pub fn compress_to_slice(
        &self,
        params: CompressParams,
        src: &[u8],
        dst: &mut [u8],
    ) -> BrotliResult<usize> {
        core::driver::compress_to_slice(self.level, &params, src, dst)
    }

    /// Wraps `writer` in an adapter that compresses everything written to it.
    ///
    /// The stream is only terminated by
    /// [`CompressorWriter::finish`]; dropping the adapter discards any
    /// buffered input.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::{CompressParams, QualityLevel, WindowBits};
    /// use std::io::Write;
    ///
    /// let compressor = Brotli::default().compressor();
    /// let params = CompressParams::new(QualityLevel::Q0, WindowBits::DEFAULT);
    /// let mut sink = compressor.compress_writer(params, Vec::new());
    /// sink.write_all(b"streamed payload")?;
    /// let compressed = sink.finish()?;
    ///
    /// assert!(!compressed.is_empty());
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn compress_writer<T: Write>(
        &self,
        params: CompressParams,
        writer: T,
    ) -> CompressorWriter<T> {
        CompressorWriter::new(writer, self.level, params)
    }

    /// Wraps `reader` in an adapter that yields the compressed stream.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::{CompressParams, QualityLevel, WindowBits};
    /// use std::io::Read;
    ///
    /// let compressor = Brotli::default().compressor();
    /// let params = CompressParams::new(QualityLevel::Q1, WindowBits::DEFAULT);
    /// let mut source = compressor.compress_reader(params, &b"streamed payload"[..]);
    /// let mut compressed = Vec::new();
    /// source.read_to_end(&mut compressed)?;
    ///
    /// assert!(!compressed.is_empty());
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn compress_reader<T: Read>(
        &self,
        params: CompressParams,
        reader: T,
    ) -> CompressorReader<T> {
        CompressorReader::new(reader, self.level, params)
    }
}

impl From<Level> for Compressor {
    fn from(value: Level) -> Self {
        Self { level: value }
    }
}

impl From<Brotli> for Compressor {
    fn from(value: Brotli) -> Self {
        Self::from(value.level)
    }
}

/// Every knob the encoder exposes, resolved per compression call.
///
/// # Examples
///
/// ```
/// use mbrotli::compressor::{CompressParams, QualityLevel, WindowBits};
///
/// let params = CompressParams::new(QualityLevel::Q5, WindowBits::DEFAULT)
///     .with_size_hint(Some(4 << 20));
///
/// assert_eq!(params.quality(), QualityLevel::Q5);
/// ```
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CompressParams {
    quality: QualityLevel,
    lgwin: WindowBits,
    lgblock: Option<BlockBits>,
    mode: CompressMode,
    size_hint: Option<usize>,
    distance_codes: DistanceCodes,
    literal_context_modeling: bool,
}

impl CompressParams {
    /// Creates compression parameters from a quality level and a window size.
    ///
    /// Everything else starts at the encoder's own default: a generic mode, an
    /// automatically chosen block size, no size hint, no direct distance codes
    /// and literal context modelling left on.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::{CompressMode, CompressParams, QualityLevel, WindowBits};
    ///
    /// let params = CompressParams::new(QualityLevel::Q0, WindowBits::DEFAULT);
    ///
    /// assert_eq!(params.lgwin(), WindowBits::DEFAULT);
    /// assert_eq!(params.mode(), CompressMode::Generic);
    /// assert!(params.lgblock().is_none());
    /// ```
    pub const fn new(quality: QualityLevel, lgwin: WindowBits) -> Self {
        Self {
            quality,
            lgwin,
            lgblock: None,
            mode: CompressMode::Generic,
            size_hint: None,
            distance_codes: DistanceCodes::DEFAULT,
            literal_context_modeling: true,
        }
    }

    /// Returns the configured quality level.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::{CompressParams, QualityLevel, WindowBits};
    ///
    /// let params = CompressParams::new(QualityLevel::Q1, WindowBits::DEFAULT);
    ///
    /// assert_eq!(usize::from(params.quality()), 1);
    /// ```
    pub const fn quality(&self) -> QualityLevel {
        self.quality
    }

    /// Returns the configured sliding window size.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::{CompressParams, QualityLevel, WindowBits};
    ///
    /// let lgwin = WindowBits::try_from(18)?;
    /// let params = CompressParams::new(QualityLevel::Q1, lgwin);
    ///
    /// assert_eq!(usize::from(params.lgwin()), 18);
    /// # Ok::<(), mbrotli::compressor::ParseWindowBitsError>(())
    /// ```
    pub const fn lgwin(&self) -> WindowBits {
        self.lgwin
    }

    /// Sets the input block size, or restores the encoder's own choice.
    ///
    /// Qualities below four ignore this: they always work in blocks of
    /// `1 << 14` bytes.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::{BlockBits, CompressParams, QualityLevel, WindowBits};
    ///
    /// let params = CompressParams::new(QualityLevel::Q5, WindowBits::DEFAULT)
    ///     .with_block_bits(Some(BlockBits::MAX));
    ///
    /// assert_eq!(params.lgblock(), Some(BlockBits::MAX));
    /// ```
    #[must_use]
    pub const fn with_block_bits(mut self, lgblock: Option<BlockBits>) -> Self {
        self.lgblock = lgblock;
        self
    }

    /// Returns the configured input block size, if one was requested.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::{CompressParams, QualityLevel, WindowBits};
    ///
    /// let params = CompressParams::new(QualityLevel::Q5, WindowBits::DEFAULT);
    ///
    /// assert!(params.lgblock().is_none());
    /// ```
    pub const fn lgblock(&self) -> Option<BlockBits> {
        self.lgblock
    }

    /// Sets the kind of data being compressed.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::{CompressMode, CompressParams, QualityLevel, WindowBits};
    ///
    /// let params = CompressParams::new(QualityLevel::Q4, WindowBits::DEFAULT)
    ///     .with_mode(CompressMode::Font);
    ///
    /// assert_eq!(params.mode(), CompressMode::Font);
    /// ```
    #[must_use]
    pub const fn with_mode(mut self, mode: CompressMode) -> Self {
        self.mode = mode;
        self
    }

    /// Returns the configured mode.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::{CompressMode, CompressParams, QualityLevel, WindowBits};
    ///
    /// let params = CompressParams::new(QualityLevel::Q4, WindowBits::DEFAULT);
    ///
    /// assert_eq!(params.mode(), CompressMode::Generic);
    /// ```
    pub const fn mode(&self) -> CompressMode {
        self.mode
    }

    /// Sets the expected total input size.
    ///
    /// Qualities four and five pick a different match finder for inputs of a
    /// mebibyte or more, so the hint changes the compressed bytes. The one-shot
    /// entry points substitute the real input length when no hint is given,
    /// which is what makes them match the reference encoder's one-shot API; the
    /// streaming adapters have no such length to substitute and treat a missing
    /// hint as zero. Set it explicitly to make both agree.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::{CompressParams, QualityLevel, WindowBits};
    ///
    /// let params = CompressParams::new(QualityLevel::Q5, WindowBits::DEFAULT)
    ///     .with_size_hint(Some(4 << 20));
    ///
    /// assert_eq!(params.size_hint(), Some(4 << 20));
    /// ```
    #[must_use]
    pub const fn with_size_hint(mut self, size_hint: Option<usize>) -> Self {
        self.size_hint = size_hint;
        self
    }

    /// Returns the configured size hint, if one was given.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::{CompressParams, QualityLevel, WindowBits};
    ///
    /// let params = CompressParams::new(QualityLevel::Q5, WindowBits::DEFAULT);
    ///
    /// assert!(params.size_hint().is_none());
    /// ```
    pub const fn size_hint(&self) -> Option<usize> {
        self.size_hint
    }

    /// Sets the distance code layout.
    ///
    /// Qualities below four always use [`DistanceCodes::DEFAULT`], and font
    /// mode overrides this with the layout the reference encoder prefers for
    /// font data.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::{CompressParams, DistanceCodes, QualityLevel, WindowBits};
    ///
    /// let codes = DistanceCodes::try_from((1u32, 4u32))?;
    /// let params = CompressParams::new(QualityLevel::Q5, WindowBits::DEFAULT)
    ///     .with_distance_codes(codes);
    ///
    /// assert_eq!(params.distance_codes(), codes);
    /// # Ok::<(), mbrotli::compressor::ParseDistanceCodesError>(())
    /// ```
    #[must_use]
    pub const fn with_distance_codes(mut self, distance_codes: DistanceCodes) -> Self {
        self.distance_codes = distance_codes;
        self
    }

    /// Returns the configured distance code layout.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::{CompressParams, DistanceCodes, QualityLevel, WindowBits};
    ///
    /// let params = CompressParams::new(QualityLevel::Q5, WindowBits::DEFAULT);
    ///
    /// assert_eq!(params.distance_codes(), DistanceCodes::DEFAULT);
    /// ```
    pub const fn distance_codes(&self) -> DistanceCodes {
        self.distance_codes
    }

    /// Enables or disables literal context modelling.
    ///
    /// Only quality five and above model literal contexts; switching it off
    /// trades compression ratio for decoding speed.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::{CompressParams, QualityLevel, WindowBits};
    ///
    /// let params = CompressParams::new(QualityLevel::Q5, WindowBits::DEFAULT)
    ///     .with_literal_context_modeling(false);
    ///
    /// assert!(!params.literal_context_modeling());
    /// ```
    #[must_use]
    pub const fn with_literal_context_modeling(mut self, enabled: bool) -> Self {
        self.literal_context_modeling = enabled;
        self
    }

    /// Returns whether literal context modelling is enabled.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::{CompressParams, QualityLevel, WindowBits};
    ///
    /// let params = CompressParams::new(QualityLevel::Q5, WindowBits::DEFAULT);
    ///
    /// assert!(params.literal_context_modeling());
    /// ```
    pub const fn literal_context_modeling(&self) -> bool {
        self.literal_context_modeling
    }
}

/// The kind of data a stream carries.
///
/// The encoder uses this as a hint only: every mode produces a valid stream
/// that any decoder reads back identically.
///
/// # Examples
///
/// ```
/// use mbrotli::compressor::CompressMode;
///
/// assert_eq!(CompressMode::default(), CompressMode::Generic);
/// ```
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum CompressMode {
    /// No assumption about the data.
    #[default]
    Generic,
    /// UTF-8 text.
    Text,
    /// Font data, in the WOFF 2.0 sense.
    Font,
}

/// Base-2 logarithm of the encoder's input block size.
///
/// The Brotli encoder restricts an explicitly requested block size to the
/// inclusive range `16..=24`; every way of building a `BlockBits` enforces that
/// range.
///
/// # Examples
///
/// ```
/// use mbrotli::compressor::BlockBits;
///
/// let lgblock = BlockBits::try_from(18)?;
///
/// assert_eq!(usize::from(lgblock), 18);
/// assert!(BlockBits::try_from(15).is_err());
/// assert!(BlockBits::try_from(25).is_err());
/// # Ok::<(), mbrotli::compressor::ParseBlockBitsError>(())
/// ```
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct BlockBits(usize);

impl BlockBits {
    /// Smallest block size the encoder accepts: 2^16 bytes.
    pub const MIN: Self = Self(16);

    /// Largest block size the encoder accepts: 2^24 bytes.
    pub const MAX: Self = Self(24);
}

impl TryFrom<usize> for BlockBits {
    type Error = ParseBlockBitsError;

    /// Creates a block size from its base-2 logarithm.
    ///
    /// # Errors
    ///
    /// Returns [`ParseBlockBitsError::LowerBound`] below [`BlockBits::MIN`] and
    /// [`ParseBlockBitsError::UpperBound`] above [`BlockBits::MAX`].
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::{BlockBits, ParseBlockBitsError};
    ///
    /// assert_eq!(BlockBits::try_from(16)?, BlockBits::MIN);
    /// assert!(matches!(
    ///     BlockBits::try_from(15),
    ///     Err(ParseBlockBitsError::LowerBound)
    /// ));
    /// # Ok::<(), ParseBlockBitsError>(())
    /// ```
    fn try_from(value: usize) -> Result<Self, Self::Error> {
        if value < Self::MIN.0 {
            return Err(ParseBlockBitsError::LowerBound);
        }
        if value > Self::MAX.0 {
            return Err(ParseBlockBitsError::UpperBound);
        }
        Ok(Self(value))
    }
}

impl From<BlockBits> for usize {
    /// Returns the base-2 logarithm of the block size.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::BlockBits;
    ///
    /// assert_eq!(usize::from(BlockBits::MIN), 16);
    /// assert_eq!(usize::from(BlockBits::MAX), 24);
    /// ```
    fn from(value: BlockBits) -> Self {
        value.0
    }
}

/// Error returned when a block size falls outside the range the encoder allows.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum ParseBlockBitsError {
    #[error("Block bits should be greater than or equal to 16")]
    LowerBound,
    #[error("Block bits should be less than or equal to 24")]
    UpperBound,
}

/// Layout of the distance alphabet: postfix bits and direct distance codes.
///
/// The two numbers are not independent. RFC 7932 allows at most three postfix
/// bits and one hundred and twenty direct codes, and the number of direct codes
/// has to be a multiple of `1 << postfix_bits` whose quotient still fits in four
/// bits. Every way of building a `DistanceCodes` enforces all three rules, so a
/// value of this type always describes an alphabet the format can express.
///
/// # Examples
///
/// ```
/// use mbrotli::compressor::DistanceCodes;
///
/// let codes = DistanceCodes::try_from((1u32, 12u32))?;
///
/// assert_eq!(codes.postfix_bits(), 1);
/// assert_eq!(codes.direct_codes(), 12);
/// // 6 is not a multiple of `1 << 2`.
/// assert!(DistanceCodes::try_from((2u32, 6u32)).is_err());
/// # Ok::<(), mbrotli::compressor::ParseDistanceCodesError>(())
/// ```
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct DistanceCodes {
    postfix_bits: u32,
    direct_codes: u32,
}

impl DistanceCodes {
    /// The alphabet with neither postfix bits nor direct distance codes.
    pub const DEFAULT: Self = Self {
        postfix_bits: 0,
        direct_codes: 0,
    };

    /// Returns the number of postfix bits.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::DistanceCodes;
    ///
    /// assert_eq!(DistanceCodes::DEFAULT.postfix_bits(), 0);
    /// ```
    pub const fn postfix_bits(&self) -> u32 {
        self.postfix_bits
    }

    /// Returns the number of direct distance codes.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::DistanceCodes;
    ///
    /// assert_eq!(DistanceCodes::DEFAULT.direct_codes(), 0);
    /// ```
    pub const fn direct_codes(&self) -> u32 {
        self.direct_codes
    }

    /// Builds a pair without validating it, for the sanitiser's own tests.
    #[cfg(test)]
    pub(crate) const fn from_raw(postfix_bits: u32, direct_codes: u32) -> Self {
        Self {
            postfix_bits,
            direct_codes,
        }
    }
}

impl Default for DistanceCodes {
    /// Returns [`DistanceCodes::DEFAULT`].
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::DistanceCodes;
    ///
    /// assert_eq!(DistanceCodes::default(), DistanceCodes::DEFAULT);
    /// ```
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl TryFrom<(u32, u32)> for DistanceCodes {
    type Error = ParseDistanceCodesError;

    /// Creates a distance alphabet from `(postfix_bits, direct_codes)`.
    ///
    /// # Errors
    ///
    /// Returns [`ParseDistanceCodesError::PostfixBits`] above three postfix
    /// bits, [`ParseDistanceCodesError::DirectCodes`] above one hundred and
    /// twenty direct codes, and [`ParseDistanceCodesError::Misaligned`] when
    /// the direct codes do not form a whole number of postfix groups that a
    /// four-bit field can hold.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::{DistanceCodes, ParseDistanceCodesError};
    ///
    /// assert!(DistanceCodes::try_from((0u32, 0u32)).is_ok());
    /// assert!(matches!(
    ///     DistanceCodes::try_from((4u32, 0u32)),
    ///     Err(ParseDistanceCodesError::PostfixBits)
    /// ));
    /// assert!(matches!(
    ///     DistanceCodes::try_from((0u32, 121u32)),
    ///     Err(ParseDistanceCodesError::DirectCodes)
    /// ));
    /// assert!(matches!(
    ///     DistanceCodes::try_from((2u32, 6u32)),
    ///     Err(ParseDistanceCodesError::Misaligned)
    /// ));
    /// ```
    fn try_from((postfix_bits, direct_codes): (u32, u32)) -> Result<Self, Self::Error> {
        if postfix_bits > 3 {
            return Err(ParseDistanceCodesError::PostfixBits);
        }
        if direct_codes > 120 {
            return Err(ParseDistanceCodesError::DirectCodes);
        }
        let groups = (direct_codes >> postfix_bits) & 0x0F;
        if (groups << postfix_bits) != direct_codes {
            return Err(ParseDistanceCodesError::Misaligned);
        }
        Ok(Self {
            postfix_bits,
            direct_codes,
        })
    }
}

/// Error returned when a distance alphabet cannot be expressed by the format.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum ParseDistanceCodesError {
    #[error("Distance postfix bits should be less than or equal to 3")]
    PostfixBits,
    #[error("Direct distance codes should be less than or equal to 120")]
    DirectCodes,
    #[error("Direct distance codes should be a whole number of postfix groups")]
    Misaligned,
}

/// Base-2 logarithm of the Brotli sliding window size.
///
/// The Brotli format restricts this value to the inclusive range
/// `10..=24`; every way of building a `WindowBits` enforces that range,
/// so a value of this type is always usable as a window size.
///
/// # Examples
///
/// ```
/// use mbrotli::compressor::WindowBits;
///
/// let lgwin = WindowBits::try_from(16)?;
///
/// assert_eq!(usize::from(lgwin), 16);
/// assert!(WindowBits::try_from(9).is_err());
/// assert!(WindowBits::try_from(25).is_err());
/// # Ok::<(), mbrotli::compressor::ParseWindowBitsError>(())
/// ```
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct WindowBits(usize);

impl WindowBits {
    /// Smallest window size allowed by the Brotli format: 2^10 bytes.
    pub const MIN: Self = Self(10);

    /// Largest window size allowed by the Brotli format: 2^24 bytes.
    pub const MAX: Self = Self(24);

    /// Window size used when no other is requested: 2^22 bytes.
    pub const DEFAULT: Self = Self(22);
}

impl Default for WindowBits {
    /// Returns [`WindowBits::DEFAULT`].
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::WindowBits;
    ///
    /// assert_eq!(WindowBits::default(), WindowBits::DEFAULT);
    /// ```
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl TryFrom<usize> for WindowBits {
    type Error = ParseWindowBitsError;

    /// Creates a window size from its base-2 logarithm.
    ///
    /// # Errors
    ///
    /// Returns [`ParseWindowBitsError::LowerBound`] when `value` is below
    /// [`WindowBits::MIN`] and [`ParseWindowBitsError::UpperBound`] when
    /// it is above [`WindowBits::MAX`].
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::{WindowBits, ParseWindowBitsError};
    ///
    /// assert_eq!(WindowBits::try_from(10)?, WindowBits::MIN);
    /// assert_eq!(WindowBits::try_from(24)?, WindowBits::MAX);
    /// assert!(matches!(
    ///     WindowBits::try_from(9),
    ///     Err(ParseWindowBitsError::LowerBound)
    /// ));
    /// assert!(matches!(
    ///     WindowBits::try_from(25),
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

impl From<WindowBits> for usize {
    /// Returns the base-2 logarithm of the window size.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::WindowBits;
    ///
    /// assert_eq!(usize::from(WindowBits::MIN), 10);
    /// assert_eq!(usize::from(WindowBits::MAX), 24);
    /// assert_eq!(usize::from(WindowBits::DEFAULT), 22);
    /// ```
    fn from(value: WindowBits) -> Self {
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

/// Compression quality: how much work the encoder spends per byte.
///
/// The variants are ordered the way the format numbers them, so they compare
/// and sort by effort.
///
/// # Examples
///
/// ```
/// use mbrotli::compressor::QualityLevel;
///
/// assert!(QualityLevel::Q1 < QualityLevel::Q5);
/// assert_eq!(usize::from(QualityLevel::Q5), 5);
/// ```
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum QualityLevel {
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

impl From<QualityLevel> for usize {
    /// Returns the numeric quality understood by the Brotli format.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::QualityLevel;
    ///
    /// assert_eq!(usize::from(QualityLevel::Q0), 0);
    /// assert_eq!(usize::from(QualityLevel::Q11), 11);
    /// ```
    fn from(value: QualityLevel) -> Self {
        match value {
            QualityLevel::Q0 => 0,
            QualityLevel::Q1 => 1,
            QualityLevel::Q2 => 2,
            QualityLevel::Q3 => 3,
            QualityLevel::Q4 => 4,
            QualityLevel::Q5 => 5,
            QualityLevel::Q6 => 6,
            QualityLevel::Q7 => 7,
            QualityLevel::Q8 => 8,
            QualityLevel::Q9 => 9,
            QualityLevel::Q11 => 11,
        }
    }
}

impl TryFrom<usize> for QualityLevel {
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
    /// use mbrotli::compressor::{QualityLevel, ParseQualityLevelError};
    ///
    /// assert_eq!(usize::from(QualityLevel::try_from(0)?), 0);
    /// assert_eq!(usize::from(QualityLevel::try_from(11)?), 11);
    /// assert!(matches!(
    ///     QualityLevel::try_from(12),
    ///     Err(ParseQualityLevelError::UpperBound)
    /// ));
    /// assert!(matches!(
    ///     QualityLevel::try_from(10),
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
