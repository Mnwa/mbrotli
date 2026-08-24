//! The caller-owned mutable shared context, and the builder that prepares it.

use crate::compressor::core::rfc9841::context::{
    Budget, PrefixMatch as CorePrefixMatch, SharedContextInner, check_quality,
};
use crate::compressor::shared::{SharedBrotliError, SharedContextLimits};
use crate::compressor::{BrotliResult, QualityLevel};

/// Dictionaries attached to a stream, prepared once and reused many times.
///
/// A shared context is the [RFC 9841] object a caller keeps: it owns the
/// dictionary bytes, the indexes built over them, and the small amount of
/// state one compression session may change. Preparing it is the expensive
/// part — parsing, validating and indexing — and using it is meant to be
/// cheap, which is why the type is handed to the compressor by exclusive
/// borrow rather than rebuilt per call.
///
/// # Ownership
///
/// There is no `Arc`, no `Mutex`, no atomic and no interior mutability inside
/// this type. It is `Send`, so it moves freely between threads, and every
/// operation that changes it takes `&mut`, so one context backs at most one
/// active compression session and the borrow checker is what proves it. A
/// caller who wants one context shared by several threads wraps it themselves:
///
/// ```
/// # use mbrotli::Brotli;
/// # use mbrotli::compressor::QualityLevel;
/// use std::sync::{Arc, Mutex};
///
/// let compressor = Brotli::default().compressor();
/// let context = compressor.shared_context_builder(QualityLevel::Q5).prepare()?;
/// let context = Arc::new(Mutex::new(context));
/// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
/// ```
///
/// For genuinely parallel compression, prepare one context per worker instead:
/// a lock around one context serialises the compression, not just the access.
///
/// # Examples
///
/// ```
/// use mbrotli::Brotli;
/// use mbrotli::compressor::QualityLevel;
///
/// let compressor = Brotli::default().compressor();
/// let context = compressor
///     .shared_context_builder(QualityLevel::Q5)
///     .add_prefix_dictionary(b"common response prefix".to_vec())
///     .prepare()?;
///
/// assert_eq!(context.max_quality(), QualityLevel::Q5);
/// assert_eq!(context.prefix_dictionary_count(), 1);
/// assert_eq!(context.source_size(), 22);
/// assert!(!context.has_custom_static_dictionary());
/// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
/// ```
///
/// [RFC 9841]: https://www.rfc-editor.org/rfc/rfc9841.html
#[derive(Debug)]
pub struct SharedContext {
    /// The dictionaries, the indexes and the session state.
    inner: SharedContextInner,
    /// The highest quality this context was prepared for.
    max_quality: QualityLevel,
}

impl SharedContext {
    /// Returns the highest quality this context may be used at.
    ///
    /// Every lower quality may use it too; a higher one is refused with
    /// [`SharedBrotliError::SharedContextQualityMismatch`] rather than
    /// silently ignored.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::QualityLevel;
    ///
    /// let compressor = Brotli::default().compressor();
    /// let context = compressor.shared_context_builder(QualityLevel::Q9).prepare()?;
    ///
    /// assert_eq!(context.max_quality(), QualityLevel::Q9);
    /// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
    /// ```
    pub const fn max_quality(&self) -> QualityLevel {
        self.max_quality
    }

    /// Returns how many dictionaries were attached, in any form.
    ///
    /// Equal to [`SharedContext::prefix_dictionary_count`] today, because a
    /// prefix dictionary is the only kind that can be attached; a serialized
    /// dictionary will count here as one attachment as well.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::QualityLevel;
    ///
    /// let compressor = Brotli::default().compressor();
    /// let context = compressor
    ///     .shared_context_builder(QualityLevel::Q5)
    ///     .add_prefix_dictionary(b"older".to_vec())
    ///     .add_prefix_dictionary(b"newer".to_vec())
    ///     .prepare()?;
    ///
    /// assert_eq!(context.attachment_count(), 2);
    /// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
    /// ```
    pub fn attachment_count(&self) -> usize {
        self.inner.dictionaries().prefix().segment_count()
    }

    /// Returns how many LZ77 prefix dictionaries the context holds.
    ///
    /// At most fifteen, which is the limit RFC 9841 sets.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::QualityLevel;
    ///
    /// let compressor = Brotli::default().compressor();
    /// let context = compressor.shared_context_builder(QualityLevel::Q5).prepare()?;
    ///
    /// assert_eq!(context.prefix_dictionary_count(), 0);
    /// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
    /// ```
    pub fn prefix_dictionary_count(&self) -> usize {
        self.inner.dictionaries().prefix().segment_count()
    }

