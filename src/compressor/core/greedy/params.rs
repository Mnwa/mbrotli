//! Sanitised encoder parameters and the deterministic hasher plan.
//!
//! Ports `SanitizeParams`, `ComputeLgBlock`, `ComputeRbBits`,
//! `MaxMetablockSize` and `ChooseHasher` from `c/enc/quality.h`, together with
//! `ChooseDistanceParams` from `c/enc/encode.c` and
//! `BrotliInitDistanceParams` from `c/enc/metablock.c` of the pinned reference
//! (`google/brotli` v1.2.0, commit `028fb5a`).
//!
//! Everything here is resolved once, before any hot loop runs: the quality
//! routing, the block sizes, the distance alphabet and the matcher choice are
//! all functions of the caller's parameters alone. In particular the matcher
//! never depends on the instruction set, which is what keeps the output
//! identical across SIMD backends.

use crate::compressor::core::shared::constants::WINDOW_GAP;
use crate::compressor::{
    BrotliCompressError, CompressMode, CompressParams, DistanceCodes, QualityLevel,
};

/// Largest number of postfix bits the format allows (`BROTLI_MAX_NPOSTFIX`).
pub(crate) const MAX_NPOSTFIX: u32 = 3;

/// Largest number of direct distance codes (`BROTLI_MAX_NDIRECT`).
pub(crate) const MAX_NDIRECT: u32 = 120;

/// Number of short distance codes (`BROTLI_NUM_DISTANCE_SHORT_CODES`).
pub(crate) const NUM_DISTANCE_SHORT_CODES: u32 = 16;

/// Largest number of distance bits in RFC 7932 (`BROTLI_MAX_DISTANCE_BITS`).
pub(crate) const MAX_DISTANCE_BITS: u32 = 24;

/// Distance symbols a histogram reserves
/// (`BROTLI_NUM_HISTOGRAM_DISTANCE_SYMBOLS`).
pub(crate) const NUM_HISTOGRAM_DISTANCE_SYMBOLS: usize = 544;

/// Distance alphabet size assuming no postfix or direct codes.
///
/// `MAX_SIMPLE_DISTANCE_ALPHABET_SIZE` in `c/enc/brotli_bit_stream.c`, computed
/// for the large-window bit count even though this encoder never uses it.
pub(crate) const MAX_SIMPLE_DISTANCE_ALPHABET_SIZE: usize = 140;

/// Smallest quality that splits meta-blocks into blocks.
pub(crate) const MIN_QUALITY_FOR_BLOCK_SPLIT: usize = 4;

/// Smallest quality that searches every candidate at a delayed position.
pub(crate) const MIN_QUALITY_FOR_EXTENSIVE_REFERENCE_SEARCH: usize = 5;

/// Smallest quality that models literal contexts.
pub(crate) const MIN_QUALITY_FOR_CONTEXT_MODELING: usize = 5;

/// Symbols buffered before a quality below four has to flush.
pub(crate) const MAX_NUM_DELAYED_SYMBOLS: usize = 0x2FFF;

/// The three qualities this encoder implements.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum GreedyQuality {
    /// Quick matcher, trivial meta-block storage.
    Q3,
    /// Block splitting, histogram optimisation, distance parameters.
    Q4,
    /// Extensive search and literal context modelling.
    Q5,
}

impl GreedyQuality {
    /// Returns the numeric quality the reference formulas are written in.
    pub(crate) const fn number(self) -> usize {
        match self {
            Self::Q3 => 3,
            Self::Q4 => 4,
            Self::Q5 => 5,
        }
    }

    /// Returns whether this quality splits a meta-block into blocks.
    pub(crate) const fn splits_blocks(self) -> bool {
        self.number() >= MIN_QUALITY_FOR_BLOCK_SPLIT
    }

    /// Returns whether a delayed search re-examines every candidate.
    pub(crate) const fn extensive_reference_search(self) -> bool {
        self.number() >= MIN_QUALITY_FOR_EXTENSIVE_REFERENCE_SEARCH
    }

    /// Returns whether this quality may model literal contexts at all.
    pub(crate) const fn models_literal_contexts(self) -> bool {
        self.number() >= MIN_QUALITY_FOR_CONTEXT_MODELING
    }
}

impl TryFrom<QualityLevel> for GreedyQuality {
    type Error = BrotliCompressError;

