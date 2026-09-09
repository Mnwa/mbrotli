use super::{
    DecodeConfigError, DecodeError, DecodeOperation, DecodeStreamConfig, DecoderConfig,
    DecoderSession, DecoderStatus, core::Stream,
};
use crate::{Backend, RetentionPolicy, dictionary::DictionaryRef};
use ::core::ops::Range;
use alloc::vec::Vec;

/// Reusable Brotli decoder with exclusive per-operation access.
#[derive(Debug)]
pub struct Decompressor {
    pub(super) config: DecoderConfig,
    backend: Backend,
    retention: RetentionPolicy,
    pub(super) workspace: Stream,
    pub(super) active: bool,
}

/// Decoder construction with an optional backend and retention policy.
#[derive(Debug, Clone, Copy)]
pub struct DecompressorBuilder {
    config: DecoderConfig,
    backend: Option<Backend>,
    retention: RetentionPolicy,
}

impl DecompressorBuilder {
    /// Sets the policy applied after each session is dropped.
    pub const fn with_retention(mut self, value: RetentionPolicy) -> Self {
        self.retention = value;
        self
    }
    /// Selects an already host-validated execution backend.
    pub const fn with_backend(mut self, value: Backend) -> Self {
        self.backend = Some(value);
        self
    }
    /// Constructs an empty decoder without allocating a history window.
    ///
    /// # Errors
    /// Configuration values are validated at construction; current typed
    /// configurations have no additional cross-field restrictions.
    pub fn build(self) -> Result<Decompressor, DecodeConfigError> {
        Ok(Decompressor {
            config: self.config,
            backend: self.backend.unwrap_or_default(),
            retention: self.retention,
            workspace: Stream::default(),
            active: false,
        })
    }
}

impl Decompressor {
    /// Constructs a reusable decoder with default retention and host backend.
    ///
    /// # Errors
    /// Returns invalid configuration errors, as [`DecompressorBuilder::build`].
    pub fn new(config: DecoderConfig) -> Result<Self, DecodeConfigError> {
        Self::builder(config).build()
    }
    /// Starts configuring a decoder without detecting CPU capabilities yet.
    pub const fn builder(config: DecoderConfig) -> DecompressorBuilder {
        DecompressorBuilder {
            config,
            backend: None,
            retention: RetentionPolicy::Aggressive,
        }
    }
    /// Returns the reusable policy.
    pub const fn config(&self) -> &DecoderConfig {
        &self.config
    }
    /// Returns the configured retention policy.
    pub const fn retention(&self) -> RetentionPolicy {
        self.retention
    }
    /// Changes policy and clears abandoned or previous stream state.
    ///
    /// # Errors
    /// Returns invalid configuration errors without changing the decoder.
    pub fn reconfigure(&mut self, config: DecoderConfig) -> Result<(), DecodeConfigError> {
        if self.active
            || self.retention == RetentionPolicy::ReleaseAll
            || (config != self.config && self.retention == RetentionPolicy::CurrentConfig)
        {
            self.recover();
        }
        self.config = config;
        self.workspace.reset(config);
        self.active = false;
        Ok(())
    }
    /// Returns live retained decoder heap storage, excluding caller output.
    pub const fn retained_bytes(&self) -> usize {
        self.workspace.retained_bytes()
    }
    /// Applies a retention policy once, preserving abandoned-session protection.
    pub fn trim(&mut self, policy: RetentionPolicy) {
        if policy == RetentionPolicy::ReleaseAll
            || matches!(policy, RetentionPolicy::Bounded { max_bytes } if self.retained_bytes() > max_bytes)
        {
            self.workspace = Stream::default();
        }
    }
    /// Clears abandoned sessions and releases all owned workspace.
    pub fn recover(&mut self) {
        self.workspace = Stream::default();
        self.active = false;
    }
    /// Copies configuration into an independent object without copying storage.
    pub fn fork_empty(&self) -> Self {
        Self {
            config: self.config,
            backend: self.backend,
            retention: self.retention,
            workspace: Stream::default(),
            active: false,
        }
    }
    /// Borrows this decoder for an incremental operation.
    ///
    /// # Errors
    /// Rejects abandoned sessions and exact sizes exceeding the output budget.
    pub fn start(
        &mut self,
        stream: DecodeStreamConfig,
    ) -> Result<DecoderSession<'_, 'static>, DecodeError> {
        DecoderSession::start(self, stream, None)
    }
    /// Decodes all input into a new vector. Single mode rejects trailing data.
    ///
    /// # Errors
    /// Returns codec, resource, lifecycle, or trailing-data errors.
    pub fn decompress(&mut self, src: &[u8]) -> Result<Vec<u8>, DecodeError> {
        let mut dst = Vec::new();
        self.decompress_into(src, &mut dst)?;
        Ok(dst)
    }
    /// Appends a decoded operation, rolling back its entire append on error.
    ///
    /// # Errors
    /// Returns codec, resource, lifecycle, allocation, or trailing-data errors.
    pub fn decompress_into(
        &mut self,
        src: &[u8],
        dst: &mut Vec<u8>,
    ) -> Result<Range<usize>, DecodeError> {
        self.decode_into(None, src, dst)
    }

