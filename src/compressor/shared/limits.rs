//! Resource limits a shared context is prepared under.

use crate::compressor::core::rfc9841::context::Budget;

/// How much memory a [`SharedContext`](super::SharedContext) may spend on a
/// caller's dictionaries.
///
/// These are implementation resource limits, not wire-format limits: they
/// change which contexts this crate agrees to build, never what a stream that
/// was built looks like. They exist because dictionary bytes usually arrive
/// from somewhere less trusted than the code that compresses with them, and a
/// prepared index is several times the size of the dictionary it indexes.
///
/// The defaults are sized for ordinary production dictionaries — a few
/// megabytes of prefix — with a wide margin, and are documented on each
/// accessor. Raise them deliberately; a caller who has already validated the
/// dictionary is the only one who knows it is safe to.
///
/// # Examples
///
/// ```
/// use mbrotli::compressor::shared::SharedContextLimits;
///
/// let limits = SharedContextLimits::default().with_max_prefix_bytes(1 << 20);
///
/// assert_eq!(limits.max_prefix_bytes(), 1 << 20);
/// assert_eq!(
///     limits.max_total_source_bytes(),
///     SharedContextLimits::default().max_total_source_bytes()
/// );
/// ```
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SharedContextLimits {
    /// Largest total of every attached source byte.
    max_total_source_bytes: u64,
    /// Largest logical LZ77 prefix.
    max_prefix_bytes: u64,
    /// Largest peak allocation preparing the context may reach.
    max_allocated_bytes: u64,
}

impl SharedContextLimits {
    /// Default ceiling on the attached source bytes: 64 MiB.
    ///
    /// Far above any ordinary production dictionary, and at the size where the
    /// prepared index stops scaling its bucket count, so a larger dictionary
    /// costs proportionally more to index than it repays.
    pub const DEFAULT_MAX_TOTAL_SOURCE_BYTES: u64 = 64 << 20;

    /// Default ceiling on the logical LZ77 prefix: 64 MiB.
    pub const DEFAULT_MAX_PREFIX_BYTES: u64 = 64 << 20;

    /// Default ceiling on the peak allocation of preparing a context: 1 GiB.
    ///
    /// Preparation costs roughly eight bytes per source byte at its peak — a
    /// four-byte chain link and a four-byte index entry for every position —
    /// plus the bucket tables. A dictionary at
    /// [`SharedContextLimits::DEFAULT_MAX_PREFIX_BYTES`] therefore fits this
    /// with room to spare, and the two defaults do not contradict each other.
    pub const DEFAULT_MAX_ALLOCATED_BYTES: u64 = 1 << 30;

    /// Sets the largest total of attached source bytes.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::shared::SharedContextLimits;
    ///
    /// let limits = SharedContextLimits::default().with_max_total_source_bytes(4096);
    ///
    /// assert_eq!(limits.max_total_source_bytes(), 4096);
    /// ```
    #[must_use]
    pub const fn with_max_total_source_bytes(mut self, bytes: u64) -> Self {
        self.max_total_source_bytes = bytes;
        self
    }

    /// Returns the largest total of attached source bytes.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::shared::SharedContextLimits;
    ///
    /// assert_eq!(
    ///     SharedContextLimits::default().max_total_source_bytes(),
    ///     SharedContextLimits::DEFAULT_MAX_TOTAL_SOURCE_BYTES
    /// );
    /// ```
    pub const fn max_total_source_bytes(self) -> u64 {
        self.max_total_source_bytes
    }

    /// Sets the largest logical LZ77 prefix.
    ///
    /// The prefix is every attached dictionary laid end to end, which is what
    /// a backward distance past the sliding window addresses.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::shared::SharedContextLimits;
    ///
    /// let limits = SharedContextLimits::default().with_max_prefix_bytes(4096);
    ///
    /// assert_eq!(limits.max_prefix_bytes(), 4096);
    /// ```
    #[must_use]
    pub const fn with_max_prefix_bytes(mut self, bytes: u64) -> Self {
        self.max_prefix_bytes = bytes;
        self
    }

    /// Returns the largest logical LZ77 prefix.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::shared::SharedContextLimits;
    ///
    /// assert_eq!(
    ///     SharedContextLimits::default().max_prefix_bytes(),
    ///     SharedContextLimits::DEFAULT_MAX_PREFIX_BYTES
    /// );
    /// ```
    pub const fn max_prefix_bytes(self) -> u64 {
        self.max_prefix_bytes
    }