    /// Routes qualities three to five to the greedy path.
    ///
    /// # Errors
    ///
    /// Returns [`BrotliCompressError::UnsupportedQuality`] for every other
    /// quality, which belongs to a different encoder.
    fn try_from(value: QualityLevel) -> Result<Self, Self::Error> {
        match value {
            QualityLevel::Q3 => Ok(Self::Q3),
            QualityLevel::Q4 => Ok(Self::Q4),
            QualityLevel::Q5 => Ok(Self::Q5),
            other => Err(BrotliCompressError::UnsupportedQuality(usize::from(other))),
        }
    }
}

/// Resolved distance alphabet (`BrotliDistanceParams`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct DistanceParams {
    /// Number of postfix bits, `NPOSTFIX`.
    pub(crate) postfix_bits: u32,
    /// Number of direct distance codes, `NDIRECT`.
    pub(crate) num_direct: u32,
    /// Size of the distance alphabet that is written to the stream.
    pub(crate) alphabet_size_max: u32,
    /// Size of the distance alphabet that can actually occur.
    pub(crate) alphabet_size_limit: u32,
    /// Largest distance this alphabet can express.
    pub(crate) max_distance: u32,
}

impl DistanceParams {
    /// Builds the alphabet for `postfix_bits` and `num_direct`.
    ///
    /// Mirrors `BrotliInitDistanceParams` for the RFC 7932 window sizes; the
    /// large-window extension is not reachable through the public API, so the
    /// limit and the maximum alphabet always coincide.
    pub(crate) const fn new(postfix_bits: u32, num_direct: u32) -> Self {
        let alphabet_size_max =
            NUM_DISTANCE_SHORT_CODES + num_direct + (MAX_DISTANCE_BITS << (postfix_bits + 1));
        let max_distance = num_direct + (1u32 << (MAX_DISTANCE_BITS + postfix_bits + 2))
            - (1u32 << (postfix_bits + 2));
        Self {
            postfix_bits,
            num_direct,
            alphabet_size_max,
            alphabet_size_limit: alphabet_size_max,
            max_distance,
        }
    }
}

impl Default for DistanceParams {
    /// Returns the alphabet with neither postfix nor direct codes.
    fn default() -> Self {
        Self::new(0, 0)
    }
}

/// Chooses the distance alphabet for a quality, mode and caller request.
///
/// Mirrors `ChooseDistanceParams`: font mode asks for one postfix bit and
/// twelve direct codes, every other mode uses whatever the caller configured,
/// and a combination the format cannot express falls back to zero. Qualities
/// below four always use zero.
pub(crate) const fn choose_distance_params(
    quality: GreedyQuality,
    mode: CompressMode,
    codes: DistanceCodes,
) -> DistanceParams {
    let mut postfix_bits = 0u32;
    let mut num_direct = 0u32;

    if quality.number() >= MIN_QUALITY_FOR_BLOCK_SPLIT {
        if matches!(mode, CompressMode::Font) {
            postfix_bits = 1;
            num_direct = 12;
        } else {
            postfix_bits = codes.postfix_bits();
            num_direct = codes.direct_codes();
        }
        let ndirect_msb = (num_direct >> postfix_bits) & 0x0F;
        if postfix_bits > MAX_NPOSTFIX
            || num_direct > MAX_NDIRECT
            || (ndirect_msb << postfix_bits) != num_direct
        {
            postfix_bits = 0;
            num_direct = 0;
        }
    }

    DistanceParams::new(postfix_bits, num_direct)
}

/// The matcher a set of parameters selects, with its shape baked in.
///
/// `ChooseHasher` picks this from quality, window size and size hint only.
/// Nothing about the running machine takes part, so two runs with the same
/// parameters always search the same candidates in the same order.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum HasherPlan {
    /// Quality 3: quick matcher, sixteen bucket bits, one candidate slot.
    H3,
    /// Quality 4 below the size-hint threshold: quick matcher with a
    /// shallow static-dictionary probe.
    H4,
    /// Quality 4 at or above the size-hint threshold: wider quick matcher,
    /// seven-byte hash, no dictionary probe.
    H54,
    /// Quality 5 with a small window: forgetful chains over one bank.
    H40,
    /// Quality 5: bucketed matcher over a four-byte hash.
    H5 {
        /// Base-2 logarithm of the number of buckets.
        bucket_bits: u32,
        /// Base-2 logarithm of the number of slots per bucket.
        block_bits: u32,
        /// How many cached distances are probed before the bucket.
        last_distances: usize,
    },
    /// Quality 5 for large inputs: bucketed matcher over an eight-byte hash.
    H6 {
        /// Base-2 logarithm of the number of buckets.
        bucket_bits: u32,
        /// Base-2 logarithm of the number of slots per bucket.
        block_bits: u32,
        /// How many cached distances are probed before the bucket.
        last_distances: usize,
    },
}