    /// Returns whether the context carries custom static dictionary data.
    ///
    /// Always `false` today: custom word lists and transform lists arrive with
    /// the serialized dictionary format, which is not implemented yet.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::QualityLevel;
    ///
    /// let compressor = Brotli::default().compressor();
    /// let context = compressor.shared_context_builder(QualityLevel::Q5).prepare()?;
    ///
    /// assert!(!context.has_custom_static_dictionary());
    /// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
    /// ```
    pub const fn has_custom_static_dictionary(&self) -> bool {
        false
    }

    /// Returns how many dictionary bytes the caller handed over.
    ///
    /// The sum of every attachment's length: what a decoder has to attach, in
    /// the same order, to read the stream back.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::QualityLevel;
    ///
    /// let compressor = Brotli::default().compressor();
    /// let context = compressor
    ///     .shared_context_builder(QualityLevel::Q5)
    ///     .add_prefix_dictionary(b"twelve bytes".to_vec())
    ///     .prepare()?;
    ///
    /// assert_eq!(context.source_size(), 12);
    /// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
    /// ```
    pub fn source_size(&self) -> usize {
        self.inner.dictionaries().source_size()
    }

    /// Returns how many bytes this context owns.
    ///
    /// Counts the dictionary bytes and the prepared indexes together, which
    /// are the two categories a context is responsible for; the encoder
    /// workspace a session uses is not part of the context and is not counted
    /// here. Reading it needs no synchronisation, because there is none to
    /// take.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::QualityLevel;
    ///
    /// let compressor = Brotli::default().compressor();
    /// let empty = compressor.shared_context_builder(QualityLevel::Q5).prepare()?;
    /// let filled = compressor
    ///     .shared_context_builder(QualityLevel::Q5)
    ///     .add_prefix_dictionary(b"the quick brown fox".to_vec())
    ///     .prepare()?;
    ///
    /// assert!(filled.allocated_size() > empty.allocated_size());
    /// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
    /// ```
    pub fn allocated_size(&self) -> usize {
        self.inner.allocated_size()
    }

    /// Returns the backward distance that addresses `dictionary_offset`.
    ///
    /// RFC 9841 places the attached prefix immediately beyond the ordinary
    /// sliding window: distances `1..=max_backward` are the stream's own
    /// history, `max_backward + 1` is the *last* prefix byte — the one
    /// immediately before the stream begins — and `max_backward + prefix
    /// length` is the very first. `max_backward` is the largest distance the
    /// window can express at the position the copy starts from.
    ///
    /// Returns `None` for an offset past the end of the prefix, and when the
    /// distance would not fit a `u64`. Nothing wraps.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::QualityLevel;
    ///
    /// let compressor = Brotli::default().compressor();
    /// let context = compressor
    ///     .shared_context_builder(QualityLevel::Q5)
    ///     .add_prefix_dictionary(b"oldest".to_vec())
    ///     .add_prefix_dictionary(b"newest".to_vec())
    ///     .prepare()?;
    ///
    /// // Twelve prefix bytes: the last is one past the window, the first twelve past.
    /// assert_eq!(context.backward_distance(11, 1000), Some(1001));
    /// assert_eq!(context.backward_distance(0, 1000), Some(1012));
    /// assert_eq!(context.backward_distance(12, 1000), None);
    /// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
    /// ```
    pub fn backward_distance(&self, dictionary_offset: u64, max_backward: u64) -> Option<u64> {
        self.inner
            .dictionaries()
            .prefix()
            .distance_of(dictionary_offset, max_backward)
    }

    /// Returns the prefix offset a backward `distance` addresses.
    ///
    /// The inverse of [`SharedContext::backward_distance`], and the mapping a
    /// decoder performs. Returns `None` when the distance falls inside the
    /// ordinary sliding window or past the end of the prefix.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::QualityLevel;
    ///
    /// let compressor = Brotli::default().compressor();
    /// let context = compressor
    ///     .shared_context_builder(QualityLevel::Q5)
    ///     .add_prefix_dictionary(b"oldest".to_vec())
    ///     .add_prefix_dictionary(b"newest".to_vec())
    ///     .prepare()?;
    ///
    /// assert_eq!(context.dictionary_offset(1001, 1000), Some(11));
    /// assert_eq!(context.dictionary_offset(1012, 1000), Some(0));
    /// // Inside the window, and past the whole prefix.
    /// assert_eq!(context.dictionary_offset(1000, 1000), None);
    /// assert_eq!(context.dictionary_offset(1013, 1000), None);
    /// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
    /// ```
    pub fn dictionary_offset(&self, backward_distance: u64, max_backward: u64) -> Option<u64> {
        self.inner
            .dictionaries()
            .prefix()
            .address_of(backward_distance, max_backward)
    }

