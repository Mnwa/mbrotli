//! Bounded synchronous adapters over the same decoder session as one-shot APIs.
//!
//! Each adapter owns an 8 KiB byte buffer. Source/sink errors preserve codec
//! progress, and dropping an adapter never performs I/O.

mod reader;
mod writer;

pub use reader::{DecoderReader, DecoderReaderParts};
pub use writer::DecoderWriter;

use super::{DecodeError, DecodeStreamConfig, Decompressor};
use crate::dictionary::DictionaryRef;
use std::io::{Read, Write};

const BUFFER_BYTES: usize = 8192;

fn buffer() -> Result<Vec<u8>, DecodeError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(BUFFER_BYTES)
        .map_err(|_| DecodeError::AllocationFailed)?;
    bytes.resize(BUFFER_BYTES, 0);
    Ok(bytes)
}

impl Decompressor {
    /// Wraps a compressed source and produces decompressed payload.
    ///
    /// # Errors
    /// Returns session-start or fallible adapter-buffer allocation errors.
    pub fn reader<R: Read>(
        &mut self,
        reader: R,
        stream: DecodeStreamConfig,
    ) -> Result<DecoderReader<'_, 'static, R>, DecodeError> {
        DecoderReader::new(self.start(stream)?, reader)
    }
    /// Wraps a source using a dictionary borrowed for the reader's lifetime.
    ///
    /// # Errors
    /// Returns session-start or fallible adapter-buffer allocation errors.
    pub fn reader_with_dictionary<'d, 'dict, R: Read>(
        &'d mut self,
        dictionary: impl Into<DictionaryRef<'dict>>,
        reader: R,
        stream: DecodeStreamConfig,
    ) -> Result<DecoderReader<'d, 'dict, R>, DecodeError> {
        DecoderReader::new(self.start_with_dictionary(dictionary, stream)?, reader)
    }
    /// Wraps a payload sink and accepts compressed input through `Write`.
    ///
    /// # Errors
    /// Returns session-start or fallible adapter-buffer allocation errors.
    pub fn writer<W: Write>(
        &mut self,
        writer: W,
        stream: DecodeStreamConfig,
    ) -> Result<DecoderWriter<'_, 'static, W>, DecodeError> {
        DecoderWriter::new(self.start(stream)?, writer)
    }
    /// Wraps a sink using an independently borrowed external dictionary.
    ///
    /// # Errors
    /// Returns session-start or fallible adapter-buffer allocation errors.
    pub fn writer_with_dictionary<'d, 'dict, W: Write>(
        &'d mut self,
        dictionary: impl Into<DictionaryRef<'dict>>,
        writer: W,
        stream: DecodeStreamConfig,
    ) -> Result<DecoderWriter<'d, 'dict, W>, DecodeError> {
        DecoderWriter::new(self.start_with_dictionary(dictionary, stream)?, writer)
    }
}
