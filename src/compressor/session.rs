//! The incremental encoder: one stream, driven a chunk at a time.
//!
//! [`EncoderSession`] is the low-level state machine every streaming path in
//! this crate is built on. [`EncoderReader`](super::io::EncoderReader) and
//! [`EncoderWriter`](super::io::EncoderWriter) are adapters over it, and they
//! add buffering and `std::io` conventions rather than a second encoder.
//!
//! A session borrows its compressor exclusively for as long as it lives, and
//! borrows at most one dictionary immutably. It never keeps the caller's input
//! or output slices: everything it needs between calls lives in the
//! compressor's own retained buffers.
//!
//! # One-shot and streaming are not the same bytes
//!
//! The reference encoder's one-shot entry point applies two shortcuts its
//! streaming entry point does not: an empty input becomes a single byte, and a
//! stream that grew is rewritten as uncompressed meta-blocks. This crate
//! reproduces both encoders faithfully, so
//! [`Compressor::compress`](super::Compressor::compress) applies those
//! shortcuts and a session does not. For every other input, a session fed
//! [`InputSize::Exact`] with no explicit flush produces exactly the one-shot
//! bytes.

use super::dictionary::PreparedDictionary;
use super::encoder::Compressor;
use super::error::EncodeError;

/// How much input a stream will carry, when that is known in advance.
///
/// Qualities four and five choose a different match finder for inputs of a
/// mebibyte or more, so telling the encoder how much is coming changes the
/// bytes it emits. [`InputSize::Exact`] is what makes a streamed stream match
/// the same bytes compressed in one shot.
///
/// `Exact(0)` declares a stream that is known to be empty, which is a different
/// statement from `Unknown` even though the reference resolves the same match
/// finder for both.
///
/// # Examples
///
/// ```
/// use mbrotli::InputSize;
///
/// assert_eq!(InputSize::default(), InputSize::Unknown);
/// assert_eq!(InputSize::from(4096u64), InputSize::Exact(4096));
/// ```
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum InputSize {
    /// How much input is coming is not known.
    #[default]
    Unknown,
    /// The stream will carry exactly this many bytes.
    Exact(u64),
}

impl InputSize {
    /// Returns the size hint the encoders resolve their match finder from.
    ///
    /// An unknown size is zero, which is what the reference's streaming entry
    /// point leaves `BROTLI_PARAM_SIZE_HINT` at.
    pub(crate) const fn hint(self) -> usize {
        match self {
            Self::Unknown => 0,
            // A hint wider than the address space cannot select a different
            // match finder than the widest one that fits, so saturating here
            // changes no decision.
            Self::Exact(size) if size > usize::MAX as u64 => usize::MAX,
            Self::Exact(size) => size as usize,
        }
    }
}

impl From<u64> for InputSize {
    /// Declares an exactly known input size.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::InputSize;
    ///
    /// assert_eq!(InputSize::from(0u64), InputSize::Exact(0));
    /// ```
    fn from(value: u64) -> Self {
        Self::Exact(value)
    }
}

/// What a single stream knows about itself.
///
/// Everything here belongs to one stream rather than to the encoder: how much
/// input is coming, and where the stream sits logically. The encoder's own
/// settings are in [`EncoderConfig`](super::EncoderConfig).
///
/// # Examples
///
/// ```
/// use mbrotli::{InputSize, StreamConfig};
///
/// let stream = StreamConfig::from(InputSize::Exact(4096));
///
/// assert_eq!(stream.input_size(), InputSize::Exact(4096));
/// assert_eq!(stream.stream_offset(), 0);
/// assert_eq!(StreamConfig::default().input_size(), InputSize::Unknown);
/// ```
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct StreamConfig {
    /// How much input the stream will carry.
    input_size: InputSize,
    /// Where the stream begins, logically.
    stream_offset: u64,
}

impl StreamConfig {
    /// Sets how much input the stream will carry.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::{InputSize, StreamConfig};
    ///
    /// let stream = StreamConfig::default().with_input_size(InputSize::Exact(10));
    ///
    /// assert_eq!(stream.input_size(), InputSize::Exact(10));
    /// ```
    #[must_use]
    pub const fn with_input_size(mut self, input_size: InputSize) -> Self {
        self.input_size = input_size;
        self
    }

