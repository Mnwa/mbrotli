//! Streaming compression from a [`Read`] source.

use crate::compressor::CompressParams;
use crate::compressor::core::driver::Encoder;
use fearless_simd::Level;
use std::io::{Error, ErrorKind, Read, Result};

/// Adapter that yields the compressed form of an inner reader.
///
/// Each [`Read::read`] call serves bytes from an internal queue, refilling it
/// by compressing one fragment at a time. The stream terminates when the inner
/// reader reports end of file.
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
///
/// let mut source = compressor.compress_reader(params, &b"payload payload payload"[..]);
/// let mut compressed = Vec::new();
/// source.read_to_end(&mut compressed)?;
///
/// assert_eq!(compressed, compressor.compress(params, b"payload payload payload")?);
/// # Ok::<(), std::io::Error>(())
/// ```
pub struct CompressorReader<T: Read> {
    pub(crate) reader: T,
    pub(crate) level: Level,
    pub(crate) params: CompressParams,
    pub(crate) encoder: Option<Encoder>,
    pub(crate) input: Vec<u8>,
    pub(crate) output: Vec<u8>,
    pub(crate) served: usize,
    pub(crate) eof: bool,
}

impl<T: Read> std::fmt::Debug for CompressorReader<T> {
    /// Reports the session's parameters and queue state.
    ///
    /// Neither the inner reader nor the encoder is shown: the reader has no
    /// `Debug` bound, and the encoder is a private implementation type.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompressorReader")
            .field("params", &self.params)
            .field("started", &self.encoder.is_some())
            .field("buffered", &self.input.len())
            .field("queued", &(self.output.len() - self.served))
            .field("eof", &self.eof)
            .finish_non_exhaustive()
    }
}

impl<T: Read> CompressorReader<T> {
    /// Creates an adapter compressing the bytes produced by `reader`.
    pub(crate) const fn new(reader: T, level: Level, params: CompressParams) -> Self {
        Self {
            reader,
            level,
            params,
            encoder: None,
            input: Vec::new(),
            output: Vec::new(),
            served: 0,
            eof: false,
        }
    }

    /// Returns a shared reference to the inner reader.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::{CompressParams, QualityLevel, WindowBits};
    ///
    /// let compressor = Brotli::default().compressor();
    /// let params = CompressParams::new(QualityLevel::Q0, WindowBits::DEFAULT);
    /// let source = compressor.compress_reader(params, &b"data"[..]);
    ///
    /// assert_eq!(source.get_ref().len(), 4);
    /// ```
    pub const fn get_ref(&self) -> &T {
        &self.reader
    }

    /// Returns the encoder, creating it on first use.
    fn encoder(&mut self) -> Result<&mut Encoder> {
        if self.encoder.is_none() {
            let encoder = Encoder::new(
                self.level,
                &self.params,
                self.params.size_hint().unwrap_or(0),
            )?;
            self.input.reserve(encoder.block_size_limit());
            self.encoder = Some(encoder);
        }
        match self.encoder.as_mut() {
            Some(encoder) => Ok(encoder),
            None => Err(Error::other("encoder was not initialised")),
        }
    }

    /// Buffers one byte more than a fragment, so the last one can be detected.
    fn fill_input(&mut self, limit: usize) -> Result<()> {
        while !self.eof && self.input.len() <= limit {
            let filled = self.input.len();
            self.input.resize(limit + 1, 0);
            let outcome = self.reader.read(&mut self.input[filled..]);
            match outcome {
                Ok(0) => {
                    self.input.truncate(filled);
                    self.eof = true;
                }
                Ok(count) => self.input.truncate(filled + count),
                Err(error) if error.kind() == ErrorKind::Interrupted => {
                    self.input.truncate(filled);
                }
                Err(error) => {
                    self.input.truncate(filled);
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    /// Compresses the next fragment into the output queue.
    ///
    /// Returns `false` once the final meta-block has been produced.
    fn refill(&mut self) -> Result<bool> {
        let limit = self.encoder()?.block_size_limit();
        if self.encoder()?.is_finished() {
            return Ok(false);
        }

        self.fill_input(limit)?;
        // Reading one byte past the fragment is what distinguishes the final
        // fragment from a full one that happens to end at the boundary.
        let is_last = self.input.len() <= limit;
        let take = self.input.len().min(limit);

        let Self {
            encoder,
            input,
            output,
            ..
        } = self;
        let Some(encoder) = encoder.as_mut() else {
            return Err(Error::other("encoder was not initialised"));
        };
        let block = encoder.encode_block(&input[..take], is_last)?;
        output.clear();
        output.extend_from_slice(block);
        self.input.drain(..take);
        self.served = 0;
        Ok(true)
    }
}

impl<T: Read> Read for CompressorReader<T> {
    /// Fills `buf` with the next compressed bytes.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        while self.served == self.output.len() {
            if !self.refill()? {
                return Ok(0);
            }
        }
        let available = self.output.len() - self.served;
        let count = available.min(buf.len());
        buf[..count].copy_from_slice(&self.output[self.served..self.served + count]);
        self.served += count;
        Ok(count)
    }
}
