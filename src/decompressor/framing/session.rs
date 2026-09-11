use super::{core::Tag, *};
use crate::DecodeOperation;

/// Exact progress and at most one semantic event from the current call.
#[derive(Debug)]
pub struct FramedDecodeProgress<'a> {
    /// Accepted input prefix.
    pub consumed: usize,
    /// Delivered resource payload prefix.
    pub produced: usize,
    /// Event or backpressure reason.
    pub status: FramedDecoderStatus<'a>,
}
/// Incremental progress reason.
#[derive(Debug)]
pub enum FramedDecoderStatus<'a> {
    /// All offered input was accepted; supply more or declare EOF.
    NeedsInput,
    /// The next payload byte requires output space.
    NeedsOutput,
    /// Exactly one semantic event.
    Event(FramedEvent<'a>),
    /// Complete validation, with zero progress; subsequent calls remain finished.
    Finished,
}
/// Exclusive operation. External borrows live only here; Drop cancels without I/O.
///
/// Forgetting a session protects the owner until `recover` or `reconfigure`.
/// Borrowed events prevent another mutating call until their last use.
///
/// ```compile_fail
/// use mbrotli::framing::{FramedDecompressor, FramedDecodeConfig, InputMode};
/// use mbrotli::DecodeOperation;
/// let mut decoder = FramedDecompressor::new(
///     FramedDecodeConfig::default().with_input_mode(InputMode::Auto)).unwrap();
/// let mut session = decoder.start(Default::default()).unwrap();
/// let mut output = [0; 1];
/// let event = session.process(&[0x3b], &mut output, DecodeOperation::Finish).unwrap();
/// session.process(&[0x3b], &mut [], DecodeOperation::Finish).unwrap();
/// println!("{event:?}"); // The first borrow is still live.
/// ```
pub struct FramedDecoderSession<'d, 'dict> {
    pub(super) owner: &'d mut FramedDecompressor,
    pub(super) resolver: Option<DictionaryResolverRef<'dict>>,
}
impl FramedDecoderSession<'_, '_> {
    /// Advances one object without retaining input or output references.
    /// After the first `Finish`, reoffer precisely the remaining suffix with
    /// `Finish` on every call. Output events borrow `output[..produced]`.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::framing::*;
    /// use mbrotli::DecodeOperation;
    /// let bytes = [0x91, 10, 66, 82, 0, 6, 2, 0, 0, b'a', b'b', b'c'];
    /// let mut decoder = FramedDecompressor::new(Default::default())?;
    /// let mut session = decoder.start(Default::default())?;
    /// let mut remaining = bytes.as_slice();
    /// let mut payload = Vec::new();
    /// loop {
    ///     let mut buffer = [0; 1];
    ///     let progress = session.process(remaining, &mut buffer, DecodeOperation::Finish)?;
    ///     remaining = &remaining[progress.consumed..];
    ///     match progress.status {
    ///         FramedDecoderStatus::Event(FramedEvent::ResourceData(data)) =>
    ///             payload.extend_from_slice(data.bytes),
    ///         FramedDecoderStatus::Finished => break,
    ///         _ => {}
    ///     }
    /// }
    /// assert_eq!(payload, b"abc");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    /// Terminal validation, policy, codec, or allocation failure with exact progress.
    /// Reusing a failed session or changing the final boundary yields `InvalidState`.
    // The by-value failure contract preserves progress even when allocation fails;
    // boxing this 128-byte record would require an allocation on the error path.
    #[expect(
        clippy::result_large_err,
        reason = "allocation-independent progress is part of the public contract"
    )]
    pub fn process<'a>(
        &'a mut self,
        input: &[u8],
        output: &'a mut [u8],
        operation: DecodeOperation,
    ) -> Result<FramedDecodeProgress<'a>, FramedDecodeFailure> {
        let (consumed, produced, tag) = self.step(input, output, operation)?;
        let status = match tag {
            Tag::Input => FramedDecoderStatus::NeedsInput,
            Tag::Output => FramedDecoderStatus::NeedsOutput,
            Tag::Finished => FramedDecoderStatus::Finished,
            _ => FramedDecoderStatus::Event(
                self.owner
                    .engine
                    .event(tag, &output[..produced])
                    .ok_or(FramedDecodeFailure {
                        error: FramedDecodeError::InvalidState,
                        consumed,
                        produced,
                        last_output: None,
                    })?,
            ),
        };
        Ok(FramedDecodeProgress {
            consumed,
            produced,
            status,
        })
    }
    // The by-value failure contract preserves progress even when allocation fails;
    // boxing this 128-byte record would require an allocation on the error path.
    #[expect(
        clippy::result_large_err,
        reason = "allocation-independent progress is part of the public contract"
    )]
    pub(super) fn step(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        operation: DecodeOperation,
    ) -> Result<(usize, usize, Tag), FramedDecodeFailure> {
        self.owner
            .engine
            .process(input, output, operation, self.owner.backend, self.resolver)
    }
    /// Accepted wire bytes, including failing calls.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::framing::*;
    /// let mut decoder = FramedDecompressor::new(Default::default())?;
    /// let session = decoder.start(Default::default())?;
    /// assert_eq!(session.total_in(), 0);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub const fn total_in(&self) -> u64 {
        self.owner.engine.total_in
    }
    /// Delivered resource bytes, including failing calls.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::framing::*;
    /// let mut decoder = FramedDecompressor::new(Default::default())?;
    /// let session = decoder.start(Default::default())?;
    /// assert_eq!(session.total_out(), 0);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub const fn total_out(&self) -> u64 {
        self.owner.engine.total_out
    }
    /// Regenerated resource and metadata bytes.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::framing::*;
    /// let mut decoder = FramedDecompressor::new(Default::default())?;
    /// let session = decoder.start(Default::default())?;
    /// assert_eq!(session.total_decoded(), 0);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub const fn total_decoded(&self) -> u64 {
        self.owner.engine.total_decoded
    }
    /// Locally completed resource payloads; not an authentication count.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::framing::*;
    /// let mut decoder = FramedDecompressor::new(Default::default())?;
    /// let session = decoder.start(Default::default())?;
    /// assert_eq!(session.resources_decoded(), 0);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub const fn resources_decoded(&self) -> u64 {
        self.owner.engine.completed
    }
    /// Whether all object validation succeeded.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::framing::*;
    /// let mut decoder = FramedDecompressor::new(Default::default())?;
    /// let session = decoder.start(Default::default())?;
    /// assert_eq!(session.is_finished(), false);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub const fn is_finished(&self) -> bool {
        self.owner.engine.finished
    }
    /// Selected input format, or `None` before detection.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::framing::*;
    /// let mut decoder = FramedDecompressor::new(Default::default())?;
    /// let session = decoder.start(Default::default())?;
    /// assert_eq!(session.input_format(), None);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub const fn input_format(&self) -> Option<StreamInfo> {
        self.owner.engine.format
    }
}
impl Drop for FramedDecoderSession<'_, '_> {
    fn drop(&mut self) {
        self.owner.cancel();
    }
}
impl ::core::fmt::Debug for FramedDecoderSession<'_, '_> {
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        f.debug_struct("FramedDecoderSession")
            .field("total_in", &self.total_in())
            .field("total_out", &self.total_out())
            .field("format", &self.input_format())
            .finish_non_exhaustive()
    }
}
