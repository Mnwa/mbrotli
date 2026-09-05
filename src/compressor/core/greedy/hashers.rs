//! Match finders for qualities three to nine.
//!
//! Ports `hash_longest_match_quickly_inc.h` (H3, H4, H54),
//! `hash_longest_match_inc.h` (H5), `hash_longest_match64_inc.h` (H6) and
//! `hash_forgetful_chain_inc.h` (H40, H41, H42) from the pinned reference
//! (`google/brotli` v1.2.0, commit `028fb5a`).
//!
//! Which of them runs is decided once, from the caller's parameters, by
//! [`super::params::choose_hasher`]. The hash width and the bucket count are
//! compile-time constants of the matcher type, so the hash itself is a fixed
//! shift; the candidate depth, the chain depth and the number of cached
//! distances are ordinary fields, because they only bound loops and turning
//! five bucket depths into five monomorphisations would cost far more
//! instruction cache than the bound is worth.
//!
//! Qualities seven and up probe more than four cached distances, which is
//! where [`prepare_distance_cache`] earns its keep: the extra entries are
//! near misses derived from the two freshest distances.

use fearless_simd::Simd;

use super::params::{BucketShape, ChainShape, HasherPlan};
use crate::compressor::core::shared::constants::HASH_MUL32;
use crate::compressor::core::shared::dictionary::{self, DictionaryStats};
use crate::compressor::core::shared::match_len::find_match_length;
use crate::compressor::core::shared::score::{
    SearchResult, backward_reference_penalty_using_last_distance, backward_reference_score,
    backward_reference_score_using_last_distance,
};

/// Sixty-four-bit hash multiplier (`kHashMul64`).
const HASH_MUL64: u64 = 0x1FE3_5A7B_D357_9BD3;

/// The sixteen distances a search may probe (`BROTLI_NUM_DISTANCE_SHORT_CODES`).
///
/// Only the first four are real history; the rest are near misses derived from
/// them by [`prepare_distance_cache`].
pub(crate) type DistanceCache = [i32; 16];

/// The four cache entries the encoder actually remembers across meta-blocks.
pub(crate) const NUM_REMEMBERED_DISTANCES: usize = 4;

/// Distance cache the reference starts every stream with.
pub(crate) const INITIAL_DISTANCE_CACHE: DistanceCache =
    [4, 11, 15, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/// Fills the derived entries of the distance cache (`PrepareDistanceCache`).
///
/// A matcher that probes more than four distances also probes the values one,
/// two and three either side of the two freshest ones. They are recomputed
/// whenever the first four change, and left alone entirely when the matcher
/// only looks at those four.
#[inline]
pub(crate) fn prepare_distance_cache(cache: &mut DistanceCache, num_distances: usize) {
    if num_distances <= NUM_REMEMBERED_DISTANCES {
        return;
    }
    let last = cache[0];
    cache[4] = last - 1;
    cache[5] = last + 1;
    cache[6] = last - 2;
    cache[7] = last + 2;
    cache[8] = last - 3;
    cache[9] = last + 3;
    if num_distances > 10 {
        let next_last = cache[1];
        cache[10] = next_last - 1;
        cache[11] = next_last + 1;
        cache[12] = next_last - 2;
        cache[13] = next_last + 2;
        cache[14] = next_last - 3;
        cache[15] = next_last + 3;
    }
}

/// Reads eight little-endian bytes at `offset`, or zero past the end.
#[inline(always)]
fn read_u64(data: &[u8], offset: usize) -> u64 {
    match data.get(offset..).and_then(<[u8]>::first_chunk::<8>) {
        Some(chunk) => u64::from_le_bytes(*chunk),
        None => 0,
    }
}

/// Reads four little-endian bytes at `offset`, or zero past the end.
#[inline(always)]
fn read_u32(data: &[u8], offset: usize) -> u32 {
    match data.get(offset..).and_then(<[u8]>::first_chunk::<4>) {
        Some(chunk) => u32::from_le_bytes(*chunk),
        None => 0,
    }
}

/// Reads one byte at `offset`, or zero past the end.
#[inline(always)]
fn read_u8(data: &[u8], offset: usize) -> u8 {
    match data.get(offset) {
        Some(&byte) => byte,
        None => 0,
    }
}

/// Everything a match finder needs from its caller, gathered once.
///
/// Passed by value: it is a handful of words, and keeping it in registers is
/// what stops the search from rebuilding it in memory at every position.
#[derive(Copy, Clone)]
pub(crate) struct MatchQuery<'a> {
    #[cfg(feature = "experimental")]
    pub(crate) custom:
        Option<&'a crate::compressor::core::rfc9841::static_index::StaticCombination>,
    /// The ring buffer being searched.
    pub(crate) data: &'a [u8],
    /// Mask that turns an absolute position into a buffer index.
    pub(crate) mask: usize,
    /// The four distances that have short codes.
    pub(crate) cache: &'a DistanceCache,
    /// Absolute position the match would start at.
    pub(crate) cur_ix: usize,
    /// Longest match the remaining input allows.
    pub(crate) max_length: usize,
    /// Longest backward distance inside the window.
    pub(crate) max_backward: usize,
    /// Distance at which the static dictionary begins.
    pub(crate) dictionary_distance: usize,
    /// Longest distance the distance alphabet can express.
    pub(crate) max_distance: usize,
}

impl MatchQuery<'_> {
    fn search_dictionary(self, stats: &mut DictionaryStats, out: &mut SearchResult, shallow: bool) {
        let data = self.data.get(self.cur_ix & self.mask..).unwrap_or_default();
        #[cfg(feature = "experimental")]
        if let Some(custom) = self.custom {
            dictionary::search_custom(
                custom,
                stats,
                data,
                self.max_length,
                self.dictionary_distance,
                self.max_distance,
                out,
                shallow,
            );
            return;
        }
        dictionary::search(
            stats,
            data,
            self.max_length,
            self.dictionary_distance,
            self.max_distance,
            out,
            shallow,
        );
    }
}

/// A match finder over the ring buffer.
pub(crate) trait Matcher {
    /// Bytes a candidate needs available to be hashed (`HashTypeLength`).
    const HASH_TYPE_LENGTH: usize;

    /// Bytes a store needs available (`StoreLookahead`).
    const STORE_LOOKAHEAD: usize;

    /// Returns how many cached distances a search probes.
    ///
    /// Mirrors the `NUM_LAST_DISTANCES_TO_CHECK` a matcher was instantiated
    /// with; [`prepare_distance_cache`] needs it to decide how much of the
    /// cache to derive.
    fn last_distances_to_check(&self) -> usize {
        NUM_REMEMBERED_DISTANCES
    }

    /// Clears the table before the first block (`Prepare`).
    ///
    /// Returns whether the partial sweep was used — that is, whether only the
    /// slots the first `input_size` positions hash to were cleared, rather
    /// than the whole table. A caller that wants to reuse the matcher for
    /// another stream needs to know: replaying the same sweep afterwards
    /// clears exactly the slots that stream could have dirtied, which is far
    /// cheaper than wiping the table, while a full sweep leaves the table
    /// dirty enough that the next stream has to take the full path.
    fn prepare(&mut self, one_shot: bool, input_size: usize, data: &[u8]) -> bool;

