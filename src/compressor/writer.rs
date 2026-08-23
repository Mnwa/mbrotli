//! Streaming compression into a [`Write`] sink.

use crate::compressor::BrotliCompressParams;
use crate::compressor::core::fast::FastEncoder;
use fearless_simd::Level;
use std::io::{Error, Result, Write};

/// Adapter that compresses everything written to it into an inner writer.
///
/// Input is buffered until a whole fragment is available, so the compressed
/// stream is only complete after [`BrotliCompressorWriter::finish`]. Dropping
/// the adapter without finishing discards the buffered tail; [`Write::flush`]
/// only flushes the inner writer, because a fragment boundary does not
/// necessarily fall on a byte boundary.
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
///
/// let mut sink = compressor.compress_writer(params, Vec::new());
/// sink.write_all(b"chunk one ")?;
/// sink.write_all(b"chunk two ")?;
/// let compressed = sink.finish()?;
///
/// assert_eq!(compressed, compressor.compress(params, b"chunk one chunk two ")?);
/// # Ok::<(), std::io::Error>(())
/// ```
pub struct BrotliCompressorWriter<T: Write> {
    pub(crate) writer: T,
    pub(crate) level: Level,
    pub(crate) params: BrotliCompressParams,
    pub(crate) encoder: Option<FastEncoder>,
    pub(crate) pending: Vec<u8>,
}

impl<T: Write> BrotliCompressorWriter<T> {
    /// Creates an adapter writing compressed data into `writer`.
    pub(crate) const fn new(writer: T, level: Level, params: BrotliCompressParams) -> Self {
        Self {
            writer,
            level,
            params,
            encoder: None,
            pending: Vec::new(),
        }
    }

    /// Returns a shared reference to the inner writer.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::{BrotliCompressParams, BrotliQualityLevel, BrotliWindowBits};
    ///
    /// let compressor = Brotli::default().compressor();
    /// let params = BrotliCompressParams::new(BrotliQualityLevel::Q0, BrotliWindowBits::DEFAULT);
    /// let sink = compressor.compress_writer(params, Vec::new());
    ///
    /// assert!(sink.get_ref().is_empty());
    /// ```
    pub const fn get_ref(&self) -> &T {
        &self.writer
    }

    /// Returns the encoder, creating it on first use.
    fn encoder(&mut self) -> Result<&mut FastEncoder> {
        if self.encoder.is_none() {
            let encoder = FastEncoder::new(self.level, &self.params)?;
            self.pending.reserve(encoder.block_size_limit());
            self.encoder = Some(encoder);
        }
        match self.encoder.as_mut() {
            Some(encoder) => Ok(encoder),
            None => Err(Error::other("encoder was not initialised")),
        }
    }

    /// Compresses `take` buffered bytes and forwards the result.
    fn emit(&mut self, take: usize, is_last: bool) -> Result<()> {
        self.encoder()?;
        let Self {
            writer,
            encoder,
            pending,
            ..
        } = self;
        let Some(encoder) = encoder.as_mut() else {
            return Err(Error::other("encoder was not initialised"));
        };
        let block = encoder.encode_block(&pending[..take], is_last)?;
        writer.write_all(block)?;
        pending.drain(..take);
        Ok(())
    }

    /// Writes the final meta-block and returns the inner writer.
    ///
    /// # Errors
    ///
    /// Propagates IO errors from the inner writer and encoder errors such as
    /// [`crate::compressor::BrotliCompressError::UnsupportedQuality`].
    pub fn finish(mut self) -> Result<T> {
        let limit = self.encoder()?.block_size_limit();
        while self.pending.len() > limit {
            self.emit(limit, false)?;
        }
        let remaining = self.pending.len();
        self.emit(remaining, true)?;
        self.writer.flush()?;
        Ok(self.writer)
    }
}

impl<T: Write> Write for BrotliCompressorWriter<T> {
    /// Buffers `buf` and compresses every whole fragment it completes.
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        let limit = self.encoder()?.block_size_limit();
        self.pending.extend_from_slice(buf);
        while self.pending.len() > limit {
            self.emit(limit, false)?;
        }
        Ok(buf.len())
    }

    /// Flushes the inner writer without terminating the Brotli stream.
    fn flush(&mut self) -> Result<()> {
        self.writer.flush()
    }
}
