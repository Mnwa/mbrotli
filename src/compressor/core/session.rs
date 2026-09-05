//! Public-session ownership and error boundary over the shared block state machine.

use super::stream::{Buffers, Destination, Output, Phase, StreamState};
use crate::compressor::dictionary::PreparedDictionary;
use crate::compressor::encoder::Compressor;
use crate::compressor::error::EncodeError;
use crate::compressor::session::{Operation, Progress, StreamConfig};

/// Exclusive stream state over the compressor's retained buffers and encoder.
#[derive(Debug)]
pub(crate) struct SessionCore<'c, 'd> {
    compressor: &'c mut Compressor,
    dictionary: Option<&'d PreparedDictionary>,
    state: StreamState,
    #[cfg(feature = "experimental")]
    logical_position: u64,
}

impl<'c, 'd> SessionCore<'c, 'd> {
    /// Starts after stream validation and workspace acquisition have succeeded.
    pub(crate) fn new(
        compressor: &'c mut Compressor,
        dictionary: Option<&'d PreparedDictionary>,
        limit: usize,
        stream: StreamConfig,
    ) -> Self {
        #[cfg(not(feature = "experimental"))]
        let _ = stream;
        #[cfg(feature = "experimental")]
        let flint = stream.stream_offset() != 0;
        #[cfg(not(feature = "experimental"))]
        let flint = false;
        Self {
            compressor,
            dictionary,
            state: StreamState::new(limit, flint),
            #[cfg(feature = "experimental")]
            logical_position: stream.stream_offset(),
        }
    }

    /// Validates session state and logical positions, then runs the shared scheduler.
    pub(crate) fn process(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        operation: Operation,
    ) -> Result<Progress, EncodeError> {
        if self.state.phase == Phase::Failed {
            return Err(EncodeError::InvalidState {
                attempted: "process a stream that has already failed",
            });
        }
        #[cfg(feature = "experimental")]
        if self.state.phase != Phase::Finished
            && self
                .logical_position
                .checked_add(input.len() as u64)
                .is_none_or(|end| end > (1u64 << 63) - 1)
        {
            return Err(EncodeError::StreamPositionOverflow {
                position: self.logical_position,
                input_bytes: input.len() as u64,
            });
        }

        let Compressor {
            workspace,
            staging,
            pending,
            served,
            ..
        } = &mut *self.compressor;
        let Some(encoder) = workspace.encoder() else {
            self.state.phase = Phase::Failed;
            return Err(EncodeError::InternalInvariant {
                detail: "a session outlived the encoder it was started with",
            });
        };
        let outcome = self.state.process(
            encoder,
            self.dictionary.map(PreparedDictionary::inner),
            Buffers {
                staging,
                pending,
                served,
                allow_pending: true,
            },
            input,
            Output::new(Destination::Slice(output)),
            operation,
        );
        let progress = match outcome {
            Ok(progress) => progress,
            Err(error) => return Err(EncodeError::from_core(error, 0)),
        };
        #[cfg(feature = "experimental")]
        {
            self.logical_position += progress.consumed as u64;
        }
        Ok(progress)
    }

    /// Termination is observable only after all pending output was delivered.
    #[must_use]
    pub(crate) const fn is_finished(&self) -> bool {
        matches!(self.state.phase, Phase::Finished) && !self.compressor.has_pending()
    }
}

impl Drop for SessionCore<'_, '_> {
    fn drop(&mut self) {
        if self.state.phase != Phase::Finished {
            self.compressor.workspace.invalidate();
        }
        self.compressor.staging.clear();
        self.compressor.pending.clear();
        self.compressor.served = 0;
        self.compressor.active = false;
        self.compressor.finish_operation();
    }
}