    /// Records the position `ix` in the table (`Store`).
    fn store(&mut self, data: &[u8], mask: usize, ix: usize);

    /// Records every position in `start..end` (`StoreRange`).
    fn store_range(&mut self, data: &[u8], mask: usize, start: usize, end: usize) {
        for ix in start..end {
            self.store(data, mask, ix);
        }
    }

    /// Records the three positions that span the previous block boundary.
    ///
    /// Mirrors `StitchToPreviousBlock`: their hashes need bytes from both
    /// blocks, so they could not be computed when the previous block was
    /// processed.
    fn stitch_to_previous_block(
        &mut self,
        num_bytes: usize,
        position: usize,
        data: &[u8],
        mask: usize,
    ) {
        if num_bytes >= Self::HASH_TYPE_LENGTH - 1 && position >= 3 {
            self.store(data, mask, position - 3);
            self.store(data, mask, position - 2);
            self.store(data, mask, position - 1);
        }
    }

    /// Searches for the best match at `query.cur_ix` (`FindLongestMatch`).
    ///
    /// `out` is only improved, never worsened: a search that finds nothing
    /// leaves the incoming candidate in place.
    fn find_longest_match<S: Simd>(
        &mut self,
        simd: S,
        stats: &mut DictionaryStats,
        query: MatchQuery<'_>,
        out: &mut SearchResult,
    );
}

/// Quick match finder with one hash bucket sweep (`HashLongestMatchQuickly`).
///
/// `BUCKET_BITS` sizes the table, `SWEEP_BITS` says how many neighbouring slots
/// one hash owns, `HASH_LEN` how many bytes feed the hash, and `USE_DICTIONARY`
/// whether a miss falls back to the static dictionary.
pub(crate) struct QuickMatcher<
    const BUCKET_BITS: u32,
    const SWEEP_BITS: u32,
    const HASH_LEN: u32,
    const USE_DICTIONARY: bool,
> {
    buckets: Vec<u32>,
}

impl<const BUCKET_BITS: u32, const SWEEP_BITS: u32, const HASH_LEN: u32, const USE_DICTIONARY: bool>
    QuickMatcher<BUCKET_BITS, SWEEP_BITS, HASH_LEN, USE_DICTIONARY>
{
    /// Number of slots in the table.
    const BUCKET_SIZE: usize = 1usize << BUCKET_BITS;

    /// Mask that keeps a slot index inside the table.
    const BUCKET_MASK: usize = Self::BUCKET_SIZE - 1;

    /// Number of slots one hash sweeps over.
    const SWEEP: usize = 1usize << SWEEP_BITS;

    /// Mask picking the slot of the sweep a position is stored into.
    const SWEEP_MASK: usize = (Self::SWEEP - 1) << 3;

    /// Creates an empty table.
    /// Returns the bytes this match finder keeps allocated.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.buckets.capacity() * size_of::<u32>()
    }

    pub(crate) fn new() -> Self {
        Self {
            buckets: vec![0u32; Self::BUCKET_SIZE],
        }
    }

    /// Returns the bucket of the bytes at `offset` (`HashBytes`).
    #[inline(always)]
    fn hash(data: &[u8], offset: usize) -> usize {
        let value = read_u64(data, offset) << (64 - 8 * HASH_LEN as u64);
        (value.wrapping_mul(HASH_MUL64) >> (64 - BUCKET_BITS)) as usize
    }
}

impl<const BUCKET_BITS: u32, const SWEEP_BITS: u32, const HASH_LEN: u32, const USE_DICTIONARY: bool>
    Matcher for QuickMatcher<BUCKET_BITS, SWEEP_BITS, HASH_LEN, USE_DICTIONARY>
{
    const HASH_TYPE_LENGTH: usize = 8;
    const STORE_LOOKAHEAD: usize = 8;

    fn prepare(&mut self, one_shot: bool, input_size: usize, data: &[u8]) -> bool {
        // Clearing only the slots a short input can reach is far cheaper than
        // wiping the whole table, and reaches exactly the same slots the
        // search will later look at.
        let partial_prepare_threshold = Self::BUCKET_SIZE >> 5;
        let partial = one_shot && input_size <= partial_prepare_threshold;
        if partial {
            for offset in 0..input_size {
                let key = Self::hash(data, offset);
                if Self::SWEEP == 1 {
                    if let Some(slot) = self.buckets.get_mut(key) {
                        *slot = 0;
                    }
                } else {
                    for sweep in 0..Self::SWEEP {
                        if let Some(slot) = self
                            .buckets
                            .get_mut((key + (sweep << 3)) & Self::BUCKET_MASK)
                        {
                            *slot = 0;
                        }
                    }
                }
            }
        } else {
            self.buckets.fill(0);
        }
        partial
    }

    #[inline(always)]
    fn store(&mut self, data: &[u8], mask: usize, ix: usize) {
        let key = Self::hash(data, ix & mask);
        let slot = if Self::SWEEP == 1 {
            key
        } else {
            (key + (ix & Self::SWEEP_MASK)) & Self::BUCKET_MASK
        };
        if let Some(entry) = self.buckets.get_mut(slot) {
            *entry = ix as u32;
        }
    }

    #[inline(always)]
    fn find_longest_match<S: Simd>(
        &mut self,
        simd: S,
        stats: &mut DictionaryStats,
        query: MatchQuery<'_>,
        out: &mut SearchResult,
    ) {
        let data = query.data;
        let Some(buckets) = self.buckets.get_mut(..Self::BUCKET_SIZE) else {
            return;
        };
        let cur_ix_masked = query.cur_ix & query.mask;
        let best_len_in = out.len;
        let mut compare_char = read_u8(data, cur_ix_masked + best_len_in);
        let key = Self::hash(data, cur_ix_masked);
        let min_score = out.score;
        let mut best_score = out.score;
        let mut best_len = best_len_in;

        out.len_code_delta = 0;

        let cached_backward = query.cache[0] as usize;
        let prev_ix = query.cur_ix.wrapping_sub(cached_backward);
        if prev_ix < query.cur_ix {
            let prev_ix = prev_ix & query.mask;
            if compare_char == read_u8(data, prev_ix + best_len) {
                let len = find_match_length(simd, data, prev_ix, cur_ix_masked, query.max_length);
                if len >= 4 {
                    let score = backward_reference_score_using_last_distance(len);
                    if best_score < score {
                        out.len = len;
                        out.distance = cached_backward;
                        out.score = score;
                        if Self::SWEEP == 1 {
                            buckets[key & Self::BUCKET_MASK] = query.cur_ix as u32;
                            return;
                        }
                        best_len = len;
                        best_score = score;
                        compare_char = read_u8(data, cur_ix_masked + len);
                    }
                }
            }
        }

        // The slot the sweeping variant writes back to at the very end. The
        // single-slot variant has already written its own, which is why the
        // reference guards the trailing store with `BUCKET_SWEEP != 1`.
        let mut key_out = None;

        if Self::SWEEP == 1 {
            // Only one candidate: the store happens before the comparison, so
            // the slot always ends up holding the current position.
            let prev_ix = buckets[key & Self::BUCKET_MASK] as usize;
            buckets[key & Self::BUCKET_MASK] = query.cur_ix as u32;
            let backward = query.cur_ix - prev_ix;
            let prev_ix = prev_ix & query.mask;
            if compare_char != read_u8(data, prev_ix + best_len_in) {
                return;
            }
            if backward == 0 || backward > query.max_backward {
                return;
            }
            let len = find_match_length(simd, data, prev_ix, cur_ix_masked, query.max_length);
            if len >= 4 {
                let score = backward_reference_score(len, backward);
                if best_score < score {
                    out.len = len;
                    out.distance = backward;
                    out.score = score;
                    // A hit here is final: the reference returns rather than
                    // falling through to the dictionary.
                    return;
                }
            }
            // Anything else falls through to the dictionary search, which is
            // what `H2` — the only single-slot matcher that consults it — is
            // reached by.
        } else {
            let mut keys = [0usize; 4];
            for (sweep, slot) in keys.iter_mut().enumerate().take(Self::SWEEP) {
                *slot = (key + (sweep << 3)) & Self::BUCKET_MASK;
            }
            key_out = Some(keys[(query.cur_ix & Self::SWEEP_MASK) >> 3]);
            for &slot in keys.iter().take(Self::SWEEP) {
                let prev_ix = buckets[slot & Self::BUCKET_MASK] as usize;
                let backward = query.cur_ix - prev_ix;
                let prev_ix = prev_ix & query.mask;
                if compare_char != read_u8(data, prev_ix + best_len) {
                    continue;
                }
                if backward == 0 || backward > query.max_backward {
                    continue;
                }
                let len = find_match_length(simd, data, prev_ix, cur_ix_masked, query.max_length);
                if len >= 4 {
                    let score = backward_reference_score(len, backward);
                    if best_score < score {
                        best_len = len;
                        out.len = len;
                        compare_char = read_u8(data, cur_ix_masked + len);
                        best_score = score;
                        out.score = score;
                        out.distance = backward;
                    }
                }
            }
        }

        if USE_DICTIONARY && min_score == out.score {
            query.search_dictionary(stats, out, true);
        }
        if let Some(slot) = key_out {
            buckets[slot & Self::BUCKET_MASK] = query.cur_ix as u32;
        }
    }
}

