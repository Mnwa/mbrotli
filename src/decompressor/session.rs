use super::{
    DecodeError, DecodeStreamConfig, Decompressor, MemberMode, OutputSize,
    core::{Input, Output, Stop},
};
use crate::{Window, dictionary::DictionaryRef};

/// Whether more input may follow this call.
///
/// Start with [`Self::Process`] while more chunks may arrive. Once EOF is known,
/// use [`Self::Finish`] on every remaining call, advancing input by the reported
/// consumed count. See [`DecoderSession::process`] for a complete loop.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DecodeOperation {
    /// More input may follow; an empty slice is not EOF.
    #[default]
    Process,
    /// This is the final input; retries must pass the exact remaining suffix.
    Finish,
}
/// Reason an incremental decoding call stopped.
///
/// Always handle the counts in [`DecodeProgress`] before acting on the status.
/// `NeedsInput` requires more input or an EOF declaration; `NeedsOutput`
/// requires fresh output space, even if all offered input was consumed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecoderStatus {
    /// All offered input was consumed and additional input is required.
    NeedsInput,
    /// The next payload byte needs output space.
    NeedsOutput,
    /// The operation is complete and its output size has been validated.
    Finished,
}
/// Bytes accepted and delivered during one successful call.
///
/// Advance input by `consumed` and use only `output[..produced]`. These counts
/// are per call, not cumulative. See [`DecoderSession::process`] for an example.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodeProgress {
    /// Accepted prefix length of this call's input.
    pub consumed: usize,
    /// Written prefix length of this call's output.
    pub produced: usize,
    /// Reason decoding stopped.
    pub status: DecoderStatus,
}
/// Terminal codec failure with exact progress for the failing call.
///
/// Output reported by `produced` has already been written and is not rolled
/// back. It is only a partial result; the operation has failed validation.
/// Drop the session before starting another operation on the same decoder.
///
/// # Examples
///
/// ```
/// use mbrotli::{DecodeError, DecodeOperation, DecoderConfig, Decompressor};
/// let mut decoder = Decompressor::new(DecoderConfig::default())?;
/// let mut session = decoder.start(Default::default())?;
/// // Physical EOF without even one member is truncated input.
/// let failure = session.process(&[], &mut [], DecodeOperation::Finish).unwrap_err();
/// assert_eq!((failure.consumed, failure.produced), (0, 0));
/// assert!(matches!(failure.into_error(), DecodeError::UnexpectedEndOfInput));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub struct DecodeFailure {
    /// Underlying typed failure.
    #[source]
    pub error: DecodeError,
    /// Accepted input prefix, excluding all unaccepted suffix bytes.
    pub consumed: usize,
    /// Payload prefix delivered before the failure.
    pub produced: usize,
}
impl DecodeFailure {
    /// Extracts the codec error when progress has already been handled.
    pub fn into_error(self) -> DecodeError {
        self.error
    }
}

/// Exclusive incremental decoding operation. Drop clears per-stream state.
///
/// The decoder and any external dictionary remain borrowed for the session's
/// lifetime. Dropping the session permits decoder reuse and applies its
/// retention policy, even after failure or incomplete input. Forgetting it with
/// [`core::mem::forget`] requires [`Decompressor::recover`] before reuse.
/// See [`Self::process`] for a loop with a small output buffer.
#[derive(Debug)]
pub struct DecoderSession<'d, 'dict> {
    decoder: &'d mut Decompressor,
    stream: DecodeStreamConfig,
    total_in: u64,
    total_out: u64,
    members: u64,
    window: Option<Window>,
    final_end: u64,
    finishing: bool,
    boundary: bool,
    finished: bool,
    failed: bool,
    dictionary: Option<DictionaryRef<'dict>>,
}

impl<'d, 'dict> DecoderSession<'d, 'dict> {
    pub(super) fn start(
        decoder: &'d mut Decompressor,
        stream: DecodeStreamConfig,
        dictionary: Option<DictionaryRef<'dict>>,
    ) -> Result<Self, DecodeError> {
        if decoder.active {
            return Err(DecodeError::AbandonedSession);
        }
        if let (OutputSize::Exact(expected), Some(limit)) = (
            stream.output_size(),
            decoder.config.limits().max_output_bytes(),
        ) && expected > limit
        {
            return Err(DecodeError::OutputLimitExceeded { limit });
        }
        if decoder
            .config
            .limits()
            .max_workspace_bytes()
            .is_some_and(|limit| decoder.retained_bytes() > limit)
        {
            decoder.recover();
        }
        decoder.workspace.reset(decoder.config);
        decoder.active = true;
        Ok(Self {
            decoder,
            stream,
            total_in: 0,
            total_out: 0,
            members: 0,
            window: None,
            final_end: 0,
            finishing: false,
            boundary: false,
            finished: false,
            failed: false,
            dictionary,
        })
    }
}

