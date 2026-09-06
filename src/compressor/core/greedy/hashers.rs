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

use fearless_simd::{Level, Simd, SimdBase, SimdMask, u8x16, u8x32};

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
///
/// Every buffer index in this crate is below 2^32 (positions are 32-bit in
/// the format), so `offset` is masked to that width: it changes nothing for
/// a real index and lets the compiler see that `offset + 8` cannot overflow,
/// which leaves a single compare in front of the load.
#[inline(always)]
fn read_u64(data: &[u8], offset: usize) -> u64 {
    debug_assert!(offset <= u32::MAX as usize);
    let offset = offset & u32::MAX as usize;
    match data.get(offset..offset + 8) {
        Some(chunk) => u64::from_le_bytes(chunk.try_into().unwrap_or([0; 8])),
        None => 0,
    }
}

/// Reads four little-endian bytes at `offset`, or zero past the end.
///
/// See [`read_u64`] for the offset mask.
#[inline(always)]
fn read_u32(data: &[u8], offset: usize) -> u32 {
    debug_assert!(offset <= u32::MAX as usize);
    let offset = offset & u32::MAX as usize;
    match data.get(offset..offset + 4) {
        Some(chunk) => u32::from_le_bytes(chunk.try_into().unwrap_or([0; 4])),
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
    /// The ring buffer being searched, tail copy and margin included.
    pub(crate) data: &'a [u8],
    /// `data` cut to the window: a position the mask admits indexes it, so
    /// a guard against the window's end is also a bounds proof.
    pub(crate) window: &'a [u8],
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

/// What [`Matcher::prepare`] did to the table, which tells a caller how to
/// leave it clean for the next stream.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Sweep {
    /// Only the slots the first `input_size` positions hash to were cleared.
    /// Replaying the same sweep after the stream clears exactly the slots it
    /// could have dirtied, which is far cheaper than wiping the table.
    Partial,
    /// The whole table was cleared; the stream will dirty it in places no
    /// cheap sweep could find, so the next stream has to wipe it again.
    Full,
    /// The table empties itself on every `prepare` at a cost that does not
    /// depend on the stream, so nothing needs replaying and nothing is dirty.
    SelfCleaning,
}

/// A match finder's tables borrowed for one block of searches and stores.
///
/// The reference hoists its table pointers into `restrict` locals for a
/// whole block, so a store through one never makes the compiler reload the
/// others. A run is the same idea: it holds the tables as slices bound once,
/// and the hot loop stores through those rather than through the finder,
/// which would otherwise reload every field it needs at every position.
pub(crate) trait MatchRun {
    /// Bytes a candidate needs available to be hashed (`HashTypeLength`).
    const HASH_TYPE_LENGTH: usize;

    /// Records the position `ix` in the table (`Store`).
    fn store(&mut self, data: &[u8], mask: usize, ix: usize);

    /// Records every position in `start..end` (`StoreRange`).
    fn store_range(&mut self, data: &[u8], mask: usize, start: usize, end: usize);

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

/// A finder whose tables need no separate view runs through itself.
///
/// The forwarding methods are inlined by force so the finder's own inline
/// methods land in the search loop rather than behind a call per position.
impl<M: Matcher> MatchRun for &mut M {
    const HASH_TYPE_LENGTH: usize = M::HASH_TYPE_LENGTH;

    #[inline(always)]
    fn store(&mut self, data: &[u8], mask: usize, ix: usize) {
        M::store(self, data, mask, ix);
    }

    #[inline(always)]
    fn store_range(&mut self, data: &[u8], mask: usize, start: usize, end: usize) {
        M::store_range(self, data, mask, start, end);
    }

    #[inline(always)]
    fn find_longest_match<S: Simd>(
        &mut self,
        simd: S,
        stats: &mut DictionaryStats,
        query: MatchQuery<'_>,
        out: &mut SearchResult,
    ) {
        M::find_longest_match(self, simd, stats, query, out);
    }
}

/// A match finder over the ring buffer.
pub(crate) trait Matcher {
    /// Bytes a candidate needs available to be hashed (`HashTypeLength`).
    const HASH_TYPE_LENGTH: usize;

    /// Bytes a store needs available (`StoreLookahead`).
    const STORE_LOOKAHEAD: usize;

    /// The view a block of searches runs through; see [`MatchRun`].
    type Run<'a>: MatchRun
    where
        Self: 'a;

    /// Borrows the tables for a block of searches and stores.
    fn run(&mut self) -> Self::Run<'_>;

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
    /// `clear` may be false only when construction or a previous reset sweep
    /// already left every reachable entry empty. Sweep selection still returns
    /// the information needed to clear the next stream.
    ///
    /// Returns which [`Sweep`] was taken, so a caller that wants to reuse the
    /// matcher for another stream knows how to leave the table clean.
    fn prepare(&mut self, one_shot: bool, input_size: usize, data: &[u8], clear: bool) -> Sweep;

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

/// Sparse logical slots for short streams. Missing keys have the same zero
/// position as a freshly initialized full quick-matcher table.
#[derive(Default)]
struct SmallSlots {
    entries: Vec<u64>,
    count: usize,
}

impl SmallSlots {
    const EMPTY: u64 = u64::MAX;

    #[inline(always)]
    fn read(&self, key: usize) -> u32 {
        if self.entries.is_empty() {
            return 0;
        }
        let mask = self.entries.len() - 1;
        let mut slot = key & mask;
        loop {
            let entry = self.entries[slot];
            if entry == Self::EMPTY {
                return 0;
            }
            if (entry >> 32) as usize == key {
                return entry as u32;
            }
            slot = (slot + 1) & mask;
        }
    }

    #[inline(always)]
    fn write(&mut self, key: usize, value: u32) {
        if 2 * (self.count + 1) > self.entries.len() {
            self.grow();
        }
        let mask = self.entries.len() - 1;
        let mut slot = key & mask;
        loop {
            let entry = self.entries[slot];
            if entry == Self::EMPTY || (entry >> 32) as usize == key {
                self.count += usize::from(entry == Self::EMPTY);
                self.entries[slot] = ((key as u64) << 32) | u64::from(value);
                return;
            }
            slot = (slot + 1) & mask;
        }
    }

    fn grow(&mut self) {
        let size = (self.entries.len() * 2).max(32);
        let previous = std::mem::replace(&mut self.entries, vec![Self::EMPTY; size]);
        self.count = 0;
        for entry in previous {
            if entry != Self::EMPTY {
                self.write((entry >> 32) as usize, entry as u32);
            }
        }
    }

    /// Empties the map, sized so `input_size` distinct keys never grow it.
    fn reset(&mut self, input_size: usize) {
        let size = (2 * input_size).next_power_of_two().max(32);
        if self.entries.len() < size {
            self.entries = vec![Self::EMPTY; size];
        } else {
            self.entries.fill(Self::EMPTY);
        }
        self.count = 0;
    }
}

/// Reads slot `key` of a full table; the key is in range by construction,
/// and masking with the length lets the compiler see it without a check.
#[inline(always)]
fn quick_read<const COMPACT: bool>(buckets: &[u32], compact: &SmallSlots, key: usize) -> u32 {
    if COMPACT {
        compact.read(key)
    } else if buckets.is_empty() {
        0
    } else {
        buckets[key & (buckets.len() - 1)]
    }
}

/// Writes slot `key` of a full table; see [`quick_read`].
#[inline(always)]
fn quick_write<const COMPACT: bool>(
    buckets: &mut [u32],
    compact: &mut SmallSlots,
    key: usize,
    value: u32,
) {
    if COMPACT {
        compact.write(key, value);
    } else if !buckets.is_empty() {
        buckets[key & (buckets.len() - 1)] = value;
    }
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
    const COMPACT: bool = false,
> {
    buckets: Vec<u32>,
    compact: SmallSlots,
}

impl<
    const BUCKET_BITS: u32,
    const SWEEP_BITS: u32,
    const HASH_LEN: u32,
    const USE_DICTIONARY: bool,
    const COMPACT: bool,
> QuickMatcher<BUCKET_BITS, SWEEP_BITS, HASH_LEN, USE_DICTIONARY, COMPACT>
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
            + self.compact.entries.capacity() * size_of::<u64>()
    }

    pub(crate) fn new() -> Self {
        Self {
            buckets: if COMPACT {
                Vec::new()
            } else {
                vec![0u32; Self::BUCKET_SIZE]
            },
            compact: SmallSlots::default(),
        }
    }

    /// Returns the bucket of the bytes at `offset` (`HashBytes`).
    #[inline(always)]
    fn hash(data: &[u8], offset: usize) -> usize {
        let value = read_u64(data, offset) << (64 - 8 * HASH_LEN as u64);
        (value.wrapping_mul(HASH_MUL64) >> (64 - BUCKET_BITS)) as usize
    }
}

impl<
    const BUCKET_BITS: u32,
    const SWEEP_BITS: u32,
    const HASH_LEN: u32,
    const USE_DICTIONARY: bool,
    const COMPACT: bool,
> Matcher for QuickMatcher<BUCKET_BITS, SWEEP_BITS, HASH_LEN, USE_DICTIONARY, COMPACT>
{
    const HASH_TYPE_LENGTH: usize = 8;
    const STORE_LOOKAHEAD: usize = 8;

    type Run<'a>
        = &'a mut Self
    where
        Self: 'a;

    fn run(&mut self) -> Self::Run<'_> {
        self
    }

    fn prepare(&mut self, one_shot: bool, input_size: usize, data: &[u8], clear: bool) -> Sweep {
        // Clearing only the slots a short input can reach is far cheaper than
        // wiping the whole table, and reaches exactly the same slots the
        // search will later look at.
        let partial_prepare_threshold = Self::BUCKET_SIZE >> 5;
        let partial = if one_shot && input_size <= partial_prepare_threshold {
            Sweep::Partial
        } else {
            Sweep::Full
        };
        if COMPACT {
            // A fresh map is sized for the input either way, so it never
            // grows and rehashes while the stream stores into it.
            if clear || self.compact.entries.is_empty() {
                self.compact.reset(input_size);
            }
            return partial;
        }
        if !clear {
            return partial;
        }
        if partial == Sweep::Partial {
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
        quick_write::<COMPACT>(&mut self.buckets, &mut self.compact, slot, ix as u32);
    }

    fn store_range(&mut self, data: &[u8], mask: usize, start: usize, end: usize) {
        // The table is bound once, so a store never makes the loop reload it.
        let Self { buckets, compact } = self;
        let buckets = &mut buckets[..];
        for ix in start..end {
            let key = Self::hash(data, ix & mask);
            let slot = if Self::SWEEP == 1 {
                key
            } else {
                (key + (ix & Self::SWEEP_MASK)) & Self::BUCKET_MASK
            };
            quick_write::<COMPACT>(buckets, compact, slot, ix as u32);
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
        let compact = &mut self.compact;
        let size = if COMPACT { 0 } else { Self::BUCKET_SIZE };
        let Some(buckets) = self.buckets.get_mut(..size) else {
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
                            quick_write::<COMPACT>(
                                buckets,
                                compact,
                                key & Self::BUCKET_MASK,
                                query.cur_ix as u32,
                            );
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
            let prev_ix = quick_read::<COMPACT>(buckets, compact, key & Self::BUCKET_MASK) as usize;
            quick_write::<COMPACT>(
                buckets,
                compact,
                key & Self::BUCKET_MASK,
                query.cur_ix as u32,
            );
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
                let prev_ix =
                    quick_read::<COMPACT>(buckets, compact, slot & Self::BUCKET_MASK) as usize;
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
            quick_write::<COMPACT>(
                buckets,
                compact,
                slot & Self::BUCKET_MASK,
                query.cur_ix as u32,
            );
        }
    }
}

/// Input length up to which a one-shot stream indexes its buckets through a
/// small map instead of the sparse entry table.
///
/// The table costs 128 KiB (fourteen bucket bits) or 256 KiB (fifteen) to
/// zero, which is most of what a sixteen-byte call pays in total. A kilobyte
/// of input reaches at most a thousand buckets, which a map of two thousand
/// slots indexes after a sixteen-kilobyte clear.
const COMPACT_INPUT_LIMIT: usize = 1024;

/// Slots a sparse block starts with; its fifth store grows it to full depth.
///
/// Quality nine keeps two hundred and fifty-six positions per bucket, a
/// kilobyte each, and a short input activates a bucket for almost every
/// position it hashes. Starting small keeps that input from zeroing a
/// mebibyte it never reads.
const STARTER_SLOTS: usize = 4;

/// Bits of a sparse entry holding the wrapping store counter.
const COUNT_BITS: u32 = 16;

/// Bits of a sparse entry holding the block offset and its starter flag.
///
/// Thirty-two thousand buckets of two hundred and sixty slots need
/// twenty-four bits; the flag is the twenty-fifth.
const OFFSET_BITS: u32 = 25;

/// Marks an encoded offset as a starter block.
const STARTER_FLAG: u32 = 1 << (OFFSET_BITS - 1);

/// Mask of the encoded offset inside a sparse entry.
const OFFSET_MASK: u64 = (1 << OFFSET_BITS) - 1;

/// Bit position of the generation stamp inside a sparse entry.
const GENERATION_SHIFT: u32 = COUNT_BITS + OFFSET_BITS;

/// Generations a sparse table lives through before it is wiped.
const MAX_GENERATION: u64 = (1 << (u64::BITS - GENERATION_SHIFT)) - 1;

/// Open-addressing index from bucket key to store counter and block offset.
///
/// Entries pack the key above the counter above the offset; the all-ones
/// word is empty, which no real key reaches.
#[derive(Default)]
struct KeyMap {
    entries: Vec<u64>,
    count: usize,
}

impl KeyMap {
    const EMPTY: u64 = u64::MAX;

    /// Returns the counter and offset stored for `key`, or zeros.
    #[inline(always)]
    fn get(&self, key: usize) -> (u16, u32) {
        if self.entries.is_empty() {
            return (0, 0);
        }
        let mask = self.entries.len() - 1;
        let mut slot = key & mask;
        loop {
            let entry = self.entries[slot & mask];
            if entry == Self::EMPTY {
                return (0, 0);
            }
            if (entry >> 48) as usize == key {
                return ((entry >> 32) as u16, entry as u32);
            }
            slot = (slot + 1) & mask;
        }
    }

    /// Records `count` and `offset` for `key`, growing the map when it is
    /// more than half full.
    #[inline(always)]
    fn set(&mut self, key: usize, count: u16, offset: u32) {
        if 2 * (self.count + 1) > self.entries.len() {
            self.grow();
        }
        let mask = self.entries.len() - 1;
        let mut slot = key & mask;
        loop {
            let entry = self.entries[slot & mask];
            if entry == Self::EMPTY || (entry >> 48) as usize == key {
                self.count += usize::from(entry == Self::EMPTY);
                self.entries[slot & mask] =
                    ((key as u64) << 48) | (u64::from(count) << 32) | u64::from(offset);
                return;
            }
            slot = (slot + 1) & mask;
        }
    }

    fn grow(&mut self) {
        let size = (self.entries.len() * 2).max(64);
        let previous = std::mem::replace(&mut self.entries, vec![Self::EMPTY; size]);
        self.count = 0;
        for entry in previous {
            if entry != Self::EMPTY {
                self.set((entry >> 48) as usize, (entry >> 32) as u16, entry as u32);
            }
        }
    }

    /// Empties the map, sized so `input_size` distinct keys never grow it.
    fn reset(&mut self, input_size: usize) {
        let size = (2 * input_size).next_power_of_two().max(64);
        if self.entries.len() < size {
            self.entries = vec![Self::EMPTY; size];
        } else {
            self.entries.fill(Self::EMPTY);
        }
        self.count = 0;
    }
}

/// How a bucket matcher lays out its blocks for the current stream.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Layout {
    /// Blocks activated on demand, indexed through the key map.
    Compact,
    /// Blocks activated on demand, indexed through the sparse entry table.
    Sparse,
    /// Every bucket's block preallocated at `key << block_bits`.
    Dense,
}

/// Bucketed match finder keeping the most recent positions per hash.
///
/// `HASH64` selects the H6 variant, which hashes eight bytes instead of four
/// and pre-filters candidates on their first four bytes. `BUCKET_BITS` fixes
/// the table size, and therefore the hash shift, at compile time; the bucket
/// depth and the number of cached distances vary with quality and are fields.
///
/// # Storage
///
/// The reference allocates every bucket's block up front and never
/// initialises it, reading a slot only below the counter that guards it.
/// Safe Rust has to initialise what it reads, so the layout follows the
/// input. A matcher built for at least [`BucketMatcher::dense_limit`] bytes
/// gets the reference's dense table: a block per bucket at
/// `key << block_bits`, zeroed once per matcher and never again, with a
/// two-byte counter per bucket cleared per stream. Every other stream,
/// including one of unknown length, activates blocks on demand instead, each
/// starting with [`STARTER_SLOTS`] entries until its fifth store, and indexes
/// them through a table of packed entries — generation stamp, block offset,
/// counter — that a new stream empties by bumping the generation. A one-shot
/// stream of at most [`COMPACT_INPUT_LIMIT`] bytes indexes them through a
/// small [`KeyMap`] instead, so a sixteen-byte call never zeroes the table.
///
/// Slots fill downwards, as the reference's tagged matchers do: the newest
/// position sits at the lowest occupied slot and older ones follow it
/// upwards, so a scan walks the block by ascending slot from the newest.
///
/// # Equivalence with the tagged reference matchers
///
/// The reference builds `H58`/`H68` in place of `H5`/`H6` when
/// `BROTLI_MAX_SIMD_QUALITY` is defined. Those variants store a one-byte tag
/// beside every position and visit only the slots whose tag matches the
/// current one. They select the same bucket — the tagged `HashBytes` merely
/// keeps eight more low bits, which the key shifts straight back off — and
/// they walk it newest to oldest, exactly as this loop does. A tag is a
/// function of the hashed bytes, so within the same bucket two positions
/// whose first four bytes agree share a tag; a slot the tag mask drops
/// differs in those four bytes, and a candidate that differs there can never
/// reach the reference's `len >= 4` acceptance test. Both matchers also stop
/// at the first candidate beyond `max_backward`, and positions grow
/// monotonically along the ring, so both stop having seen the same prefix of
/// candidates. The accepted-match sets coincide, and so do the streams. Like
/// the pinned C build, only the shallow quality five and six blocks carry
/// tags; deeper blocks measured slower with the mask. The SIMD backends use
/// the tag mask; the scalar backend and starter blocks keep the unfiltered
/// scan as an oracle.
pub(crate) struct BucketMatcher<const HASH64: bool, const BUCKET_BITS: u32> {
    layout: Layout,
    /// Dense layout: one wrapping store counter per bucket.
    num: Vec<u16>,
    /// Dense layout: `block_size` slots per bucket, allocated once.
    dense: Vec<u32>,
    /// Dense layout: one tag per slot; empty for untagged shapes.
    dense_tags: Vec<u8>,
    /// Sparse layout: one packed entry per bucket, allocated on first use.
    entries: Vec<u64>,
    /// Generation stamp a live sparse entry carries; never zero.
    generation: u64,
    /// Compact layout: counter and offset per activated bucket.
    compact: KeyMap,
    /// Compact and sparse layouts: activated blocks, `block_size` slots each
    /// except starters.
    blocks: Vec<u32>,
    /// One tag per slot of `blocks`; empty for untagged shapes.
    block_tags: Vec<u8>,
    block_bits: u32,
    block_size: usize,
    /// Whether blocks carry tags (the shallow `H58`/`H68` shapes).
    tagged: bool,
    last_distances: usize,
    /// Total input the matcher was built for; zero when unknown.
    size_hint: usize,
}

impl<const HASH64: bool, const BUCKET_BITS: u32> BucketMatcher<HASH64, BUCKET_BITS> {
    /// Number of buckets in the table.
    const BUCKET_SIZE: usize = 1usize << BUCKET_BITS;

    /// Returns the bytes this match finder keeps allocated.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.num.capacity() * size_of::<u16>()
            + (self.dense.capacity() + self.blocks.capacity()) * size_of::<u32>()
            + (self.entries.capacity() + self.compact.entries.capacity()) * size_of::<u64>()
            + self.dense_tags.capacity()
            + self.block_tags.capacity()
    }

    /// Creates an empty table of the shape `shape` describes, expecting
    /// `size_hint` bytes of input in total (zero when unknown).
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub(crate) fn new(shape: BucketShape, size_hint: usize) -> Self {
        debug_assert_eq!(shape.bucket_bits, BUCKET_BITS);
        Self {
            layout: Layout::Sparse,
            num: Vec::new(),
            dense: Vec::new(),
            dense_tags: Vec::new(),
            entries: Vec::new(),
            generation: 1,
            compact: KeyMap::default(),
            blocks: Vec::new(),
            block_tags: Vec::new(),
            block_bits: shape.block_bits,
            block_size: 1usize << shape.block_bits,
            tagged: shape.block_bits <= 5,
            last_distances: shape.last_distances,
            size_hint,
        }
    }

    /// Whether blocks carry tags (the shallow `H58`/`H68` shapes).
    const fn tagged(&self) -> bool {
        self.tagged
    }

    /// Shortest known input that gets the dense table.
    ///
    /// The table is zeroed once per matcher, which on a fresh one costs the
    /// page faults of its whole size, so it has to be small against the
    /// input: an eighth of the tagged shapes' one or two mebibytes, whose
    /// stores and probes the dense layout serves at about twice the speed of
    /// the on-demand one; the deep shapes, whose blocks the on-demand layouts
    /// keep packed, run faster on demand anyway and only switch for an input
    /// at least half the size of their eight to thirty-two mebibytes.
    const fn dense_limit(&self) -> usize {
        let table_bytes = (Self::BUCKET_SIZE << self.block_bits) * size_of::<u32>();
        if self.tagged {
            table_bytes / 8
        } else {
            table_bytes / 2
        }
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

    /// Returns the counter and encoded block offset of `key` in an on-demand
    /// layout. A sparse entry from an earlier generation still names its
    /// block but counts as empty.
    #[inline(always)]
    fn entry(&self, key: usize) -> (u16, u32) {
        if self.layout == Layout::Compact {
            return self.compact.get(key);
        }
        let Some(&entry) = self.entries.get(key) else {
            return (0, 0);
        };
        let offset = ((entry >> COUNT_BITS) & OFFSET_MASK) as u32;
        let count = if entry >> GENERATION_SHIFT == self.generation {
            entry as u16
        } else {
            0
        };
        (count, offset)
    }

    /// Records the counter and encoded block offset of `key` in an on-demand
    /// layout.
    #[inline(always)]
    fn set_entry(&mut self, key: usize, count: u16, offset: u32) {
        if self.layout == Layout::Compact {
            self.compact.set(key, count, offset);
            return;
        }
        if let Some(entry) = self.entries.get_mut(key) {
            *entry = (self.generation << GENERATION_SHIFT)
                | (u64::from(offset) << COUNT_BITS)
                | u64::from(count);
        }
    }

    /// Decodes an on-demand offset into a block start and its capacity.
    #[inline(always)]
    const fn block(&self, offset: u32) -> Option<(usize, usize)> {
        if offset == 0 {
            return None;
        }
        let base = ((offset & !STARTER_FLAG) - 1) as usize;
        let capacity = if offset & STARTER_FLAG != 0 {
            STARTER_SLOTS
        } else {
            self.block_size
        };
        Some((base, capacity))
    }

    /// Appends a zeroed on-demand block of `slots` entries; returns its start.
    fn allocate_block(&mut self, slots: usize) -> usize {
        let start = self.blocks.len();
        self.blocks.resize(start + slots, 0);
        if self.tagged() {
            self.block_tags.resize(start + slots, 0);
        }
        start
    }

    /// Returns the on-demand block a store into a bucket writes, activating
    /// or growing it when `count` stores have already filled what it has.
    ///
    /// The encoded offset comes back with the start and capacity so the
    /// caller can record it; it is unchanged whenever the block had room.
    #[inline(always)]
    fn block_for_store(&mut self, count: u16, offset: u32) -> (usize, usize, u32) {
        if offset == 0 {
            let start = self.allocate_block(STARTER_SLOTS);
            return (start, STARTER_SLOTS, (start as u32 + 1) | STARTER_FLAG);
        }
        let base = ((offset & !STARTER_FLAG) - 1) as usize;
        if offset & STARTER_FLAG == 0 {
            return (base, self.block_size, offset);
        }
        if usize::from(count) < STARTER_SLOTS {
            return (base, STARTER_SLOTS, offset);
        }
        // Both fill downwards from the top, so the starter's four slots are
        // the top four of a full block.
        let start = self.allocate_block(self.block_size);
        let top = start + self.block_size - STARTER_SLOTS;
        self.blocks.copy_within(base..base + STARTER_SLOTS, top);
        if self.tagged() {
            self.block_tags.copy_within(base..base + STARTER_SLOTS, top);
        }
        (start, self.block_size, start as u32 + 1)
    }

    /// Stores `ix` with `tag` into `key`.
    #[inline(always)]
    fn push(&mut self, key: usize, ix: u32, tag: u8) {
        if self.layout == Layout::Dense {
            dense_store(
                &mut self.num,
                &mut self.dense,
                &mut self.dense_tags,
                self.block_bits,
                key,
                ix,
                tag,
            );
            return;
        }
        let (count, offset) = self.entry(key);
        let (base, capacity, offset) = self.block_for_store(count, offset);
        let slot = base + (!usize::from(count) & (capacity - 1));
        if let Some(entry) = self.blocks.get_mut(slot) {
            *entry = ix;
        }
        if let Some(entry) = self.block_tags.get_mut(slot) {
            *entry = tag;
        }
        self.set_entry(key, count.wrapping_add(1), offset);
    }

    /// Selects the layout for a stream and readies its index.
    fn select_layout(&mut self, one_shot: bool, input_size: usize) {
        if one_shot && input_size <= COMPACT_INPUT_LIMIT {
            self.layout = Layout::Compact;
            self.compact.reset(input_size);
            self.blocks.clear();
            self.block_tags.clear();
        } else if self.size_hint < self.dense_limit() {
            // A multi-block one-shot stream is not "one shot" at its first
            // block, so this rests on the size hint: known short inputs and
            // inputs of unknown length alike stay on demand.
            if self.entries.is_empty() {
                self.entries = vec![0; Self::BUCKET_SIZE];
                self.generation = 1;
            } else if self.layout != Layout::Sparse || self.generation == MAX_GENERATION {
                // The blocks the entries name belonged to another layout's
                // stream, or the stamps have run out; start over.
                self.entries.fill(0);
                self.generation = 1;
                self.blocks.clear();
                self.block_tags.clear();
            } else {
                self.generation += 1;
            }
            self.layout = Layout::Sparse;
        } else {
            if self.dense.is_empty() {
                self.num = vec![0; Self::BUCKET_SIZE];
                self.dense = vec![0; Self::BUCKET_SIZE << self.block_bits];
                if self.tagged() {
                    self.dense_tags = vec![0; Self::BUCKET_SIZE << self.block_bits];
                }
            } else {
                // Slots are only ever read below the counter that guards
                // them, so the blocks themselves stay as they are.
                self.num.fill(0);
            }
            self.layout = Layout::Dense;
        }
    }
}

/// Stores `ix` with `tag` into bucket `key` of a dense table.
///
/// The slices are bound once by the caller, so their lengths stay in
/// registers across the stores; the key and slot are in range by
/// construction, and masking with the lengths lets the compiler see it
/// without a check.
#[inline(always)]
fn dense_store(
    num: &mut [u16],
    dense: &mut [u32],
    tags: &mut [u8],
    block_bits: u32,
    key: usize,
    ix: u32,
    tag: u8,
) {
    if num.is_empty() || dense.is_empty() {
        return;
    }
    let count = &mut num[key & (num.len() - 1)];
    let current = *count;
    *count = current.wrapping_add(1);
    let slot = (key << block_bits) + (!usize::from(current) & ((1 << block_bits) - 1));
    dense[slot & (dense.len() - 1)] = ix;
    if !tags.is_empty() {
        tags[slot & (tags.len() - 1)] = tag;
    }
}

/// Splits a raw tag-equality mask into the candidate slots at or above
/// `newest` and those below it, dropping slots no store has filled.
///
/// `bits` is the block capacity, `available` how many of its slots hold
/// positions. Slots fill downwards, so the newest position sits at `newest`
/// and older ones follow it upwards, wrapping to the bottom: visiting the
/// first mask by ascending slot and then the second walks the block newest
/// to oldest.
#[inline(always)]
fn split_candidates(equal: u32, bits: u32, newest: u32, available: u32) -> (u32, u32) {
    let lanes = u32::MAX.checked_shr(32 - bits).unwrap_or(0);
    let filled = u32::MAX.checked_shr(32 - available).unwrap_or(0);
    // A rotation within `bits` lanes.
    let allowed = ((filled << newest) | filled.checked_shr(bits - newest).unwrap_or(0)) & lanes;
    let candidates = equal & allowed;
    let above = u32::MAX << newest;
    (candidates & above, candidates & !above)
}

/// One bit per slot of `tags` whose byte equals `tag`.
///
/// The scalar backend and any block too short for a vector compare report
/// every slot, which keeps the unfiltered scan as an independent oracle for
/// the mask.
#[inline(always)]
fn tag_equality<S: Simd>(simd: S, tags: &[u8], tag: u8) -> u32 {
    if matches!(simd.level(), Level::Fallback(_)) {
        return u32::MAX;
    }
    match (tags.first_chunk::<32>(), tags.first_chunk::<16>()) {
        (Some(bytes), _) => u8x32::load_array_ref(simd, bytes)
            .simd_eq(u8x32::splat(simd, tag))
            .to_bitmask() as u32,
        (None, Some(bytes)) => u32::from(
            u8x16::load_array_ref(simd, bytes)
                .simd_eq(u8x16::splat(simd, tag))
                .to_bitmask() as u16,
        ),
        (None, None) => u32::MAX,
    }
}

/// The running best of one bucket scan.
struct BucketScan {
    /// The searched position, masked into the ring buffer.
    cur_ix_masked: usize,
    /// Kept narrow: an index built from it and a ring position cannot
    /// overflow, which is what lets the candidate reads go unchecked.
    best_len: u32,
    best_score: usize,
    /// The four bytes ending one past `best_len`, which a candidate has to
    /// reproduce before it is worth measuring.
    cur_word: u32,
}

/// Judges one bucket candidate at `prev_ix`; `false` ends the scan.
///
/// The window is the ring buffer cut to the mask, so the guard against its
/// end is the reference's mask guard and a bounds proof at once. Inlined by
/// force: it holds the vector match scanner, which makes LLVM outline it and
/// pay a full call per candidate otherwise.
#[inline(always)]
fn consider<S: Simd, const HASH64: bool>(
    simd: S,
    query: &MatchQuery<'_>,
    prev_ix: u32,
    scan: &mut BucketScan,
    out: &mut SearchResult,
) -> bool {
    let window = query.window;
    let cur_ix_masked = scan.cur_ix_masked;
    let backward = query.cur_ix.wrapping_sub(prev_ix as usize);
    if backward > query.max_backward {
        return false;
    }
    let prev_ix = prev_ix as usize & query.mask;
    // The four bytes ending one past `best_len`. Both operands are below
    // 2^32 — the offset is a wrapped `u32` on purpose — so the end cannot
    // overflow and the guard against the window's end bounds the read.
    let start = prev_ix + scan.best_len.wrapping_sub(3) as usize;
    let Some(word) = window.get(start..start + 4) else {
        return true;
    };
    if scan.cur_word != u32::from_le_bytes(word.try_into().unwrap_or([0; 4])) {
        return true;
    }
    let data = query.data;
    let len = if HASH64 {
        if read_u32(data, cur_ix_masked) != read_u32(window, prev_ix) {
            return true;
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
            return true;
        }
        len
    };
    let score = backward_reference_score(len, backward);
    if scan.best_score < score {
        scan.best_score = score;
        scan.best_len = len as u32;
        out.len = len;
        out.distance = backward;
        out.score = score;
        scan.cur_word = read_u32(data, cur_ix_masked + len - 3);
    }
    true
}

/// Judges the candidates `slots` names, lowest slot first; `false` ends the
/// scan.
///
/// A candidate lives at `store[(base + slot) & index_mask]`: the mask is the
/// store's length less one, which the compiler can see keeps the index in
/// range, and is the identity for every slot a bucket owns.
#[inline(always)]
fn scan_slots<S: Simd, const HASH64: bool>(
    simd: S,
    query: &MatchQuery<'_>,
    store: &[u32],
    base: usize,
    mut slots: u32,
    scan: &mut BucketScan,
    out: &mut SearchResult,
) -> bool {
    let index_mask = store.len().wrapping_sub(1);
    while slots != 0 {
        let slot = slots.trailing_zeros() as usize;
        slots &= slots - 1;
        let prev_ix = store[(base + slot) & index_mask];
        if !consider::<S, HASH64>(simd, query, prev_ix, scan, out) {
            return false;
        }
    }
    true
}

/// The dense tables of a [`BucketMatcher`], bound as slices for a block.
pub(crate) struct DenseRun<'a> {
    num: &'a mut [u16],
    dense: &'a mut [u32],
    tags: &'a mut [u8],
    block_bits: u32,
    tagged: bool,
    last_distances: usize,
}

/// A [`BucketMatcher`]'s tables borrowed for one block; see [`MatchRun`].
pub(crate) enum BucketRun<'a, const HASH64: bool, const BUCKET_BITS: u32> {
    /// The dense layout, through slices bound once.
    Dense(DenseRun<'a>),
    /// An on-demand layout, whose blocks may still grow.
    OnDemand(&'a mut BucketMatcher<HASH64, BUCKET_BITS>),
}

/// Runs the cached-distance probes and the bucket scan for one search.
///
/// The bucket's slots are `store[base..base + capacity]`, `count` stores
/// deep; `tags` is empty for an untagged shape. Shared by both layouts so
/// the reference's decision order lives in one place.
#[expect(
    clippy::too_many_arguments,
    reason = "the two halves of FindLongestMatch, minus the table update"
)]
#[inline(always)]
fn search_bucket<S: Simd, const HASH64: bool>(
    simd: S,
    query: &MatchQuery<'_>,
    out: &mut SearchResult,
    tag: u8,
    count: u16,
    store: &[u32],
    base: usize,
    capacity: usize,
    tags: &[u8],
    last_distances: usize,
) {
    let data = query.data;
    let window = query.window;
    let mask = query.mask;
    let cur_ix_masked = query.cur_ix & mask;
    let mut best_score = out.score;
    let mut best_len = out.len;
    let available = usize::from(count).min(capacity);
    // Stores fill downwards from the top of a block, so the `age`-th
    // newest position sits `age` slots above the newest one, wrapping.
    let newest = !usize::from(count.wrapping_sub(1)) & capacity.wrapping_sub(1);
    // Fetch the bucket's tags and its newest slot before the cache probes so
    // both misses overlap them, as the reference's two prefetches do. The
    // slot's value is not needed yet; `black_box` keeps the load from being
    // sunk to its use in the scan, where it would serialise behind the tags.
    let equal = if available != 0 && !tags.is_empty() {
        tag_equality(simd, tags, tag)
    } else {
        u32::MAX
    };
    let index_mask = store.len().wrapping_sub(1);
    core::hint::black_box(store.get((base + newest) & index_mask).copied());

    out.len = 0;
    out.len_code_delta = 0;

    let cache: &DistanceCache = query.cache;
    for (index, &backward) in cache.iter().enumerate().take(last_distances) {
        let backward = backward as usize;
        let prev_ix = query.cur_ix.wrapping_sub(backward);
        if prev_ix >= query.cur_ix || backward > query.max_backward {
            continue;
        }
        let prev_ix = prev_ix & mask;
        // The reference guards against the mask; the window ends there, so
        // the same guards prove the two byte reads in bounds.
        let cur_at = cur_ix_masked + best_len;
        if cur_at >= window.len() {
            break;
        }
        let prev_at = prev_ix + best_len;
        if prev_at >= window.len() || window[cur_at] != window[prev_at] {
            continue;
        }
        let len = find_match_length(simd, data, prev_ix, cur_ix_masked, query.max_length);
        // Two-byte matches are only worth scoring for the two freshest
        // cached distances; anything shorter never wins. Written as one
        // comparison: `len >= 3 || (len == 2 && index < 2)`.
        if len + usize::from(index < 2) >= 3 {
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

    if available == 0 {
        return;
    }
    let mut scan = BucketScan {
        cur_ix_masked,
        best_len: best_len as u32,
        best_score,
        cur_word: read_u32(data, cur_ix_masked + best_len - 3),
    };
    if capacity <= 32 {
        let (above, below) =
            split_candidates(equal, capacity as u32, newest as u32, available as u32);
        if scan_slots::<S, HASH64>(simd, query, store, base, above, &mut scan, out) {
            scan_slots::<S, HASH64>(simd, query, store, base, below, &mut scan, out);
        }
    } else {
        let slot_mask = capacity.wrapping_sub(1);
        let mut slot = newest;
        let mut remaining = available;
        while remaining != 0 {
            let prev_ix = store[(base + slot) & index_mask];
            if !consider::<S, HASH64>(simd, query, prev_ix, &mut scan, out) {
                break;
            }
            slot = (slot + 1) & slot_mask;
            remaining -= 1;
        }
    }
}

impl<const HASH64: bool, const BUCKET_BITS: u32> BucketMatcher<HASH64, BUCKET_BITS> {
    /// Searches through an on-demand layout; see [`MatchRun::find_longest_match`].
    #[inline(always)]
    fn search_on_demand<S: Simd>(
        &mut self,
        simd: S,
        stats: &mut DictionaryStats,
        query: MatchQuery<'_>,
        out: &mut SearchResult,
    ) {
        let cur_ix_masked = query.cur_ix & query.mask;
        let min_score = out.score;
        let hash = Self::hash_with_tag(query.data, cur_ix_masked);
        let key = hash >> 8;
        let tag = hash as u8;
        let (count, offset) = self.entry(key);
        let (base, capacity) = self.block(offset).unwrap_or((0, 0));
        search_bucket::<S, HASH64>(
            simd,
            &query,
            out,
            tag,
            count,
            self.blocks.get(base..base + capacity).unwrap_or_default(),
            0,
            capacity,
            self.block_tags
                .get(base..base + capacity)
                .unwrap_or_default(),
            self.last_distances,
        );
        self.push(key, query.cur_ix as u32, tag);
        if min_score == out.score {
            query.search_dictionary(stats, out, false);
        }
    }
}

impl<const HASH64: bool, const BUCKET_BITS: u32> MatchRun for BucketRun<'_, HASH64, BUCKET_BITS> {
    const HASH_TYPE_LENGTH: usize = if HASH64 { 8 } else { 4 };

    #[inline(always)]
    fn store(&mut self, data: &[u8], mask: usize, ix: usize) {
        let hash = BucketMatcher::<HASH64, BUCKET_BITS>::hash_with_tag(data, ix & mask);
        match self {
            Self::Dense(run) => dense_store(
                run.num,
                run.dense,
                run.tags,
                run.block_bits,
                hash >> 8,
                ix as u32,
                hash as u8,
            ),
            Self::OnDemand(matcher) => matcher.push(hash >> 8, ix as u32, hash as u8),
        }
    }

    fn store_range(&mut self, data: &[u8], mask: usize, start: usize, end: usize) {
        let Self::Dense(run) = self else {
            for ix in start..end {
                self.store(data, mask, ix);
            }
            return;
        };
        if run.num.is_empty() || run.dense.is_empty() {
            return;
        }
        for ix in start..end {
            let hash = BucketMatcher::<HASH64, BUCKET_BITS>::hash_with_tag(data, ix & mask);
            dense_store(
                run.num,
                run.dense,
                run.tags,
                run.block_bits,
                hash >> 8,
                ix as u32,
                hash as u8,
            );
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
        let run = match self {
            Self::Dense(run) => run,
            Self::OnDemand(matcher) => return matcher.search_on_demand(simd, stats, query, out),
        };
        let cur_ix_masked = query.cur_ix & query.mask;
        let min_score = out.score;
        let hash = BucketMatcher::<HASH64, BUCKET_BITS>::hash_with_tag(query.data, cur_ix_masked);
        let key = hash >> 8;
        let tag = hash as u8;
        // The key is in range by construction; masking with the length
        // lets the compiler see it without a check.
        let count = if run.num.is_empty() {
            0
        } else {
            run.num[key & (run.num.len() - 1)]
        };
        let base = key << run.block_bits;
        let capacity = 1usize << run.block_bits;
        let tags: &[u8] = if run.tagged {
            run.tags
                .get(base..)
                .and_then(|tags| tags.get(..capacity))
                .unwrap_or_default()
        } else {
            &[]
        };
        search_bucket::<S, HASH64>(
            simd,
            &query,
            out,
            tag,
            count,
            run.dense,
            base,
            capacity,
            tags,
            run.last_distances,
        );
        dense_store(
            run.num,
            run.dense,
            run.tags,
            run.block_bits,
            key,
            query.cur_ix as u32,
            tag,
        );
        if min_score == out.score {
            query.search_dictionary(stats, out, false);
        }
    }
}

impl<const HASH64: bool, const BUCKET_BITS: u32> Matcher for BucketMatcher<HASH64, BUCKET_BITS> {
    const HASH_TYPE_LENGTH: usize = if HASH64 { 8 } else { 4 };
    const STORE_LOOKAHEAD: usize = Self::HASH_TYPE_LENGTH;

    type Run<'a>
        = BucketRun<'a, HASH64, BUCKET_BITS>
    where
        Self: 'a;

    fn run(&mut self) -> Self::Run<'_> {
        if self.layout != Layout::Dense {
            return BucketRun::OnDemand(self);
        }
        let Self {
            num,
            dense,
            dense_tags,
            block_bits,
            tagged,
            last_distances,
            ..
        } = self;
        BucketRun::Dense(DenseRun {
            num: &mut num[..],
            dense: &mut dense[..],
            tags: &mut dense_tags[..],
            block_bits: *block_bits,
            tagged: *tagged,
            last_distances: *last_distances,
        })
    }

    fn last_distances_to_check(&self) -> usize {
        self.last_distances
    }

    fn prepare(&mut self, one_shot: bool, input_size: usize, _data: &[u8], _clear: bool) -> Sweep {
        // Every layout empties itself in time that does not depend on what
        // the stream stored: the dense counters are a fixed memset, the
        // sparse table a generation bump, and the compact map is sized by
        // the input.
        self.select_layout(one_shot, input_size);
        Sweep::SelfCleaning
    }

    fn store(&mut self, data: &[u8], mask: usize, ix: usize) {
        self.run().store(data, mask, ix);
    }

    fn store_range(&mut self, data: &[u8], mask: usize, start: usize, end: usize) {
        self.run().store_range(data, mask, start, end);
    }

    fn find_longest_match<S: Simd>(
        &mut self,
        simd: S,
        stats: &mut DictionaryStats,
        query: MatchQuery<'_>,
        out: &mut SearchResult,
    ) {
        self.run().find_longest_match(simd, stats, query, out);
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
    type Run<'a>
        = &'a mut Self
    where
        Self: 'a;

    fn run(&mut self) -> Self::Run<'_> {
        self
    }

    const HASH_TYPE_LENGTH: usize = 4;
    const STORE_LOOKAHEAD: usize = 4;

    fn last_distances_to_check(&self) -> usize {
        self.last_distances
    }

    fn prepare(&mut self, one_shot: bool, input_size: usize, data: &[u8], clear: bool) -> Sweep {
        let partial_prepare_threshold = CHAIN_BUCKET_SIZE >> 6;
        let partial = if one_shot && input_size <= partial_prepare_threshold {
            Sweep::Partial
        } else {
            Sweep::Full
        };
        if !clear {
            return partial;
        }
        if partial == Sweep::Partial {
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
    /// Short-input storage with the H2 hash and candidate order.
    H2Small(QuickMatcher<16, 0, 5, true, true>),
    /// Short-input storage with the H3 hash and candidate order.
    H3Small(QuickMatcher<16, 1, 5, false, true>),
    /// Short-input storage with the H4 hash and candidate order.
    H4Small(QuickMatcher<17, 2, 5, true, true>),

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
                    Self::H5Narrow(BucketMatcher::new(shape, 0))
                } else {
                    Self::H5Wide(BucketMatcher::new(shape, 0))
                }
            }
            HasherPlan::H6(shape) => Self::H6(BucketMatcher::new(shape, 0)),
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
            MatchFinder::H2Small($matcher) => $body,
            MatchFinder::H3Small($matcher) => $body,
            MatchFinder::H4Small($matcher) => $body,
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
    /// Selects compact physical storage for an expected short input. Hashes,
    /// logical slots and match order are unchanged; the map grows if needed.
    pub(crate) fn for_input(plan: HasherPlan, size_hint: usize) -> Self {
        if size_hint > 0 && size_hint <= 2048 {
            match plan {
                HasherPlan::H2 => return Self::H2Small(QuickMatcher::new()),
                HasherPlan::H3 => return Self::H3Small(QuickMatcher::new()),
                HasherPlan::H4 => return Self::H4Small(QuickMatcher::new()),
                _ => {}
            }
        }
        match plan {
            HasherPlan::H5(shape) if shape.bucket_bits == 14 => {
                Self::H5Narrow(BucketMatcher::new(shape, size_hint))
            }
            HasherPlan::H5(shape) => Self::H5Wide(BucketMatcher::new(shape, size_hint)),
            HasherPlan::H6(shape) => Self::H6(BucketMatcher::new(shape, size_hint)),
            _ => Self::from(plan),
        }
    }

    /// Clears the table before the first block of a stream (`Prepare`).
    ///
    /// Returns which sweep was taken; see [`Matcher::prepare`].
    pub(crate) fn prepare(
        &mut self,
        one_shot: bool,
        input_size: usize,
        data: &[u8],
        clear: bool,
    ) -> Sweep {
        with_matcher!(self, |matcher| matcher
            .prepare(one_shot, input_size, data, clear))
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
    fn compact_slots_preserve_colliding_keys_through_growth_overwrites_and_reset() {
        let mut slots = SmallSlots::default();
        assert_eq!(slots.read(3), 0);
        for key in 0..1024 {
            slots.write(key * 32 + 3, key as u32 + 1);
        }
        for key in 0..1024 {
            assert_eq!(slots.read(key * 32 + 3), key as u32 + 1);
            slots.write(key * 32 + 3, u32::MAX);
            assert_eq!(slots.read(key * 32 + 3), u32::MAX);
        }
        assert_eq!(slots.count, 1024);
        assert_eq!(slots.read(4), 0);
        let capacity = slots.entries.capacity();
        slots.reset(16);
        assert_eq!(slots.entries.capacity(), capacity);
        assert_eq!(slots.read(3), 0);
        slots.write(3, 7);
        assert_eq!(slots.read(3), 7);
    }

    #[test]
    fn compact_quick_matchers_preserve_the_full_tables_results_on_every_backend() {
        let data = repeated();
        for plan in [HasherPlan::H2, HasherPlan::H3, HasherPlan::H4] {
            for backend in crate::compressor::Backend::available() {
                let expected = with_matcher!(MatchFinder::from(plan), |matcher| {
                    let mut matcher = primed(matcher, &data);
                    search_with(backend.0, &mut matcher, &data, REPEAT_AT)
                });
                let actual = with_matcher!(MatchFinder::for_input(plan, 16), |matcher| {
                    let mut matcher = primed(matcher, &data);
                    search_with(backend.0, &mut matcher, &data, REPEAT_AT)
                });
                assert_eq!(actual, expected, "{backend:?}, {plan:?}");
            }
        }
    }

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
        matcher.prepare(false, data.len(), &data, true);
        matcher.store(&data, usize::MAX, 0);
        assert_eq!(matcher.retained_bytes(), retained);
    }

    #[test]
    fn a_short_input_activates_starter_blocks_only_as_it_stores() {
        let mut matcher = BucketMatcher::<false, 15>::new(Q9_BUCKET, 0);
        let data = [b'a'; 64];
        matcher.prepare(true, data.len(), &data, true);
        assert_eq!(matcher.layout, Layout::Compact);
        assert!(matcher.retained_bytes() < 16 * 1024);
        matcher.store(&data, usize::MAX, 0);
        assert_eq!(matcher.blocks.len(), STARTER_SLOTS);
        matcher.store(&data, usize::MAX, 1);
        assert_eq!(matcher.blocks.len(), STARTER_SLOTS);
    }

    #[test]
    fn a_starter_block_grows_into_the_top_of_a_full_block() {
        let mut matcher = BucketMatcher::<false, 15>::new(Q9_BUCKET, 0);
        let data = [b'a'; 64];
        matcher.prepare(true, data.len(), &data, true);
        for position in 0..5 {
            matcher.store(&data, usize::MAX, position);
        }
        let block_size = matcher.block_size;
        assert_eq!(matcher.blocks.len(), STARTER_SLOTS + block_size);
        let full = &matcher.blocks[STARTER_SLOTS..];
        // Stores fill downwards from the top; the fifth lands below the four
        // the starter held.
        assert_eq!(&full[block_size - 5..], &[4, 3, 2, 1, 0]);
        let key = BucketMatcher::<false, 15>::hash_with_tag(&data, 0) >> 8;
        let (count, offset) = matcher.entry(key);
        assert_eq!(count, 5);
        assert_eq!(matcher.block(offset), Some((STARTER_SLOTS, block_size)));
        assert_eq!(matcher.block(0), None);
    }

    #[test]
    fn every_layout_finds_the_same_match_and_forgets_it_on_prepare() {
        let data = repeated();
        for backend in crate::compressor::Backend::available() {
            let mut expected = None;
            for (one_shot, input_size, size_hint, layout) in [
                (true, data.len(), data.len(), Layout::Compact),
                (true, COMPACT_INPUT_LIMIT + 1, 0, Layout::Sparse),
                (false, data.len(), 1 << 20, Layout::Dense),
            ] {
                let mut matcher = BucketMatcher::<false, 14>::new(Q5_BUCKET, size_hint);
                matcher.prepare(one_shot, input_size, &data, true);
                assert_eq!(matcher.layout, layout, "{backend:?}");
                matcher.store_range(&data, usize::MAX, 0, REPEAT_AT);
                let found = search_with(backend.0, &mut matcher, &data, REPEAT_AT);
                assert_eq!(
                    (found.distance, found.len),
                    (64, 64),
                    "{backend:?} {layout:?}"
                );
                let found = (found.distance, found.len, found.score);
                assert_eq!(
                    *expected.get_or_insert(found),
                    found,
                    "{backend:?} {layout:?}"
                );
                // A new stream of any layout starts from an empty table.
                for (next_one_shot, next_size) in [
                    (one_shot, input_size),
                    (true, 16),
                    (true, COMPACT_INPUT_LIMIT + 1),
                    (false, 0),
                ] {
                    matcher.prepare(next_one_shot, next_size, &data, true);
                    assert!(
                        !search_with(backend.0, &mut matcher, &data, REPEAT_AT).is_match(),
                        "{backend:?} {layout:?} -> {next_one_shot} {next_size}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_sparse_table_wipes_itself_when_its_generations_run_out() {
        let data = repeated();
        let mut matcher = BucketMatcher::<false, 14>::new(Q5_BUCKET, 0);
        matcher.prepare(true, COMPACT_INPUT_LIMIT + 1, &data, true);
        matcher.store_range(&data, usize::MAX, 0, REPEAT_AT);
        matcher.generation = MAX_GENERATION;
        matcher.prepare(true, COMPACT_INPUT_LIMIT + 1, &data, true);
        assert_eq!(matcher.generation, 1);
        assert!(matcher.blocks.is_empty());
        assert!(!search_at(&mut matcher, &data, REPEAT_AT).is_match());
        // A bump is what an ordinary new stream costs.
        matcher.prepare(true, COMPACT_INPUT_LIMIT + 1, &data, true);
        assert_eq!(matcher.generation, 2);
    }

    #[test]
    fn a_dense_table_keeps_its_blocks_and_clears_only_its_counters() {
        let data = repeated();
        let mut matcher = BucketMatcher::<false, 14>::new(Q5_BUCKET, 1 << 20);
        matcher.prepare(false, data.len(), &data, true);
        matcher.store_range(&data, usize::MAX, 0, REPEAT_AT);
        let retained = matcher.retained_bytes();
        matcher.prepare(false, data.len(), &data, true);
        assert_eq!(matcher.retained_bytes(), retained);
        assert!(matcher.num.iter().all(|&count| count == 0));
        assert!(!search_at(&mut matcher, &data, REPEAT_AT).is_match());
    }

    #[test]
    fn the_key_map_keeps_colliding_keys_through_growth_and_reset() {
        let mut map = KeyMap::default();
        assert_eq!(map.get(3), (0, 0));
        map.reset(4);
        for key in 0..1024 {
            map.set(key * 64 + 3, key as u16 + 1, key as u32 + 7);
        }
        for key in 0..1024 {
            assert_eq!(map.get(key * 64 + 3), (key as u16 + 1, key as u32 + 7));
        }
        assert_eq!(map.get(5), (0, 0));
        map.set(67, 9, 9);
        assert_eq!(map.get(67), (9, 9));
        map.reset(4);
        assert_eq!(map.get(3), (0, 0));
        assert_eq!(map.count, 0);
    }

    #[test]
    fn candidate_masks_walk_a_block_newest_to_oldest() {
        // A block of eight slots whose newest position is at slot 5, holding
        // three positions: slots 5, 6 and 7 in that order.
        let (above, below) = split_candidates(u32::MAX, 8, 5, 3);
        assert_eq!((above, below), (0b1110_0000, 0));
        // Six positions: slots 5, 6, 7, then 0, 1, 2.
        let (above, below) = split_candidates(u32::MAX, 8, 5, 6);
        assert_eq!((above, below), (0b1110_0000, 0b0000_0111));
        // A full block visits everything; the equality mask filters it.
        let (above, below) = split_candidates(0b1010_1010, 8, 2, 8);
        assert_eq!((above, below), (0b1010_1000, 0b0000_0010));
        // Thirty-two lanes at the top of the word.
        let (above, below) = split_candidates(u32::MAX, 32, 31, 32);
        assert_eq!((above, below), (1 << 31, u32::MAX >> 1));
    }

    #[test]
    fn tag_equality_matches_scalar_comparison_on_every_backend() {
        for backend in crate::compressor::Backend::available() {
            for capacity in [16usize, 32] {
                let tags: Vec<u8> = (0..capacity).map(|i| (i % 5) as u8).collect();
                for tag in 0..5u8 {
                    let expected: u32 = tags
                        .iter()
                        .enumerate()
                        .filter(|&(_, &value)| value == tag)
                        .fold(0, |mask, (slot, _)| mask | 1 << slot);
                    let actual = dispatch!(backend.0, simd => tag_equality(simd, &tags, tag));
                    if backend == crate::compressor::Backend::SCALAR {
                        assert_eq!(actual, u32::MAX);
                    } else {
                        assert_eq!(actual, expected, "{backend:?} {capacity} {tag}");
                    }
                }
            }
            // A starter block is too short for a vector compare.
            assert_eq!(
                dispatch!(backend.0, simd => tag_equality(simd, &[1, 2, 3, 4], 2)),
                u32::MAX
            );
        }
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
            window: data,
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
        matcher.prepare(true, data.len(), data, true);
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

        let mut h5 = primed(BucketMatcher::<false, 14>::new(Q5_BUCKET, 0), &data);
        let found = search_at(&mut h5, &data, REPEAT_AT);
        assert_eq!((found.distance, found.len), (64, 64));

        let mut h6 = primed(
            BucketMatcher::<true, 15>::new(
                BucketShape {
                    bucket_bits: 15,
                    ..Q5_BUCKET
                },
                0,
            ),
            &data,
        );
        let found = search_at(&mut h6, &data, REPEAT_AT);
        assert_eq!((found.distance, found.len), (64, 64));

        // The deepest bucket quality nine asks for finds the same repeat.
        let mut deep = primed(BucketMatcher::<false, 15>::new(Q9_BUCKET, 0), &data);
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
            let mut matcher = BucketMatcher::<false, 15>::new(
                BucketShape {
                    bucket_bits: 15,
                    ..shape
                },
                0,
            );
            matcher.prepare(true, data.len(), &data, true);
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
        let mut matcher = BucketMatcher::<false, 14>::new(Q5_BUCKET, 0);
        matcher.prepare(true, data.len(), &data, true);
        matcher.store_range(&data, usize::MAX, 0, 100);
        let found = search_at(&mut matcher, &data, 100);
        assert!(found.is_match());
        assert!(found.distance <= 16);
    }

    #[test]
    fn nothing_is_found_when_the_table_holds_no_candidate() {
        let data = repeated();
        let mut matcher = QuickMatcher::<16, 1, 5, false>::new();
        matcher.prepare(true, data.len(), &data, true);
        assert!(!search_at(&mut matcher, &data, REPEAT_AT).is_match());

        let mut chain = ChainMatcher::<1, 16>::new(Q5_CHAIN);
        chain.prepare(true, data.len(), &data, true);
        assert!(!search_at(&mut chain, &data, REPEAT_AT).is_match());

        let mut bucket = BucketMatcher::<false, 14>::new(Q5_BUCKET, 0);
        bucket.prepare(true, data.len(), &data, true);
        assert!(!search_at(&mut bucket, &data, REPEAT_AT).is_match());
    }

    #[test]
    fn a_full_preparation_clears_what_a_previous_stream_stored() {
        let data = repeated();
        let mut matcher = primed(QuickMatcher::<16, 1, 5, false>::new(), &data);
        assert!(search_at(&mut matcher, &data, REPEAT_AT).is_match());
        matcher.prepare(false, 0, &data, true);
        assert!(!search_at(&mut matcher, &data, REPEAT_AT).is_match());

        let mut chain = primed(ChainMatcher::<1, 16>::new(Q5_CHAIN), &data);
        assert!(search_at(&mut chain, &data, REPEAT_AT).is_match());
        chain.prepare(false, 0, &data, true);
        assert!(!search_at(&mut chain, &data, REPEAT_AT).is_match());

        let mut bucket = primed(BucketMatcher::<false, 14>::new(Q5_BUCKET, 0), &data);
        assert!(search_at(&mut bucket, &data, REPEAT_AT).is_match());
        bucket.prepare(false, 0, &data, true);
        assert!(!search_at(&mut bucket, &data, REPEAT_AT).is_match());
    }

    #[test]
    fn every_backend_agrees_on_the_match_it_finds() {
        let data = repeated();
        for block_bits in 4..=8 {
            let shape = BucketShape {
                bucket_bits: if block_bits <= 5 { 14 } else { 15 },
                block_bits,
                last_distances: 4,
            };
            for plan in [
                HasherPlan::H5(shape),
                HasherPlan::H6(BucketShape {
                    bucket_bits: 15,
                    ..shape
                }),
            ] {
                let mut results = Vec::new();
                for backend in crate::compressor::Backend::available() {
                    let finder = MatchFinder::from(plan);
                    let found = with_matcher!(finder, |matcher| {
                        let mut matcher = primed(matcher, &data);
                        search_with(backend.0, &mut matcher, &data, REPEAT_AT)
                    });
                    assert_eq!((found.distance, found.len), (64, 64));
                    results.push(found);
                }
                assert!(results.windows(2).all(|pair| pair[0] == pair[1]));
            }
        }
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
        matcher.prepare(true, data.len(), &data, true);
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
        matcher.prepare(true, data.len(), &data, true);
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
            BucketMatcher::<false, 15>::new(Q9_BUCKET, 0).last_distances_to_check(),
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
