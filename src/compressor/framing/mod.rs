//! Experimental RFC 9841 containers over a reusable compressor.
//!
//! Finish each resource, then finish the container. Neither destructor writes.
//! Chunk methods accept a chunk into a bounded queue; [`FramedWriter::flush`]
//! drains that queue. After a sink error, retry draining or finalization.

mod core;

use super::dictionary::PreparedDictionary;
use super::{Compressor, EncodeError, EncoderSession, StreamConfig};
use std::io::{self, Write};
use thiserror::Error;

/// Container policy and explicit resource ceilings.
#[derive(Debug, Clone, Copy)]
pub struct FramingConfig {
    /// Include a final footer and permit multiple resources and metadata.
    pub container: bool,
    /// Emit a central directory containing every original content header.
    pub central_directory: bool,
    /// Repeat all resource metadata before the central directory.
    pub repeat_metadata: bool,
    /// Maximum uncompressed input retained for one resource chunk (default 64 KiB).
    pub chunk_bytes: usize,
    /// Maximum aggregate metadata content (default 1 MiB).
    pub max_metadata_bytes: usize,
    /// Maximum framing storage (default 8 MiB), excluding the sink, compressor
    /// workspace, and separately prepared dictionaries.
    pub max_buffer_bytes: usize,
    /// Maximum number of resources (default 10,000).
    pub max_resources: u64,
    /// Maximum number of chunks, including generated directory/footer chunks.
    pub max_chunks: u64,
}

impl Default for FramingConfig {
    fn default() -> Self {
        Self {
            container: true,
            central_directory: true,
            repeat_metadata: false,
            chunk_bytes: 65536,
            max_metadata_bytes: 1 << 20,
            max_buffer_bytes: 8 << 20,
            max_resources: 10000,
            max_chunks: 1000000,
        }
    }
}

/// A caller-supplied 256-bit HighwayHash value. No key or hashing policy is implied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DictionaryId(pub [u8; 32]);

/// An explicit dictionary source, in decoder attachment order.
#[derive(Debug, Clone, Copy)]
pub enum DictionaryReference {
    /// An application-resolved external prefix dictionary.
    PrefixId(DictionaryId),
    /// An application-resolved serialized dictionary.
    SerializedId(DictionaryId),
    /// A complete, earlier resource containing prefix bytes.
    PrefixResource(u64),
    /// A complete, earlier resource containing a serialized dictionary.
    SerializedResource(u64),
    /// The contents of an earlier individual chunk, used as a prefix.
    PrefixChunk(u64),
}

/// Resource visibility and optional caller-supplied checksum.
#[derive(Debug, Default, Clone, Copy)]
pub struct ResourceOptions {
    /// Suppress implicit extraction, for example for dictionary resources.
    pub hidden: bool,
    /// Checksum of the whole uncompressed resource, emitted on its last chunk.
    pub id: Option<DictionaryId>,
}

/// Where metadata applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataKind {
    /// Applies to the next resource; permits `id`, `mt`, and uppercase codes.
    Resource,
    /// Applies to the preceding resource; permits uppercase codes only.
    Footer,
    /// Applies to the container; permits uppercase codes only.
    Global,
}

/// One borrowed metadata field. Codes and reserved value shapes are validated.
#[derive(Debug, Clone, Copy)]
pub struct MetadataField<'a> {
    /// Two uppercase ASCII letters, or a recognized lowercase code.
    pub code: [u8; 2],
    /// Raw field content. `id` is UTF-8; `mt` is an eight-byte signed timestamp.
    pub value: &'a [u8],
}

