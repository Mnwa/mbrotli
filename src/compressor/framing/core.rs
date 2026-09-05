//! Container ordering, checked wire sizes, and durable output cursors.

use std::io::{self, Write};
mod resource;
use super::{DictionaryReference, FramingConfig, FramingError, MetadataField, MetadataKind};
use crate::compressor::core::rfc9841::varint;
pub(super) use resource::Resource;

pub(super) fn number(value: u64, output: &mut Vec<u8>) -> Result<(), FramingError> {
    varint::write(value, output).map_err(|_| FramingError::Overflow)
}

#[derive(Debug)]
struct Record {
    offset: u64,
    kind: u8,
    header: Vec<u8>,
    metadata: Vec<u8>,
}

#[derive(Debug)]
pub(super) struct Container<W> {
    pub(super) writer: W,
    pub(super) config: FramingConfig,
    pending: Vec<u8>,
    cursor: usize,
    offset: u64,
    chunks: u64,
    pub(super) resources: u64,
    pub(super) active: bool,
    pub(super) after_resource: bool,
    metadata_pending: bool,
    metadata_bytes: usize,
    records: Vec<Record>,
    retained: usize,
    finishing: bool,
    repeat_cursor: usize,
    repeat_offset: u64,
    directory_offset: u64,
    directory_done: bool,
    finished: bool,
}

impl<W: Write> Container<W> {
    pub(super) const fn offset(&self) -> u64 {
        self.offset
    }
    pub(super) fn new(writer: W, config: FramingConfig) -> Result<Self, FramingError> {
        if !config.container && (config.central_directory || config.repeat_metadata) {
            return Err(FramingError::Invalid(
                "a single-resource profile cannot contain a directory or repeated metadata",
            ));
        }
        if config.repeat_metadata && !config.central_directory {
            return Err(FramingError::Invalid(
                "repeat metadata requires a central directory",
            ));
        }
        if config.chunk_bytes == 0
            || config.chunk_bytes > (1 << 24)
            || config.max_buffer_bytes < config.chunk_bytes.saturating_mul(4).saturating_add(8192)
        {
            return Err(FramingError::Limit(
                "chunk size must be 1..=16 MiB and fit four times plus 8 KiB in the buffer budget",
            ));
        }
        Ok(Self {
            writer,
            config,
            pending: vec![0x91, 0x0a, 0x42, 0x52, if config.container { 4 } else { 0 }],
            cursor: 0,
            offset: 5,
            chunks: 0,
            resources: 0,
            active: false,
            after_resource: false,
            metadata_pending: false,
            metadata_bytes: 0,
            records: Vec::new(),
            retained: 0,
            finishing: false,
            repeat_cursor: 0,
            repeat_offset: 0,
            directory_offset: 0,
            directory_done: false,
            finished: false,
        })
    }

    pub(super) fn check_buffer(&self, extra: usize) -> Result<(), FramingError> {
        if self.retained.saturating_add(extra) > self.config.max_buffer_bytes {
            Err(FramingError::Limit("retained framing bytes"))
        } else {
            Ok(())
        }
    }

    pub(super) fn drain(&mut self) -> Result<(), FramingError> {
        while self.cursor < self.pending.len() {
            match self.writer.write(&self.pending[self.cursor..]) {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero).into()),
                Ok(count) if count <= self.pending.len() - self.cursor => self.cursor += count,
                Ok(_) => return Err(io::Error::other("sink reported an oversized write").into()),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        self.pending.clear();
        self.cursor = 0;
        Ok(())
    }

    fn idle(&self) -> Result<(), FramingError> {
        if self.active || self.finishing {
            return Err(FramingError::Invalid(
                "resource is unfinished, abandoned, or container is finishing",
            ));
        }
        Ok(())
    }

    pub(super) fn begin(&mut self) -> Result<(), FramingError> {
        self.idle()?;
        if self.chunks >= self.config.max_chunks {
            return Err(FramingError::Limit("chunk count"));
        }
        if self.resources >= self.config.max_resources
            || (!self.config.container && self.resources != 0)
        {
            return Err(FramingError::Limit("resource count"));
        }
        self.check_buffer(
            self.config
                .chunk_bytes
                .saturating_mul(4)
                .saturating_add(8192),
        )?;
        self.drain()?;
        Ok(())
    }