    /// Returns how much input the stream will carry.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::{InputSize, StreamConfig};
    ///
    /// assert_eq!(StreamConfig::default().input_size(), InputSize::Unknown);
    /// ```
    #[must_use]
    pub const fn input_size(&self) -> InputSize {
        self.input_size
    }

    /// Sets where the stream begins, logically.
    ///
    /// A non-zero offset requires the `experimental` feature and quality 2 or
    /// higher. It emits a headerless continuation after a byte-aligned flush,
    /// with no references to unavailable prior history. The caller must join
    /// it to a compatible stream; it is not independently decodable. Logical
    /// positions, including the input, must fit in 63 bits.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::StreamConfig;
    ///
    /// assert_eq!(StreamConfig::default().with_stream_offset(64).stream_offset(), 64);
    /// ```
    #[must_use]
    pub const fn with_stream_offset(mut self, stream_offset: u64) -> Self {
        self.stream_offset = stream_offset;
        self
    }

    /// Returns where the stream begins, logically.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::StreamConfig;
    ///
    /// assert_eq!(StreamConfig::default().stream_offset(), 0);
    /// ```
    #[must_use]
    pub const fn stream_offset(&self) -> u64 {
        self.stream_offset
    }
}

impl From<InputSize> for StreamConfig {
    /// Builds a stream configuration from its size alone, at offset zero.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::{InputSize, StreamConfig};
    ///
    /// assert_eq!(
    ///     StreamConfig::from(InputSize::Unknown),
    ///     StreamConfig::default()
    /// );
    /// ```
    fn from(value: InputSize) -> Self {
        Self {
            input_size: value,
            stream_offset: 0,
        }
    }
}

/// What a call to [`EncoderSession::process`] should do with the stream.
///
/// # Examples
///
/// ```
/// use mbrotli::Operation;
///
/// assert_eq!(Operation::default(), Operation::Process);
/// ```
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum Operation {
    /// Take input and emit whatever completes; keep gathering otherwise.
    #[default]
    Process,
    /// Make everything accepted so far decodable, without ending the stream.
    ///
    /// Costs ratio: the meta-block ends early, so its entropy codes are built
    /// from less data, and an empty metadata block is added to realign the
    /// stream to a byte boundary. Flushing per small write can make the output
    /// larger than the input; flush on the boundaries the protocol has.
    Flush,
    /// Emit everything left and terminate the stream.
    Finish,
}

/// What a session needs next.
///
/// # Examples
///
/// ```
/// use mbrotli::EncoderStatus;
///
/// assert_ne!(EncoderStatus::NeedsInput, EncoderStatus::Finished);
/// ```
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum EncoderStatus {
    /// More source bytes are needed before anything else can happen.
    NeedsInput,
    /// Encoded bytes are waiting; call again with room to put them.
    NeedsOutput,
    /// The stream is complete and the final bytes have been delivered.
    Finished,
}

/// What one [`EncoderSession::process`] call did.
///
/// `consumed` and `produced` are exact: the session never takes a byte it did
/// not stage, and never claims a byte it did not write.
///
/// # Examples
///
/// ```
/// use mbrotli::{EncoderStatus, Progress};
///
/// let progress = Progress { consumed: 4, produced: 0, status: EncoderStatus::NeedsInput };
///
/// assert_eq!(progress.consumed, 4);
/// ```
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Progress {
    /// How many bytes were taken from the caller's input.
    pub consumed: usize,
    /// How many bytes were written into the caller's output.
    pub produced: usize,
    /// What the session needs next.
    pub status: EncoderStatus,
}

/// Where a session is in the life of its stream.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Phase {
    /// Taking input.
    Open,
    /// A flush has been emitted and no input has arrived since.
    Flushed,
    /// The final meta-block has been emitted and delivered.
    Finished,
    /// An error ended the stream; nothing more may be encoded.
    Failed,
}