/// Bucketed match finder keeping the most recent positions per hash.
///
/// `HASH64` selects the H6 variant, which hashes eight bytes instead of four
/// and pre-filters candidates on their first four bytes. `BUCKET_BITS` fixes
/// the table size, and therefore the hash shift, at compile time; the bucket
/// depth and the number of cached distances vary with quality and are fields.
///
/// # Equivalence with the tagged reference matchers
///
/// The reference builds `H58`/`H68` in place of `H5`/`H6` when
/// `BROTLI_MAX_SIMD_QUALITY` is defined. Those variants store a one-byte tag
/// beside every position and iterate only the slots whose tag matches the
/// current one. They select the same bucket — the tagged `HashBytes` merely
/// keeps eight more low bits, which the key shifts straight back off — and
/// they walk it newest to oldest, exactly as this loop does. A tag is a
/// function of the hashed bytes, so within the same bucket two positions whose
/// first four bytes agree share a tag; a slot the tag mask drops differs in those
/// four bytes, and a candidate that differs there can never reach the
/// reference's `len >= 4` acceptance test. Both matchers also stop at the
/// first candidate beyond `max_backward`, and positions grow monotonically
/// along the ring, so both stop having seen the same prefix of candidates.
/// The accepted-match sets coincide, and so do the streams. The SIMD backend
/// uses the tag mask; the scalar backend keeps the unfiltered scan as an oracle.
pub(crate) struct BucketMatcher<const HASH64: bool, const BUCKET_BITS: u32> {
    num: Vec<u16>,
    buckets: Vec<u32>,
    /// Compact block offsets plus one; zero means no payload was allocated.
    offsets: Vec<u32>,
    /// One initialized tag per compact position slot (H58/H68 rejection).
    tags: Vec<u8>,
    block_size: usize,
    block_mask: u16,
    last_distances: usize,
}

impl<const HASH64: bool, const BUCKET_BITS: u32> BucketMatcher<HASH64, BUCKET_BITS> {
    /// Number of buckets in the table.
    const BUCKET_SIZE: usize = 1usize << BUCKET_BITS;
    /// A four-position starter block; promoted once its fifth slot is needed.
    const SPARSE: u32 = 1 << 31;

    /// Creates an empty table of the shape `shape` describes.
    /// Returns the bytes this match finder keeps allocated.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.num.capacity() * size_of::<u16>()
            + (self.buckets.capacity() + self.offsets.capacity()) * size_of::<u32>()
            + self.tags.capacity()
    }

    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub(crate) fn new(shape: BucketShape) -> Self {
        debug_assert_eq!(shape.bucket_bits, BUCKET_BITS);
        let block_size = 1usize << shape.block_bits;
        Self {
            num: vec![0u16; Self::BUCKET_SIZE],
            buckets: Vec::new(),
            offsets: vec![0; Self::BUCKET_SIZE],
            tags: Vec::new(),
            block_size,
            block_mask: (block_size - 1) as u16,
            last_distances: shape.last_distances,
        }
    }

    /// Returns the bucket of the bytes at `offset` (`HashBytes`).
    #[inline(always)]
    fn hash(data: &[u8], offset: usize) -> usize {
        Self::hash_with_tag(data, offset) >> 8
    }

    /// The reference bucket hash plus eight rejection bits below its key.
    #[inline(always)]
    fn hash_with_tag(data: &[u8], offset: usize) -> usize {
        if HASH64 {
            // H6 tunes the multiplier to a five-byte match and always takes
            // fifteen bits, whatever the bucket count is.
            let hash_mul = HASH_MUL64 << (64 - 5 * 8);
            (read_u64(data, offset).wrapping_mul(hash_mul) >> (64 - 15 - 8)) as usize
        } else {
            (read_u32(data, offset).wrapping_mul(HASH_MUL32) >> (32 - BUCKET_BITS - 8)) as usize
        }
    }

    /// Activates one initialized compact block on first use. Existing offsets
    /// survive reset; counters alone govern which payload entries are valid.
    #[inline(always)]
    fn activate_bucket(&mut self, key: usize) -> usize {
        let offset = self.offsets[key];
        if offset != 0 {
            let old = ((offset & !Self::SPARSE) - 1) as usize;
            if offset & Self::SPARSE == 0 || self.num[key] < 4 {
                return old;
            }
            let start = self.buckets.len();
            self.buckets.resize(start + self.block_size, 0);
            self.buckets.copy_within(old..old + 4, start);
            self.offsets[key] = start as u32 + 1;
            return start;
        }
        let start = self.buckets.len();
        let sparse = self.block_size > 32;
        let initial_size = if sparse { 4 } else { self.block_size };
        self.buckets.resize(start + initial_size, 0);
        // Like the pinned C build, tag only q5/q6. Deeper buckets measured
        // slower with tag-mask traversal; retain their unfiltered search.
        if self.block_size <= 32 {
            self.tags.resize(start + self.block_size, 0);
        }
        // At most 2^15 buckets of (2^8 + 4) entries, below the flag bit.
        self.offsets[key] = (start as u32 + 1) | if sparse { Self::SPARSE } else { 0 };
        start
    }
}