/// Failure to validate, encode, or deliver a container.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FramingError {
    /// Invalid options, references, metadata, or chunk order.
    #[error("invalid framing operation: {0}")]
    Invalid(&'static str),
    /// A configured resource ceiling would be exceeded before allocation.
    #[error("framing resource limit exceeded: {0}")]
    Limit(&'static str),
    /// A wire size or offset cannot fit the RFC's 63-bit varint.
    #[error("framing size or offset overflow")]
    Overflow,
    /// Compression failed. The resource must be abandoned.
    #[error("resource encoding failed: {0}")]
    Encode(#[from] EncodeError),
    /// Sink failure; the unwritten suffix is retained for retry.
    #[error("container output failed: {0}")]
    Io(#[from] io::Error),
}

impl From<FramingError> for io::Error {
    fn from(error: FramingError) -> Self {
        match error {
            FramingError::Io(error) => error,
            other => Self::other(other),
        }
    }
}

/// Recoverable finalization failure retaining the writer and its pending bytes.
#[derive(Debug)]
pub struct FramingFinishError<T> {
    /// Writer to retry or recover the sink from.
    pub writer: T,
    /// Failure reported by the last finalization attempt.
    pub error: FramingError,
}

/// A container borrowing one worker-local compressor.
#[derive(Debug)]
pub struct FramedWriter<'c, W> {
    compressor: &'c mut Compressor,
    core: core::Container<W>,
}

impl Compressor {
    /// Starts an experimental RFC 9841 container without performing I/O.
    ///
    /// The header is queued; the first operation or `flush` delivers it.
    /// Allocation is bounded by `config`. Drop never finalizes a container.
    ///
    /// # Errors
    /// Rejects inconsistent profiles and buffer limits.
    ///
    /// # Examples
    /// ```
    /// use mbrotli::{Compressor, framing::{FramingConfig, ResourceOptions}};
    /// use std::io::Write;
    /// let mut compressor = Compressor::new(Default::default())?;
    /// let mut container = compressor.framed_writer(Vec::new(), FramingConfig::default())?;
    /// let mut resource = container.resource(ResourceOptions::default(), Default::default())?;
    /// resource.write_all(b"a resource")?;
    /// resource.try_finish()?;
    /// drop(resource);
    /// let bytes = container.finish().map_err(|failure| failure.error)?;
    /// assert_eq!(&bytes[..4], &[0x91, 0x0a, 0x42, 0x52]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn framed_writer<W: Write>(
        &mut self,
        writer: W,
        config: FramingConfig,
    ) -> Result<FramedWriter<'_, W>, FramingError> {
        Ok(FramedWriter {
            compressor: self,
            core: core::Container::new(writer, config)?,
        })
    }
}