    pub(super) fn queue(
        &mut self,
        header: Vec<u8>,
        content: &[u8],
        record: bool,
    ) -> Result<(), FramingError> {
        if !self.pending.is_empty() {
            return Err(FramingError::Invalid("pending chunk must be drained"));
        }
        if self.chunks >= self.config.max_chunks {
            return Err(FramingError::Limit("chunk count"));
        }
        let length = header
            .len()
            .checked_add(content.len())
            .ok_or(FramingError::Overflow)?;
        let total = length
            .checked_add(varint::encoded_len(length as u64))
            .ok_or(FramingError::Overflow)?;
        let next_offset = self
            .offset
            .checked_add(total as u64)
            .filter(|v| *v <= varint::MAX_VARINT)
            .ok_or(FramingError::Overflow)?;
        let kind = header[0];
        let metadata = self.config.repeat_metadata && matches!(kind, 1 | 6);
        let record_capacity = if record && self.records.len() == self.records.capacity() {
            self.records.capacity().saturating_mul(2).max(4)
        } else {
            self.records.capacity()
        };
        let record_bytes = if record {
            (record_capacity - self.records.capacity()).saturating_mul(size_of::<Record>())
                + header.len()
                + 9
                + if metadata { content.len() } else { 0 }
        } else {
            0
        };
        self.check_buffer(
            total
                .saturating_mul(2)
                .saturating_add(self.pending.capacity())
                .saturating_add(record_bytes)
                .saturating_add(self.config.chunk_bytes * 2),
        )?;
        let mut complete_header = Vec::with_capacity(header.len() + 9);
        number(length as u64, &mut complete_header)?;
        complete_header.extend_from_slice(&header);
        self.pending = Vec::with_capacity(total);
        self.pending.extend_from_slice(&complete_header);
        self.pending.extend_from_slice(content);
        if record {
            if record_capacity > self.records.capacity() {
                self.records
                    .reserve_exact(record_capacity - self.records.len());
            }
            self.records.push(Record {
                offset: self.offset,
                kind,
                header: complete_header,
                metadata: if metadata {
                    content.to_vec()
                } else {
                    Vec::new()
                },
            });
            self.retained += record_bytes;
        }
        self.offset = next_offset;
        self.chunks += 1;
        Ok(())
    }

    pub(super) fn references(
        &self,
        references: &[DictionaryReference],
    ) -> Result<Vec<u8>, FramingError> {
        if references.is_empty() || references.len() > 16 {
            return Err(FramingError::Invalid(
                "shared chunks require 1..=16 dictionary references",
            ));
        }
        let mut serialized = 0;
        let mut prefixes = 0;
        let mut bytes = vec![references.len() as u8];
        for reference in references {
            let (flag, id, pointer) = match *reference {
                DictionaryReference::PrefixId(id) => (2, Some(id), None),
                DictionaryReference::SerializedId(id) => (6, Some(id), None),
                DictionaryReference::PrefixResource(offset) => (0, None, Some(offset)),
                DictionaryReference::SerializedResource(offset) => (4, None, Some(offset)),
                DictionaryReference::PrefixChunk(offset) => (1, None, Some(offset)),
            };
            if flag & 4 != 0 {
                serialized += 1;
            } else {
                prefixes += 1;
            }
            if serialized > 1 || prefixes > 15 {
                return Err(FramingError::Invalid(
                    "at most one serialized and fifteen prefix references",
                ));
            }
            bytes.push(flag);
            if let Some(id) = id {
                bytes.push(3);
                bytes.extend_from_slice(&id.0);
            }
            if let Some(offset) = pointer {
                let valid = self
                    .records
                    .iter()
                    .any(|r| r.offset == offset && (flag & 3 == 1 || matches!(r.kind, 2 | 3)));
                if !valid {
                    return Err(FramingError::Invalid(
                        "dictionary pointer must address an earlier content chunk or resource",
                    ));
                }
                number(offset, &mut bytes)?;
            }
        }
        Ok(bytes)
    }

