#[cfg(feature = "compression")]
use super::PreparedDictionary;
use crate::shared::decode_dictionary::Prefixes;

/// One external dictionary attachment, applied in caller order.
#[derive(Clone, Copy, Debug)]
pub enum DictionaryAttachment<'a> {
    /// Literal LZ77 prefix bytes; never interpreted as serialized data.
    Raw(&'a [u8]),
    /// RFC 9841 serialized description with prefixes and custom static lists.
    #[cfg(feature = "experimental")]
    Serialized(&'a [u8]),
}

/// Explicit budgets for constructing a dictionary without encoder indexes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DecodeDictionaryLimits {
    /// Sum of supplied attachment lengths, including empty attachments.
    pub max_source_bytes: Option<u64>,
    /// Peak live requested dictionary heap bytes during construction.
    pub max_owned_bytes: Option<usize>,
}

/// Owned immutable external dictionary for decoding.
///
/// Construction copies source bytes without building encoder match indexes.
#[derive(Debug)]
pub struct DecodeDictionary {
    prefixes: Prefixes,
}

impl DecodeDictionary {
    /// Applies attachments sequentially and owns their bytes after success.
    ///
    /// # Errors
    /// Rejects more than fifteen prefix slots, arithmetic overflow, exceeded
    /// budgets and failed allocations. Empty RAW attachments occupy a slot.
    pub fn new(
        attachments: &[DictionaryAttachment<'_>],
        limits: DecodeDictionaryLimits,
    ) -> Result<Self, DecodeDictionaryError> {
        Ok(Self {
            prefixes: Prefixes::build(attachments, limits)?,
        })
    }
    /// Returns the number of effective LZ77 prefix slots.
    pub const fn attachment_count(&self) -> usize {
        self.prefixes.count()
    }
    /// Returns owned heap storage, excluding this object's inline fields.
    pub const fn retained_bytes(&self) -> usize {
        self.prefixes.retained_bytes()
    }
}

/// Borrowed immutable dictionary source for a single decoding operation.
#[derive(Clone, Copy, Debug)]
pub enum DictionaryRef<'a> {
    /// Reuses a prepared encoder dictionary without copying its payload.
    #[cfg(feature = "compression")]
    Prepared(&'a PreparedDictionary),
    /// Uses a dictionary built without encoder search indexes.
    DecodeOnly(&'a DecodeDictionary),
}

#[cfg(feature = "compression")]
impl<'a> From<&'a PreparedDictionary> for DictionaryRef<'a> {
    fn from(value: &'a PreparedDictionary) -> Self {
        Self::Prepared(value)
    }
}
impl<'a> From<&'a DecodeDictionary> for DictionaryRef<'a> {
    fn from(value: &'a DecodeDictionary) -> Self {
        Self::DecodeOnly(value)
    }
}

impl DictionaryRef<'_> {
    #[cfg(feature = "experimental")]
    pub(crate) fn custom_word(
        self,
        address: u64,
        length: usize,
        context: usize,
        scratch: &mut [u8; crate::shared::dictionary::transform::SCRATCH_BYTES],
    ) -> Option<Result<usize, crate::DecodeError>> {
        match self {
            #[cfg(feature = "compression")]
            Self::Prepared(value) => value
                .inner()
                .static_index
                .as_ref()
                .map(|index| index.decode_word(address, length, context, scratch)),
            Self::DecodeOnly(value) => value
                .prefixes
                .description
                .as_ref()
                .map(|description| description.resolve(address, length, context, scratch)),
        }
    }

    pub(crate) fn prefix_len(self) -> u64 {
        match self {
            #[cfg(feature = "compression")]
            Self::Prepared(value) => value.inner().dictionaries().prefix().total_len(),
            Self::DecodeOnly(value) => value.prefixes.total(),
        }
    }
    pub(crate) fn prefix_byte(self, offset: u64) -> Option<u8> {
        match self {
            #[cfg(feature = "compression")]
            Self::Prepared(value) => value
                .inner()
                .dictionaries()
                .prefix()
                .run_from(offset)
                .first()
                .copied(),
            Self::DecodeOnly(value) => value.prefixes.byte(offset),
        }
    }
}

/// Failure while constructing an immutable decode-only dictionary.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DecodeDictionaryError {
    /// The serialized description violates its format.
    #[cfg(feature = "experimental")]
    #[error("invalid serialized decode dictionary")]
    InvalidSerializedDictionary,
    /// Two active custom static descriptions cannot be combined.
    #[cfg(feature = "experimental")]
    #[error("conflicting custom static dictionaries")]
    ConflictingStaticDictionaries,
    /// The format permits at most fifteen prefix attachments.
    #[error("more than fifteen dictionary prefix attachments")]
    TooManyAttachments,
    /// Supplied source buffers exceed the explicit budget.
    #[error("dictionary source exceeds {limit} bytes")]
    SourceLimitExceeded {
        /// Configured source budget.
        limit: u64,
    },
    /// Peak owned storage exceeds the explicit budget.
    #[error("dictionary storage exceeds {limit} bytes")]
    MemoryLimitExceeded {
        /// Configured memory budget.
        limit: usize,
    },
    /// A fallible dictionary allocation failed.
    #[error("dictionary allocation failed")]
    AllocationFailed,
    /// A size exceeds the counter or address space.
    #[error("dictionary size overflow")]
    SizeOverflow,
}