impl<const HASH64: bool, const BUCKET_BITS: u32> Matcher for BucketMatcher<HASH64, BUCKET_BITS> {
    const HASH_TYPE_LENGTH: usize = if HASH64 { 8 } else { 4 };
    const STORE_LOOKAHEAD: usize = Self::HASH_TYPE_LENGTH;

    fn last_distances_to_check(&self) -> usize {
        self.last_distances
    }

    fn prepare(&mut self, one_shot: bool, input_size: usize, data: &[u8]) -> bool {
        let partial_prepare_threshold = Self::BUCKET_SIZE >> 6;
        let partial = one_shot && input_size <= partial_prepare_threshold;
        if partial {
            for offset in 0..input_size {
                let key = Self::hash(data, offset);
                if let Some(count) = self.num.get_mut(key) {
                    *count = 0;
                }
            }
        } else {
            self.num.fill(0);
        }
        // `buckets` is deliberately left alone, here and in the constructor's
        // sibling: a slot is only ever read below the counter that guards it,
        // and every counter this touched is now zero.
        partial
    }

    #[inline(always)]
    fn store(&mut self, data: &[u8], mask: usize, ix: usize) {
        let hash = Self::hash_with_tag(data, ix & mask);
        let key = hash >> 8;
        let base = self.activate_bucket(key);
        let Some(count) = self.num.get_mut(key) else {
            return;
        };
        let minor_ix = usize::from(*count & self.block_mask);
        *count = count.wrapping_add(1);
        if let Some(slot) = self.buckets.get_mut(minor_ix + base) {
            *slot = ix as u32;
        }
        if let Some(tag) = self.tags.get_mut(minor_ix + base) {
            *tag = hash as u8;
        }
    }

    #[inline(always)]
    fn find_longest_match<S: Simd>(
        &mut self,
        simd: S,
        stats: &mut DictionaryStats,
        query: MatchQuery<'_>,
        out: &mut SearchResult,
    ) {
        let data = query.data;
        let mask = query.mask;
        let cur_ix_masked = query.cur_ix & mask;
        let min_score = out.score;
        let mut best_score = out.score;
        let mut best_len = out.len;
        let hash = Self::hash_with_tag(data, cur_ix_masked);
        let key = hash >> 8;
        let bucket_base = self.activate_bucket(key);

        out.len = 0;
        out.len_code_delta = 0;

        for index in 0..self.last_distances {
            let backward = query.cache[index] as usize;
            let prev_ix = query.cur_ix.wrapping_sub(backward);
            if prev_ix >= query.cur_ix || backward > query.max_backward {
                continue;
            }
            let prev_ix = prev_ix & mask;
            if cur_ix_masked + best_len > mask {
                break;
            }
            if prev_ix + best_len > mask
                || read_u8(data, cur_ix_masked + best_len) != read_u8(data, prev_ix + best_len)
            {
                continue;
            }
            let len = find_match_length(simd, data, prev_ix, cur_ix_masked, query.max_length);
            // Two-byte matches are only worth scoring for the two freshest
            // cached distances; anything shorter never wins.
            if len >= 3 || (len == 2 && index < 2) {
                let mut score = backward_reference_score_using_last_distance(len);
                if best_score < score {
                    if index != 0 {
                        score -= backward_reference_penalty_using_last_distance(index);
                    }
                    if best_score < score {
                        best_score = score;
                        best_len = len;
                        out.len = best_len;
                        out.distance = backward;
                        out.score = best_score;
                    }
                }
            }
        }
        // Raising the floor to three lets the bucket loop compare four bytes
        // unconditionally.
        if best_len < 3 {
            best_len = 3;
        }

        let count = self.num.get(key).copied().unwrap_or(0);
        let mut candidates = super::tags::Candidates::new(count, self.block_size);
        let tags = self
            .tags
            .get(bucket_base..bucket_base + self.block_size)
            .unwrap_or_default();
        while let Some(index) = candidates.next(simd, tags, hash as u8) {
            let slot = bucket_base + index;
            let prev_ix = self.buckets.get(slot).copied().unwrap_or(0) as usize;
            let backward = query.cur_ix - prev_ix;
            if backward > query.max_backward {
                break;
            }
            let prev_ix = prev_ix & mask;
            if cur_ix_masked + best_len > mask {
                break;
            }
            if prev_ix + best_len > mask
                || read_u32(data, cur_ix_masked + best_len - 3)
                    != read_u32(data, prev_ix + best_len - 3)
            {
                continue;
            }
            let len = if HASH64 {
                if read_u32(data, cur_ix_masked) != read_u32(data, prev_ix) {
                    continue;
                }
                find_match_length(
                    simd,
                    data,
                    prev_ix + 4,
                    cur_ix_masked + 4,
                    query.max_length - 4,
                ) + 4
            } else {
                let len = find_match_length(simd, data, prev_ix, cur_ix_masked, query.max_length);
                if len < 4 {
                    continue;
                }
                len
            };
            let score = backward_reference_score(len, backward);
            if best_score < score {
                best_score = score;
                best_len = len;
                out.len = best_len;
                out.distance = backward;
                out.score = best_score;
            }
        }

        if let Some(counter) = self.num.get_mut(key) {
            let slot = bucket_base + usize::from(*counter & self.block_mask);
            if let Some(entry) = self.buckets.get_mut(slot) {
                *entry = query.cur_ix as u32;
            }
            if let Some(tag) = self.tags.get_mut(slot) {
                *tag = hash as u8;
            }
            *counter = counter.wrapping_add(1);
        }

        if min_score == out.score {
            query.search_dictionary(stats, out, false);
        }
    }
}

/// Number of buckets the forgetful chains hash into (`BUCKET_BITS` 15).
const CHAIN_BUCKET_BITS: u32 = 15;

/// Number of buckets the forgetful chain hashes into.
const CHAIN_BUCKET_SIZE: usize = 1 << CHAIN_BUCKET_BITS;

/// Address value that terminates a chain after its first node.
///
/// Positions never reach three gibibytes plus sixty-four mebibytes, so a
/// bucket seeded with this always produces a delta larger than any window.
const CHAIN_EMPTY_ADDR: u32 = 0xCCCC_CCCC;

/// Head value the partial preparation seeds a bucket with.
const CHAIN_EMPTY_HEAD: u16 = 0xCCCC;

/// One node of a forgetful chain.
#[derive(Copy, Clone, Debug, Default)]
struct ChainSlot {
    delta: u16,
    next: u16,
}