impl<W: Write> FramedWriter<'_, W> {
    /// Starts a Brotli resource. Input is buffered at most one chunk at a time.
    ///
    /// # Errors
    /// Rejects invalid chunk order, exhausted limits, or invalid stream settings;
    /// propagates a pending sink error before starting the new resource.
    pub fn resource(
        &mut self,
        options: ResourceOptions,
        stream: StreamConfig,
    ) -> Result<ResourceWriter<'_, 'static, W>, FramingError> {
        if stream.stream_offset() != 0 {
            return Err(FramingError::Invalid(
                "a new resource requires a stream header; its offset must be zero",
            ));
        }
        self.core.begin()?;
        let references =
            if self.compressor.config().window().encoding() == super::WindowEncoding::Large {
                vec![0]
            } else {
                Vec::new()
            };
        let session = self.compressor.start(stream)?;
        Ok(ResourceWriter::new(
            &mut self.core,
            Some(session),
            options,
            references,
        ))
    }

    /// Starts a Shared Brotli resource with explicit out-of-band references.
    ///
    /// References must describe the bytes used to prepare `dictionary`, in the
    /// same order. Identifiers are caller-supplied; no resolution or hashing runs.
    ///
    /// # Errors
    /// As [`Self::resource`], plus malformed, forward, or excessive references.
    pub fn resource_with_dictionary<'a, 'd>(
        &'a mut self,
        options: ResourceOptions,
        stream: StreamConfig,
        dictionary: &'d PreparedDictionary,
        references: &[DictionaryReference],
    ) -> Result<ResourceWriter<'a, 'd, W>, FramingError> {
        if stream.stream_offset() != 0 {
            return Err(FramingError::Invalid(
                "a new resource requires a stream header; its offset must be zero",
            ));
        }
        let encoded = self.core.references(references)?;
        self.core.begin()?;
        let session = self.compressor.start_with_dictionary(dictionary, stream)?;
        Ok(ResourceWriter::new(
            &mut self.core,
            Some(session),
            options,
            encoded,
        ))
    }

    /// Starts an uncompressed resource with the same bounded, retryable writer.
    ///
    /// # Errors
    /// Rejects invalid order or resource limits, or a pending sink error.
    pub fn uncompressed_resource(
        &mut self,
        options: ResourceOptions,
    ) -> Result<ResourceWriter<'_, 'static, W>, FramingError> {
        self.core.begin()?;
        Ok(ResourceWriter::new(
            &mut self.core,
            None,
            options,
            Vec::new(),
        ))
    }

    /// Queues validated, uncompressed metadata. Fields are copied once.
    ///
    /// # Errors
    /// Rejects malformed fields, duplicate reserved codes, invalid order or limits.
    pub fn metadata(
        &mut self,
        kind: MetadataKind,
        fields: &[MetadataField<'_>],
    ) -> Result<(), FramingError> {
        self.core.metadata(kind, fields)
    }

    /// Queues a single padding chunk with `bytes` zero content bytes.
    ///
    /// # Errors
    /// Rejects exhausted buffer/chunk limits or a pending sink error.
    pub fn padding(&mut self, bytes: usize) -> Result<(), FramingError> {
        self.core.padding(bytes)
    }

    /// Drains queued bytes and flushes the sink; retry after a recoverable error.
    ///
    /// # Errors
    /// Reports sink errors while retaining the unwritten suffix.
    pub fn flush(&mut self) -> Result<(), FramingError> {
        self.core.drain()?;
        self.core.writer.flush()?;
        Ok(())
    }

    /// Finalizes repeats, directory and footer, retaining progress on sink errors.
    ///
    /// # Errors
    /// Rejects an unfinished/abandoned resource or reports a retryable sink error.
    pub fn try_finish(&mut self) -> Result<(), FramingError> {
        self.core.finish()
    }

    /// Finalizes and returns the sink, or preserves this writer for retry.
    ///
    /// # Errors
    /// As [`Self::try_finish`].
    pub fn finish(mut self) -> Result<W, Box<FramingFinishError<Self>>> {
        match self.try_finish() {
            Ok(()) => Ok(self.core.writer),
            Err(error) => Err(Box::new(FramingFinishError {
                writer: self,
                error,
            })),
        }
    }

    /// Borrows the sink, for inspection.
    pub const fn get_ref(&self) -> &W {
        &self.core.writer
    }

    /// Borrows the sink to repair an I/O failure. Do not write bytes through it.
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.core.writer
    }

    /// Returns the offset at which the next queued chunk will start.
    /// Record it immediately before a resource or metadata call to reference
    /// that content from a later resource.
    pub const fn next_chunk_offset(&self) -> u64 {
        self.core.offset()
    }

    /// Abandons the container and returns the sink. No I/O is performed.
    pub fn into_inner(self) -> W {
        self.core.writer
    }
}

/// One bounded resource stream. Finish explicitly before starting another.
#[derive(Debug)]
pub struct ResourceWriter<'a, 'd, W> {
    inner: core::Resource<'a, 'd, W>,
}

impl<'a, 'd, W: Write> ResourceWriter<'a, 'd, W> {
    fn new(
        core: &'a mut core::Container<W>,
        session: Option<EncoderSession<'a, 'd>>,
        options: ResourceOptions,
        references: Vec<u8>,
    ) -> Self {
        Self {
            inner: core::Resource::new(core, session, options, references),
        }
    }

    /// Finishes the resource and drains its last chunk. Safe to retry after I/O errors.
    ///
    /// # Errors
    /// Reports encoding/limit failures or a sink error with its suffix retained.
    pub fn try_finish(&mut self) -> Result<(), FramingError> {
        self.inner.try_finish()
    }

    /// Borrows the sink to repair a failure. Do not insert container bytes.
    pub fn get_mut(&mut self) -> &mut W {
        self.inner.get_mut()
    }
}

impl<W: Write> Write for ResourceWriter<'_, '_, W> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        self.inner.write(input)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
