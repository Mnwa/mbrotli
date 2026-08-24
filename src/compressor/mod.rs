//! Public compression API.
//!
//! [`Compressor`] pairs a resolved SIMD level with per-call
//! [`CompressParams`] and exposes one-shot and streaming entry points.
//! The algorithms themselves live in the private `core` tree.

mod core;
pub mod reader;
pub mod shared;
pub mod writer;

use crate::Brotli;
use crate::compressor::reader::CompressorReader;
use crate::compressor::shared::{
    PrefixMatch, SharedBrotliError, SharedContext, SharedContextBuilder,
};
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

    /// Starts building a shared context for `max_quality` and every quality below it.
    ///
    /// The context that comes out is the caller's: it owns its dictionary
    /// bytes, it is passed to the shared entry points by exclusive borrow, and
    /// nothing in this crate keeps a second handle on it.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::QualityLevel;
    ///
    /// let compressor = Brotli::default().compressor();
    /// let context = compressor
    ///     .shared_context_builder(QualityLevel::Q5)
    ///     .add_prefix_dictionary(b"common response prefix".to_vec())
    ///     .prepare()?;
    ///
    /// assert_eq!(context.prefix_dictionary_count(), 1);
    /// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
    /// ```
    pub fn shared_context_builder(&self, max_quality: QualityLevel) -> SharedContextBuilder {
        SharedContextBuilder::new(max_quality)
    }

    /// Returns an upper bound on the compressed size of a shared call.
    ///
    /// Takes the context by shared reference, because a bound activates
    /// nothing and changes nothing. The number is the ordinary
    /// [`Compressor::calculate_bound`]: an attached dictionary only ever adds
    /// places a match may come from, so it can shorten a stream but never
    /// lengthen one, and it changes no header or parameter the bound counts.
    ///
    /// # Errors
    ///
    /// Returns [`BrotliCompressError::Shared`] when the context was prepared
    /// for a lower quality than `params` asks for, and
    /// [`BrotliCompressError::BoundOverflow`] when the bound does not fit in a
    /// `usize`.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::{CompressParams, QualityLevel, WindowBits};
    ///
    /// let compressor = Brotli::default().compressor();
    /// let context = compressor.shared_context_builder(QualityLevel::Q5).prepare()?;
    /// let params = CompressParams::new(QualityLevel::Q5, WindowBits::DEFAULT);
    ///
    /// assert_eq!(
    ///     compressor.calculate_shared_bound(&params, &context, 4096)?,
    ///     compressor.calculate_bound(&params, 4096)?
    /// );
    /// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
    /// ```
    pub fn calculate_shared_bound(
        &self,
        params: &CompressParams,
        context: &SharedContext,
        input_size: usize,
    ) -> BrotliResult<usize> {
        context.check_quality(params.quality())?;
        self.calculate_bound(params, input_size)
    }

    /// Compresses `src` against `context` into a freshly allocated stream.
    ///
    /// The context is borrowed exclusively for the call and returned to its
    /// idle reusable state before this method comes back, whether it succeeded
    /// or failed. Nothing about the context is written into the stream: a
    /// decoder has to be given the same dictionary bytes, in the same order,
    /// out of band.
    ///
    /// An empty context produces exactly the bytes [`Compressor::compress`]
    /// produces for the same parameters.
    ///
    /// # Errors
    ///
    /// Returns [`BrotliCompressError::Shared`] when the context was prepared
    /// for a lower quality than `params` asks for, or when the quality cannot
    /// compress against an attached dictionary yet — which is currently every
    /// quality, so a non-empty context is refused rather than ignored. Also
    /// propagates [`BrotliCompressError::UnsupportedQuality`].
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::{CompressParams, QualityLevel, WindowBits};
    ///
    /// let compressor = Brotli::default().compressor();
    /// let mut context = compressor.shared_context_builder(QualityLevel::Q5).prepare()?;
    /// let params = CompressParams::new(QualityLevel::Q5, WindowBits::DEFAULT);
    ///
    /// assert_eq!(
    ///     compressor.compress_shared(params, &mut context, b"payload payload")?,
    ///     compressor.compress(params, b"payload payload")?
    /// );
    /// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
    /// ```
    pub fn compress_shared(
        &self,
        params: CompressParams,
        context: &mut SharedContext,
        src: &[u8],
    ) -> BrotliResult<Vec<u8>> {
        context.check_quality(params.quality())?;
        let mut output = Vec::with_capacity(self.calculate_bound(&params, src.len())?);
        core::driver::compress_shared_to_vec(
            self.level,
            &params,
            context.inner(),
            src,
            &mut output,
        )?;
        Ok(output)
    }

    /// Compresses `src` against `context` into `dst`.
    ///
    /// Size `dst` with [`Compressor::calculate_shared_bound`]. On
    /// [`BrotliCompressError::OutputTooSmall`] the contents of `dst` are
    /// unspecified and no successful truncated stream is reported, exactly as
    /// for [`Compressor::compress_to_slice`]; the context still returns to its
    /// idle reusable state.
    ///
    /// # Errors
    ///
    /// Returns [`BrotliCompressError::OutputTooSmall`] when `dst` cannot hold
    /// the whole stream, and the same shared-context errors as
    /// [`Compressor::compress_shared`].
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::{CompressParams, QualityLevel, WindowBits};
    ///
    /// let compressor = Brotli::default().compressor();
    /// let mut context = compressor.shared_context_builder(QualityLevel::Q5).prepare()?;
    /// let params = CompressParams::new(QualityLevel::Q5, WindowBits::DEFAULT);
    /// let mut buffer = vec![0u8; compressor.calculate_shared_bound(&params, &context, 5)?];
    /// let written = compressor.compress_shared_to_slice(params, &mut context, b"aaaaa", &mut buffer)?;
    ///
    /// assert_eq!(&buffer[..written], compressor.compress(params, b"aaaaa")?.as_slice());
    /// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
    /// ```
    pub fn compress_shared_to_slice(
        &self,
        params: CompressParams,
        context: &mut SharedContext,
        src: &[u8],
        dst: &mut [u8],
    ) -> BrotliResult<usize> {
        context.check_quality(params.quality())?;
        core::driver::compress_shared_to_slice(self.level, &params, context.inner(), src, dst)
    }

    /// Returns the longest match `context` offers at the start of `input`.
    ///
    /// This is the prefix search the encoders will run at every input position
    /// once they consult attached dictionaries; run directly, it answers how
    /// well a candidate dictionary actually covers a corpus, which is the
    /// question worth asking before shipping one.
    ///
    /// The result is deterministic: attachments are searched oldest first,
    /// each attachment's bucket chain newest first, and only a strictly longer
    /// match displaces the incumbent. A match may begin in one attachment and
    /// continue into the next, because the attachments are one logical byte
    /// sequence. The scan itself is scalar, so the answer does not depend on
    /// which backend this compressor resolved; it lives here rather than on
    /// the context so that a vectorised scan can be dispatched from the level
    /// this type already holds, without moving the method.
    ///
    /// Returns `None` when nothing matched, and when `input` is shorter than
    /// the eight bytes the prepared index is keyed on.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::QualityLevel;
    ///
    /// let compressor = Brotli::default().compressor();
    /// let context = compressor
    ///     .shared_context_builder(QualityLevel::Q5)
    ///     .add_prefix_dictionary(b"HTTP/1.1 200 OK\r\nContent-Type: ".to_vec())
    ///     .prepare()?;
    ///
    /// let found = compressor
    ///     .longest_prefix_match(&context, b"Content-Type: text/html")
    ///     .expect("the dictionary covers the header");
    /// assert_eq!(found.length(), 14);
    ///
    /// assert!(compressor.longest_prefix_match(&context, b"nothing alike").is_none());
    /// assert!(compressor.longest_prefix_match(&context, b"HTTP").is_none());
    /// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
    /// ```
    pub fn longest_prefix_match(
        &self,
        context: &SharedContext,
        input: &[u8],
    ) -> Option<PrefixMatch> {
        context
            .inner()
            .longest_prefix_match(input)
            .map(PrefixMatch::from)
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
    /// let lgwin = WindowBits::standard(18)?;
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

/// Base-2 logarithm of the Brotli sliding window size, and the syntax it uses.
///
/// Brotli has two stream headers for the window, and which one a stream carries
/// is a choice rather than a consequence of the size:
///
/// - [`WindowBits::standard`] writes the RFC 7932 header and allows `10..=24`.
/// - [`WindowBits::large`] writes the fourteen-bit [RFC 9841] Large Window
///   header and allows `10..=62`.
///
/// The two ranges overlap on purpose. `WindowBits::large(22)` and
/// `WindowBits::standard(22)` describe the same window size but produce
/// different streams, because the header and the distance alphabet differ, so a
/// large window is never inferred from a size — it is asked for by name.
///
/// Those two constructors are the only way to build a value, and each validates
/// its own range, so a `WindowBits` always describes a window some header can
/// express. Read it back with [`WindowBits::bits`] and
/// [`WindowBits::is_large`].
///
/// A large window above 30 bits is written to the header faithfully but costs
/// nothing: the encoder never keeps more than 30 bits of history, which is also
/// where the reference C encoder stops.
///
/// [RFC 9841]: https://www.rfc-editor.org/rfc/rfc9841.html
///
/// # Examples
///
/// ```
/// use mbrotli::compressor::WindowBits;
///
/// let ordinary = WindowBits::standard(16)?;
/// let large = WindowBits::large(30)?;
///
/// assert_eq!(ordinary.bits(), 16);
/// assert!(!ordinary.is_large());
/// assert!(large.is_large());
///
/// // The same size, asked for two ways, is two different windows.
/// assert_ne!(WindowBits::standard(22)?, WindowBits::large(22)?);
///
/// assert!(WindowBits::standard(25).is_err());
/// assert!(WindowBits::large(63).is_err());
/// # Ok::<(), mbrotli::compressor::ParseWindowBitsError>(())
/// ```
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct WindowBits(WindowKind);

/// Which header a window is written with, and how wide it is.
///
/// Private so that the only way to build a [`WindowBits`] is through a
/// constructor that has checked the range for the header it picked.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
enum WindowKind {
    /// An RFC 7932 window: `10..=24` bits, the ordinary stream header.
    Standard(u8),
    /// An RFC 9841 Large Window: `10..=62` bits, the fourteen-bit header.
    Large(u8),
}

impl WindowBits {
    /// Smallest window either header allows: 2^10 bytes.
    pub const MIN: Self = Self(WindowKind::Standard(10));

    /// Largest window the RFC 7932 header allows: 2^24 bytes.
    pub const MAX: Self = Self(WindowKind::Standard(24));

    /// Window size used when no other is requested: 2^22 bytes.
    pub const DEFAULT: Self = Self(WindowKind::Standard(22));

    /// Smallest window the RFC 9841 Large Window header allows: 2^10 bytes.
    pub const LARGE_MIN: Self = Self(WindowKind::Large(10));

    /// Largest window the RFC 9841 Large Window header allows: 2^62 bytes.
    pub const LARGE_MAX: Self = Self(WindowKind::Large(62));

    /// Smallest base-2 logarithm either header allows.
    const MIN_BITS: u8 = 10;

    /// Largest base-2 logarithm the RFC 7932 header allows.
    const MAX_STANDARD_BITS: u8 = 24;

    /// Largest base-2 logarithm the RFC 9841 Large Window header allows.
    const MAX_LARGE_BITS: u8 = 62;

    /// Creates an ordinary RFC 7932 window from its base-2 logarithm.
    ///
    /// # Errors
    ///
    /// Returns [`ParseWindowBitsError::LowerBound`] below ten and
    /// [`ParseWindowBitsError::UpperBound`] above twenty-four. A wider window
    /// needs [`WindowBits::large`], which changes the stream header.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::{ParseWindowBitsError, WindowBits};
    ///
    /// assert_eq!(WindowBits::standard(10)?, WindowBits::MIN);
    /// assert_eq!(WindowBits::standard(24)?, WindowBits::MAX);
    /// assert!(matches!(
    ///     WindowBits::standard(9),
    ///     Err(ParseWindowBitsError::LowerBound)
    /// ));
    /// assert!(matches!(
    ///     WindowBits::standard(25),
    ///     Err(ParseWindowBitsError::UpperBound)
    /// ));
    /// # Ok::<(), ParseWindowBitsError>(())
    /// ```
    pub const fn standard(bits: u8) -> Result<Self, ParseWindowBitsError> {
        if bits < Self::MIN_BITS {
            return Err(ParseWindowBitsError::LowerBound);
        }
        if bits > Self::MAX_STANDARD_BITS {
            return Err(ParseWindowBitsError::UpperBound);
        }
        Ok(Self(WindowKind::Standard(bits)))
    }

    /// Creates an RFC 9841 Large Window from its base-2 logarithm.
    ///
    /// Selecting this is always explicit, including for a size an ordinary
    /// window could have expressed: it changes the stream header and the
    /// distance alphabet, so it is never inferred from the size, the input, the
    /// quality or the target.
    ///
    /// # Errors
    ///
    /// Returns [`ParseWindowBitsError::LowerBound`] below ten and
    /// [`ParseWindowBitsError::LargeUpperBound`] above sixty-two.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::{ParseWindowBitsError, WindowBits};
    ///
    /// assert_eq!(WindowBits::large(10)?, WindowBits::LARGE_MIN);
    /// assert_eq!(WindowBits::large(62)?, WindowBits::LARGE_MAX);
    /// assert!(matches!(
    ///     WindowBits::large(63),
    ///     Err(ParseWindowBitsError::LargeUpperBound)
    /// ));
    /// # Ok::<(), ParseWindowBitsError>(())
    /// ```
    pub const fn large(bits: u8) -> Result<Self, ParseWindowBitsError> {
        if bits < Self::MIN_BITS {
            return Err(ParseWindowBitsError::LowerBound);
        }
        if bits > Self::MAX_LARGE_BITS {
            return Err(ParseWindowBitsError::LargeUpperBound);
        }
        Ok(Self(WindowKind::Large(bits)))
    }

    /// Returns the base-2 logarithm of the window size.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::WindowBits;
    ///
    /// assert_eq!(WindowBits::DEFAULT.bits(), 22);
    /// assert_eq!(WindowBits::LARGE_MAX.bits(), 62);
    /// ```
    pub const fn bits(self) -> u8 {
        match self.0 {
            WindowKind::Standard(bits) | WindowKind::Large(bits) => bits,
        }
    }

    /// Returns whether this window uses the RFC 9841 Large Window header.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::WindowBits;
    ///
    /// assert!(!WindowBits::DEFAULT.is_large());
    /// assert!(WindowBits::LARGE_MAX.is_large());
    /// ```
    pub const fn is_large(self) -> bool {
        matches!(self.0, WindowKind::Large(_))
    }
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

impl From<WindowBits> for usize {
    /// Returns the base-2 logarithm of the window size.
    ///
    /// The header the window uses is dropped; [`WindowBits::is_large`] keeps
    /// it.
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
        Self::from(value.bits())
    }
}

/// Error returned when a window size falls outside the range its header allows.
#[derive(Error, Debug, Copy, Clone, Eq, PartialEq)]
#[non_exhaustive]
pub enum ParseWindowBitsError {
    #[error("Window bits should be greater than or equal to 10")]
    LowerBound,
    #[error("Window bits should be less than or equal to 24")]
    UpperBound,
    #[error("Large window bits should be less than or equal to 62")]
    LargeUpperBound,
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
    Q10,
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
            QualityLevel::Q10 => 10,
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
    /// Returns [`ParseQualityLevelError::UpperBound`] above 11.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::{QualityLevel, ParseQualityLevelError};
    ///
    /// assert_eq!(usize::from(QualityLevel::try_from(0)?), 0);
    /// assert_eq!(usize::from(QualityLevel::try_from(11)?), 11);
    /// assert_eq!(usize::from(QualityLevel::try_from(10)?), 10);
    /// assert!(matches!(
    ///     QualityLevel::try_from(12),
    ///     Err(ParseQualityLevelError::UpperBound)
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
            10 => Ok(Self::Q10),
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
    /// An RFC 9841 shared-Brotli feature reported a failure.
    #[error(transparent)]
    Shared(#[from] SharedBrotliError),
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