/// Forgetful-chain match finder (`HashForgetfulChain`: H40, H41, H42).
///
/// Chains share storage banks, so old nodes are overwritten rather than freed
/// and several chains may end up sharing a tail. A one-byte truncated hash
/// rejects cached-distance candidates before they are compared.
///
/// `NUM_BANKS` and `BANK_BITS` are compile-time because they decide the bank
/// index arithmetic in the inner hop loop: H40 and H41 keep one bank of
/// 65,536 slots, H42 five hundred and twelve banks of 512.
pub(crate) struct ChainMatcher<const NUM_BANKS: usize, const BANK_BITS: u32> {
    addr: Vec<u32>,
    head: Vec<u16>,
    tiny_hash: Vec<u8>,
    slots: Vec<ChainSlot>,
    /// Compact bank offsets plus one, retained across logical resets.
    bank_offsets: Vec<u32>,
    // Heap-allocated rather than a `[u16; NUM_BANKS]` field: H42 needs five
    // hundred and twelve of these, and inlining a kibibyte would make every
    // other `MatchFinder` variant carry the same footprint.
    free_slot_idx: Vec<u16>,
    last_distances: usize,
    max_hops: usize,
}

impl<const NUM_BANKS: usize, const BANK_BITS: u32> ChainMatcher<NUM_BANKS, BANK_BITS> {
    /// Slots one bank holds (`BANK_SIZE`).
    const BANK_SIZE: usize = 1usize << BANK_BITS;

    /// Mask that keeps a slot index inside its bank.
    const BANK_MASK: usize = Self::BANK_SIZE - 1;

    /// Mask that maps a bucket key onto a bank.
    const BANK_SELECT: usize = NUM_BANKS - 1;

    /// Creates an empty chain table of the shape `shape` describes.
    /// Returns the bytes this match finder keeps allocated.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.addr.capacity() * size_of::<u32>()
            + self.head.capacity() * size_of::<u16>()
            + self.tiny_hash.capacity()
            + self.slots.capacity() * size_of::<ChainSlot>()
            + self.bank_offsets.capacity() * size_of::<u32>()
            + self.free_slot_idx.capacity() * size_of::<u16>()
    }

    pub(crate) fn new(shape: ChainShape) -> Self {
        debug_assert_eq!(shape.num_banks, NUM_BANKS);
        debug_assert_eq!(shape.bank_bits, BANK_BITS);
        Self {
            addr: vec![CHAIN_EMPTY_ADDR; CHAIN_BUCKET_SIZE],
            head: vec![0u16; CHAIN_BUCKET_SIZE],
            tiny_hash: vec![0u8; 1 << 16],
            slots: Vec::new(),
            bank_offsets: vec![0; NUM_BANKS],
            free_slot_idx: vec![0u16; NUM_BANKS],
            last_distances: shape.last_distances,
            max_hops: shape.max_hops,
        }
    }

    /// Returns the bucket of the bytes at `offset` (`HashBytes`).
    #[inline(always)]
    fn hash(data: &[u8], offset: usize) -> usize {
        (read_u32(data, offset).wrapping_mul(HASH_MUL32) >> (32 - CHAIN_BUCKET_BITS)) as usize
    }

    /// Materializes a bank without changing its circular slot numbering.
    #[inline(always)]
    fn activate_bank(&mut self, bank: usize) -> usize {
        let offset = self.bank_offsets[bank];
        if offset != 0 {
            return (offset - 1) as usize;
        }
        let start = self.slots.len();
        self.slots
            .resize(start + Self::BANK_SIZE, ChainSlot::default());
        self.bank_offsets[bank] = start as u32 + 1;
        start
    }
}

impl<const NUM_BANKS: usize, const BANK_BITS: u32> Matcher for ChainMatcher<NUM_BANKS, BANK_BITS> {
    const HASH_TYPE_LENGTH: usize = 4;
    const STORE_LOOKAHEAD: usize = 4;

    fn last_distances_to_check(&self) -> usize {
        self.last_distances
    }

    fn prepare(&mut self, one_shot: bool, input_size: usize, data: &[u8]) -> bool {
        let partial_prepare_threshold = CHAIN_BUCKET_SIZE >> 6;
        let partial = one_shot && input_size <= partial_prepare_threshold;
        if partial {
            for offset in 0..input_size {
                let bucket = Self::hash(data, offset);
                if let Some(slot) = self.addr.get_mut(bucket) {
                    *slot = CHAIN_EMPTY_ADDR;
                }
                if let Some(slot) = self.head.get_mut(bucket) {
                    *slot = CHAIN_EMPTY_HEAD;
                }
            }
        } else {
            self.addr.fill(CHAIN_EMPTY_ADDR);
            self.head.fill(0);
        }
        self.tiny_hash.fill(0);
        self.free_slot_idx.fill(0);
        // `slots` is left alone: a chain is only entered through `addr`, and
        // every entry this cleared now reads as empty.
        partial
    }

    #[inline(always)]
    fn store(&mut self, data: &[u8], mask: usize, ix: usize) {
        let key = Self::hash(data, ix & mask);
        let bank = key & Self::BANK_SELECT;
        let bank_base = self.activate_bank(bank);
        let free = self.free_slot_idx.get_mut(bank).map_or(0u16, |slot| {
            let current = *slot;
            *slot = current.wrapping_add(1);
            current
        });
        let idx = usize::from(free) & Self::BANK_MASK;
        let previous = self.addr.get(key).copied().unwrap_or(CHAIN_EMPTY_ADDR);
        let delta = ix.wrapping_sub(previous as usize);
        if let Some(slot) = self.tiny_hash.get_mut(ix as u16 as usize) {
            *slot = key as u8;
        }
        let delta = if delta > 0xFFFF { 0xFFFF } else { delta as u16 };
        let head = self.head.get(key).copied().unwrap_or(0);
        if let Some(slot) = self.slots.get_mut(bank_base + idx) {
            slot.delta = delta;
            slot.next = head;
        }
        if let Some(slot) = self.addr.get_mut(key) {
            *slot = ix as u32;
        }
        if let Some(slot) = self.head.get_mut(key) {
            *slot = idx as u16;
        }
    }