    /// Sets the largest allocation preparing a context may reach.
    ///
    /// Bounds the *peak*, not just the finished context: the build holds its
    /// scratch tables and the finished ones at once. It is checked against an
    /// upper bound computed before anything is allocated, so a context that
    /// would exceed it is refused rather than allocated and discarded.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::shared::SharedContextLimits;
    ///
    /// let limits = SharedContextLimits::default().with_max_allocated_bytes(1 << 20);
    ///
    /// assert_eq!(limits.max_allocated_bytes(), 1 << 20);
    /// ```
    #[must_use]
    pub const fn with_max_allocated_bytes(mut self, bytes: u64) -> Self {
        self.max_allocated_bytes = bytes;
        self
    }

    /// Returns the largest allocation preparing a context may reach.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::shared::SharedContextLimits;
    ///
    /// assert_eq!(
    ///     SharedContextLimits::default().max_allocated_bytes(),
    ///     SharedContextLimits::DEFAULT_MAX_ALLOCATED_BYTES
    /// );
    /// ```
    pub const fn max_allocated_bytes(self) -> u64 {
        self.max_allocated_bytes
    }
}

impl Default for SharedContextLimits {
    /// Returns the documented production defaults.
    ///
    /// # Examples
    ///
    /// ```
    /// use mbrotli::compressor::shared::SharedContextLimits;
    ///
    /// let limits = SharedContextLimits::default();
    ///
    /// assert_eq!(limits.max_prefix_bytes(), 64 << 20);
    /// assert_eq!(limits.max_allocated_bytes(), 1 << 30);
    /// ```
    fn default() -> Self {
        Self {
            max_total_source_bytes: Self::DEFAULT_MAX_TOTAL_SOURCE_BYTES,
            max_prefix_bytes: Self::DEFAULT_MAX_PREFIX_BYTES,
            max_allocated_bytes: Self::DEFAULT_MAX_ALLOCATED_BYTES,
        }
    }
}

impl From<SharedContextLimits> for Budget {
    /// Flattens the public limits into the form the checks compare against.
    fn from(value: SharedContextLimits) -> Self {
        Self {
            max_total_source_bytes: value.max_total_source_bytes,
            max_prefix_bytes: value.max_prefix_bytes,
            max_allocated_bytes: value.max_allocated_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_documented_constants() {
        let limits = SharedContextLimits::default();
        assert_eq!(
            limits.max_total_source_bytes(),
            SharedContextLimits::DEFAULT_MAX_TOTAL_SOURCE_BYTES
        );
        assert_eq!(
            limits.max_prefix_bytes(),
            SharedContextLimits::DEFAULT_MAX_PREFIX_BYTES
        );
        assert_eq!(
            limits.max_allocated_bytes(),
            SharedContextLimits::DEFAULT_MAX_ALLOCATED_BYTES
        );
    }

    #[test]
    fn each_setter_changes_only_its_own_limit() {
        let base = SharedContextLimits::default();
        let sources = base.with_max_total_source_bytes(1);
        assert_eq!(sources.max_total_source_bytes(), 1);
        assert_eq!(sources.max_prefix_bytes(), base.max_prefix_bytes());
        assert_eq!(sources.max_allocated_bytes(), base.max_allocated_bytes());

        let prefix = base.with_max_prefix_bytes(2);
        assert_eq!(prefix.max_prefix_bytes(), 2);
        assert_eq!(
            prefix.max_total_source_bytes(),
            base.max_total_source_bytes()
        );

        let allocated = base.with_max_allocated_bytes(3);
        assert_eq!(allocated.max_allocated_bytes(), 3);
        assert_eq!(allocated.max_prefix_bytes(), base.max_prefix_bytes());
    }

    #[test]
    fn the_budget_carries_every_public_limit() {
        let limits = SharedContextLimits::default()
            .with_max_total_source_bytes(11)
            .with_max_prefix_bytes(22)
            .with_max_allocated_bytes(33);
        let budget = Budget::from(limits);
        assert_eq!(budget.max_total_source_bytes, 11);
        assert_eq!(budget.max_prefix_bytes, 22);
        assert_eq!(budget.max_allocated_bytes, 33);
    }
}