/// One incremental Brotli stream.
///
/// Created by [`Compressor::start`](super::Compressor::start) or
/// [`Compressor::start_with_dictionary`](super::Compressor::start_with_dictionary),
/// and driven by [`EncoderSession::process`] until it reports
/// [`EncoderStatus::Finished`].
///
/// Dropping a session before it finishes abandons the stream: the bytes emitted
/// so far are not a complete Brotli stream and no decoder will accept them. The
/// compressor is left ready for the next stream either way.
///
/// # Examples
///
/// ```
/// use mbrotli::{Compressor, EncoderConfig, EncoderStatus, InputSize, Operation, Quality};
///
/// let mut encoder = Compressor::new(EncoderConfig::default().with_quality(Quality::Q5))?;
/// let payload = b"a payload compressed one chunk at a time".repeat(10);
///
/// let mut compressed = Vec::new();
/// let mut buffer = [0u8; 64];
/// let mut input = payload.as_slice();
/// {
///     let mut session = encoder.start(InputSize::Exact(payload.len() as u64).into())?;
///     loop {
///         let progress = session.process(input, &mut buffer, Operation::Finish)?;
///         input = &input[progress.consumed..];
///         compressed.extend_from_slice(&buffer[..progress.produced]);
///         if progress.status == EncoderStatus::Finished {
///             break;
///         }
///     }
/// }
///
/// assert!(!compressed.is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct EncoderSession<'c, 'd> {
    /// The compressor whose encoder and buffers this stream is using.
    compressor: &'c mut Compressor,
    /// The dictionary the match finders consult, if one was attached.
    dictionary: Option<&'d PreparedDictionary>,
    /// Largest input one encoder call accepts.
    limit: usize,
    /// Where the stream is.
    phase: Phase,
    #[cfg(feature = "experimental")]
    logical_position: u64,
    #[cfg(feature = "experimental")]
    flint: bool,
}

impl<'c, 'd> EncoderSession<'c, 'd> {
    /// Starts a stream on `compressor`.
    ///
    /// The caller has already validated the stream configuration and acquired
    /// the encoder, which is what fixes `limit`.
    pub(crate) fn new(
        compressor: &'c mut Compressor,
        dictionary: Option<&'d PreparedDictionary>,
        limit: usize,
        stream: StreamConfig,
    ) -> Self {
        #[cfg(not(feature = "experimental"))]
        let _ = stream;
        Self {
            compressor,
            dictionary,
            limit,
            phase: Phase::Open,
            #[cfg(feature = "experimental")]
            logical_position: stream.stream_offset(),
            #[cfg(feature = "experimental")]
            flint: stream.stream_offset() != 0,
        }
    }