    #[inline(always)]
    fn find_longest_match<S: Simd>(
        &mut self,
        simd: S,
        stats: &mut DictionaryStats,
        query: MatchQuery<'_>,
        out: &mut SearchResult,
    ) {
        let data = query.data;
        let mask = query.mask;
        let cur_ix_masked = query.cur_ix & mask;
        let min_score = out.score;
        let mut best_score = out.score;
        let mut best_len = out.len;
        let key = Self::hash(data, cur_ix_masked);
        let tiny_hash = key as u8;

        out.len = 0;
        out.len_code_delta = 0;

        for index in 0..self.last_distances {
            let backward = query.cache[index] as usize;
            let prev_ix = query.cur_ix.wrapping_sub(backward);
            // Distance code zero is worth trying even for a two-byte match, so
            // it skips the truncated-hash rejection.
            if index > 0
                && self
                    .tiny_hash
                    .get(prev_ix as u16 as usize)
                    .copied()
                    .unwrap_or(0)
                    != tiny_hash
            {
                continue;
            }
            if prev_ix >= query.cur_ix || backward > query.max_backward {
                continue;
            }
            let prev_ix = prev_ix & mask;
            let len = find_match_length(simd, data, prev_ix, cur_ix_masked, query.max_length);
            if len >= 2 {
                let mut score = backward_reference_score_using_last_distance(len);
                if best_score < score {
                    if index != 0 {
                        score -= backward_reference_penalty_using_last_distance(index);
                    }
                    if best_score < score {
                        best_score = score;
                        best_len = len;
                        out.len = best_len;
                        out.distance = backward;
                        out.score = best_score;
                    }
                }
            }
        }
        if best_len < 3 {
            best_len = 3;
        }

        let bank = key & Self::BANK_SELECT;
        let bank_base = self.activate_bank(bank);
        let mut backward = 0usize;
        let mut delta = query
            .cur_ix
            .wrapping_sub(self.addr.get(key).copied().unwrap_or(CHAIN_EMPTY_ADDR) as usize);
        let mut slot = usize::from(self.head.get(key).copied().unwrap_or(0));
        for _ in 0..self.max_hops {
            let last = slot;
            backward = backward.wrapping_add(delta);
            if backward > query.max_backward {
                break;
            }
            let prev_ix = (query.cur_ix.wrapping_sub(backward)) & mask;
            let node = self
                .slots
                .get(bank_base + (last & Self::BANK_MASK))
                .copied()
                .unwrap_or_default();
            slot = usize::from(node.next);
            delta = usize::from(node.delta);
            if cur_ix_masked + best_len > mask
                || prev_ix + best_len > mask
                || read_u32(data, cur_ix_masked + best_len - 3)
                    != read_u32(data, prev_ix + best_len - 3)
            {
                continue;
            }
            let len = find_match_length(simd, data, prev_ix, cur_ix_masked, query.max_length);
            if len >= 4 {
                let score = backward_reference_score(len, backward);
                if best_score < score {
                    best_score = score;
                    best_len = len;
                    out.len = best_len;
                    out.distance = backward;
                    out.score = best_score;
                }
            }
        }
        self.store(data, mask, query.cur_ix);

        if out.score == min_score {
            query.search_dictionary(stats, out, false);
        }
    }
}

/// The match finder a stream is using, chosen once from its parameters.
///
/// The tagged reference matchers `H58` and `H68` are not separate variants:
/// they are byte-for-byte equivalent to `H5` and `H6`, as argued on
/// [`BucketMatcher`].
pub(crate) enum MatchFinder {
    /// Quality 2: one candidate slot per bucket, with a dictionary probe.
    H2(QuickMatcher<16, 0, 5, true>),
    /// Quality 3.
    H3(QuickMatcher<16, 1, 5, false>),
    /// Quality 4, small inputs.
    H4(QuickMatcher<17, 2, 5, true>),
    /// Quality 4, large inputs.
    H54(QuickMatcher<20, 2, 7, false>),
    /// Qualities 5 to 8, small windows: `H40` and `H41`.
    H40(ChainMatcher<1, 16>),
    /// Quality 9, small windows: `H42`.
    H42(ChainMatcher<512, 9>),
    /// Qualities 5 and 6, ordinary inputs: fourteen bucket bits.
    H5Narrow(BucketMatcher<false, 14>),
    /// Qualities 7 to 9, ordinary inputs: fifteen bucket bits.
    H5Wide(BucketMatcher<false, 15>),
    /// Qualities 5 to 9, large inputs and wide windows.
    H6(BucketMatcher<true, 15>),
}

impl From<HasherPlan> for MatchFinder {
    /// Allocates the match finder a plan calls for.
    fn from(plan: HasherPlan) -> Self {
        match plan {
            HasherPlan::H2 => Self::H2(QuickMatcher::new()),
            HasherPlan::H3 => Self::H3(QuickMatcher::new()),
            HasherPlan::H4 => Self::H4(QuickMatcher::new()),
            HasherPlan::H54 => Self::H54(QuickMatcher::new()),
            HasherPlan::Chain(shape) => {
                if shape.num_banks == 1 {
                    Self::H40(ChainMatcher::new(shape))
                } else {
                    Self::H42(ChainMatcher::new(shape))
                }
            }
            HasherPlan::H5(shape) => {
                if shape.bucket_bits == 14 {
                    Self::H5Narrow(BucketMatcher::new(shape))
                } else {
                    Self::H5Wide(BucketMatcher::new(shape))
                }
            }
            HasherPlan::H6(shape) => Self::H6(BucketMatcher::new(shape)),
        }
    }
}

/// Runs `body` on whichever concrete matcher `finder` holds.
///
/// The dispatch happens once per block; everything inside `body` is
/// monomorphised on the matcher type it was handed.
macro_rules! with_matcher {
    ($finder:expr, |$matcher:ident| $body:expr) => {
        match $finder {
            MatchFinder::H2($matcher) => $body,
            MatchFinder::H3($matcher) => $body,
            MatchFinder::H4($matcher) => $body,
            MatchFinder::H54($matcher) => $body,
            MatchFinder::H40($matcher) => $body,
            MatchFinder::H42($matcher) => $body,
            MatchFinder::H5Narrow($matcher) => $body,
            MatchFinder::H5Wide($matcher) => $body,
            MatchFinder::H6($matcher) => $body,
        }
    };
}

pub(crate) use with_matcher;

impl MatchFinder {
    /// Clears the table before the first block of a stream (`Prepare`).
    ///
    /// Returns whether only the slots the input reaches were cleared; see
    /// [`Matcher::prepare`].
    pub(crate) fn prepare(&mut self, one_shot: bool, input_size: usize, data: &[u8]) -> bool {
        with_matcher!(self, |matcher| matcher.prepare(one_shot, input_size, data))
    }

    /// Returns the bytes the chosen match finder keeps allocated.
    pub(crate) fn retained_bytes(&self) -> usize {
        with_matcher!(self, |matcher| matcher.retained_bytes())
    }

