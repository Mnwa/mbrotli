//! Resource compression and bounded partial-chunk emission.
use super::super::{FramingError, ResourceOptions};
use crate::compressor::{EncoderSession, EncoderStatus, Operation};
use std::io::{self, Write};

/// One bounded resource stream. Finish explicitly before starting another.
#[derive(Debug)]
pub(in crate::compressor::framing) struct Resource<'a, 'd, W> {
    core: &'a mut super::Container<W>,
    session: Option<EncoderSession<'a, 'd>>,
    options: ResourceOptions,
    references: Vec<u8>,
    input: Vec<u8>,
    first: bool,
    finished: bool,
    failed: bool,
}

impl<'a, 'd, W: Write> Resource<'a, 'd, W> {
    pub(in crate::compressor::framing) fn new(
        core: &'a mut super::Container<W>,
        session: Option<EncoderSession<'a, 'd>>,
        options: ResourceOptions,
        references: Vec<u8>,
    ) -> Self {
        core.active = true;
        core.after_resource = false;
        core.metadata_pending = false;
        let input = Vec::with_capacity(core.config.chunk_bytes);
        Self {
            core,
            session,
            options,
            references,
            input,
            first: true,
            finished: false,
            failed: false,
        }
    }

    fn emit(&mut self, last: bool) -> Result<(), FramingError> {
        self.core.drain()?;
        if self.failed {
            return Err(FramingError::Invalid("resource encoding previously failed"));
        }
        self.core.check_buffer(
            self.core
                .config
                .chunk_bytes
                .saturating_mul(4)
                .saturating_add(8192),
        )?;
        if self.core.chunks >= self.core.config.max_chunks {
            return Err(FramingError::Limit("chunk count"));
        }
        self.failed = true;
        let mut content =
            Vec::with_capacity(self.input.len().saturating_mul(2).saturating_add(1024));
        if let Some(session) = &mut self.session {
            let mut remaining = self.input.as_slice();
            let mut output = [0u8; 8192];
            loop {
                let progress = session.process(
                    remaining,
                    &mut output,
                    if last {
                        Operation::Finish
                    } else {
                        Operation::Flush
                    },
                )?;
                remaining = &remaining[progress.consumed..];
                self.core.check_buffer(
                    content
                        .len()
                        .saturating_add(progress.produced)
                        .saturating_add(self.input.capacity()),
                )?;
                content.extend_from_slice(&output[..progress.produced]);
                if matches!(
                    progress.status,
                    EncoderStatus::Finished | EncoderStatus::NeedsInput
                ) {
                    break;
                }
                if progress.consumed == 0 && progress.produced == 0 {
                    return Err(FramingError::Invalid("encoder made no progress"));
                }
            }
        } else {
            content.extend_from_slice(&self.input);
        }
        let kind = match (self.first, last) {
            (true, true) => 2,
            (true, false) => 3,
            (false, false) => 4,
            (false, true) => 5,
        };
        let codec = if self.session.is_none() {
            0
        } else if !self.first {
            1
        } else if self.references.is_empty() {
            2
        } else {
            3
        };
        let mut header = vec![kind, codec];
        if codec != 0 {
            super::number(self.input.len() as u64, &mut header)?;
        }
        if codec == 3 {
            header.extend_from_slice(&self.references);
        }
        header.push(
            u8::from(self.first && self.options.hidden)
                | (u8::from(last && self.options.id.is_some()) << 1),
        );
        if last && let Some(id) = self.options.id {
            header.push(3);
            header.extend_from_slice(&id.0);
        }
        self.core.queue(header, &content, true)?;
        self.failed = false;
        self.input.clear();
        self.first = false;
        if last {
            self.finished = true;
            self.core.active = false;
            self.core.after_resource = true;
            self.core.resources += 1;
        }
        Ok(())
    }

    /// Finishes the resource and drains its last chunk. Safe to retry after I/O errors.
    ///
    /// # Errors
    /// Returns a retained sink error, an encoding failure, or a resource limit.
    pub fn try_finish(&mut self) -> Result<(), FramingError> {
        if !self.finished {
            self.emit(true)?;
        }
        self.core.drain()
    }

    /// Borrows the sink to repair an I/O failure; do not insert container bytes.
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.core.writer
    }
}

impl<W: Write> Write for Resource<'_, '_, W> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if input.is_empty() {
            return Ok(0);
        }
        if self.finished || self.failed {
            return Err(io::Error::other("resource is finished"));
        }
        self.core.drain().map_err(io::Error::from)?;
        if self.input.len() == self.core.config.chunk_bytes {
            self.emit(false).map_err(io::Error::from)?;
            self.core.drain().map_err(io::Error::from)?;
        }
        let count = input
            .len()
            .min(self.core.config.chunk_bytes - self.input.len());
        self.input.extend_from_slice(&input[..count]);
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        if !self.input.is_empty() && !self.finished {
            self.emit(false).map_err(io::Error::from)?;
        }
        self.core.drain().map_err(io::Error::from)?;
        self.core.writer.flush()
    }
}