    /// Moves the stream forward by one step.
    ///
    /// Takes what it can from `input`, writes what it can into `output`, and
    /// reports exactly how much of each it moved along with what it needs next.
    /// Both slices may be empty, and either may be a single byte; the session
    /// never spins on a call that made no progress, it reports why instead.
    ///
    /// A call returns [`EncoderStatus::NeedsOutput`] while encoded bytes are
    /// still waiting, [`EncoderStatus::NeedsInput`] when the operation it was
    /// given has done all it can, and [`EncoderStatus::Finished`] once a
    /// [`Operation::Finish`] has been completed and delivered. After that it is
    /// idempotent: further calls consume nothing, produce nothing and report
    /// `Finished`.
    ///
    /// The operation may change between calls. A `Finish` that returns
    /// `NeedsOutput` must be repeated — with the same operation — until it
    /// reports `Finished`; the final meta-block is encoded once however many
    /// calls it takes to deliver.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::InvalidState`] when the stream has already failed,
    /// and propagates whatever the encoder reports. A failed session encodes
    /// nothing further.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::{Compressor, EncoderConfig, EncoderStatus, Operation, Quality};
    ///
    /// let mut encoder = Compressor::new(EncoderConfig::default().with_quality(Quality::Q1))?;
    /// let mut session = encoder.start(Default::default())?;
    /// let mut output = [0u8; 256];
    ///
    /// // An empty input still ends in a complete stream.
    /// let progress = session.process(b"", &mut output, Operation::Finish)?;
    /// assert_eq!(progress.status, EncoderStatus::Finished);
    /// assert!(progress.produced > 0);
    ///
    /// // And the finished session stays finished.
    /// let again = session.process(b"", &mut output, Operation::Finish)?;
    /// assert_eq!(again, mbrotli::Progress {
    ///     consumed: 0,
    ///     produced: 0,
    ///     status: EncoderStatus::Finished,
    /// });
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn process(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        operation: Operation,
    ) -> Result<Progress, EncodeError> {
        if self.phase == Phase::Failed {
            return Err(EncodeError::InvalidState {
                attempted: "process a stream that has already failed",
            });
        }

        let mut consumed = 0usize;
        let mut produced = 0usize;
        #[cfg(feature = "experimental")]
        if self
            .logical_position
            .checked_add(input.len() as u64)
            .is_none_or(|end| end > (1u64 << 63) - 1)
        {
            return Err(EncodeError::StreamPositionOverflow {
                position: self.logical_position,
                input_bytes: input.len() as u64,
            });
        }

        loop {
            produced += self.compressor.drain_pending(&mut output[produced..]);
            if self.compressor.has_pending() {
                return Ok(Progress {
                    consumed,
                    produced,
                    status: EncoderStatus::NeedsOutput,
                });
            }
            if self.phase == Phase::Finished {
                return Ok(Progress {
                    consumed,
                    produced,
                    status: EncoderStatus::Finished,
                });
            }

            // Take what the staging buffer still has room for.
            let limit = self.limit;
            #[cfg(feature = "experimental")]
            let limit = if self.flint { 2 } else { limit };
            let room = limit - self.compressor.staging.len();
            let take = room.min(input.len() - consumed);
            if take > 0 {
                self.compressor
                    .staging
                    .extend_from_slice(&input[consumed..consumed + take]);
                consumed += take;
                #[cfg(feature = "experimental")]
                {
                    self.logical_position += take as u64;
                }
                self.phase = Phase::Open;
            }

            // A whole block is only encoded once something is known to follow
            // it, so that the last block of a stream is always the one carrying
            // `is_last`. That is what makes a streamed stream reproduce the
            // one-shot bytes for an input that is a whole number of blocks.
            #[cfg(feature = "experimental")]
            if self.flint
                && self.compressor.staging.len() == 2
                && (consumed < input.len() || operation != Operation::Finish)
            {
                self.flush(output, &mut produced)?;
                self.flint = false;
                continue;
            }
            if self.compressor.staging.len() == self.limit && consumed < input.len() {
                self.encode(false, output, &mut produced)?;
                continue;
            }

            match operation {
                Operation::Process => {
                    return Ok(Progress {
                        consumed,
                        produced,
                        status: EncoderStatus::NeedsInput,
                    });
                }
                Operation::Flush => {
                    if self.phase == Phase::Flushed {
                        return Ok(Progress {
                            consumed,
                            produced,
                            status: EncoderStatus::NeedsInput,
                        });
                    }
                    self.flush(output, &mut produced)?;
                    self.phase = Phase::Flushed;
                }
                Operation::Finish => {
                    self.encode(true, output, &mut produced)?;
                    self.phase = Phase::Finished;
                }
            }
        }
    }