    /// Records the positions spanning the previous block boundary.
    pub(crate) fn stitch_to_previous_block(
        &mut self,
        num_bytes: usize,
        position: usize,
        data: &[u8],
        mask: usize,
    ) {
        with_matcher!(self, |matcher| matcher
            .stitch_to_previous_block(num_bytes, position, data, mask));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cold_q9_chain_allocates_only_the_bank_it_uses() {
        let mut matcher = ChainMatcher::<512, 9>::new(Q9_CHAIN);
        assert!(matcher.slots.is_empty());
        let data = [b'a'; 64];
        matcher.store(&data, usize::MAX, 0);
        assert_eq!(matcher.slots.len(), 512);
        matcher.store(&data, usize::MAX, 1);
        assert_eq!(matcher.slots.len(), 512);
        let retained = matcher.retained_bytes();
        matcher.prepare(false, data.len(), &data);
        matcher.store(&data, usize::MAX, 0);
        assert_eq!(matcher.retained_bytes(), retained);
    }

    #[test]
    fn a_cold_q9_bucket_table_allocates_payload_only_when_activated() {
        let mut matcher = BucketMatcher::<false, 15>::new(Q9_BUCKET);
        assert!(matcher.retained_bytes() < 256 * 1024);
        let data = [b'a'; 64];
        matcher.store(&data, usize::MAX, 0);
        assert_eq!(matcher.buckets.len(), 4);
        matcher.store(&data, usize::MAX, 1);
        assert_eq!(matcher.buckets.len(), 4);
    }

    #[test]
    fn a_sparse_bucket_promotes_without_losing_its_recent_positions() {
        let mut matcher = BucketMatcher::<false, 15>::new(Q9_BUCKET);
        let data = [b'a'; 64];
        for position in 0..5 {
            matcher.store(&data, usize::MAX, position);
        }
        let base = matcher.activate_bucket(BucketMatcher::<false, 15>::hash(&data, 0));
        assert_eq!(&matcher.buckets[base..base + 5], &[0, 1, 2, 3, 4]);
        let capacity = matcher.buckets.capacity();
        matcher.prepare(false, data.len(), &data);
        matcher.store(&data, usize::MAX, 0);
        assert_eq!(matcher.buckets.capacity(), capacity);
    }
    use fearless_simd::{Level, dispatch};

    /// A payload whose last third repeats the middle third.
    ///
    /// The head deliberately differs, so position zero — which an empty table
    /// hands back for every bucket — is never a match by accident.
    fn repeated() -> Vec<u8> {
        let mut data: Vec<u8> = (0..64u32).map(|i| (i % 97) as u8 + 128).collect();
        let body: Vec<u8> = (0..64u32).map(|i| (i * 7 % 251) as u8 + 1).collect();
        data.extend_from_slice(&body);
        data.extend_from_slice(&body);
        data.extend_from_slice(&[0u8; 8]);
        data
    }

    /// Position the repeat starts at, and the distance back to its original.
    const REPEAT_AT: usize = 128;

    fn query<'a>(data: &'a [u8], cache: &'a DistanceCache, cur_ix: usize) -> MatchQuery<'a> {
        MatchQuery {
            #[cfg(feature = "experimental")]
            custom: None,
            data,
            mask: usize::MAX,
            cache,
            cur_ix,
            max_length: data.len() - cur_ix,
            max_backward: cur_ix,
            dictionary_distance: cur_ix,
            max_distance: u32::MAX as usize,
        }
    }

    fn search_at<M: Matcher>(matcher: &mut M, data: &[u8], cur_ix: usize) -> SearchResult {
        search_with(Level::new(), matcher, data, cur_ix)
    }

    fn search_with<M: Matcher>(
        level: Level,
        matcher: &mut M,
        data: &[u8],
        cur_ix: usize,
    ) -> SearchResult {
        let cache = INITIAL_DISTANCE_CACHE;
        let mut out = SearchResult::empty();
        let mut stats = DictionaryStats::default();
        let query = query(data, &cache, cur_ix);
        dispatch!(level, simd => matcher.find_longest_match(simd, &mut stats, query, &mut out));
        out
    }

    /// Fills a matcher with the first `REPEAT_AT` positions of `data`.
    fn primed<M: Matcher>(mut matcher: M, data: &[u8]) -> M {
        matcher.prepare(true, data.len(), data);
        matcher.store_range(data, usize::MAX, 0, REPEAT_AT);
        matcher
    }

    /// The bucket shape quality five resolves to.
    const Q5_BUCKET: BucketShape = BucketShape {
        bucket_bits: 14,
        block_bits: 4,
        last_distances: 4,
    };

    /// The bucket shape quality nine resolves to.
    const Q9_BUCKET: BucketShape = BucketShape {
        bucket_bits: 15,
        block_bits: 8,
        last_distances: 16,
    };

    /// The chain shape quality five resolves to.
    const Q5_CHAIN: ChainShape = ChainShape {
        num_banks: 1,
        bank_bits: 16,
        last_distances: 4,
        max_hops: 16,
    };

    /// The chain shape quality nine resolves to.
    const Q9_CHAIN: ChainShape = ChainShape {
        num_banks: 512,
        bank_bits: 9,
        last_distances: 16,
        max_hops: 224,
    };

    #[test]
    fn the_quick_matcher_finds_a_repeat_it_has_stored() {
        let data = repeated();
        let mut matcher = primed(QuickMatcher::<16, 1, 5, false>::new(), &data);
        let found = search_at(&mut matcher, &data, REPEAT_AT);
        assert!(found.is_match());
        assert_eq!((found.distance, found.len), (64, 64));
    }

    #[test]
    fn every_quick_shape_finds_the_same_repeat() {
        let data = repeated();

        let mut h4 = primed(QuickMatcher::<17, 2, 5, true>::new(), &data);
        let found = search_at(&mut h4, &data, REPEAT_AT);
        assert_eq!((found.distance, found.len), (64, 64));

        let mut h54 = primed(QuickMatcher::<20, 2, 7, false>::new(), &data);
        let found = search_at(&mut h54, &data, REPEAT_AT);
        assert_eq!((found.distance, found.len), (64, 64));
    }

    #[test]
    fn the_bucket_matchers_find_a_repeat_they_have_stored() {
        let data = repeated();

        let mut h5 = primed(BucketMatcher::<false, 14>::new(Q5_BUCKET), &data);
        let found = search_at(&mut h5, &data, REPEAT_AT);
        assert_eq!((found.distance, found.len), (64, 64));

        let mut h6 = primed(
            BucketMatcher::<true, 15>::new(BucketShape {
                bucket_bits: 15,
                ..Q5_BUCKET
            }),
            &data,
        );
        let found = search_at(&mut h6, &data, REPEAT_AT);
        assert_eq!((found.distance, found.len), (64, 64));

        // The deepest bucket quality nine asks for finds the same repeat.
        let mut deep = primed(BucketMatcher::<false, 15>::new(Q9_BUCKET), &data);
        let found = search_at(&mut deep, &data, REPEAT_AT);
        assert_eq!((found.distance, found.len), (64, 64));
    }

    #[test]
    fn the_chain_matchers_find_a_repeat_they_have_stored() {
        let data = repeated();
        let mut h40 = primed(ChainMatcher::<1, 16>::new(Q5_CHAIN), &data);
        let found = search_at(&mut h40, &data, REPEAT_AT);
        assert_eq!((found.distance, found.len), (64, 64));

        // H42 spreads the same chains over five hundred and twelve banks.
        let mut h42 = primed(ChainMatcher::<512, 9>::new(Q9_CHAIN), &data);
        let found = search_at(&mut h42, &data, REPEAT_AT);
        assert_eq!((found.distance, found.len), (64, 64));
    }

    #[test]
    fn the_derived_distance_cache_brackets_the_two_freshest_entries() {
        let mut cache = INITIAL_DISTANCE_CACHE;
        prepare_distance_cache(&mut cache, 4);
        assert_eq!(cache[4..], [0; 12]);

        prepare_distance_cache(&mut cache, 10);
        assert_eq!(cache[4..10], [3, 5, 2, 6, 1, 7]);
        // Nothing past the tenth entry is touched below the threshold.
        assert_eq!(cache[10..], [0; 6]);

        prepare_distance_cache(&mut cache, 16);
        assert_eq!(cache[10..], [10, 12, 9, 13, 8, 14]);
    }

    #[test]
    fn a_deep_bucket_remembers_more_positions_than_a_shallow_one() {
        // Every position hashes to the same bucket, so the depth is exactly
        // how far back a match can still be found.
        let data = vec![b'a'; 1024];
        for (shape, depth) in [(Q5_BUCKET, 16usize), (Q9_BUCKET, 256)] {
            let mut matcher = BucketMatcher::<false, 15>::new(BucketShape {
                bucket_bits: 15,
                ..shape
            });
            matcher.prepare(true, data.len(), &data);
            matcher.store_range(&data, usize::MAX, 0, 512);
            let found = search_at(&mut matcher, &data, 512);
            assert!(found.is_match());
            assert!(
                found.distance <= depth,
                "{shape:?} reached {}",
                found.distance
            );
        }
    }

    #[test]
    fn a_bucket_forgets_all_but_its_newest_sixteen_positions() {
        // Every position hashes to the same bucket, so a store past the
        // sixteenth has to push the oldest one out.
        let data = vec![b'a'; 256];
        let mut matcher = BucketMatcher::<false, 14>::new(Q5_BUCKET);
        matcher.prepare(true, data.len(), &data);
        matcher.store_range(&data, usize::MAX, 0, 100);
        let found = search_at(&mut matcher, &data, 100);
        assert!(found.is_match());
        assert!(found.distance <= 16);
    }

    #[test]
    fn nothing_is_found_when_the_table_holds_no_candidate() {
        let data = repeated();
        let mut matcher = QuickMatcher::<16, 1, 5, false>::new();
        matcher.prepare(true, data.len(), &data);
        assert!(!search_at(&mut matcher, &data, REPEAT_AT).is_match());

        let mut chain = ChainMatcher::<1, 16>::new(Q5_CHAIN);
        chain.prepare(true, data.len(), &data);
        assert!(!search_at(&mut chain, &data, REPEAT_AT).is_match());

        let mut bucket = BucketMatcher::<false, 14>::new(Q5_BUCKET);
        bucket.prepare(true, data.len(), &data);
        assert!(!search_at(&mut bucket, &data, REPEAT_AT).is_match());
    }

    #[test]
    fn a_full_preparation_clears_what_a_previous_stream_stored() {
        let data = repeated();
        let mut matcher = primed(QuickMatcher::<16, 1, 5, false>::new(), &data);
        assert!(search_at(&mut matcher, &data, REPEAT_AT).is_match());
        matcher.prepare(false, 0, &data);
        assert!(!search_at(&mut matcher, &data, REPEAT_AT).is_match());

        let mut chain = primed(ChainMatcher::<1, 16>::new(Q5_CHAIN), &data);
        assert!(search_at(&mut chain, &data, REPEAT_AT).is_match());
        chain.prepare(false, 0, &data);
        assert!(!search_at(&mut chain, &data, REPEAT_AT).is_match());

        let mut bucket = primed(BucketMatcher::<false, 14>::new(Q5_BUCKET), &data);
        assert!(search_at(&mut bucket, &data, REPEAT_AT).is_match());
        bucket.prepare(false, 0, &data);
        assert!(!search_at(&mut bucket, &data, REPEAT_AT).is_match());
    }

    #[test]
    fn every_backend_agrees_on_the_match_it_finds() {
        let data = repeated();
        let mut results = Vec::new();
        for level in [Level::new(), Level::baseline(), Level::fallback()] {
            let mut matcher = primed(BucketMatcher::<false, 14>::new(Q5_BUCKET), &data);
            results.push(search_with(level, &mut matcher, &data, REPEAT_AT));
        }
        assert!(results.windows(2).all(|pair| pair[0] == pair[1]));
    }

    /// A payload with three copies of the body, for the stitching tests.
    fn thrice_repeated() -> Vec<u8> {
        let mut data = repeated();
        data.truncate(REPEAT_AT + 64);
        let body: Vec<u8> = data[64..REPEAT_AT].to_vec();
        data.extend_from_slice(&body);
        data.extend_from_slice(&[0u8; 8]);
        data
    }

    #[test]
    fn stitching_stores_the_three_positions_before_the_boundary() {
        let data = thrice_repeated();
        let mut matcher = QuickMatcher::<16, 1, 5, false>::new();
        matcher.prepare(true, data.len(), &data);
        matcher.stitch_to_previous_block(64, REPEAT_AT, &data, usize::MAX);
        // Position 125 was stored, so the position 64 further on repeats it.
        let found = search_at(&mut matcher, &data, REPEAT_AT + 61);
        assert!(found.is_match());
        assert_eq!(found.distance, 64);
    }

    #[test]
    fn stitching_does_nothing_at_the_start_of_a_stream() {
        let data = thrice_repeated();
        let mut matcher = QuickMatcher::<16, 1, 5, false>::new();
        matcher.prepare(true, data.len(), &data);
        matcher.stitch_to_previous_block(64, 2, &data, usize::MAX);
        matcher.stitch_to_previous_block(1, REPEAT_AT, &data, usize::MAX);
        assert!(!search_at(&mut matcher, &data, REPEAT_AT + 61).is_match());
    }

    #[test]
    fn the_plan_selects_the_matching_finder() {
        assert!(matches!(
            MatchFinder::from(HasherPlan::H3),
            MatchFinder::H3(_)
        ));
        assert!(matches!(
            MatchFinder::from(HasherPlan::H4),
            MatchFinder::H4(_)
        ));
        assert!(matches!(
            MatchFinder::from(HasherPlan::H54),
            MatchFinder::H54(_)
        ));
        assert!(matches!(
            MatchFinder::from(HasherPlan::Chain(Q5_CHAIN)),
            MatchFinder::H40(_)
        ));
        assert!(matches!(
            MatchFinder::from(HasherPlan::Chain(Q9_CHAIN)),
            MatchFinder::H42(_)
        ));
        assert!(matches!(
            MatchFinder::from(HasherPlan::H5(Q5_BUCKET)),
            MatchFinder::H5Narrow(_)
        ));
        assert!(matches!(
            MatchFinder::from(HasherPlan::H5(Q9_BUCKET)),
            MatchFinder::H5Wide(_)
        ));
        assert!(matches!(
            MatchFinder::from(HasherPlan::H6(Q9_BUCKET)),
            MatchFinder::H6(_)
        ));
    }

    #[test]
    fn a_matcher_reports_the_cached_distance_count_its_shape_asked_for() {
        assert_eq!(
            BucketMatcher::<false, 15>::new(Q9_BUCKET).last_distances_to_check(),
            16
        );
        assert_eq!(
            ChainMatcher::<1, 16>::new(Q5_CHAIN).last_distances_to_check(),
            4
        );
        assert_eq!(
            ChainMatcher::<512, 9>::new(Q9_CHAIN).last_distances_to_check(),
            16
        );
        // The quick matchers always use the plain four.
        assert_eq!(
            QuickMatcher::<16, 1, 5, false>::new().last_distances_to_check(),
            NUM_REMEMBERED_DISTANCES
        );
    }
}