    /// Returns the private state, for the compression entry points.
    pub(crate) const fn inner(&self) -> &SharedContextInner {
        &self.inner
    }

    /// Checks that a call at `quality` may use this context.
    ///
    /// # Errors
    ///
    /// Returns [`SharedBrotliError::SharedContextQualityMismatch`] when the
    /// call asks for more than the context was prepared for.
    pub(crate) fn check_quality(&self, quality: QualityLevel) -> Result<(), SharedBrotliError> {
        check_quality(quality, self.max_quality)
    }
}

/// Where an attached dictionary matched an input, and for how long.
///
/// Returned by
/// [`Compressor::longest_prefix_match`](crate::compressor::Compressor::longest_prefix_match).
/// The offset is into the *logical* prefix — every attachment laid end to end
/// in attachment order — not into any one attachment, because a match is
/// allowed to run from one attachment into the next.
///
/// # Examples
///
/// ```
/// use mbrotli::Brotli;
/// use mbrotli::compressor::QualityLevel;
///
/// let compressor = Brotli::default().compressor();
/// let context = compressor
///     .shared_context_builder(QualityLevel::Q5)
///     .add_prefix_dictionary(b"the quick brown fox".to_vec())
///     .prepare()?;
/// let found = compressor
///     .longest_prefix_match(&context, b"quick brown foxes")
///     .expect("the dictionary covers this");
///
/// assert_eq!(found.dictionary_offset(), 4);
/// assert_eq!(found.length(), 15);
/// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
/// ```
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PrefixMatch {
    /// Where the match starts in the logical prefix.
    offset: u64,
    /// How many bytes matched.
    length: usize,
}

impl PrefixMatch {
    /// Returns where the match starts in the logical prefix.
    ///
    /// Zero is the first byte of the first attachment, which is the oldest
    /// byte a backward distance can reach.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::QualityLevel;
    ///
    /// let compressor = Brotli::default().compressor();
    /// let context = compressor
    ///     .shared_context_builder(QualityLevel::Q5)
    ///     .add_prefix_dictionary(b"oldest bytes ".to_vec())
    ///     .add_prefix_dictionary(b"newest bytes".to_vec())
    ///     .prepare()?;
    ///
    /// assert_eq!(
    ///     compressor
    ///         .longest_prefix_match(&context, b"newest bytes")
    ///         .map(|found| found.dictionary_offset()),
    ///     Some(13)
    /// );
    /// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
    /// ```
    pub const fn dictionary_offset(self) -> u64 {
        self.offset
    }

    /// Returns how many bytes matched.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::QualityLevel;
    ///
    /// let compressor = Brotli::default().compressor();
    /// let context = compressor
    ///     .shared_context_builder(QualityLevel::Q5)
    ///     .add_prefix_dictionary(b"common response prefix".to_vec())
    ///     .prepare()?;
    ///
    /// assert_eq!(
    ///     compressor
    ///         .longest_prefix_match(&context, b"common response prefix and more")
    ///         .map(|found| found.length()),
    ///     Some(22)
    /// );
    /// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
    /// ```
    pub const fn length(self) -> usize {
        self.length
    }
}

impl From<CorePrefixMatch> for PrefixMatch {
    /// Lifts the private search result into the public one.
    fn from(value: CorePrefixMatch) -> Self {
        Self {
            offset: value.offset,
            length: value.length,
        }
    }
}

/// Collects dictionaries in attachment order and prepares them all at once.
///
/// Call order is prefix order: the first dictionary attached holds the oldest
/// bytes, and the last one the bytes immediately before the stream's own
/// output. A decoder has to attach exactly the same bytes in exactly the same
/// order.
///
/// Nothing is validated or indexed until [`SharedContextBuilder::prepare`],
/// which is all-or-nothing: it returns a whole context or an error, never a
/// partially usable one.
///
/// # Examples
///
/// ```
/// use mbrotli::Brotli;
/// use mbrotli::compressor::QualityLevel;
/// use mbrotli::compressor::shared::SharedContextLimits;
///
/// let compressor = Brotli::default().compressor();
/// let context = compressor
///     .shared_context_builder(QualityLevel::Q5)
///     .add_prefix_dictionary(b"oldest bytes".to_vec())
///     .add_prefix_dictionary(b"newest bytes".to_vec())
///     .with_limits(SharedContextLimits::default().with_max_prefix_bytes(1 << 20))
///     .prepare()?;
///
/// assert_eq!(context.prefix_dictionary_count(), 2);
/// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
/// ```
#[derive(Debug)]
pub struct SharedContextBuilder {
    /// The highest quality the prepared context will serve.
    max_quality: QualityLevel,
    /// The limits [`SharedContextBuilder::prepare`] checks against.
    limits: SharedContextLimits,
    /// Owned dictionary bytes, oldest first.
    attachments: Vec<Box<[u8]>>,
}