impl DecoderSession<'_, '_> {
    /// Decodes available bytes without retaining caller slices.
    ///
    /// Deliver `output[..progress.produced]` and advance `input` by
    /// `progress.consumed` after each call. An empty input with `Process` does
    /// not declare EOF. After the first `Finish`, keep using `Finish` with
    /// exactly the unconsumed suffix, including an empty suffix when only output
    /// remains. Input bytes themselves must remain unchanged between retries.
    ///
    /// In single-member mode, `Finished` can leave a protocol suffix unconsumed.
    /// Concatenated mode needs `Finish` to confirm the final member boundary.
    /// A completed session returns zero counts and `Finished` on further calls.
    ///
    /// # Errors
    /// Returns terminal format/resource errors and exact call progress in
    /// [`DecodeFailure`]. Truncated final input yields
    /// [`DecodeError::UnexpectedEndOfInput`]. Changing the operation or final
    /// suffix length after `Finish`, or using a failed session, yields
    /// [`DecodeError::InvalidState`]. A failure cannot be retried in this session.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::{DecodeOperation, DecoderConfig, DecoderStatus, Decompressor, OutputSize};
    /// let compressed = [0x0b, 0x02, 0x80, b'h', b'e', b'l', b'l', b'o', 0x03];
    /// let mut decoder = Decompressor::new(DecoderConfig::default())?;
    /// let mut session = decoder.start(OutputSize::Exact(5).into())?;
    /// assert_eq!(session.window(), None); // No header accepted yet.
    /// let mut remaining = compressed.as_slice();
    /// let mut decoded = Vec::new();
    /// loop {
    ///     let mut buffer = [0; 2];
    ///     let progress = session.process(remaining, &mut buffer, DecodeOperation::Finish)?;
    ///     remaining = &remaining[progress.consumed..];
    ///     decoded.extend_from_slice(&buffer[..progress.produced]);
    ///     if progress.status == DecoderStatus::Finished {
    ///         break;
    ///     }
    ///     assert_eq!(progress.status, DecoderStatus::NeedsOutput);
    /// }
    /// assert_eq!(decoded, b"hello");
    /// assert!(remaining.is_empty());
    /// assert!(session.is_finished());
    /// assert_eq!(session.total_in(), compressed.len() as u64);
    /// assert_eq!(session.total_out(), 5);
    /// assert_eq!(session.members_decoded(), 1);
    /// assert!(session.window().is_some());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn process(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        operation: DecodeOperation,
    ) -> Result<DecodeProgress, DecodeFailure> {
        let invalid = |error| DecodeFailure {
            error,
            consumed: 0,
            produced: 0,
        };
        if self.failed {
            return Err(invalid(DecodeError::InvalidState));
        }
        if self.finished {
            return Ok(DecodeProgress {
                consumed: 0,
                produced: 0,
                status: DecoderStatus::Finished,
            });
        }
        let Some(end) = self.total_in.checked_add(input.len() as u64) else {
            self.failed = true;
            return Err(invalid(DecodeError::SizeOverflow));
        };
        if self.finishing {
            if operation != DecodeOperation::Finish || end != self.final_end {
                self.failed = true;
                return Err(invalid(DecodeError::InvalidState));
            }
        } else if operation == DecodeOperation::Finish {
            self.final_end = end;
            self.finishing = true;
        }
        let config = self.decoder.config;
        let limits = config.limits();
        let mut input = Input {
            bytes: input,
            consumed: 0,
            total_before: self.total_in,
            limit: limits.max_input_bytes(),
        };
        let mut output = Output {
            bytes: output,
            produced: 0,
            total_before: self.total_out,
            limit: limits.max_output_bytes(),
            exact: self.stream.output_size(),
        };
        let result = (|| loop {
            if self.boundary {
                if config.member_mode() == MemberMode::Single
                    || (self.finishing && input.consumed == input.bytes.len())
                {
                    if let OutputSize::Exact(expected) = self.stream.output_size() {
                        let actual = self.total_out + output.produced as u64;
                        if actual != expected {
                            return Err(DecodeError::OutputSizeMismatch { expected, actual });
                        }
                    }
                    self.finished = true;
                    return Ok(DecoderStatus::Finished);
                }
                if input.consumed == input.bytes.len() {
                    return Ok(DecoderStatus::NeedsInput);
                }
                self.decoder.workspace.reset(config);
                self.boundary = false;
            }
            let outcome =
                self.decoder
                    .workspace
                    .run(&mut input, &mut output, config, self.dictionary);
            if let Some(window) = self.decoder.workspace.window {
                self.window = Some(window);
            }
            match outcome? {
                Stop::Input if self.finishing => {
                    return Err(DecodeError::UnexpectedEndOfInput);
                }
                Stop::Input => return Ok(DecoderStatus::NeedsInput),
                Stop::Output => return Ok(DecoderStatus::NeedsOutput),
                Stop::Member => {
                    self.members = self
                        .members
                        .checked_add(1)
                        .ok_or(DecodeError::SizeOverflow)?;
                    self.boundary = true;
                }
            }
        })();
        self.total_in += input.consumed as u64;
        self.total_out += output.produced as u64;
        match result {
            Ok(status) => Ok(DecodeProgress {
                consumed: input.consumed,
                produced: output.produced,
                status,
            }),
            Err(error) => {
                self.failed = true;
                Err(DecodeFailure {
                    error,
                    consumed: input.consumed,
                    produced: output.produced,
                })
            }
        }
    }
    /// Whether all members and the exact-size contract were validated.
    pub const fn is_finished(&self) -> bool {
        self.finished
    }
    /// Total accepted compressed bytes, including failing calls.
    pub const fn total_in(&self) -> u64 {
        self.total_in
    }
    /// Total payload bytes delivered, including failing calls.
    pub const fn total_out(&self) -> u64 {
        self.total_out
    }
    /// Number of validated members whose output has been delivered.
    pub const fn members_decoded(&self) -> u64 {
        self.members
    }
    /// Most recently accepted window header.
    ///
    /// Returns `None` before the first header is accepted. Between concatenated
    /// members, retains the preceding member's window until a new one is accepted.
    pub const fn window(&self) -> Option<Window> {
        self.window
    }
}

impl Drop for DecoderSession<'_, '_> {
    fn drop(&mut self) {
        self.decoder.active = false;
        self.decoder.workspace.reset(self.decoder.config);
        self.decoder.trim(self.decoder.retention());
    }
}