/// Size hint at which quality four and five switch to their large matchers.
pub(crate) const LARGE_INPUT_SIZE_HINT: usize = 1 << 20;

/// Selects the matcher for `quality`, `lgwin` and `size_hint`.
///
/// Mirrors `ChooseHasher` restricted to qualities three to five. The
/// large-window matchers `H35`, `H55` and `H65` are unreachable because the
/// public window size stops at twenty-four bits.
pub(crate) const fn choose_hasher(
    quality: GreedyQuality,
    lgwin: usize,
    size_hint: usize,
) -> HasherPlan {
    match quality {
        GreedyQuality::Q3 => HasherPlan::H3,
        GreedyQuality::Q4 => {
            if size_hint >= LARGE_INPUT_SIZE_HINT {
                HasherPlan::H54
            } else {
                HasherPlan::H4
            }
        }
        GreedyQuality::Q5 => {
            if lgwin <= 16 {
                HasherPlan::H40
            } else if size_hint >= LARGE_INPUT_SIZE_HINT && lgwin >= 19 {
                HasherPlan::H6 {
                    bucket_bits: 15,
                    block_bits: 4,
                    last_distances: 4,
                }
            } else {
                HasherPlan::H5 {
                    bucket_bits: 14,
                    block_bits: 4,
                    last_distances: 4,
                }
            }
        }
    }
}

/// Every parameter the greedy encoder needs, already sanitised.
#[derive(Copy, Clone, Debug)]
pub(crate) struct GreedyParams {
    /// Quality this encoder runs at.
    pub(crate) quality: GreedyQuality,
    /// Base-2 logarithm of the sliding window.
    pub(crate) lgwin: usize,
    /// Base-2 logarithm of the input block size.
    pub(crate) lgblock: usize,
    /// Expected total input size, used only to select the matcher.
    pub(crate) size_hint: usize,
    /// Whether literal context modelling is switched off by the caller.
    pub(crate) disable_literal_context_modeling: bool,
    /// Resolved distance alphabet.
    pub(crate) dist: DistanceParams,
    /// Resolved matcher.
    pub(crate) hasher: HasherPlan,
}

impl GreedyParams {
    /// Resolves `params` for a stream whose size hint is `size_hint`.
    ///
    /// # Errors
    ///
    /// Returns [`BrotliCompressError::UnsupportedQuality`] when the quality is
    /// outside the range this encoder implements.
    pub(crate) fn new(
        params: &CompressParams,
        size_hint: usize,
    ) -> Result<Self, BrotliCompressError> {
        let quality = GreedyQuality::try_from(params.quality())?;
        let lgwin = usize::from(params.lgwin());
        let lgblock = compute_lgblock(quality, params.lgblock().map(usize::from));
        Ok(Self {
            quality,
            lgwin,
            lgblock,
            size_hint,
            disable_literal_context_modeling: !params.literal_context_modeling(),
            dist: choose_distance_params(quality, params.mode(), params.distance_codes()),
            hasher: choose_hasher(quality, lgwin, size_hint),
        })
    }

    /// Returns the number of bytes one `process` call may consume.
    pub(crate) const fn input_block_size(&self) -> usize {
        1usize << self.lgblock
    }

    /// Returns the ring buffer's window size in bits (`ComputeRbBits`).
    pub(crate) const fn rb_bits(&self) -> usize {
        1 + if self.lgwin > self.lgblock {
            self.lgwin
        } else {
            self.lgblock
        }
    }

    /// Returns the largest meta-block this encoder emits.
    pub(crate) const fn max_metablock_size(&self) -> usize {
        let bits = if self.rb_bits() < MAX_INPUT_BLOCK_BITS {
            self.rb_bits()
        } else {
            MAX_INPUT_BLOCK_BITS
        };
        1usize << bits
    }

    /// Returns the largest backward distance (`BROTLI_MAX_BACKWARD_LIMIT`).
    pub(crate) const fn max_backward_limit(&self) -> usize {
        (1usize << self.lgwin) - WINDOW_GAP
    }