impl SharedContextBuilder {
    /// Creates a builder that will prepare for `max_quality` and below.
    pub(crate) fn new(max_quality: QualityLevel) -> Self {
        Self {
            max_quality,
            limits: SharedContextLimits::default(),
            attachments: Vec::new(),
        }
    }

    /// Attaches one LZ77 prefix dictionary after the ones already attached.
    ///
    /// The bytes are moved into the builder and then into the context: no
    /// reference counting, no borrow of the caller's buffer to keep alive.
    /// Passing a `Vec<u8>` or a `Box<[u8]>` moves it without copying; passing
    /// a `&[u8]` copies it once.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::QualityLevel;
    ///
    /// let compressor = Brotli::default().compressor();
    /// let context = compressor
    ///     .shared_context_builder(QualityLevel::Q5)
    ///     .add_prefix_dictionary(b"moved without copying".to_vec())
    ///     .add_prefix_dictionary(&b"copied once"[..])
    ///     .prepare()?;
    ///
    /// assert_eq!(context.prefix_dictionary_count(), 2);
    /// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
    /// ```
    #[must_use]
    pub fn add_prefix_dictionary<B>(mut self, bytes: B) -> Self
    where
        B: Into<Box<[u8]>>,
    {
        self.attachments.push(bytes.into());
        self
    }

