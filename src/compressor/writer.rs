//! Streaming compression into a [`Write`] sink.

use crate::compressor::CompressParams;
use crate::compressor::core::driver::Encoder;
use fearless_simd::Level;
use std::io::{Error, Result, Write};

/// Adapter that compresses everything written to it into an inner writer.
///
/// Input is buffered until a whole fragment is available, so the compressed
/// stream is only terminated by [`CompressorWriter::finish`]. Dropping the
/// adapter without finishing discards the buffered tail and leaves the stream
/// unterminated, which no decoder will accept.
///
/// [`Write::flush`] compresses everything buffered so far and realigns the
/// stream to a byte boundary, so a reader on the far end can decode every byte
/// written up to that point without waiting for the stream to end. It does not
/// terminate the stream, and it costs some ratio; see its own documentation.
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
///
/// let mut sink = compressor.compress_writer(params, Vec::new());
/// sink.write_all(b"chunk one ")?;
/// sink.write_all(b"chunk two ")?;
/// let compressed = sink.finish()?;
///
/// assert_eq!(compressed, compressor.compress(params, b"chunk one chunk two ")?);
/// # Ok::<(), std::io::Error>(())
/// ```
pub struct CompressorWriter<T: Write> {
    pub(crate) writer: T,
    pub(crate) level: Level,
    pub(crate) params: CompressParams,
    pub(crate) encoder: Option<Encoder>,
    pub(crate) pending: Vec<u8>,
}

impl<T: Write> std::fmt::Debug for CompressorWriter<T> {
    /// Reports the session's parameters and how much input is still buffered.
    ///
    /// Neither the inner writer nor the encoder is shown: the writer has no
    /// `Debug` bound, and the encoder is a private implementation type.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompressorWriter")
            .field("params", &self.params)
            .field("started", &self.encoder.is_some())
            .field("pending", &self.pending.len())
            .finish_non_exhaustive()
    }
}

impl<T: Write> CompressorWriter<T> {
    /// Creates an adapter writing compressed data into `writer`.
    pub(crate) const fn new(writer: T, level: Level, params: CompressParams) -> Self {
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
    /// use mbrotli::compressor::{CompressParams, QualityLevel, WindowBits};
    ///
    /// let compressor = Brotli::default().compressor();
    /// let params = CompressParams::new(QualityLevel::Q0, WindowBits::DEFAULT);
    /// let sink = compressor.compress_writer(params, Vec::new());
    ///
    /// assert!(sink.get_ref().is_empty());
    /// ```
    pub const fn get_ref(&self) -> &T {
        &self.writer
    }

    /// Returns the encoder, creating it on first use.
    fn encoder(&mut self) -> Result<&mut Encoder> {
        if self.encoder.is_none() {
            let encoder = Encoder::new(
                self.level,
                &self.params,
                self.params.size_hint().unwrap_or(0),
            )?;
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

    /// Compresses `take` buffered bytes, closes the meta-block and realigns.
    fn emit_flush(&mut self, take: usize) -> Result<()> {
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
        let block = encoder.flush_block(&pending[..take])?;
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

impl<T: Write> Write for CompressorWriter<T> {
    /// Buffers `buf` and compresses every whole fragment it completes.
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        let limit = self.encoder()?.block_size_limit();
        self.pending.extend_from_slice(buf);
        while self.pending.len() > limit {
            self.emit(limit, false)?;
        }
        Ok(buf.len())
    }

    /// Compresses everything buffered so far and flushes the inner writer.
    ///
    /// The Brotli stream is *not* terminated — [`CompressorWriter::finish`]
    /// still has to be called — but it is brought to a point a decoder can
    /// read up to: every byte written into the adapter so far has been
    /// compressed, written on, and followed by the empty metadata block that
    /// realigns the stream to a byte boundary. That is what makes the adapter
    /// usable for an interactive protocol, where the reader on the far end
    /// needs the bytes before the sender knows the stream is over.
    ///
    /// Flushing costs compression: it ends the meta-block early, so the
    /// entropy codes are built from less data, and it adds the two or three
    /// bytes of the padding block. Flushing per small write can easily make
    /// the output larger than the input. Flush on the boundaries the protocol
    /// actually has, not on every write.
    ///
    /// Flushing a writer that has taken no input still emits the stream
    /// header, since the header is what has to be realigned.
    ///
    /// # Errors
    ///
    /// Propagates IO errors from the inner writer and encoder errors such as
    /// [`crate::compressor::BrotliCompressError::UnsupportedQuality`].
    fn flush(&mut self) -> Result<()> {
        let limit = self.encoder()?.block_size_limit();
        while self.pending.len() > limit {
            self.emit(limit, false)?;
        }
        let remaining = self.pending.len();
        self.emit_flush(remaining)?;
        self.writer.flush()
    }
}