    /// Returns the literal spree length that triggers the sparse search.
    ///
    /// `LiteralSpreeLengthForSparseSearch` uses sixty-four below quality nine.
    pub(crate) const fn random_heuristics_window_size(&self) -> usize {
        64
    }
}

/// Smallest explicit input block size, in bits.
pub(crate) const MIN_INPUT_BLOCK_BITS: usize = 16;

/// Largest input block size, in bits.
pub(crate) const MAX_INPUT_BLOCK_BITS: usize = 24;

/// Returns the block size `quality` uses (`ComputeLgBlock`).
///
/// Qualities below four always use fourteen bits, whatever the caller asked
/// for; the others default to sixteen and clamp an explicit request into the
/// range the format allows.
pub(crate) const fn compute_lgblock(quality: GreedyQuality, requested: Option<usize>) -> usize {
    if quality.number() < MIN_QUALITY_FOR_BLOCK_SPLIT {
        return 14;
    }
    match requested {
        None => 16,
        Some(lgblock) => {
            if lgblock < MIN_INPUT_BLOCK_BITS {
                MIN_INPUT_BLOCK_BITS
            } else if lgblock > MAX_INPUT_BLOCK_BITS {
                MAX_INPUT_BLOCK_BITS
            } else {
                lgblock
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compressor::{BlockBits, WindowBits};

    #[test]
    fn quality_routing_accepts_only_the_greedy_range() {
        assert_eq!(
            GreedyQuality::try_from(QualityLevel::Q3).ok(),
            Some(GreedyQuality::Q3)
        );
        assert_eq!(
            GreedyQuality::try_from(QualityLevel::Q5).ok(),
            Some(GreedyQuality::Q5)
        );
        assert!(matches!(
            GreedyQuality::try_from(QualityLevel::Q2),
            Err(BrotliCompressError::UnsupportedQuality(2))
        ));
        assert!(matches!(
            GreedyQuality::try_from(QualityLevel::Q6),
            Err(BrotliCompressError::UnsupportedQuality(6))
        ));
    }

    #[test]
    fn quality_three_never_splits_and_never_searches_extensively() {
        assert!(!GreedyQuality::Q3.splits_blocks());
        assert!(GreedyQuality::Q4.splits_blocks());
        assert!(!GreedyQuality::Q4.extensive_reference_search());
        assert!(GreedyQuality::Q5.extensive_reference_search());
    }

    #[test]
    fn only_quality_five_models_literal_contexts() {
        assert!(!GreedyQuality::Q3.models_literal_contexts());
        assert!(!GreedyQuality::Q4.models_literal_contexts());
        assert!(GreedyQuality::Q5.models_literal_contexts());
    }

    #[test]
    fn lgblock_is_fourteen_below_the_block_split_quality() {
        assert_eq!(compute_lgblock(GreedyQuality::Q3, None), 14);
        assert_eq!(compute_lgblock(GreedyQuality::Q3, Some(24)), 14);
        assert_eq!(compute_lgblock(GreedyQuality::Q4, None), 16);
        assert_eq!(compute_lgblock(GreedyQuality::Q5, None), 16);
        assert_eq!(compute_lgblock(GreedyQuality::Q5, Some(18)), 18);
        assert_eq!(compute_lgblock(GreedyQuality::Q5, Some(10)), 16);
        assert_eq!(compute_lgblock(GreedyQuality::Q5, Some(30)), 24);
    }

    #[test]
    fn hasher_selection_follows_the_size_hint_and_window() {
        assert_eq!(choose_hasher(GreedyQuality::Q3, 22, 0), HasherPlan::H3);
        assert_eq!(
            choose_hasher(GreedyQuality::Q3, 22, LARGE_INPUT_SIZE_HINT),
            HasherPlan::H3
        );

        assert_eq!(
            choose_hasher(GreedyQuality::Q4, 22, LARGE_INPUT_SIZE_HINT - 1),
            HasherPlan::H4
        );
        assert_eq!(
            choose_hasher(GreedyQuality::Q4, 22, LARGE_INPUT_SIZE_HINT),
            HasherPlan::H54
        );
        assert_eq!(
            choose_hasher(GreedyQuality::Q4, 22, LARGE_INPUT_SIZE_HINT + 1),
            HasherPlan::H54
        );

        assert_eq!(choose_hasher(GreedyQuality::Q5, 16, 0), HasherPlan::H40);
        assert_eq!(
            choose_hasher(GreedyQuality::Q5, 16, LARGE_INPUT_SIZE_HINT),
            HasherPlan::H40
        );
        assert!(matches!(
            choose_hasher(GreedyQuality::Q5, 17, 0),
            HasherPlan::H5 {
                bucket_bits: 14,
                block_bits: 4,
                last_distances: 4
            }
        ));
        assert!(matches!(
            choose_hasher(GreedyQuality::Q5, 18, LARGE_INPUT_SIZE_HINT),
            HasherPlan::H5 { .. }
        ));
        assert!(matches!(
            choose_hasher(GreedyQuality::Q5, 19, LARGE_INPUT_SIZE_HINT - 1),
            HasherPlan::H5 { .. }
        ));
        assert!(matches!(
            choose_hasher(GreedyQuality::Q5, 19, LARGE_INPUT_SIZE_HINT),
            HasherPlan::H6 {
                bucket_bits: 15,
                block_bits: 4,
                last_distances: 4
            }
        ));
    }

    #[test]
    fn distance_alphabet_matches_the_reference_formulas() {
        let default = DistanceParams::default();
        assert_eq!(default.alphabet_size_max, 64);
        assert_eq!(default.alphabet_size_limit, 64);
        assert_eq!(default.max_distance, 0x3FF_FFFC);

        let font = DistanceParams::new(1, 12);
        assert_eq!(font.alphabet_size_max, 16 + 12 + (24 << 2));
        assert_eq!(font.postfix_bits, 1);
    }

    #[test]
    fn font_mode_asks_for_the_reference_distance_parameters() {
        let font = choose_distance_params(
            GreedyQuality::Q4,
            CompressMode::Font,
            DistanceCodes::DEFAULT,
        );
        assert_eq!((font.postfix_bits, font.num_direct), (1, 12));

        let generic = choose_distance_params(
            GreedyQuality::Q4,
            CompressMode::Generic,
            DistanceCodes::DEFAULT,
        );
        assert_eq!((generic.postfix_bits, generic.num_direct), (0, 0));

        // Quality three never uses non-zero distance parameters.
        let low = choose_distance_params(
            GreedyQuality::Q3,
            CompressMode::Font,
            DistanceCodes::DEFAULT,
        );
        assert_eq!((low.postfix_bits, low.num_direct), (0, 0));
    }

    #[test]
    fn invalid_distance_parameters_fall_back_to_zero() {
        // `ndirect` must be a multiple of `1 << npostfix` and its high nibble
        // must fit in four bits; the reference drops both otherwise.
        let bad = choose_distance_params(
            GreedyQuality::Q5,
            CompressMode::Generic,
            DistanceCodes::from_raw(2, 6),
        );
        assert_eq!((bad.postfix_bits, bad.num_direct), (0, 0));

        let good = choose_distance_params(
            GreedyQuality::Q5,
            CompressMode::Generic,
            DistanceCodes::from_raw(2, 8),
        );
        assert_eq!((good.postfix_bits, good.num_direct), (2, 8));

        let too_many = choose_distance_params(
            GreedyQuality::Q5,
            CompressMode::Generic,
            DistanceCodes::from_raw(0, 121),
        );
        assert_eq!((too_many.postfix_bits, too_many.num_direct), (0, 0));
    }

    #[test]
    fn derived_sizes_follow_the_window_and_block() -> Result<(), BrotliCompressError> {
        let params = CompressParams::new(QualityLevel::Q5, WindowBits::DEFAULT);
        let resolved = GreedyParams::new(&params, 0)?;
        assert_eq!(resolved.lgblock, 16);
        assert_eq!(resolved.input_block_size(), 1 << 16);
        assert_eq!(resolved.rb_bits(), 23);
        assert_eq!(resolved.max_metablock_size(), 1 << 23);
        assert_eq!(resolved.max_backward_limit(), (1 << 22) - 16);
        assert_eq!(resolved.random_heuristics_window_size(), 64);
        Ok(())
    }

    #[test]
    fn a_huge_ring_buffer_is_capped_at_the_input_block_limit() -> Result<(), BrotliCompressError> {
        let params = CompressParams::new(QualityLevel::Q5, WindowBits::MAX)
            .with_block_bits(Some(BlockBits::MAX));
        let resolved = GreedyParams::new(&params, 0)?;
        assert_eq!(resolved.rb_bits(), 25);
        assert_eq!(resolved.max_metablock_size(), 1 << MAX_INPUT_BLOCK_BITS);
        Ok(())
    }
}