    /// Replaces the resource limits the context is prepared under.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::QualityLevel;
    /// use mbrotli::compressor::shared::{SharedBrotliError, SharedContextLimits};
    /// use mbrotli::compressor::BrotliCompressError;
    ///
    /// let compressor = Brotli::default().compressor();
    /// let outcome = compressor
    ///     .shared_context_builder(QualityLevel::Q5)
    ///     .add_prefix_dictionary(b"far too long for this limit".to_vec())
    ///     .with_limits(SharedContextLimits::default().with_max_prefix_bytes(8))
    ///     .prepare();
    ///
    /// assert!(matches!(
    ///     outcome,
    ///     Err(BrotliCompressError::Shared(
    ///         SharedBrotliError::DictionaryTooLarge { .. }
    ///     ))
    /// ));
    /// ```
    #[must_use]
    pub const fn with_limits(mut self, limits: SharedContextLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Validates and indexes every attachment, producing the context.
    ///
    /// This is where the expensive work happens: the counts and sizes are
    /// checked, the logical prefix is laid out, and one hash index is built
    /// per attachment. Compression afterwards reuses all of it.
    ///
    /// # Errors
    ///
    /// Returns [`BrotliCompressError::Shared`] carrying
    /// [`SharedBrotliError::TooManyPrefixDictionaries`] past fifteen
    /// attachments, [`SharedBrotliError::DictionaryTooLarge`] when an
    /// attachment or the whole prefix exceeds its limit, and
    /// [`SharedBrotliError::SharedContextTooLarge`] when the prepared indexes
    /// would exceed the allocation limit. Nothing is retained on failure.
    ///
    /// [`BrotliCompressError::Shared`]: crate::compressor::BrotliCompressError::Shared
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::Brotli;
    /// use mbrotli::compressor::QualityLevel;
    ///
    /// let compressor = Brotli::default().compressor();
    /// let context = compressor.shared_context_builder(QualityLevel::Q11).prepare()?;
    ///
    /// assert_eq!(context.attachment_count(), 0);
    /// assert_eq!(context.source_size(), 0);
    /// # Ok::<(), mbrotli::compressor::BrotliCompressError>(())
    /// ```
    pub fn prepare(self) -> BrotliResult<SharedContext> {
        let budget = Budget::from(self.limits);
        let inner = SharedContextInner::new(self.attachments, &budget)?;
        Ok(SharedContext {
            inner,
            max_quality: self.max_quality,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Brotli;

    fn builder(max_quality: QualityLevel) -> SharedContextBuilder {
        Brotli::default()
            .compressor()
            .shared_context_builder(max_quality)
    }

    #[test]
    fn a_context_is_send_and_sync() {
        const fn assert_send<T: Send>() {}
        const fn assert_sync<T: Sync>() {}
        assert_send::<SharedContext>();
        assert_sync::<SharedContext>();
        assert_send::<SharedContextBuilder>();
    }

    #[test]
    fn an_empty_builder_prepares_an_empty_context() {
        let context = builder(QualityLevel::Q5).prepare().expect("prepared");
        assert_eq!(context.max_quality(), QualityLevel::Q5);
        assert_eq!(context.attachment_count(), 0);
        assert_eq!(context.prefix_dictionary_count(), 0);
        assert_eq!(context.source_size(), 0);
        assert!(!context.has_custom_static_dictionary());
        assert!(context.inner().is_empty());
    }

    #[test]
    fn attachment_order_is_call_order() {
        let context = builder(QualityLevel::Q9)
            .add_prefix_dictionary(b"oldest".to_vec())
            .add_prefix_dictionary(b"middle".to_vec())
            .add_prefix_dictionary(b"newest".to_vec())
            .prepare()
            .expect("prepared");
        let prefix = context.inner().dictionaries().prefix();
        assert_eq!(prefix.segment(0), b"oldest");
        assert_eq!(prefix.segment(1), b"middle");
        assert_eq!(prefix.segment(2), b"newest");
        assert_eq!(context.attachment_count(), 3);
        assert_eq!(context.source_size(), 18);
    }

    #[test]
    fn every_owned_byte_form_is_accepted() {
        let boxed: Box<[u8]> = b"boxed".to_vec().into_boxed_slice();
        let context = builder(QualityLevel::Q5)
            .add_prefix_dictionary(b"vector".to_vec())
            .add_prefix_dictionary(boxed)
            .add_prefix_dictionary(&b"borrowed"[..])
            .prepare()
            .expect("prepared");
        assert_eq!(context.attachment_count(), 3);
        assert_eq!(context.source_size(), 6 + 5 + 8);
    }

    #[test]
    fn limits_are_carried_from_the_builder_into_preparation() {
        let outcome = builder(QualityLevel::Q5)
            .add_prefix_dictionary(b"nine byte".to_vec())
            .with_limits(SharedContextLimits::default().with_max_prefix_bytes(8))
            .prepare();
        assert!(matches!(
            outcome,
            Err(crate::compressor::BrotliCompressError::Shared(
                SharedBrotliError::DictionaryTooLarge { bytes: 9, limit: 8 }
            ))
        ));
    }

    #[test]
    fn the_quality_check_admits_lower_qualities_only() {
        let context = builder(QualityLevel::Q5).prepare().expect("prepared");
        assert!(context.check_quality(QualityLevel::Q0).is_ok());
        assert!(context.check_quality(QualityLevel::Q5).is_ok());
        assert!(matches!(
            context.check_quality(QualityLevel::Q6),
            Err(SharedBrotliError::SharedContextQualityMismatch {
                requested: 6,
                prepared: 5
            })
        ));
    }

    #[test]
    fn allocated_size_grows_with_the_prepared_index() {
        let empty = builder(QualityLevel::Q5).prepare().expect("prepared");
        let short = builder(QualityLevel::Q5)
            .add_prefix_dictionary(b"tiny".to_vec())
            .prepare()
            .expect("prepared");
        let indexed = builder(QualityLevel::Q5)
            .add_prefix_dictionary(b"long enough to be indexed".to_vec())
            .prepare()
            .expect("prepared");
        assert!(short.allocated_size() > empty.allocated_size());
        assert!(indexed.allocated_size() > short.allocated_size());
    }

    #[test]
    fn a_prepared_context_holds_no_stream_state_to_carry_over() {
        let context = builder(QualityLevel::Q5)
            .add_prefix_dictionary(b"the quick brown fox".to_vec())
            .prepare()
            .expect("prepared");
        // Everything a context owns is derived from the attached bytes alone,
        // which is what makes repeated use trivially deterministic: there is
        // no history, distance cache or input position in it to reset.
        let again = builder(QualityLevel::Q5)
            .add_prefix_dictionary(b"the quick brown fox".to_vec())
            .prepare()
            .expect("prepared");
        assert_eq!(context.allocated_size(), again.allocated_size());
        assert_eq!(context.source_size(), again.source_size());
    }
}
