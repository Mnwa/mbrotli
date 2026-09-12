use super::*;
use crate::dictionary::PreparedDictionary;
use crate::{Operation, StreamConfig};

/// Container output operation; payload is supplied through a resource guard.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FramedEncodeOperation {
    /// Drain queued output and accept further structural commands.
    #[default]
    Process,
    /// Generate and deliver repeats, directory and footer exactly once.
    Finish,
}
/// Required next action after a native encoding call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FramedEncoderStatus {
    /// Queue is empty; more input or another command may be accepted.
    NeedsInput,
    /// Pending wire bytes need destination space.
    NeedsOutput,
    /// This resource or container is fully encoded and delivered.
    Finished,
}
/// Exact per-call payload acceptance and wire output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FramedEncodeProgress {
    /// Payload accepted; always zero for container `process`.
    pub consumed: usize,
    /// Initialized destination prefix length.
    pub produced: usize,
    /// Next action required.
    pub status: FramedEncoderStatus,
}
/// Exclusive container operation. Drop cancels without I/O and releases the owner.
///
/// A forgotten session requires explicit owner recovery.
/// ```compile_fail
/// use mbrotli::framing::*;
/// let mut owner = FramedCompressor::new(Default::default()).unwrap();
/// let first = owner.start(Default::default()).unwrap();
/// let second = owner.start(Default::default()).unwrap();
/// drop(first);
/// ```
#[derive(Debug)]
pub struct FramedEncoderSession<'c> {
    pub(super) owner: &'c mut FramedCompressor,
}
impl FramedEncoderSession<'_> {
    /// Drains the header/pending command, or incrementally finalizes the container.
    /// # Errors
    /// Process errors are terminal and carry exact per-call progress. Command errors
    /// before commit, including `OutputPending`, leave that command unaccepted.
    pub fn process(
        &mut self,
        output: &mut [u8],
        operation: FramedEncodeOperation,
    ) -> Result<FramedEncodeProgress, FramedEncodeFailure> {
        self.owner.engine.process(output, operation)
    }
    /// Queues uncompressed ordered metadata.
    /// # Errors
    /// Rejects pending output, ordering, invalid fields and exhausted budgets before commit.
    pub fn metadata(
        &mut self,
        kind: MetadataKind,
        fields: &[MetadataField<'_>],
    ) -> Result<(), FramedEncodeError> {
        self.metadata_with_options(kind, fields, Default::default())
    }
    /// Queues independently encoded original and repeated metadata.
    /// # Errors
    /// Rejects invalid commands before commit; dictionary is borrowed only during this call.
    pub fn metadata_with_options(
        &mut self,
        kind: MetadataKind,
        fields: &[MetadataField<'_>],
        options: MetadataOptions<'_>,
    ) -> Result<(), FramedEncodeError> {
        self.owner
            .engine
            .metadata(&mut self.owner.raw, kind, fields, options)
    }
    /// Selects repeated fields before the first metadata command.
    /// # Errors
    /// Rejects pending output, disabled repetition, invalid codes or a late selection.
    pub fn repeat_metadata_fields(&mut self, codes: &[[u8; 2]]) -> Result<(), FramedEncodeError> {
        self.owner.engine.repeat(codes)
    }
    /// Queues one padding command without accepting payload.
    /// # Errors
    /// Rejects pending output, invalid state or budgets before commit.
    pub fn padding(&mut self, bytes: usize) -> Result<(), FramedEncodeError> {
        self.owner.engine.padding(bytes)
    }
    /// Starts a compressed resource; Finish and drop it before the next command.
    /// # Errors
    /// Rejects pending output, invalid ordering/settings or exhausted budgets.
    pub fn resource(
        &mut self,
        options: ResourceOptions,
        stream: StreamConfig,
    ) -> Result<FramedResourceSession<'_, 'static>, FramedEncodeError> {
        self.owner.engine.begin_resource(
            &mut self.owner.raw,
            options,
            stream,
            ResourceEncoding::Brotli,
        )?;
        Ok(FramedResourceSession {
            owner: self.owner,
            dictionary: None,
        })
    }
    /// Starts a resource borrowing an explicit prepared dictionary.
    /// # Errors
    /// As `resource`, plus invalid references or unsupported dictionary settings.
    /// # Examples
    /// The dictionary lives only for one resource, and dies before the container finishes.
    /// ```
    /// use mbrotli::{Operation, dictionary::DictionaryBuilder, framing::*};
    /// let mut owner = FramedCompressor::new(Default::default())?;
    /// let mut session = owner.start(Default::default())?;
    /// let mut output = [0; 32];
    /// let mut wire = Vec::new();
    /// loop {
    ///     let p = session.process(&mut output, FramedEncodeOperation::Process)?;
    ///     wire.extend_from_slice(&output[..p.produced]);
    ///     if p.status == FramedEncoderStatus::NeedsInput { break; }
    /// }
    /// {
    ///     let dictionary = DictionaryBuilder::new().add_prefix(&b"shared words"[..]).build()?;
    ///     let references = [DictionaryReference::PrefixId(DictionaryId([7; 32]))];
    ///     let mut resource = session.resource_with_dictionary(
    ///         Default::default(), Default::default(), &dictionary, &references,
    ///     )?;
    ///     let mut input = &b"shared words shared words"[..];
    ///     loop {
    ///         let p = resource.process(input, &mut output, Operation::Finish)?;
    ///         input = &input[p.consumed..];
    ///         wire.extend_from_slice(&output[..p.produced]);
    ///         if p.status == FramedEncoderStatus::Finished { break; }
    ///     }
    /// } // Finished resource guard, then dictionary, are destroyed here.
    /// loop {
    ///     let p = session.process(&mut output, FramedEncodeOperation::Finish)?;
    ///     wire.extend_from_slice(&output[..p.produced]);
    ///     if p.status == FramedEncoderStatus::Finished { break; }
    /// }
    /// assert_eq!(session.resources_encoded(), 1);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn resource_with_dictionary<'s, 'dict>(
        &'s mut self,
        options: ResourceOptions,
        stream: StreamConfig,
        dictionary: &'dict PreparedDictionary,
        references: &[DictionaryReference],
    ) -> Result<FramedResourceSession<'s, 'dict>, FramedEncodeError> {
        self.owner.engine.begin_resource(
            &mut self.owner.raw,
            options,
            stream,
            ResourceEncoding::Shared {
                dictionary,
                references,
            },
        )?;
        Ok(FramedResourceSession {
            owner: self.owner,
            dictionary: Some(dictionary),
        })
    }
    /// Starts a verbatim resource using the same chunk and lifecycle rules.
    /// # Errors
    /// Rejects pending output, invalid ordering or exhausted budgets.
    pub fn uncompressed_resource(
        &mut self,
        options: ResourceOptions,
    ) -> Result<FramedResourceSession<'_, 'static>, FramedEncodeError> {
        self.owner.engine.begin_resource(
            &mut self.owner.raw,
            options,
            Default::default(),
            ResourceEncoding::Uncompressed,
        )?;
        Ok(FramedResourceSession {
            owner: self.owner,
            dictionary: None,
        })
    }
    /// Accepted payload across resources, including hidden resources.
    pub const fn total_in(&self) -> u64 {
        self.owner.engine.total_in
    }
    /// Wire bytes delivered to callers, relative to this container's start.
    pub const fn total_out(&self) -> u64 {
        self.owner.engine.total_out
    }
    /// Resources whose final chunks have been completely delivered.
    pub const fn resources_encoded(&self) -> u64 {
        self.owner.engine.resources()
    }
    /// Whether the suffix was generated and fully delivered.
    pub const fn is_finished(&self) -> bool {
        self.owner.engine.finished()
    }
    /// Queued offset of the next chunk, independent of destination transport.
    pub const fn next_chunk_offset(&self) -> u64 {
        self.owner.engine.offset()
    }
}
impl Drop for FramedEncoderSession<'_> {
    fn drop(&mut self) {
        self.owner.cancel();
    }
}
/// Exclusive byte-input resource guard. An unfinished drop abandons the container.
///
/// The dictionary cannot be destroyed while its resource guard is live:
/// ```compile_fail
/// use mbrotli::framing::*;
/// let mut owner = FramedCompressor::new(Default::default()).unwrap();
/// let mut session = owner.start(Default::default()).unwrap();
/// session.process(&mut [0; 5], FramedEncodeOperation::Process).unwrap();
/// let dictionary = mbrotli::dictionary::DictionaryBuilder::new().add_prefix(&b"prefix"[..]).build().unwrap();
/// let resource = session.resource_with_dictionary(Default::default(), Default::default(), &dictionary,
///     &[DictionaryReference::PrefixId(DictionaryId([0; 32]))]).unwrap();
/// drop(dictionary);
/// drop(resource);
/// ```
#[derive(Debug)]
pub struct FramedResourceSession<'session, 'dict> {
    pub(super) owner: &'session mut FramedCompressor,
    dictionary: Option<&'dict PreparedDictionary>,
}
impl FramedResourceSession<'_, '_> {
    /// Accepts payload and emits bounded framing chunks. Flush/Finish calls must
    /// retain their operation and remaining input suffix until completed.
    /// # Errors
    /// Terminal failures retain exact per-call progress; later calls return InvalidState.
    pub fn process(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        operation: Operation,
    ) -> Result<FramedEncodeProgress, FramedEncodeFailure> {
        self.owner.engine.process_resource(
            &mut self.owner.raw,
            self.dictionary,
            input,
            output,
            operation,
        )
    }
    /// Whether the final resource chunk has been completely delivered.
    pub const fn is_finished(&self) -> bool {
        self.owner.engine.resource_finished()
    }
    /// Accepted resource payload bytes.
    pub const fn total_in(&self) -> u64 {
        self.owner.engine.resource_in()
    }
    /// Delivered wire bytes for this resource, including its chunk headers.
    pub const fn total_out(&self) -> u64 {
        self.owner.engine.resource_out()
    }
}

impl Drop for FramedResourceSession<'_, '_> {
    fn drop(&mut self) {
        self.owner.engine.release_resource(&mut self.owner.raw);
    }
}
