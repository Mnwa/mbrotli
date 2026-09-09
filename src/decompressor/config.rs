use super::DecodeConfigError;

/// Number of complete raw streams accepted by an operation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MemberMode {
    /// Stop at the first complete stream.
    #[default]
    Single,
    /// Decode successive streams until the caller declares final input.
    Concatenated,
}

/// Accepted window headers and their maximum bit count.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowLimit {
    bits: u8,
    large: bool,
}

impl WindowLimit {
    /// Accepts only RFC 7932 headers up to this bit count.
    ///
    /// # Errors
    /// Returns an error unless `max_bits` is in `10..=24`.
    pub const fn standard(max_bits: u8) -> Result<Self, DecodeConfigError> {
        if max_bits < 10 || max_bits > 24 {
            return Err(DecodeConfigError::StandardWindow { max_bits });
        }
        Ok(Self {
            bits: max_bits,
            large: false,
        })
    }

    /// Accepts standard and extended headers up to this bit count.
    ///
    /// # Errors
    /// Returns an error unless `max_bits` is in `10..=62`.
    pub const fn large(max_bits: u8) -> Result<Self, DecodeConfigError> {
        if max_bits < 10 || max_bits > 62 {
            return Err(DecodeConfigError::LargeWindow { max_bits });
        }
        Ok(Self {
            bits: max_bits,
            large: true,
        })
    }

    /// Largest accepted window bit count.
    pub const fn max_bits(self) -> u8 {
        self.bits
    }
    /// Whether extended headers are accepted, including those below 25 bits.
    pub const fn allows_large(self) -> bool {
        self.large
    }
}

/// Optional cumulative input/output and live workspace budgets.
///
/// Defaults impose no numeric budgets. Dictionary storage and caller-owned
/// output do not count towards the workspace budget.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DecodeLimits {
    input: Option<u64>,
    output: Option<u64>,
    workspace: Option<usize>,
}

impl DecodeLimits {
    /// Sets the operation's compressed input budget, including metadata.
    pub const fn with_max_input_bytes(mut self, value: Option<u64>) -> Self {
        self.input = value;
        self
    }
    /// Sets the operation's regenerated output budget across all members.
    pub const fn with_max_output_bytes(mut self, value: Option<u64>) -> Self {
        self.output = value;
        self
    }
    /// Sets the live requested heap budget owned by the decoder.
    pub const fn with_max_workspace_bytes(mut self, value: Option<usize>) -> Self {
        self.workspace = value;
        self
    }
    /// Returns the compressed input budget.
    pub const fn max_input_bytes(self) -> Option<u64> {
        self.input
    }
    /// Returns the regenerated output budget.
    pub const fn max_output_bytes(self) -> Option<u64> {
        self.output
    }
    /// Returns the decoder workspace budget.
    pub const fn max_workspace_bytes(self) -> Option<usize> {
        self.workspace
    }
}

/// Reusable decoder policy. Defaults accept extended windows and one member.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecoderConfig {
    window: WindowLimit,
    members: MemberMode,
    limits: DecodeLimits,
}

impl Default for DecoderConfig {
    fn default() -> Self {
        Self {
            window: WindowLimit {
                bits: 62,
                large: true,
            },
            members: MemberMode::Single,
            limits: DecodeLimits::default(),
        }
    }
}

impl DecoderConfig {
    /// Sets the accepted window headers.
    pub const fn with_window_limit(mut self, value: WindowLimit) -> Self {
        self.window = value;
        self
    }
    /// Sets the operation's member policy.
    pub const fn with_member_mode(mut self, value: MemberMode) -> Self {
        self.members = value;
        self
    }
    /// Sets explicit resource budgets.
    pub const fn with_limits(mut self, value: DecodeLimits) -> Self {
        self.limits = value;
        self
    }
    /// Returns the accepted window headers.
    pub const fn window_limit(&self) -> WindowLimit {
        self.window
    }
    /// Returns the member policy.
    pub const fn member_mode(&self) -> MemberMode {
        self.members
    }
    /// Returns the resource budgets.
    pub const fn limits(&self) -> DecodeLimits {
        self.limits
    }
}

/// Expected total output, validated without trusting it as an allocation size.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OutputSize {
    /// No externally declared size.
    #[default]
    Unknown,
    /// Exact total payload bytes across the operation's members.
    Exact(u64),
}

/// Per-operation output validation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DecodeStreamConfig {
    size: OutputSize,
}

impl DecodeStreamConfig {
    /// Sets the exact size contract, or disables it with `Unknown`.
    pub const fn with_output_size(mut self, value: OutputSize) -> Self {
        self.size = value;
        self
    }
    /// Returns the expected output size.
    pub const fn output_size(&self) -> OutputSize {
        self.size
    }
}

impl From<OutputSize> for DecodeStreamConfig {
    fn from(size: OutputSize) -> Self {
        Self { size }
    }
}