    fn decode_into(
        &mut self,
        dictionary: Option<DictionaryRef<'_>>,
        src: &[u8],
        dst: &mut Vec<u8>,
    ) -> Result<Range<usize>, DecodeError> {
        let start = dst.len();
        let result = (|| {
            let mut session =
                DecoderSession::start(self, DecodeStreamConfig::default(), dictionary)?;
            let mut consumed = 0;
            let mut buffer = [0u8; 8192];
            loop {
                let progress = session
                    .process(&src[consumed..], &mut buffer, DecodeOperation::Finish)
                    .map_err(super::DecodeFailure::into_error)?;
                consumed += progress.consumed;
                dst.try_reserve(progress.produced)
                    .map_err(|_| DecodeError::AllocationFailed)?;
                dst.extend_from_slice(&buffer[..progress.produced]);
                if progress.status == DecoderStatus::Finished {
                    if consumed != src.len() {
                        return Err(DecodeError::TrailingData {
                            offset: consumed as u64,
                        });
                    }
                    return Ok(start..dst.len());
                }
            }
        })();
        if result.is_err() {
            dst.truncate(start);
        }
        result
    }
    /// Decodes into a fixed slice, leaving its unused suffix unchanged.
    ///
    /// # Errors
    /// Returns codec/resource errors, trailing data, or `OutputTooSmall`.
    /// Bytes written before an error are not rolled back.
    pub fn decompress_to_slice(
        &mut self,
        src: &[u8],
        dst: &mut [u8],
    ) -> Result<usize, DecodeError> {
        self.decode_to_slice(None, src, dst)
    }

    fn decode_to_slice(
        &mut self,
        dictionary: Option<DictionaryRef<'_>>,
        src: &[u8],
        dst: &mut [u8],
    ) -> Result<usize, DecodeError> {
        let mut session = DecoderSession::start(self, DecodeStreamConfig::default(), dictionary)?;
        let progress = session
            .process(src, dst, DecodeOperation::Finish)
            .map_err(super::DecodeFailure::into_error)?;
        if progress.status == DecoderStatus::NeedsOutput {
            return Err(DecodeError::OutputTooSmall {
                written: progress.produced,
            });
        }
        if progress.consumed != src.len() {
            return Err(DecodeError::TrailingData {
                offset: progress.consumed as u64,
            });
        }
        Ok(progress.produced)
    }
}

impl Decompressor {
    /// Starts a session borrowing its dictionary independently of the decoder.
    ///
    /// # Errors
    /// Rejects an abandoned session or an exact size exceeding the output budget.
    pub fn start_with_dictionary<'d, 'dict>(
        &'d mut self,
        dictionary: impl Into<DictionaryRef<'dict>>,
        stream: DecodeStreamConfig,
    ) -> Result<DecoderSession<'d, 'dict>, DecodeError> {
        DecoderSession::start(self, stream, Some(dictionary.into()))
    }

    /// Decodes a stream using the supplied effective external dictionary.
    ///
    /// # Errors
    /// Returns codec, resource, allocation, lifecycle or trailing-data errors.
    /// Raw Brotli cannot always detect an incorrect external dictionary.
    pub fn decompress_with_dictionary<'dict>(
        &mut self,
        dictionary: impl Into<DictionaryRef<'dict>>,
        src: &[u8],
    ) -> Result<Vec<u8>, DecodeError> {
        let mut output = Vec::new();
        self.decode_into(Some(dictionary.into()), src, &mut output)?;
        Ok(output)
    }

    /// Appends dictionary-decoded bytes and rolls back the append on any error.
    ///
    /// # Errors
    /// Returns the same failures as [`Self::decompress_with_dictionary`].
    pub fn decompress_with_dictionary_into<'dict>(
        &mut self,
        dictionary: impl Into<DictionaryRef<'dict>>,
        src: &[u8],
        dst: &mut Vec<u8>,
    ) -> Result<Range<usize>, DecodeError> {
        self.decode_into(Some(dictionary.into()), src, dst)
    }

    /// Decodes with an external dictionary into an exact or larger slice.
    ///
    /// # Errors
    /// Returns codec/resource failures, trailing data or `OutputTooSmall`.
    /// Any written prefix remains available on error.
    pub fn decompress_with_dictionary_to_slice<'dict>(
        &mut self,
        dictionary: impl Into<DictionaryRef<'dict>>,
        src: &[u8],
        dst: &mut [u8],
    ) -> Result<usize, DecodeError> {
        self.decode_to_slice(Some(dictionary.into()), src, dst)
    }
}