    /// Returns whether the stream has been terminated and delivered.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::{Compressor, EncoderConfig, Operation, Quality};
    ///
    /// let mut encoder = Compressor::new(EncoderConfig::default().with_quality(Quality::Q0))?;
    /// let mut session = encoder.start(Default::default())?;
    /// let mut output = [0u8; 256];
    ///
    /// assert!(!session.is_finished());
    /// session.process(b"payload", &mut output, Operation::Finish)?;
    /// assert!(session.is_finished());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        matches!(self.phase, Phase::Finished)
    }

    /// Encodes the staged block into `output`, holding back what does not fit.
    ///
    /// The encoder hands back a borrowed slice of its own scratch, which the
    /// next call overwrites, so it has to be copied somewhere before then.
    /// Copying straight into the caller's slice is what keeps a streaming path
    /// down to the one copy the borrow forces.
    fn encode(
        &mut self,
        is_last: bool,
        output: &mut [u8],
        produced: &mut usize,
    ) -> Result<(), EncodeError> {
        let attached = self.dictionary.map(PreparedDictionary::inner);
        let Compressor {
            workspace,
            staging,
            pending,
            served,
            ..
        } = &mut *self.compressor;
        let Some(encoder) = workspace.encoder() else {
            self.phase = Phase::Failed;
            return Err(EncodeError::InternalInvariant {
                detail: "a session outlived the encoder it was started with",
            });
        };
        let outcome = if is_last {
            encoder.encode_block_with(staging, true, attached)
        } else {
            encoder.encode_block_with(staging, false, attached)
        };
        match outcome {
            Ok(bytes) => {
                Self::deliver(bytes, output, produced, pending, served);
                staging.clear();
                Ok(())
            }
            Err(error) => {
                self.phase = Phase::Failed;
                Err(EncodeError::from_core(error, 0))
            }
        }
    }

    /// Emits the staged block and realigns the stream, leaving it open.
    fn flush(&mut self, output: &mut [u8], produced: &mut usize) -> Result<(), EncodeError> {
        let attached = self.dictionary.map(PreparedDictionary::inner);
        let Compressor {
            workspace,
            staging,
            pending,
            served,
            ..
        } = &mut *self.compressor;
        let Some(encoder) = workspace.encoder() else {
            self.phase = Phase::Failed;
            return Err(EncodeError::InternalInvariant {
                detail: "a session outlived the encoder it was started with",
            });
        };
        match encoder.flush_block(staging, attached) {
            Ok(bytes) => {
                Self::deliver(bytes, output, produced, pending, served);
                staging.clear();
                Ok(())
            }
            Err(error) => {
                self.phase = Phase::Failed;
                Err(EncodeError::from_core(error, 0))
            }
        }
    }

    /// Copies `bytes` into `output`, keeping whatever did not fit.
    fn deliver(
        bytes: &[u8],
        output: &mut [u8],
        produced: &mut usize,
        pending: &mut Vec<u8>,
        served: &mut usize,
    ) {
        let room = output.len() - *produced;
        let direct = bytes.len().min(room);
        if let (Some(target), Some(source)) = (
            output.get_mut(*produced..*produced + direct),
            bytes.get(..direct),
        ) {
            target.copy_from_slice(source);
            *produced += direct;
        }
        if let Some(rest) = bytes.get(direct..)
            && !rest.is_empty()
        {
            pending.clear();
            pending.extend_from_slice(rest);
            *served = 0;
        }
    }
}

impl Drop for EncoderSession<'_, '_> {
    /// Returns the compressor to a state the next stream can start from.
    ///
    /// A session that finished leaves its encoder retained, so the next stream
    /// of the same shape reuses every allocation. A session abandoned part-way
    /// drops the retained encoder instead: it holds half a stream, and nothing
    /// good comes of letting the next one inherit that. Either way the staging
    /// buffers keep their capacity and lose their contents.
    fn drop(&mut self) {
        if self.phase != Phase::Finished {
            self.compressor.workspace.invalidate();
        }
        self.compressor.staging.clear();
        self.compressor.pending.clear();
        self.compressor.served = 0;
        self.compressor.active = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_size_hints_zero_the_way_the_reference_does() {
        assert_eq!(InputSize::Unknown.hint(), 0);
        assert_eq!(InputSize::Exact(0).hint(), 0);
        assert_eq!(InputSize::Exact(4096).hint(), 4096);
        assert_eq!(InputSize::Exact(u64::MAX).hint(), usize::MAX);
        assert_eq!(InputSize::default(), InputSize::Unknown);
        assert_eq!(InputSize::from(7u64), InputSize::Exact(7));
    }

    #[test]
    fn a_stream_configuration_carries_only_what_one_stream_knows() {
        let stream = StreamConfig::default()
            .with_input_size(InputSize::Exact(10))
            .with_stream_offset(64);
        assert_eq!(stream.input_size(), InputSize::Exact(10));
        assert_eq!(stream.stream_offset(), 64);

        let plain = StreamConfig::from(InputSize::Exact(10));
        assert_eq!(plain.input_size(), InputSize::Exact(10));
        assert_eq!(plain.stream_offset(), 0);
        assert_eq!(
            StreamConfig::default(),
            StreamConfig::from(InputSize::Unknown)
        );
    }

    #[test]
    fn the_operation_and_status_values_are_distinct() {
        assert_eq!(Operation::default(), Operation::Process);
        assert_ne!(Operation::Flush, Operation::Finish);
        assert_ne!(EncoderStatus::NeedsInput, EncoderStatus::NeedsOutput);
        assert_ne!(EncoderStatus::NeedsOutput, EncoderStatus::Finished);
    }
}