    pub(super) fn metadata(
        &mut self,
        kind: MetadataKind,
        fields: &[MetadataField<'_>],
    ) -> Result<(), FramingError> {
        self.idle()?;
        if !self.config.container {
            return Err(FramingError::Invalid(
                "metadata requires a container footer",
            ));
        }
        if self.metadata_pending || (kind == MetadataKind::Footer && !self.after_resource) {
            return Err(FramingError::Invalid(
                "metadata is not adjacent to its resource",
            ));
        }
        let mut length = 0usize;
        let mut name = false;
        let mut modified = false;
        for field in fields {
            let valid = if field.code.iter().all(u8::is_ascii_uppercase) {
                true
            } else if kind == MetadataKind::Resource && field.code == *b"id" && !name {
                name = true;
                std::str::from_utf8(field.value).is_ok()
            } else if kind == MetadataKind::Resource && field.code == *b"mt" && !modified {
                modified = true;
                field.value.len() == 8
            } else {
                false
            };
            if !valid {
                return Err(FramingError::Invalid(
                    "invalid or duplicate reserved metadata field",
                ));
            }
            length = length
                .checked_add(2 + varint::encoded_len(field.value.len() as u64))
                .and_then(|v| v.checked_add(field.value.len()))
                .ok_or(FramingError::Overflow)?;
        }
        let total = self
            .metadata_bytes
            .checked_add(length)
            .ok_or(FramingError::Overflow)?;
        if total > self.config.max_metadata_bytes {
            return Err(FramingError::Limit("metadata bytes"));
        }
        self.check_buffer(length.saturating_mul(4).saturating_add(8192))?;
        self.drain()?;
        let mut content = Vec::with_capacity(length);
        for field in fields {
            content.extend_from_slice(&field.code);
            number(field.value.len() as u64, &mut content)?;
            content.extend_from_slice(field.value);
        }
        let code = match kind {
            MetadataKind::Resource => 1,
            MetadataKind::Footer => 6,
            MetadataKind::Global => 7,
        };
        self.queue(vec![code, 0], &content, true)?;
        self.metadata_bytes = total;
        self.metadata_pending = kind == MetadataKind::Resource;
        self.after_resource = false;
        Ok(())
    }

    pub(super) fn padding(&mut self, bytes: usize) -> Result<(), FramingError> {
        self.idle()?;
        self.check_buffer(bytes.saturating_mul(3).saturating_add(8192))?;
        self.drain()?;
        self.queue(vec![0], &vec![0; bytes], false)
    }

    pub(super) fn finish(&mut self) -> Result<(), FramingError> {
        if self.active || self.metadata_pending {
            return Err(FramingError::Invalid(
                "resource is missing, unfinished, or abandoned",
            ));
        }
        if !self.config.container && self.resources != 1 {
            return Err(FramingError::Invalid(
                "single-resource profile requires exactly one resource",
            ));
        }
        self.finishing = true;
        self.drain()?;
        if self.config.container && !self.finished {
            if self.config.repeat_metadata {
                while self.repeat_cursor < self.records.len() {
                    let record = &self.records[self.repeat_cursor];
                    if matches!(record.kind, 1 | 6) {
                        self.check_buffer(
                            record.metadata.len().saturating_mul(3).saturating_add(8192),
                        )?;
                        let header = vec![8, 0, record.kind];
                        let content = record.metadata.clone();
                        let offset = self.offset;
                        self.queue(header, &content, false)?;
                        if self.repeat_offset == 0 {
                            self.repeat_offset = offset;
                        }
                    }
                    self.repeat_cursor += 1;
                    self.drain()?;
                }
            }
            if self.config.central_directory && !self.directory_done {
                let bound = self
                    .records
                    .iter()
                    .try_fold(9usize, |sum, r| sum.checked_add(18 + r.header.len()))
                    .ok_or(FramingError::Overflow)?;
                self.check_buffer(bound.saturating_mul(3).saturating_add(8192))?;
                let mut content = Vec::with_capacity(bound);
                number(self.repeat_offset, &mut content)?;
                for record in &self.records {
                    number(record.offset, &mut content)?;
                    number(record.header.len() as u64, &mut content)?;
                    content.extend_from_slice(&record.header);
                }
                let offset = self.offset;
                self.queue(vec![9], &content, false)?;
                self.directory_offset = offset;
                self.directory_done = true;
                self.drain()?;
            }
            let content = footer(self.offset, self.directory_offset)?;
            self.queue(vec![10], &content, false)?;
            self.finished = true;
            self.drain()?;
        }
        self.finished = true;
        self.writer.flush()?;
        Ok(())
    }
}

/// The file-size field counts its own bytes and the enclosing length varint.
fn footer(offset: u64, directory: u64) -> Result<Vec<u8>, FramingError> {
    let mut size = offset;
    loop {
        let length = 1 + varint::encoded_len(size) + varint::encoded_len(directory);
        let next = offset
            .checked_add((length + varint::encoded_len(length as u64)) as u64)
            .filter(|v| *v <= varint::MAX_VARINT)
            .ok_or(FramingError::Overflow)?;
        if size == next {
            break;
        }
        size = next;
    }
    let mut bytes = Vec::new();
    number(size, &mut bytes)?;
    bytes.reverse();
    let start = bytes.len();
    number(directory, &mut bytes)?;
    bytes[start..].reverse();
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_size_converges_across_varint_width_boundaries() {
        for offset in [0, 120, 127, 128, 16375, 16383, 16384, (1 << 56) - 10] {
            let content = footer(offset, 5).expect("footer");
            let mut reversed = content.clone();
            reversed.reverse();
            let (directory, used) = varint::read(&reversed).expect("directory");
            let (size, _) = varint::read(&reversed[used..]).expect("size");
            assert_eq!(directory, 5);
            let length = content.len() + 1;
            assert_eq!(
                size,
                offset + (length + varint::encoded_len(length as u64)) as u64
            );
        }
        assert!(matches!(
            footer(varint::MAX_VARINT, 0),
            Err(FramingError::Overflow)
        ));
        assert!(matches!(
            number(u64::MAX, &mut Vec::new()),
            Err(FramingError::Overflow)
        ));
    }

    #[test]
    fn chunk_limits_and_offset_overflow_fail_without_queuing() {
        let mut container = Container::new(
            Vec::new(),
            FramingConfig {
                max_chunks: 0,
                ..Default::default()
            },
        )
        .expect("container");
        assert!(matches!(
            container.begin(),
            Err(FramingError::Limit("chunk count"))
        ));
        container.drain().expect("header");
        container.config.max_chunks = 1;
        container.offset = varint::MAX_VARINT;
        assert!(matches!(
            container.queue(vec![0], b"x", false),
            Err(FramingError::Overflow)
        ));
        assert!(container.pending.is_empty());
        assert_eq!(container.chunks, 0);
    }
}
