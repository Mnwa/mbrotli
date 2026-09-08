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

use fearless_simd::{Simd, SimdBase, SimdMask, u8x16, u8x32};

use super::params::{BucketShape, ChainShape, HasherPlan};
use crate::compressor::core::shared::constants::HASH_MUL32;
use crate::compressor::core::shared::dictionary::{self, DictionaryStats};
use crate::compressor::core::shared::match_len::{current_window, match_len_at, match_len_windows};
use crate::compressor::core::shared::score::{
    SearchResult, backward_reference_penalty_using_last_distance, backward_reference_score,
    backward_reference_score_using_last_distance,
};

/// Sixty-four-bit hash multiplier (`kHashMul64`).
const HASH_MUL64: u64 = 0x1FE3_5A7B_D357_9BD3;

/// How many distances a search may probe (`BROTLI_NUM_DISTANCE_SHORT_CODES`).
const NUM_DISTANCE_SHORT_CODES: usize = 16;

/// The sixteen distances a search may probe (`BROTLI_NUM_DISTANCE_SHORT_CODES`).
///
/// Only the first four are real history; the rest are near misses derived from
/// them by [`prepare_distance_cache`].
pub(crate) type DistanceCache = [i32; NUM_DISTANCE_SHORT_CODES];

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
    /// Offset the stream starts at (`stream_offset`), zero for ordinary ones.
    pub(crate) position_offset: usize,
    /// Cap on the distance to the start of the stream (`max_backward_limit`).
    pub(crate) dictionary_limit: usize,
    /// Distance shift that addresses the attached dictionary (`gap`).
    pub(crate) gap: usize,
    /// Longest distance the distance alphabet can express.
    pub(crate) max_distance: usize,
}

impl MatchQuery<'_> {
    /// Returns the distance to the start of the stream, capped to the window
    /// (`dictionary_start`).
    ///
    /// Derived here rather than stored: only the dictionary probe and the
    /// attached-prefix search need it, and computing it at every position
    /// for the matcher costs an add and a compare that mostly go unused.
    #[inline(always)]
    pub(crate) fn dictionary_start(&self) -> usize {
        (self.cur_ix + self.position_offset).min(self.dictionary_limit)
    }

    /// Returns the distance at which the static dictionary begins.
    #[inline(always)]
    fn dictionary_distance(&self) -> usize {
        self.dictionary_start() + self.gap
    }

    fn search_dictionary(self, stats: &mut DictionaryStats, out: &mut SearchResult, shallow: bool) {
        let data = self.data.get(self.cur_ix & self.mask..).unwrap_or_default();
        #[cfg(feature = "experimental")]
        if let Some(custom) = self.custom {
            dictionary::search_custom(
                custom,
                stats,
                data,
                self.max_length,
                self.dictionary_distance(),
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
            self.dictionary_distance(),
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

    /// Bytes a store needs available (`StoreLookahead`).
    const STORE_LOOKAHEAD: usize;

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
    const STORE_LOOKAHEAD: usize = M::STORE_LOOKAHEAD;

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

/// A block of searches, generic over the view it runs through.
///
/// A finder whose tables can be laid out more than one way hands the visitor
/// the concrete view its current layout uses, so the visitor's loop is
/// compiled once per view rather than once over a union of them: a loop that
/// carries every layout's state at once spills the one it is actually using.
pub(crate) trait RunVisitor {
    /// What the block of searches produces.
    type Output;

    /// Runs the block through `run`.
    fn visit<R: MatchRun>(self, run: R) -> Self::Output;
}

/// A match finder over the ring buffer.
pub(crate) trait Matcher {
    /// Bytes a candidate needs available to be hashed (`HashTypeLength`).
    const HASH_TYPE_LENGTH: usize;

    /// Bytes a store needs available (`StoreLookahead`).
    const STORE_LOOKAHEAD: usize;

    /// Borrows the tables for a block of searches and stores, as the view
    /// the current layout uses; see [`RunVisitor`].
    fn visit_run<V: RunVisitor>(&mut self, visitor: V) -> V::Output;

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
///
/// An entry packs the slot above the position plus one, so zero is empty
/// and the map is a zeroed word per entry: half the clearing of a two-word
/// entry, which for a kilobyte of input is most of what the map costs. The
/// packing holds for slots below 2^17 — every quick shape — and positions
/// below [`SMALL_SLOTS_MAX_INPUT`], which [`QuickMatcher::prepare`] only
/// admits for a one-shot stream of at most that many bytes.
#[derive(Default)]
struct SmallSlots {
    entries: Vec<u32>,
    count: usize,
}

/// Bits of a [`SmallSlots`] entry holding the position plus one.
const SMALL_SLOTS_VALUE_BITS: u32 = 15;

/// Mask of the position field of a [`SmallSlots`] entry.
const SMALL_SLOTS_VALUE_MASK: u32 = (1 << SMALL_SLOTS_VALUE_BITS) - 1;

/// Longest one-shot stream whose positions a [`SmallSlots`] entry can hold.
const SMALL_SLOTS_MAX_INPUT: usize = (1 << SMALL_SLOTS_VALUE_BITS) - 1;

impl SmallSlots {
    /// Returns the index `slot` occupies, or the empty index it would take.
    ///
    /// Open addressing over a power-of-two table: the index is always masked
    /// to the table's length, so the loop carries no bounds check. The map
    /// is never more than half full, so an empty index is always found.
    #[inline(always)]
    fn find(&self, slot: usize) -> usize {
        let mask = self.entries.len().wrapping_sub(1);
        let tag = (slot as u32) << SMALL_SLOTS_VALUE_BITS;
        let mut index = slot & mask;
        loop {
            let entry = self.entries[index & mask];
            if entry == 0 || entry & !SMALL_SLOTS_VALUE_MASK == tag {
                return index & mask;
            }
            index = index.wrapping_add(1);
        }
    }

    #[inline(always)]
    fn read(&self, slot: usize) -> u32 {
        if self.entries.is_empty() {
            return 0;
        }
        let entry = self.entries[self.find(slot) & (self.entries.len() - 1)];
        if entry == 0 {
            0
        } else {
            (entry & SMALL_SLOTS_VALUE_MASK) - 1
        }
    }

    #[inline(always)]
    fn write(&mut self, slot: usize, value: u32) {
        self.replace(slot, value);
    }

    /// Writes `value` into `slot`, returning what it held, or zero.
    #[inline(always)]
    fn replace(&mut self, slot: usize, value: u32) -> u32 {
        if 2 * (self.count + 1) > self.entries.len() {
            self.grow();
        }
        let index = self.find(slot) & (self.entries.len() - 1);
        let entry = &mut self.entries[index];
        let previous = if *entry == 0 {
            self.count += 1;
            0
        } else {
            (*entry & SMALL_SLOTS_VALUE_MASK) - 1
        };
        *entry = ((slot as u32) << SMALL_SLOTS_VALUE_BITS)
            | ((value & SMALL_SLOTS_VALUE_MASK).wrapping_add(1) & SMALL_SLOTS_VALUE_MASK);
        previous
    }

    fn grow(&mut self) {
        let size = (self.entries.len() * 2).max(32);
        let previous = std::mem::replace(&mut self.entries, vec![0; size]);
        self.count = 0;
        for entry in previous {
            if entry != 0 {
                self.write(
                    (entry >> SMALL_SLOTS_VALUE_BITS) as usize,
                    (entry & SMALL_SLOTS_VALUE_MASK) - 1,
                );
            }
        }
    }

    /// Empties the map, sized so `input_size` distinct keys never grow it.
    fn reset(&mut self, input_size: usize) {
        let size = (2 * input_size).next_power_of_two().max(32);
        if self.entries.len() < size {
            self.entries = vec![0; size];
        } else {
            self.entries.fill(0);
        }
        self.count = 0;
    }
}

/// Slot storage a quick run indexes through: the full table or the map.
pub(crate) trait QuickSlots {
    /// Reads slot `slot`.
    fn read(&self, slot: usize) -> u32;

    /// Writes slot `slot`.
    fn write(&mut self, slot: usize, value: u32);

    /// Writes slot `slot`, returning what it held.
    ///
    /// The single-slot shapes read and then overwrite the same slot at
    /// every position; the map finds it once for both.
    fn replace(&mut self, slot: usize, value: u32) -> u32;
}

/// The full table: an array, so a slot masked to its size needs no check.
impl<const N: usize> QuickSlots for &mut [u32; N] {
    #[inline(always)]
    fn read(&self, slot: usize) -> u32 {
        self[slot & (N - 1)]
    }

    #[inline(always)]
    fn write(&mut self, slot: usize, value: u32) {
        self[slot & (N - 1)] = value;
    }

    #[inline(always)]
    fn replace(&mut self, slot: usize, value: u32) -> u32 {
        std::mem::replace(&mut self[slot & (N - 1)], value)
    }
}

impl QuickSlots for &mut SmallSlots {
    #[inline(always)]
    fn read(&self, slot: usize) -> u32 {
        SmallSlots::read(self, slot)
    }

    #[inline(always)]
    fn write(&mut self, slot: usize, value: u32) {
        SmallSlots::write(self, slot, value);
    }

    #[inline(always)]
    fn replace(&mut self, slot: usize, value: u32) -> u32 {
        SmallSlots::replace(self, slot, value)
    }
}

/// Quick match finder with one hash bucket sweep (`HashLongestMatchQuickly`).
///
/// `BUCKETS` sizes the table, `SWEEP_BITS` says how many neighbouring slots
/// one hash owns, `HASH_LEN` how many bytes feed the hash, and
/// `USE_DICTIONARY` whether a miss falls back to the static dictionary. The
/// bucket count is compile-time so the hash shift and every slot mask are
/// immediates and the table is an array whose indexing needs no check.
///
/// A `COMPACT` matcher is built for an input of at most two kibibytes. Its
/// first stream indexes a small map instead of the table, so a one-shot call
/// never clears the table; from its second stream on it allocates the table,
/// which a warmed matcher then clears by the partial sweep like any other.
pub(crate) struct QuickMatcher<
    const BUCKETS: usize,
    const SWEEP_BITS: u32,
    const HASH_LEN: u32,
    const USE_DICTIONARY: bool,
    const COMPACT: bool = false,
> {
    /// The full table; `None` while a compact matcher is on its first stream.
    buckets: Option<Box<[u32; BUCKETS]>>,
    /// The map a compact matcher's first stream indexes through.
    compact: SmallSlots,
}

impl<
    const BUCKETS: usize,
    const SWEEP_BITS: u32,
    const HASH_LEN: u32,
    const USE_DICTIONARY: bool,
    const COMPACT: bool,
> QuickMatcher<BUCKETS, SWEEP_BITS, HASH_LEN, USE_DICTIONARY, COMPACT>
{
    /// Base-2 logarithm of the number of slots.
    const BUCKET_BITS: u32 = BUCKETS.trailing_zeros();

    /// Mask that keeps a slot index inside the table.
    const BUCKET_MASK: usize = BUCKETS - 1;

    /// Number of slots one hash sweeps over.
    const SWEEP: usize = 1usize << SWEEP_BITS;

    /// Mask picking the slot of the sweep a position is stored into.
    const SWEEP_MASK: usize = (Self::SWEEP - 1) << 3;

    /// Returns the bytes this match finder keeps allocated.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.buckets.as_ref().map_or(0, |table| table.len()) * size_of::<u32>()
            + self.compact.entries.capacity() * size_of::<u32>()
    }

    /// Creates an empty table.
    pub(crate) fn new() -> Self {
        Self {
            buckets: if COMPACT { None } else { Self::table() },
            compact: SmallSlots::default(),
        }
    }

    /// Allocates a zeroed table.
    fn table() -> Option<Box<[u32; BUCKETS]>> {
        vec![0u32; BUCKETS].into_boxed_slice().try_into().ok()
    }

    /// Returns the bucket of the bytes at `offset` (`HashBytes`).
    #[inline(always)]
    fn hash(data: &[u8], offset: usize) -> usize {
        let value = read_u64(data, offset) << (64 - 8 * HASH_LEN as u64);
        (value.wrapping_mul(HASH_MUL64) >> (64 - Self::BUCKET_BITS)) as usize
    }
}

/// The matcher a [`QuickRun`] views, for its constants.
type QuickShape<
    const BUCKETS: usize,
    const SWEEP_BITS: u32,
    const HASH_LEN: u32,
    const USE_DICTIONARY: bool,
> = QuickMatcher<BUCKETS, SWEEP_BITS, HASH_LEN, USE_DICTIONARY>;

/// A [`QuickMatcher`]'s slots borrowed for one block; see [`MatchRun`].
pub(crate) struct QuickRun<
    T: QuickSlots,
    const BUCKETS: usize,
    const SWEEP_BITS: u32,
    const HASH_LEN: u32,
    const USE_DICTIONARY: bool,
> {
    slots: T,
}

impl<
    T: QuickSlots,
    const BUCKETS: usize,
    const SWEEP_BITS: u32,
    const HASH_LEN: u32,
    const USE_DICTIONARY: bool,
> QuickRun<T, BUCKETS, SWEEP_BITS, HASH_LEN, USE_DICTIONARY>
{
    /// Returns the slot the position `ix` hashing to `key` is stored into.
    #[inline(always)]
    const fn slot_of(key: usize, ix: usize) -> usize {
        if QuickShape::<BUCKETS, SWEEP_BITS, HASH_LEN, USE_DICTIONARY>::SWEEP == 1 {
            key
        } else {
            (key + (ix & QuickShape::<BUCKETS, SWEEP_BITS, HASH_LEN, USE_DICTIONARY>::SWEEP_MASK))
                & QuickShape::<BUCKETS, SWEEP_BITS, HASH_LEN, USE_DICTIONARY>::BUCKET_MASK
        }
    }
}

impl<
    T: QuickSlots,
    const BUCKETS: usize,
    const SWEEP_BITS: u32,
    const HASH_LEN: u32,
    const USE_DICTIONARY: bool,
> MatchRun for QuickRun<T, BUCKETS, SWEEP_BITS, HASH_LEN, USE_DICTIONARY>
{
    const HASH_TYPE_LENGTH: usize = 8;
    const STORE_LOOKAHEAD: usize = 8;

    #[inline(always)]
    fn store(&mut self, data: &[u8], mask: usize, ix: usize) {
        let key =
            QuickShape::<BUCKETS, SWEEP_BITS, HASH_LEN, USE_DICTIONARY>::hash(data, ix & mask);
        self.slots.write(Self::slot_of(key, ix), ix as u32);
    }

    fn store_range(&mut self, data: &[u8], mask: usize, start: usize, end: usize) {
        for ix in start..end {
            self.store(data, mask, ix);
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
        let slots = &mut self.slots;
        let cur_ix_masked = query.cur_ix & query.mask;
        // The window a candidate is measured against, cut only when one
        // passes the byte compare; most positions never get that far.
        let cur = || current_window(data, cur_ix_masked, query.max_length);
        let best_len_in = out.len;
        let mut compare_char = read_u8(data, cur_ix_masked + best_len_in);
        let key =
            QuickShape::<BUCKETS, SWEEP_BITS, HASH_LEN, USE_DICTIONARY>::hash(data, cur_ix_masked);
        let min_score = out.score;
        let mut best_score = out.score;
        let mut best_len = best_len_in;

        out.len_code_delta = 0;

        let cached_backward = query.cache[0] as usize;
        let prev_ix = query.cur_ix.wrapping_sub(cached_backward);
        if prev_ix < query.cur_ix {
            let prev_ix = prev_ix & query.mask;
            if compare_char == read_u8(data, prev_ix + best_len) {
                let len = match_len_at(simd, data, prev_ix, cur());
                if len >= 4 {
                    let score = backward_reference_score_using_last_distance(len);
                    if best_score < score {
                        out.len = len;
                        out.distance = cached_backward;
                        out.score = score;
                        if QuickShape::<BUCKETS, SWEEP_BITS, HASH_LEN, USE_DICTIONARY>::SWEEP == 1 {
                            slots.write(key, query.cur_ix as u32);
                            return;
                        }
                        best_len = len;
                        best_score = score;
                        compare_char = read_u8(data, cur_ix_masked + len);
                    }
                }
            }
        }

        if QuickShape::<BUCKETS, SWEEP_BITS, HASH_LEN, USE_DICTIONARY>::SWEEP == 1 {
            // Only one candidate: the store happens before the comparison, so
            // the slot always ends up holding the current position.
            let prev_ix = slots.replace(key, query.cur_ix as u32) as usize;
            let backward = query.cur_ix.wrapping_sub(prev_ix);
            let prev_ix = prev_ix & query.mask;
            if compare_char != read_u8(data, prev_ix + best_len_in) {
                return;
            }
            if backward == 0 || backward > query.max_backward {
                return;
            }
            let len = match_len_at(simd, data, prev_ix, cur());
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
            for sweep in 0..QuickShape::<BUCKETS, SWEEP_BITS, HASH_LEN, USE_DICTIONARY>::SWEEP {
                let slot = (key + (sweep << 3))
                    & QuickShape::<BUCKETS, SWEEP_BITS, HASH_LEN, USE_DICTIONARY>::BUCKET_MASK;
                let prev_ix = slots.read(slot) as usize;
                let backward = query.cur_ix.wrapping_sub(prev_ix);
                let prev_ix = prev_ix & query.mask;
                if compare_char != read_u8(data, prev_ix + best_len) {
                    continue;
                }
                if backward == 0 || backward > query.max_backward {
                    continue;
                }
                let len = match_len_at(simd, data, prev_ix, cur());
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
        // The sweeping variant writes its own slot last; the single-slot one
        // has already written it, which is why the reference guards this
        // store with `BUCKET_SWEEP != 1`.
        if QuickShape::<BUCKETS, SWEEP_BITS, HASH_LEN, USE_DICTIONARY>::SWEEP != 1 {
            slots.write(Self::slot_of(key, query.cur_ix), query.cur_ix as u32);
        }
    }
}

impl<
    const BUCKETS: usize,
    const SWEEP_BITS: u32,
    const HASH_LEN: u32,
    const USE_DICTIONARY: bool,
    const COMPACT: bool,
> Matcher for QuickMatcher<BUCKETS, SWEEP_BITS, HASH_LEN, USE_DICTIONARY, COMPACT>
{
    const HASH_TYPE_LENGTH: usize = 8;
    const STORE_LOOKAHEAD: usize = 8;

    fn visit_run<V: RunVisitor>(&mut self, visitor: V) -> V::Output {
        match &mut self.buckets {
            Some(table) => {
                visitor.visit(
                    QuickRun::<_, BUCKETS, SWEEP_BITS, HASH_LEN, USE_DICTIONARY> {
                        slots: &mut **table,
                    },
                )
            }
            None => visitor.visit(
                QuickRun::<_, BUCKETS, SWEEP_BITS, HASH_LEN, USE_DICTIONARY> {
                    slots: &mut self.compact,
                },
            ),
        }
    }

    fn prepare(&mut self, one_shot: bool, input_size: usize, data: &[u8], clear: bool) -> Sweep {
        // Clearing only the slots a short input can reach is far cheaper than
        // wiping the whole table, and reaches exactly the same slots the
        // search will later look at.
        let partial_prepare_threshold = BUCKETS >> 5;
        let partial = if one_shot && input_size <= partial_prepare_threshold {
            Sweep::Partial
        } else {
            Sweep::Full
        };
        let Some(table) = &mut self.buckets else {
            if clear || !one_shot || input_size > SMALL_SLOTS_MAX_INPUT {
                // A compact matcher past its first stream, or one whose
                // stream the map cannot hold: the map has served its
                // purpose, and a fresh table needs no clearing.
                self.buckets = Self::table();
                if self.buckets.is_some() {
                    self.compact = SmallSlots::default();
                    return partial;
                }
            }
            // A fresh map is sized for the input either way, so it never
            // grows and rehashes while the stream stores into it.
            if clear || self.compact.entries.is_empty() {
                self.compact.reset(input_size);
            }
            return partial;
        };
        if !clear {
            return partial;
        }
        if partial == Sweep::Partial {
            for offset in 0..input_size {
                let key = Self::hash(data, offset);
                if Self::SWEEP == 1 {
                    table[key & Self::BUCKET_MASK] = 0;
                } else {
                    for sweep in 0..Self::SWEEP {
                        table[(key + (sweep << 3)) & Self::BUCKET_MASK] = 0;
                    }
                }
            }
        } else {
            table.fill(0);
        }
        partial
    }

    fn store(&mut self, data: &[u8], mask: usize, ix: usize) {
        self.visit_run(Store {
            data,
            mask,
            start: ix,
            end: ix + 1,
        });
    }

    fn store_range(&mut self, data: &[u8], mask: usize, start: usize, end: usize) {
        self.visit_run(Store {
            data,
            mask,
            start,
            end,
        });
    }

    fn find_longest_match<S: Simd>(
        &mut self,
        simd: S,
        stats: &mut DictionaryStats,
        query: MatchQuery<'_>,
        out: &mut SearchResult,
    ) {
        self.visit_run(Search {
            simd,
            stats,
            query,
            out,
        });
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

/// Slots a starter block holds; a bucket's fifth store grows it to full depth.
///
/// Quality nine keeps two hundred and fifty-six positions per bucket, a
/// kilobyte each, and a short input activates a bucket for almost every
/// position it hashes. Starting small keeps that input from zeroing a
/// mebibyte it never reads.
const STARTER_SLOTS: usize = 4;

/// Bits of a sparse entry holding the wrapping store counter.
const COUNT_BITS: u32 = 16;

/// Bits of a sparse entry holding the block index and its starter flag.
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

    /// Returns the slot `key` occupies, or the empty slot it would take,
    /// with the entry found there.
    ///
    /// Open addressing over a power-of-two table: the index is always masked
    /// to the table's length, so the loop carries no bounds check. The map
    /// is never more than half full, so an empty slot is always found.
    #[inline(always)]
    fn find(&self, key: usize) -> (usize, u64) {
        let mask = self.entries.len().wrapping_sub(1);
        let mut slot = key & mask;
        loop {
            let entry = self.entries[slot & mask];
            if entry == Self::EMPTY || (entry >> 48) as usize == key {
                return (slot & mask, entry);
            }
            slot = slot.wrapping_add(1);
        }
    }

    /// Decodes the counter and offset of an entry; an empty one holds zeros.
    #[inline(always)]
    const fn decode(entry: u64) -> (u16, u32) {
        if entry == Self::EMPTY {
            (0, 0)
        } else {
            ((entry >> 32) as u16, entry as u32)
        }
    }

    /// Returns the slot a store into `key` writes and the counter and
    /// offset it holds, growing the map first when it is more than half
    /// full so the slot stays valid.
    #[inline(always)]
    fn slot_for_write(&mut self, key: usize) -> (usize, u16, u32) {
        if 2 * (self.count + 1) > self.entries.len() {
            self.grow();
        }
        let (slot, entry) = self.find(key);
        let (count, offset) = Self::decode(entry);
        (slot, count, offset)
    }

    /// Records `count` and `offset` for `key` at `slot`, which
    /// [`KeyMap::slot_for_write`] returned for that key.
    #[inline(always)]
    fn write_slot(&mut self, slot: usize, key: usize, count: u16, offset: u32) {
        let mask = self.entries.len().wrapping_sub(1);
        let entry = &mut self.entries[slot & mask];
        self.count += usize::from(*entry == Self::EMPTY);
        *entry = ((key as u64) << 48) | (u64::from(count) << 32) | u64::from(offset);
    }

    /// Returns the counter and offset stored for `key`, or zeros.
    #[inline(always)]
    fn get(&self, key: usize) -> (u16, u32) {
        if self.entries.is_empty() {
            return (0, 0);
        }
        Self::decode(self.find(key).1)
    }

    /// Records `count` and `offset` for `key`, growing the map when it is
    /// more than half full.
    #[inline(always)]
    fn set(&mut self, key: usize, count: u16, offset: u32) {
        let (slot, _, _) = self.slot_for_write(key);
        self.write_slot(slot, key, count, offset);
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
    /// One chain of stored positions, indexed through the key map.
    Compact,
    /// Blocks activated on demand, indexed through the sparse entry table.
    Sparse,
    /// Every bucket's block preallocated at its key.
    Dense,
}

/// An activated on-demand block, by index into its pool.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum BlockRef {
    /// A block of [`STARTER_SLOTS`] slots.
    Starter(usize),
    /// A block of the shape's full depth.
    Full(usize),
}

/// Returns how many cached distances a bucket of `block` slots probes.
///
/// The reference's `ChooseHasher` uses four below quality seven, ten below
/// quality nine and all sixteen at quality nine; the block depth is the
/// quality less one, so the count follows from the depth.
const fn last_distances_for(block: usize) -> usize {
    if block <= 32 {
        4
    } else if block <= 128 {
        10
    } else {
        16
    }
}

/// Bucketed match finder keeping the most recent positions per hash.
///
/// `HASH64` selects the H6 variant, which hashes eight bytes instead of four
/// and pre-filters candidates on their first four bytes. `BUCKETS` is the
/// number of buckets and `BLOCK` the number of slots per bucket, both
/// compile-time so the hash shift, every block index and every slot mask is
/// an immediate and no table bound has to be held in a register: a search
/// loop that carries a runtime depth spills the values it needs at every
/// candidate. The depth also fixes how many cached distances a search
/// probes ([`last_distances_for`]) and whether slots carry tags.
///
/// # Storage
///
/// The reference allocates every bucket's block up front and never
/// initialises it, reading a slot only below the counter that guards it.
/// Safe Rust has to initialise what it reads, so the layout follows the
/// input. A matcher built for at least [`BucketMatcher::dense_limit`] bytes
/// gets the reference's dense table: one block per bucket, zeroed once per
/// matcher and never again, with a two-byte counter per bucket cleared per
/// stream. Every other stream, including one of unknown length, activates
/// blocks on demand instead, each starting as a [`STARTER_SLOTS`]-slot block
/// until its fifth store, and indexes them through a table of packed entries
/// — generation stamp, block index, counter — that a new stream empties by
/// bumping the generation. A one-shot stream of at most
/// [`COMPACT_INPUT_LIMIT`] bytes uses neither: it links every store into a
/// per-bucket chain indexed through a small [`KeyMap`], so a sixteen-byte
/// call never zeroes the table and a store is one push.
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
pub(crate) struct BucketMatcher<const HASH64: bool, const BUCKETS: usize, const BLOCK: usize> {
    layout: Layout,
    /// Dense layout: one wrapping store counter per bucket.
    num: Vec<u16>,
    /// Dense layout: one block per bucket, allocated once.
    ///
    /// Kept flat rather than as a vector of blocks: a zero-filled vector of
    /// plain integers comes straight from the allocator's zeroed pages,
    /// while one of arrays longer than sixteen elements is written out
    /// element by element, which for a two-mebibyte table costs more than
    /// compressing a hundred kibibytes. The block view is bound per run.
    dense: Vec<u32>,
    /// Dense layout: one tag per slot; empty for untagged shapes.
    dense_tags: Vec<u8>,
    /// Sparse layout: one packed entry per bucket, allocated on first use.
    ///
    /// A boxed array rather than a vector: with the bucket count a constant
    /// and the key masked to it, no index into the table needs a check.
    entries: Option<Box<[u64; BUCKETS]>>,
    /// Generation stamp a live sparse entry carries; never zero.
    generation: u64,
    /// Compact layout: counter and chain head per stored bucket.
    compact: KeyMap,
    /// Compact layout: every stored position, each linked to the previous
    /// store into its bucket. The low word is the position, the high word
    /// the one-based index of the next older node, zero at the end.
    ///
    /// A one-shot stream of at most [`COMPACT_INPUT_LIMIT`] bytes stores
    /// that many positions at most, so the chain is one push per store and
    /// the scan walks it newest to oldest, which is the order a block is
    /// scanned in; the deepest [`BLOCK`](BucketMatcher) nodes are all the
    /// block would have kept.
    chain: Vec<u64>,
    /// Compact and sparse layouts: activated full blocks.
    blocks: Vec<[u32; BLOCK]>,
    /// One tag per slot of `blocks`; empty for untagged shapes.
    block_tags: Vec<[u8; BLOCK]>,
    /// Compact and sparse layouts: activated starter blocks.
    starters: Vec<[u32; STARTER_SLOTS]>,
    /// One tag per slot of `starters`; empty for untagged shapes.
    starter_tags: Vec<[u8; STARTER_SLOTS]>,
    /// Total input the matcher was built for; zero when unknown.
    size_hint: usize,
    /// Streams prepared so far, saturating; the layout choice for a deep
    /// shape depends on whether the matcher has been reused.
    streams: u32,
}

impl<const HASH64: bool, const BUCKETS: usize, const BLOCK: usize>
    BucketMatcher<HASH64, BUCKETS, BLOCK>
{
    /// Base-2 logarithm of the number of buckets.
    const BUCKET_BITS: u32 = BUCKETS.trailing_zeros();

    /// Whether blocks carry tags (the shallow `H58`/`H68` shapes).
    const TAGGED: bool = BLOCK <= 32;

    /// How many cached distances a search probes.
    const LAST_DISTANCES: usize = last_distances_for(BLOCK);

    /// Returns the bytes this match finder keeps allocated.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.num.capacity() * size_of::<u16>()
            + self.dense.capacity() * size_of::<u32>()
            + self.blocks.capacity() * size_of::<[u32; BLOCK]>()
            + (self.entries.as_ref().map_or(0, |entries| entries.len())
                + self.compact.entries.capacity()
                + self.chain.capacity())
                * size_of::<u64>()
            + self.dense_tags.capacity()
            + self.block_tags.capacity() * size_of::<[u8; BLOCK]>()
            + self.starters.capacity() * size_of::<[u32; STARTER_SLOTS]>()
            + self.starter_tags.capacity() * size_of::<[u8; STARTER_SLOTS]>()
    }

    /// Creates an empty table expecting `size_hint` bytes of input in total
    /// (zero when unknown).
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub(crate) fn new(size_hint: usize) -> Self {
        Self {
            layout: Layout::Sparse,
            num: Vec::new(),
            dense: Vec::new(),
            dense_tags: Vec::new(),
            entries: None,
            generation: 1,
            compact: KeyMap::default(),
            chain: Vec::new(),
            blocks: Vec::new(),
            block_tags: Vec::new(),
            starters: Vec::new(),
            starter_tags: Vec::new(),
            size_hint,
            streams: 0,
        }
    }

    /// The input the layout choice is made for.
    ///
    /// A stream of unknown length is taken to be long when the table is
    /// small: the reference always uses the dense table, and a stream
    /// written in pieces without a size hint would otherwise store every
    /// position through the on-demand index's dependent loads, at several
    /// times the cost of a counter and a block. The deep shapes keep the
    /// on-demand layouts for an unknown length, because their tables cost
    /// mebibytes to clear and only a long stream repays that.
    const fn expected_input(&self) -> usize {
        if self.size_hint == 0 && Self::TAGGED {
            usize::MAX
        } else {
            self.size_hint
        }
    }

    /// Shortest known input that gets the dense table.
    ///
    /// The table is zeroed once per matcher, which costs a clear or the
    /// page faults of its whole size, so it has to be small against what
    /// the matcher will see. The tagged shapes' one or two mebibytes are
    /// worth it from a sixteenth of that in input. The deep shapes' eight
    /// to thirty-two mebibytes are cleared for a first stream only when the
    /// input is at least an eighth of the table: a short compressible
    /// stream stores few positions, and clearing sixteen mebibytes for it
    /// costs more than compressing it — a quarter-mebibyte of zeros at
    /// quality seven measured half the reference's speed with the table and
    /// twice it without — while the on-demand layouts touch only what it
    /// stores. A matcher on its second stream has shown it is reused, so
    /// the clear is paid once for every stream that follows; from then on a
    /// sixty-fourth of the table is enough, because the on-demand index's
    /// dependent load per bucket costs more than the whole search on a
    /// quarter-mebibyte incompressible input at quality seven. A matcher
    /// that already holds the table reuses it for any input the compact map
    /// does not serve.
    const fn dense_limit(&self) -> usize {
        let table_bytes = BUCKETS * BLOCK * size_of::<u32>();
        if Self::TAGGED {
            table_bytes / 16
        } else if self.streams == 0 {
            table_bytes / 8
        } else {
            table_bytes / 64
        }
    }

    /// Re-aims the matcher at a stream of `size_hint` bytes.
    ///
    /// The shape is fixed by the type; only the layout choice the next
    /// preparation makes depends on the hint, so a reused encoder need not
    /// be rebuilt when its input length changes.
    pub(crate) const fn retarget(&mut self, size_hint: usize) {
        self.size_hint = size_hint;
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
            (read_u32(data, offset).wrapping_mul(HASH_MUL32) >> (32 - Self::BUCKET_BITS - 8))
                as usize
        }
    }

    /// Decodes a sparse entry into its counter and encoded block offset.
    /// An entry from an earlier generation still names its block but
    /// counts as empty.
    #[inline(always)]
    const fn decode_entry(&self, entry: u64) -> (u16, u32) {
        let offset = ((entry >> COUNT_BITS) & OFFSET_MASK) as u32;
        let count = if entry >> GENERATION_SHIFT == self.generation {
            entry as u16
        } else {
            0
        };
        (count, offset)
    }

    /// Encodes a counter and block offset as a sparse entry of the current
    /// generation.
    #[inline(always)]
    const fn encode_entry(&self, count: u16, offset: u32) -> u64 {
        (self.generation << GENERATION_SHIFT) | ((offset as u64) << COUNT_BITS) | count as u64
    }

    /// Returns the counter and encoded block offset of `key` in an on-demand
    /// layout: the compact map when `COMPACT`, the sparse table otherwise.
    #[inline(always)]
    fn entry<const COMPACT: bool>(&self, key: usize) -> (u16, u32) {
        if COMPACT {
            return self.compact.get(key);
        }
        match &self.entries {
            Some(entries) => self.decode_entry(entries[key & (BUCKETS - 1)]),
            None => (0, 0),
        }
    }

    /// Decodes an on-demand offset into the block it names.
    #[inline(always)]
    const fn block(offset: u32) -> Option<BlockRef> {
        if offset == 0 {
            return None;
        }
        let index = ((offset & !STARTER_FLAG) - 1) as usize;
        if offset & STARTER_FLAG != 0 {
            Some(BlockRef::Starter(index))
        } else {
            Some(BlockRef::Full(index))
        }
    }

    /// Returns the on-demand block a store into a bucket writes, activating
    /// or growing it when `count` stores have already filled what it has.
    ///
    /// The encoded offset comes back with the block so the caller can
    /// record it; it is unchanged whenever the block had room.
    #[inline(always)]
    fn block_for_store(&mut self, count: u16, offset: u32) -> (BlockRef, u32) {
        match Self::block(offset) {
            None => {
                let index = self.starters.len();
                self.starters.push([0; STARTER_SLOTS]);
                if Self::TAGGED {
                    self.starter_tags.push([0; STARTER_SLOTS]);
                }
                (BlockRef::Starter(index), (index as u32 + 1) | STARTER_FLAG)
            }
            Some(BlockRef::Starter(index)) if usize::from(count) < STARTER_SLOTS => {
                (BlockRef::Starter(index), offset)
            }
            Some(BlockRef::Starter(index)) => {
                // Both fill downwards from the top, so the starter's slots
                // are the top slots of a full block.
                let mut block = [0u32; BLOCK];
                if let Some(starter) = self.starters.get(index)
                    && let Some(top) = block.last_chunk_mut::<STARTER_SLOTS>()
                {
                    *top = *starter;
                }
                let full = self.blocks.len();
                self.blocks.push(block);
                if Self::TAGGED {
                    let mut tags = [0u8; BLOCK];
                    if let Some(starter) = self.starter_tags.get(index)
                        && let Some(top) = tags.last_chunk_mut::<STARTER_SLOTS>()
                    {
                        *top = *starter;
                    }
                    self.block_tags.push(tags);
                }
                (BlockRef::Full(full), full as u32 + 1)
            }
            Some(BlockRef::Full(index)) => (BlockRef::Full(index), offset),
        }
    }

    /// Stores `ix` with `tag` into `key` of an on-demand layout: the compact
    /// map when `COMPACT`, the sparse table otherwise.
    #[inline(always)]
    fn push<const COMPACT: bool>(&mut self, key: usize, ix: u32, tag: u8) {
        if COMPACT {
            // One probe serves the read and the write: the slot is found
            // after any growth, so it stays valid across the chain update.
            let (slot, count, head) = self.compact.slot_for_write(key);
            self.push_chain(key, slot, count, head, ix);
            return;
        }
        let key = key & (BUCKETS - 1);
        let Some(entries) = &self.entries else {
            return;
        };
        let (count, offset) = self.decode_entry(entries[key]);
        self.push_found(key, count, offset, ix, tag);
    }

    /// Stores `ix` with `tag` into `key` of the sparse table, whose entry
    /// was already read as `count` and `offset`.
    ///
    /// A search reads the entry before it scans the bucket and stores the
    /// searched position afterwards; this lets it do both with one lookup.
    #[inline(always)]
    fn push_found(&mut self, key: usize, count: u16, offset: u32, ix: u32, tag: u8) {
        let offset = self.push_block(count, offset, ix, tag);
        let entry = self.encode_entry(count.wrapping_add(1), offset);
        if let Some(entries) = &mut self.entries {
            entries[key & (BUCKETS - 1)] = entry;
        }
    }

    /// Links `ix` in front of the chain of `key`, whose map entry — at
    /// `slot`, already read as `count` stores and chain head `head` — is
    /// then updated to name the new node.
    #[inline(always)]
    fn push_chain(&mut self, key: usize, slot: usize, count: u16, head: u32, ix: u32) {
        self.chain.push(u64::from(ix) | (u64::from(head) << 32));
        let node = self.chain.len() as u32;
        self.compact
            .write_slot(slot, key, count.wrapping_add(1), node);
    }

    /// Writes `ix` with `tag` into the block a bucket `count` stores deep
    /// keeps at `offset`, activating or growing it first; returns the
    /// encoded offset to record.
    #[inline(always)]
    fn push_block(&mut self, count: u16, offset: u32, ix: u32, tag: u8) -> u32 {
        let (block, offset) = self.block_for_store(count, offset);
        match block {
            BlockRef::Starter(index) => {
                let slot = !usize::from(count) & (STARTER_SLOTS - 1);
                if let Some(block) = self.starters.get_mut(index) {
                    block[slot] = ix;
                }
                if Self::TAGGED
                    && let Some(tags) = self.starter_tags.get_mut(index)
                {
                    tags[slot] = tag;
                }
            }
            BlockRef::Full(index) => {
                let slot = !usize::from(count) & (BLOCK - 1);
                if let Some(block) = self.blocks.get_mut(index) {
                    block[slot] = ix;
                }
                if Self::TAGGED
                    && let Some(tags) = self.block_tags.get_mut(index)
                {
                    tags[slot] = tag;
                }
            }
        }
        offset
    }

    /// Selects the layout for a stream and readies its index.
    fn select_layout(&mut self, one_shot: bool, input_size: usize) {
        if one_shot && input_size <= COMPACT_INPUT_LIMIT && self.entries.is_none() {
            // The map spares a cold matcher the sparse table's allocation; a
            // matcher that already has the table empties it for free. The
            // stream stores at most one node per position, so the chain is
            // reserved once and never grows while it runs.
            self.layout = Layout::Compact;
            self.compact.reset(input_size);
            self.clear_blocks();
            self.chain.clear();
            self.chain.reserve(input_size);
        } else if self.expected_input() < self.dense_limit() && self.dense.is_empty() {
            // A multi-block one-shot stream is not "one shot" at its first
            // block, so this rests on the size hint: known short inputs stay
            // on demand, unless the table already exists and costs only its
            // counters to reuse.
            match &mut self.entries {
                None => {
                    self.entries = vec![0u64; BUCKETS].into_boxed_slice().try_into().ok();
                    self.generation = 1;
                    self.clear_blocks();
                }
                Some(entries)
                    if self.layout != Layout::Sparse || self.generation == MAX_GENERATION =>
                {
                    // The blocks the entries name belonged to another
                    // layout's stream, or the stamps have run out; start
                    // over.
                    entries.fill(0);
                    self.generation = 1;
                    self.clear_blocks();
                }
                Some(_) => self.generation += 1,
            }
            self.layout = Layout::Sparse;
        } else {
            if self.dense.is_empty() {
                self.num = vec![0; BUCKETS];
                self.dense = vec![0; BUCKETS * BLOCK];
                if Self::TAGGED {
                    self.dense_tags = vec![0; BUCKETS * BLOCK];
                }
            } else {
                // Slots are only ever read below the counter that guards
                // them, so the blocks themselves stay as they are.
                self.num.fill(0);
            }
            self.layout = Layout::Dense;
        }
        self.streams = self.streams.saturating_add(1);
    }

    /// Drops every activated on-demand block, keeping the allocations.
    fn clear_blocks(&mut self) {
        self.blocks.clear();
        self.block_tags.clear();
        self.starters.clear();
        self.starter_tags.clear();
    }

    /// Binds the dense tables as arrays for one block of searches.
    ///
    /// `None` outside the dense layout, whose preparation sizes every table
    /// to exactly one entry per bucket.
    fn dense_run(&mut self) -> Option<DenseRun<'_, HASH64, BUCKETS, BLOCK>> {
        if self.layout != Layout::Dense {
            return None;
        }
        let num = self.num.first_chunk_mut::<BUCKETS>()?;
        let dense = self
            .dense
            .as_chunks_mut::<BLOCK>()
            .0
            .first_chunk_mut::<BUCKETS>()?;
        let tags = if Self::TAGGED {
            Some(
                self.dense_tags
                    .as_chunks_mut::<BLOCK>()
                    .0
                    .first_chunk_mut::<BUCKETS>()?,
            )
        } else {
            None
        };
        Some(DenseRun { num, dense, tags })
    }

    /// Searches through an on-demand layout; see [`MatchRun::find_longest_match`].
    #[inline(always)]
    fn search_on_demand<S: Simd, const COMPACT: bool>(
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
        if COMPACT {
            // The map entry is read once; the store after the scan reuses it.
            let (slot, count, head) = self.compact.slot_for_write(key);
            search_chain::<S, HASH64, BLOCK>(simd, &query, out, count, head, &self.chain);
            self.push_chain(key, slot, count, head, query.cur_ix as u32);
            if min_score == out.score {
                query.search_dictionary(stats, out, false);
            }
            return;
        }
        let (count, offset) = self.entry::<COMPACT>(key);
        match Self::block(offset) {
            Some(BlockRef::Full(index)) => {
                let bucket = self.blocks.get(index).unwrap_or(&[0; BLOCK]);
                let tags = if Self::TAGGED {
                    self.block_tags.get(index)
                } else {
                    None
                };
                search_bucket::<S, HASH64, BLOCK, BLOCK>(
                    simd, &query, out, tag, count, bucket, tags,
                );
            }
            Some(BlockRef::Starter(index)) => {
                let bucket = self.starters.get(index).unwrap_or(&[0; STARTER_SLOTS]);
                search_bucket::<S, HASH64, STARTER_SLOTS, BLOCK>(
                    simd, &query, out, tag, count, bucket, None,
                );
            }
            None => {
                search_bucket::<S, HASH64, STARTER_SLOTS, BLOCK>(
                    simd,
                    &query,
                    out,
                    tag,
                    0,
                    &[0; STARTER_SLOTS],
                    None,
                );
            }
        }
        self.push_found(key, count, offset, query.cur_ix as u32, tag);
        if min_score == out.score {
            query.search_dictionary(stats, out, false);
        }
    }
}

/// Stores `ix` with `tag` into bucket `key` of a dense table.
///
/// The tables are arrays, so with the key masked to the bucket count every
/// index is in range by construction and no check survives.
#[inline(always)]
fn dense_store<const BUCKETS: usize, const BLOCK: usize>(
    num: &mut [u16; BUCKETS],
    dense: &mut [[u32; BLOCK]; BUCKETS],
    tags: Option<&mut [[u8; BLOCK]; BUCKETS]>,
    key: usize,
    ix: u32,
    tag: u8,
) {
    let key = key & (BUCKETS - 1);
    let count = &mut num[key];
    let current = *count;
    *count = current.wrapping_add(1);
    let slot = !usize::from(current) & (BLOCK - 1);
    dense[key][slot] = ix;
    if let Some(tags) = tags {
        tags[key][slot] = tag;
    }
}

/// Rotates a raw tag-equality mask so that bit `age` names the slot `age`
/// stores older than the newest one, dropping slots no store has filled.
///
/// `bits` is the block capacity, `available` how many of its slots hold
/// positions. Slots fill downwards, so the newest position sits at `newest`
/// and older ones follow it upwards, wrapping to the bottom: a rotation
/// right by `newest` within `bits` lanes brings the newest slot to bit zero
/// and each older one to the bit after it, so ascending bits of the result
/// walk the block newest to oldest.
#[inline(always)]
fn rotate_candidates(equal: u32, bits: u32, newest: u32, available: u32) -> u32 {
    let lanes = u32::MAX.checked_shr(32 - bits).unwrap_or(0);
    let filled = u32::MAX.checked_shr(32 - available).unwrap_or(0);
    let masked = equal & lanes;
    let rotated = (masked >> newest) | (masked.checked_shl(bits - newest).unwrap_or(0) & lanes);
    rotated & filled
}

/// One bit per slot of `tags` whose byte equals `tag`.
///
/// The scalar backend and any block too short for a vector compare report
/// every slot, which keeps the unfiltered scan as an independent oracle for
/// the mask. The block width is a constant, so only one arm survives.
#[inline(always)]
fn tag_equality<S: Simd, const N: usize>(simd: S, tags: &[u8; N], tag: u8) -> u32 {
    if simd.level().is_fallback() {
        return u32::MAX;
    }
    if N == 32
        && let Some(bytes) = tags.first_chunk::<32>()
    {
        return u8x32::load_array_ref(simd, bytes)
            .simd_eq(u8x32::splat(simd, tag))
            .to_bitmask() as u32;
    }
    if N == 16
        && let Some(bytes) = tags.first_chunk::<16>()
    {
        return u32::from(
            u8x16::load_array_ref(simd, bytes)
                .simd_eq(u8x16::splat(simd, tag))
                .to_bitmask() as u16,
        );
    }
    u32::MAX
}

/// The running best of one bucket scan: what every candidate is checked
/// against before it is measured.
///
/// Two words, passed and returned by value, so the candidate loop keeps
/// them in registers.
#[derive(Copy, Clone)]
struct Best {
    /// Kept narrow: an index built from it and a ring position cannot
    /// overflow, which is what lets the candidate reads go unchecked.
    len: u32,
    /// The four bytes ending one past `len`, which a candidate has to
    /// reproduce before it is worth measuring.
    word: u32,
}

/// The match one search has found so far, held in locals until the search
/// ends so the result it is written to never lives in memory mid-search.
#[derive(Copy, Clone)]
struct Found {
    /// Length of the match; zero while nothing has been found.
    len: usize,
    distance: usize,
    /// Score to beat; the incoming result's score while nothing has been
    /// found.
    score: usize,
}

/// Measures the cached-distance candidate at `prev_ix`; returns its length
/// and score when it beats `best_score`, and `None` otherwise.
///
/// Out of line on purpose, like [`accept_candidate`]: the probe loop that
/// calls it keeps only its own filter in registers. It returns by value
/// rather than writing the result, so the result never has to live in
/// memory during the search.
#[inline(never)]
fn accept_cached<S: Simd>(
    simd: S,
    data: &[u8],
    cur_ix_masked: usize,
    max_length: usize,
    prev_ix: usize,
    index: usize,
    best_score: usize,
) -> Option<(usize, usize)> {
    let len = match_len_at(
        simd,
        data,
        prev_ix,
        current_window(data, cur_ix_masked, max_length),
    );
    // Two-byte matches are only worth scoring for the two freshest cached
    // distances; anything shorter never wins. Written as one comparison:
    // `len >= 3 || (len == 2 && index < 2)`.
    if len + usize::from(index < 2) >= 3 {
        let mut score = backward_reference_score_using_last_distance(len);
        if best_score < score {
            if index != 0 {
                score -= backward_reference_penalty_using_last_distance(index);
            }
            if best_score < score {
                return Some((len, score));
            }
        }
    }
    None
}

/// Measures the bucket candidate at `prev_ix`, which has already reproduced
/// the four bytes of [`Best::word`]; returns the new running best and its
/// score when it beats `best_score`, and `None` otherwise.
///
/// Out of line on purpose. The candidate loop is a filter — a distance
/// check and a four-byte compare — that rejects most of what it sees, and
/// it runs in registers only while it holds a dozen values at most. The
/// measurement brings the scan loop and the current window with it;
/// inlined, those compete for the loop's registers and the compiler
/// reloads the loop's own invariants from the stack at every candidate. It
/// returns by value rather than writing the result, so the result never
/// has to live in memory during the search.
#[inline(never)]
fn accept_candidate<S: Simd, const HASH64: bool>(
    simd: S,
    data: &[u8],
    cur_ix_masked: usize,
    max_length: usize,
    prev_ix: usize,
    backward: usize,
    best_score: usize,
) -> Option<(Best, usize)> {
    let cur = current_window(data, cur_ix_masked, max_length);
    let left = data.get(prev_ix..prev_ix + cur.len())?;
    let len = if HASH64 {
        if left.first_chunk::<4>() != cur.first_chunk::<4>() {
            return None;
        }
        match (left.get(4..), cur.get(4..)) {
            (Some(left), Some(cur)) => match_len_windows(simd, left, cur) + 4,
            _ => return None,
        }
    } else {
        let len = match_len_windows(simd, left, cur);
        if len < 4 {
            return None;
        }
        len
    };
    let score = backward_reference_score(len, backward);
    if best_score < score {
        let best = Best {
            len: len as u32,
            word: read_u32(data, cur_ix_masked + len - 3),
        };
        return Some((best, score));
    }
    None
}

/// What one search holds fixed while it walks a bucket.
///
/// Passed by value into every candidate check: a handful of words the
/// compiler keeps in registers once the check is inlined, where a reference
/// to a larger query would make it reload each field it needs.
#[derive(Copy, Clone)]
struct BucketScan<'a, S> {
    /// The token the measurement scans with; zero-sized.
    simd: S,
    /// The ring buffer, tail copy and margin included.
    data: &'a [u8],
    /// `data` cut to the window; see [`MatchQuery::window`].
    window: &'a [u8],
    cur_ix: usize,
    mask: usize,
    max_backward: usize,
    /// Longest match the remaining input allows.
    max_length: usize,
}

/// Judges one bucket candidate at `prev_ix`; `false` ends the scan.
///
/// The window is the ring buffer cut to the mask, so the guard against its
/// end is the reference's mask guard and a bounds proof at once.
#[inline(always)]
fn consider<S: Simd, const HASH64: bool>(
    scan: BucketScan<'_, S>,
    prev_ix: u32,
    best: &mut Best,
    found: &mut Found,
) -> bool {
    let backward = scan.cur_ix.wrapping_sub(prev_ix as usize);
    if backward > scan.max_backward {
        return false;
    }
    let prev_ix = prev_ix as usize & scan.mask;
    // The four bytes ending one past `best.len`. Both operands are below
    // 2^32 — the offset is a wrapped `u32` on purpose — so the end cannot
    // overflow and the guard against the window's end bounds the read.
    let start = prev_ix + best.len.wrapping_sub(3) as usize;
    let Some(word) = scan.window.get(start..start + 4) else {
        return true;
    };
    if best.word != u32::from_le_bytes(word.try_into().unwrap_or([0; 4])) {
        return true;
    }
    if let Some((accepted, score)) = accept_candidate::<S, HASH64>(
        scan.simd,
        scan.data,
        scan.cur_ix & scan.mask,
        scan.max_length,
        prev_ix,
        backward,
        found.score,
    ) {
        *best = accepted;
        *found = Found {
            len: accepted.len as usize,
            distance: backward,
            score,
        };
    }
    true
}

/// Runs the cached-distance probes and the bucket scan for one search.
///
/// `bucket` is the bucket's `N` slots, `count` stores deep; `tags` is its
/// tags, or `None` for an untagged shape; `BLOCK` is the shape's full depth,
/// which fixes how many cached distances are probed first. Shared by every
/// layout so the reference's decision order lives in one place.
#[inline(always)]
fn search_bucket<S: Simd, const HASH64: bool, const N: usize, const BLOCK: usize>(
    simd: S,
    query: &MatchQuery<'_>,
    out: &mut SearchResult,
    tag: u8,
    count: u16,
    bucket: &[u32; N],
    tags: Option<&[u8; N]>,
) {
    let data = query.data;
    let window = query.window;
    let mask = query.mask;
    let cur_ix = query.cur_ix;
    let max_backward = query.max_backward;
    let max_length = query.max_length;
    let available = usize::from(count).min(N);
    // Stores fill downwards from the top of a block, so the `age`-th
    // newest position sits `age` slots above the newest one, wrapping.
    let newest = !usize::from(count.wrapping_sub(1)) & (N - 1);
    // Fetch the bucket's tags before the cache probes so the miss overlaps
    // them, as the reference's prefetch does.
    let equal = match tags {
        Some(tags) if available != 0 => tag_equality::<S, N>(simd, tags, tag),
        _ => u32::MAX,
    };

    let (best_len, mut found) = probe_last_distances::<S, BLOCK>(simd, query, out);

    if available != 0 {
        scan_bucket::<S, HASH64, N>(
            BucketScan {
                simd,
                data,
                window,
                cur_ix,
                mask,
                max_backward,
                max_length,
            },
            bucket,
            equal,
            newest,
            available,
            best_len,
            &mut found,
        );
    }
    write_back(out, found);
}

/// Runs the cached-distance probes and a compact chain walk for one search.
///
/// `head` names the newest node of the bucket's chain and `count` how many
/// stores the bucket has seen; the walk visits the newest `BLOCK` of them in
/// the order a block scan would, judging each with the same filter.
#[inline(always)]
fn search_chain<S: Simd, const HASH64: bool, const BLOCK: usize>(
    simd: S,
    query: &MatchQuery<'_>,
    out: &mut SearchResult,
    count: u16,
    head: u32,
    chain: &[u64],
) {
    let (best_len, mut found) = probe_last_distances::<S, BLOCK>(simd, query, out);
    let mut remaining = usize::from(count).min(BLOCK);
    let mut link = head as usize;
    if remaining != 0 && link != 0 {
        let data = query.data;
        let scan = BucketScan {
            simd,
            data,
            window: query.window,
            cur_ix: query.cur_ix,
            mask: query.mask,
            max_backward: query.max_backward,
            max_length: query.max_length,
        };
        let cur_ix_masked = query.cur_ix & query.mask;
        let mut best = Best {
            len: best_len as u32,
            word: read_u32(data, cur_ix_masked + best_len - 3),
        };
        while remaining != 0
            && let Some(&node) = chain.get(link.wrapping_sub(1))
        {
            if !consider::<S, HASH64>(scan, node as u32, &mut best, &mut found) {
                break;
            }
            link = (node >> 32) as usize;
            remaining -= 1;
        }
    }
    write_back(out, found);
}

/// Records what a search found; an empty result leaves the score alone.
#[inline(always)]
fn write_back(out: &mut SearchResult, found: Found) {
    out.len = found.len;
    out.len_code_delta = 0;
    if found.len != 0 {
        out.distance = found.distance;
        out.score = found.score;
    }
}

/// Tries the cached distances before any bucket is consulted.
///
/// Returns the length the bucket scan's compare offset starts from —
/// raised to three so the scan can compare four bytes unconditionally —
/// and the running result. The incoming length only seeds the probes'
/// compare offset; the result starts empty.
#[inline(always)]
fn probe_last_distances<S: Simd, const BLOCK: usize>(
    simd: S,
    query: &MatchQuery<'_>,
    out: &SearchResult,
) -> (usize, Found) {
    let data = query.data;
    let window = query.window;
    let mask = query.mask;
    let cur_ix = query.cur_ix;
    let cur_ix_masked = cur_ix & mask;
    let max_backward = query.max_backward;
    let max_length = query.max_length;
    let mut best_len = out.len;
    let mut found = Found {
        len: 0,
        distance: 0,
        score: out.score,
    };

    // The probe count is a constant, so this loop unrolls; the cache holds
    // sixteen entries and no shape probes more.
    for index in 0..last_distances_for(BLOCK).min(NUM_DISTANCE_SHORT_CODES) {
        let backward = query.cache[index] as usize;
        let prev_ix = cur_ix.wrapping_sub(backward);
        if prev_ix >= cur_ix || backward > max_backward {
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
        if let Some((len, score)) = accept_cached(
            simd,
            data,
            cur_ix_masked,
            max_length,
            prev_ix,
            index,
            found.score,
        ) {
            best_len = len;
            found = Found {
                len,
                distance: backward,
                score,
            };
        }
    }
    // Raising the floor to three lets the bucket loop compare four bytes
    // unconditionally.
    if best_len < 3 {
        best_len = 3;
    }
    (best_len, found)
}

/// Walks a bucket's candidates newest to oldest, updating `found`.
///
/// `equal` is the tag mask, `newest` the slot of the newest position, and
/// `available` how many slots hold positions.
#[inline(always)]
fn scan_bucket<S: Simd, const HASH64: bool, const N: usize>(
    scan: BucketScan<'_, S>,
    bucket: &[u32; N],
    equal: u32,
    newest: usize,
    available: usize,
    best_len: usize,
    found: &mut Found,
) {
    let data = scan.data;
    let cur_ix_masked = scan.cur_ix & scan.mask;
    let mut best = Best {
        len: best_len as u32,
        word: read_u32(data, cur_ix_masked + best_len - 3),
    };
    if N <= 32 {
        // Rotating the mask so bit zero is the newest slot turns the walk
        // into one loop over ascending bits, each `age` bits above the
        // newest, wrapping; the reference rotates its mask the same way.
        let mut ages = rotate_candidates(equal, N as u32, newest as u32, available as u32);
        while ages != 0 {
            let age = ages.trailing_zeros() as usize;
            ages &= ages - 1;
            let slot = (newest + age) & (N - 1);
            let Some(&prev_ix) = bucket.get(slot) else {
                break;
            };
            if !consider::<S, HASH64>(scan, prev_ix, &mut best, found) {
                break;
            }
        }
    } else {
        let mut slot = newest;
        let mut remaining = available;
        while remaining != 0 {
            let prev_ix = bucket[slot & (N - 1)];
            if !consider::<S, HASH64>(scan, prev_ix, &mut best, found) {
                break;
            }
            slot = (slot + 1) & (N - 1);
            remaining -= 1;
        }
    }
}

/// The dense tables of a [`BucketMatcher`], bound as arrays for a block.
///
/// Arrays rather than slices: their sizes are the matcher's constants, so
/// the run carries three pointers and no bounds, and every index the key
/// or a slot mask produces is in range by construction.
pub(crate) struct DenseRun<'a, const HASH64: bool, const BUCKETS: usize, const BLOCK: usize> {
    num: &'a mut [u16; BUCKETS],
    dense: &'a mut [[u32; BLOCK]; BUCKETS],
    tags: Option<&'a mut [[u8; BLOCK]; BUCKETS]>,
}

/// A [`BucketMatcher`] in an on-demand layout, borrowed for one block; its
/// blocks may still grow, so the view is the matcher itself. `COMPACT`
/// selects the compact map over the sparse table at compile time.
pub(crate) struct OnDemandRun<
    'a,
    const HASH64: bool,
    const BUCKETS: usize,
    const BLOCK: usize,
    const COMPACT: bool,
>(&'a mut BucketMatcher<HASH64, BUCKETS, BLOCK>);

impl<const HASH64: bool, const BUCKETS: usize, const BLOCK: usize, const COMPACT: bool> MatchRun
    for OnDemandRun<'_, HASH64, BUCKETS, BLOCK, COMPACT>
{
    const HASH_TYPE_LENGTH: usize = if HASH64 { 8 } else { 4 };
    const STORE_LOOKAHEAD: usize = Self::HASH_TYPE_LENGTH;

    #[inline(always)]
    fn store(&mut self, data: &[u8], mask: usize, ix: usize) {
        let hash = BucketMatcher::<HASH64, BUCKETS, BLOCK>::hash_with_tag(data, ix & mask);
        self.0.push::<COMPACT>(hash >> 8, ix as u32, hash as u8);
    }

    fn store_range(&mut self, data: &[u8], mask: usize, start: usize, end: usize) {
        for ix in start..end {
            self.store(data, mask, ix);
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
        self.0
            .search_on_demand::<S, COMPACT>(simd, stats, query, out);
    }
}

impl<const HASH64: bool, const BUCKETS: usize, const BLOCK: usize> MatchRun
    for DenseRun<'_, HASH64, BUCKETS, BLOCK>
{
    const HASH_TYPE_LENGTH: usize = if HASH64 { 8 } else { 4 };
    const STORE_LOOKAHEAD: usize = Self::HASH_TYPE_LENGTH;

    #[inline(always)]
    fn store(&mut self, data: &[u8], mask: usize, ix: usize) {
        let hash = BucketMatcher::<HASH64, BUCKETS, BLOCK>::hash_with_tag(data, ix & mask);
        dense_store(
            self.num,
            self.dense,
            self.tags.as_deref_mut(),
            hash >> 8,
            ix as u32,
            hash as u8,
        );
    }

    fn store_range(&mut self, data: &[u8], mask: usize, start: usize, end: usize) {
        for ix in start..end {
            self.store(data, mask, ix);
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
        let cur_ix_masked = query.cur_ix & query.mask;
        let min_score = out.score;
        let hash =
            BucketMatcher::<HASH64, BUCKETS, BLOCK>::hash_with_tag(query.data, cur_ix_masked);
        let key = (hash >> 8) & (BUCKETS - 1);
        let tag = hash as u8;
        let count = self.num[key];
        let bucket = &self.dense[key];
        let tags = self.tags.as_deref().map(|tags| &tags[key]);
        search_bucket::<S, HASH64, BLOCK, BLOCK>(simd, &query, out, tag, count, bucket, tags);
        dense_store(
            self.num,
            self.dense,
            self.tags.as_deref_mut(),
            key,
            query.cur_ix as u32,
            tag,
        );
        if min_score == out.score {
            query.search_dictionary(stats, out, false);
        }
    }
}

impl<const HASH64: bool, const BUCKETS: usize, const BLOCK: usize> Matcher
    for BucketMatcher<HASH64, BUCKETS, BLOCK>
{
    const HASH_TYPE_LENGTH: usize = if HASH64 { 8 } else { 4 };
    const STORE_LOOKAHEAD: usize = Self::HASH_TYPE_LENGTH;

    fn visit_run<V: RunVisitor>(&mut self, visitor: V) -> V::Output {
        // The dense tables are bound while the borrow of `self` lasts; the
        // early return keeps the two borrows apart.
        if self.layout == Layout::Dense
            && let Some(run) = self.dense_run()
        {
            return visitor.visit(run);
        }
        if self.layout == Layout::Compact {
            visitor.visit(OnDemandRun::<HASH64, BUCKETS, BLOCK, true>(self))
        } else {
            visitor.visit(OnDemandRun::<HASH64, BUCKETS, BLOCK, false>(self))
        }
    }

    fn last_distances_to_check(&self) -> usize {
        Self::LAST_DISTANCES
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
        self.visit_run(Store {
            data,
            mask,
            start: ix,
            end: ix + 1,
        });
    }

    fn store_range(&mut self, data: &[u8], mask: usize, start: usize, end: usize) {
        self.visit_run(Store {
            data,
            mask,
            start,
            end,
        });
    }

    fn find_longest_match<S: Simd>(
        &mut self,
        simd: S,
        stats: &mut DictionaryStats,
        query: MatchQuery<'_>,
        out: &mut SearchResult,
    ) {
        self.visit_run(Search {
            simd,
            stats,
            query,
            out,
        });
    }
}

/// A [`RunVisitor`] that stores a range of positions.
struct Store<'a> {
    data: &'a [u8],
    mask: usize,
    start: usize,
    end: usize,
}

impl RunVisitor for Store<'_> {
    type Output = ();

    fn visit<R: MatchRun>(self, mut run: R) {
        run.store_range(self.data, self.mask, self.start, self.end);
    }
}

/// A [`RunVisitor`] that runs one search.
struct Search<'a, S> {
    simd: S,
    stats: &'a mut DictionaryStats,
    query: MatchQuery<'a>,
    out: &'a mut SearchResult,
}

impl<S: Simd> RunVisitor for Search<'_, S> {
    type Output = ();

    fn visit<R: MatchRun>(self, mut run: R) {
        run.find_longest_match(self.simd, self.stats, self.query, self.out);
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

    fn visit_run<V: RunVisitor>(&mut self, visitor: V) -> V::Output {
        visitor.visit(self)
    }

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
        let cur = || current_window(data, cur_ix_masked, query.max_length);
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
            let len = match_len_at(simd, data, prev_ix, cur());
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
            let len = match_len_at(simd, data, prev_ix, cur());
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
    H2Small(QuickMatcher<{ 1 << 16 }, 0, 5, true, true>),
    /// Short-input storage with the H3 hash and candidate order.
    H3Small(QuickMatcher<{ 1 << 16 }, 1, 5, false, true>),
    /// Short-input storage with the H4 hash and candidate order.
    H4Small(QuickMatcher<{ 1 << 17 }, 2, 5, true, true>),

    /// Quality 2: one candidate slot per bucket, with a dictionary probe.
    H2(QuickMatcher<{ 1 << 16 }, 0, 5, true>),
    /// Quality 3.
    H3(QuickMatcher<{ 1 << 16 }, 1, 5, false>),
    /// Quality 4, small inputs.
    H4(QuickMatcher<{ 1 << 17 }, 2, 5, true>),
    /// Quality 4, large inputs.
    H54(QuickMatcher<{ 1 << 20 }, 2, 7, false>),
    /// Qualities 5 to 8, small windows: `H40` and `H41`.
    H40(ChainMatcher<1, 16>),
    /// Quality 9, small windows: `H42`.
    H42(ChainMatcher<512, 9>),
    /// Quality 5, ordinary inputs: fourteen bucket bits, sixteen slots.
    H5Q5(BucketMatcher<false, { 1 << 14 }, 16>),
    /// Quality 6, ordinary inputs: fourteen bucket bits, thirty-two slots.
    H5Q6(BucketMatcher<false, { 1 << 14 }, 32>),
    /// Quality 7, ordinary inputs: fifteen bucket bits, sixty-four slots.
    H5Q7(BucketMatcher<false, { 1 << 15 }, 64>),
    /// Quality 8, ordinary inputs: fifteen bucket bits, 128 slots.
    H5Q8(BucketMatcher<false, { 1 << 15 }, 128>),
    /// Quality 9, ordinary inputs: fifteen bucket bits, 256 slots.
    H5Q9(BucketMatcher<false, { 1 << 15 }, 256>),
    /// Quality 5, large inputs and wide windows.
    H6Q5(BucketMatcher<true, { 1 << 15 }, 16>),
    /// Quality 6, large inputs and wide windows.
    H6Q6(BucketMatcher<true, { 1 << 15 }, 32>),
    /// Quality 7, large inputs and wide windows.
    H6Q7(BucketMatcher<true, { 1 << 15 }, 64>),
    /// Quality 8, large inputs and wide windows.
    H6Q8(BucketMatcher<true, { 1 << 15 }, 128>),
    /// Quality 9, large inputs and wide windows.
    H6Q9(BucketMatcher<true, { 1 << 15 }, 256>),
}

impl MatchFinder {
    /// Allocates the bucket matcher `shape` calls for, `HASH64` selecting
    /// `H6` over `H5`.
    ///
    /// The block depth is the quality less one, from four to eight, and it
    /// decides the bucket count with it, so the shape's depth picks the
    /// variant on its own.
    fn bucket(hash64: bool, shape: BucketShape, size_hint: usize) -> Self {
        match (hash64, shape.block_bits) {
            (false, ..=4) => Self::H5Q5(BucketMatcher::new(size_hint)),
            (false, 5) => Self::H5Q6(BucketMatcher::new(size_hint)),
            (false, 6) => Self::H5Q7(BucketMatcher::new(size_hint)),
            (false, 7) => Self::H5Q8(BucketMatcher::new(size_hint)),
            (false, 8..) => Self::H5Q9(BucketMatcher::new(size_hint)),
            (true, ..=4) => Self::H6Q5(BucketMatcher::new(size_hint)),
            (true, 5) => Self::H6Q6(BucketMatcher::new(size_hint)),
            (true, 6) => Self::H6Q7(BucketMatcher::new(size_hint)),
            (true, 7) => Self::H6Q8(BucketMatcher::new(size_hint)),
            (true, 8..) => Self::H6Q9(BucketMatcher::new(size_hint)),
        }
    }
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
            HasherPlan::H5(shape) => Self::bucket(false, shape, 0),
            HasherPlan::H6(shape) => Self::bucket(true, shape, 0),
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
            MatchFinder::H5Q5($matcher) => $body,
            MatchFinder::H5Q6($matcher) => $body,
            MatchFinder::H5Q7($matcher) => $body,
            MatchFinder::H5Q8($matcher) => $body,
            MatchFinder::H5Q9($matcher) => $body,
            MatchFinder::H6Q5($matcher) => $body,
            MatchFinder::H6Q6($matcher) => $body,
            MatchFinder::H6Q7($matcher) => $body,
            MatchFinder::H6Q8($matcher) => $body,
            MatchFinder::H6Q9($matcher) => $body,
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
            HasherPlan::H5(shape) => Self::bucket(false, shape, size_hint),
            HasherPlan::H6(shape) => Self::bucket(true, shape, size_hint),
            _ => Self::from(plan),
        }
    }

    /// Re-aims the finder at a stream of `size_hint` bytes for `plan`.
    ///
    /// Returns `false` when [`MatchFinder::for_input`] would pick another
    /// variant for that hint — the short-input quick storage against the
    /// full table — in which case the caller has to rebuild; otherwise the
    /// bucket matchers take the hint for their next layout choice and the
    /// rest, whose shape the hint never reached, are unchanged.
    pub(crate) fn retarget(&mut self, plan: HasherPlan, size_hint: usize) -> bool {
        let wants_small = size_hint > 0
            && size_hint <= 2048
            && matches!(plan, HasherPlan::H2 | HasherPlan::H3 | HasherPlan::H4);
        let is_small = matches!(self, Self::H2Small(_) | Self::H3Small(_) | Self::H4Small(_));
        if wants_small != is_small {
            return false;
        }
        match self {
            Self::H5Q5(matcher) => matcher.retarget(size_hint),
            Self::H5Q6(matcher) => matcher.retarget(size_hint),
            Self::H5Q7(matcher) => matcher.retarget(size_hint),
            Self::H5Q8(matcher) => matcher.retarget(size_hint),
            Self::H5Q9(matcher) => matcher.retarget(size_hint),
            Self::H6Q5(matcher) => matcher.retarget(size_hint),
            Self::H6Q6(matcher) => matcher.retarget(size_hint),
            Self::H6Q7(matcher) => matcher.retarget(size_hint),
            Self::H6Q8(matcher) => matcher.retarget(size_hint),
            Self::H6Q9(matcher) => matcher.retarget(size_hint),
            _ => {}
        }
        true
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
            slots.write(key * 32 + 3, SMALL_SLOTS_MAX_INPUT as u32 - 1);
            assert_eq!(slots.read(key * 32 + 3), SMALL_SLOTS_MAX_INPUT as u32 - 1);
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
    fn a_short_input_links_each_store_in_front_of_its_bucket_chain() {
        let mut matcher = BucketMatcher::<false, { 1 << 15 }, 256>::new(0);
        let data = [b'a'; 64];
        matcher.prepare(true, data.len(), &data, true);
        assert_eq!(matcher.layout, Layout::Compact);
        assert!(matcher.retained_bytes() < 16 * 1024);
        assert!(matcher.chain.is_empty());
        for position in 0..5 {
            matcher.store(&data, usize::MAX, position);
        }
        assert!(matcher.starters.is_empty());
        assert!(matcher.blocks.is_empty());
        // Every store is one node, linked to the previous store into the
        // same bucket: position 4 first, then 3, down to 0 with no link.
        assert_eq!(matcher.chain.len(), 5);
        let key = BucketMatcher::<false, { 1 << 15 }, 256>::hash_with_tag(&data, 0) >> 8;
        let (count, head) = matcher.entry::<true>(key);
        assert_eq!(count, 5);
        let mut link = head as usize;
        let mut walked = Vec::new();
        while link != 0 {
            let node = matcher.chain[link - 1];
            walked.push(node as u32);
            link = (node >> 32) as usize;
        }
        assert_eq!(walked, [4, 3, 2, 1, 0]);
    }

    #[test]
    fn a_short_input_scans_only_the_newest_block_of_a_long_chain() {
        // Every position of a run hashes into one bucket. The block keeps
        // sixteen positions, so the chain walk stops after sixteen too and
        // the compact matcher finds what the sparse one does.
        let data = [b'a'; 200];
        let level = fearless_simd::Level::new();
        let mut compact = BucketMatcher::<false, { 1 << 14 }, 16>::new(0);
        compact.prepare(true, data.len(), &data, true);
        assert_eq!(compact.layout, Layout::Compact);
        let mut sparse = BucketMatcher::<false, { 1 << 14 }, 16>::new(COMPACT_INPUT_LIMIT + 1);
        sparse.prepare(true, COMPACT_INPUT_LIMIT + 1, &data, true);
        assert_eq!(sparse.layout, Layout::Sparse);
        compact.store_range(&data, usize::MAX, 0, 100);
        sparse.store_range(&data, usize::MAX, 0, 100);
        let expected = search_with(level, &mut sparse, &data, 100);
        let actual = search_with(level, &mut compact, &data, 100);
        assert_eq!(actual, expected);
        assert!(actual.is_match());
        assert_eq!(BucketMatcher::<false, { 1 << 15 }, 256>::block(0), None);
    }

    #[test]
    fn every_layout_finds_the_same_match_and_forgets_it_on_prepare() {
        let data = repeated();
        for backend in crate::compressor::Backend::available() {
            let mut expected = None;
            for (one_shot, input_size, size_hint, layout) in [
                (true, data.len(), data.len(), Layout::Compact),
                (
                    true,
                    COMPACT_INPUT_LIMIT + 1,
                    COMPACT_INPUT_LIMIT + 1,
                    Layout::Sparse,
                ),
                (false, data.len(), 1 << 20, Layout::Dense),
                // A tagged shape takes an unknown length for a long stream.
                (false, data.len(), 0, Layout::Dense),
            ] {
                let mut matcher = BucketMatcher::<false, { 1 << 14 }, 16>::new(size_hint);
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
    fn a_deep_shape_takes_the_dense_table_once_it_is_reused() {
        // A quarter-mebibyte hint is below an eighth of the 8 MiB table, so
        // the first stream stays on demand; the second stream, a sixty-fourth
        // being enough for a reused matcher, gets the table.
        let data = repeated();
        let mut matcher = BucketMatcher::<false, { 1 << 15 }, 64>::new(1 << 18);
        matcher.prepare(false, data.len(), &data, true);
        assert_eq!(matcher.layout, Layout::Sparse);
        matcher.prepare(false, data.len(), &data, true);
        assert_eq!(matcher.layout, Layout::Dense);
        // A hint below a sixty-fourth stays on demand however often it is
        // reused; retargeting to a longer stream changes that.
        let mut short = BucketMatcher::<false, { 1 << 15 }, 64>::new(1 << 16);
        for _ in 0..3 {
            short.prepare(false, data.len(), &data, true);
            assert_eq!(short.layout, Layout::Sparse);
        }
        short.retarget(1 << 18);
        short.prepare(false, data.len(), &data, true);
        assert_eq!(short.layout, Layout::Dense);
    }

    #[test]
    fn retargeting_keeps_a_finder_only_when_its_variant_would_not_change() {
        let mut small = MatchFinder::for_input(HasherPlan::H2, 16);
        assert!(matches!(small, MatchFinder::H2Small(_)));
        assert!(small.retarget(HasherPlan::H2, 2048));
        assert!(!small.retarget(HasherPlan::H2, 2049));
        assert!(!small.retarget(HasherPlan::H2, 0));
        let mut full = MatchFinder::for_input(HasherPlan::H2, 0);
        assert!(matches!(full, MatchFinder::H2(_)));
        assert!(full.retarget(HasherPlan::H2, 1 << 20));
        assert!(!full.retarget(HasherPlan::H2, 100));
        let mut bucket = MatchFinder::for_input(HasherPlan::H5(Q5_BUCKET), 100);
        assert!(bucket.retarget(HasherPlan::H5(Q5_BUCKET), 1 << 20));
        let MatchFinder::H5Q5(inner) = &bucket else {
            panic!("shape changed");
        };
        assert_eq!(inner.size_hint, 1 << 20);
    }

    #[test]
    fn a_sparse_table_wipes_itself_when_its_generations_run_out() {
        let data = repeated();
        let mut matcher = BucketMatcher::<false, { 1 << 14 }, 16>::new(COMPACT_INPUT_LIMIT + 1);
        matcher.prepare(true, COMPACT_INPUT_LIMIT + 1, &data, true);
        matcher.store_range(&data, usize::MAX, 0, REPEAT_AT);
        matcher.generation = MAX_GENERATION;
        matcher.prepare(true, COMPACT_INPUT_LIMIT + 1, &data, true);
        assert_eq!(matcher.generation, 1);
        assert!(matcher.blocks.is_empty());
        assert!(matcher.starters.is_empty());
        assert!(!search_at(&mut matcher, &data, REPEAT_AT).is_match());
        // A bump is what an ordinary new stream costs.
        matcher.prepare(true, COMPACT_INPUT_LIMIT + 1, &data, true);
        assert_eq!(matcher.generation, 2);
    }

    #[test]
    fn a_dense_table_keeps_its_blocks_and_clears_only_its_counters() {
        let data = repeated();
        let mut matcher = BucketMatcher::<false, { 1 << 14 }, 16>::new(1 << 20);
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
        // three positions: slots 5, 6 and 7 become ages 0, 1 and 2.
        assert_eq!(rotate_candidates(u32::MAX, 8, 5, 3), 0b0000_0111);
        // Six positions: slots 5, 6, 7, then 0, 1, 2.
        assert_eq!(rotate_candidates(u32::MAX, 8, 5, 6), 0b0011_1111);
        // A full block visits everything; the equality mask filters it:
        // slots 3, 5, 7 and 1 are ages 1, 3, 5 and 7 from slot 2.
        assert_eq!(rotate_candidates(0b1010_1010, 8, 2, 8), 0b1010_1010);
        // Thirty-two lanes at the top of the word: slot 31 is age 0 and
        // slot 0 age 1.
        assert_eq!(rotate_candidates(u32::MAX, 32, 31, 32), u32::MAX);
        assert_eq!(rotate_candidates(1, 32, 31, 32), 0b10);
        // Nothing stored means nothing to walk, whatever the tags say.
        assert_eq!(rotate_candidates(u32::MAX, 16, 3, 0), 0);
    }

    #[test]
    fn tag_equality_matches_scalar_comparison_on_every_backend() {
        fn check<const N: usize>(backend: crate::compressor::Backend) {
            let tags: [u8; N] = std::array::from_fn(|i| (i % 5) as u8);
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
                    assert_eq!(actual, expected, "{backend:?} {N} {tag}");
                }
            }
        }
        for backend in crate::compressor::Backend::available() {
            check::<16>(backend);
            check::<32>(backend);
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
            position_offset: 0,
            dictionary_limit: cur_ix,
            gap: 0,
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
        let mut matcher = primed(QuickMatcher::<{ 1 << 16 }, 1, 5, false>::new(), &data);
        let found = search_at(&mut matcher, &data, REPEAT_AT);
        assert!(found.is_match());
        assert_eq!((found.distance, found.len), (64, 64));
    }

    #[test]
    fn every_quick_shape_finds_the_same_repeat() {
        let data = repeated();

        let mut h4 = primed(QuickMatcher::<{ 1 << 17 }, 2, 5, true>::new(), &data);
        let found = search_at(&mut h4, &data, REPEAT_AT);
        assert_eq!((found.distance, found.len), (64, 64));

        let mut h54 = primed(QuickMatcher::<{ 1 << 20 }, 2, 7, false>::new(), &data);
        let found = search_at(&mut h54, &data, REPEAT_AT);
        assert_eq!((found.distance, found.len), (64, 64));
    }

    #[test]
    fn the_bucket_matchers_find_a_repeat_they_have_stored() {
        let data = repeated();

        let mut h5 = primed(BucketMatcher::<false, { 1 << 14 }, 16>::new(0), &data);
        let found = search_at(&mut h5, &data, REPEAT_AT);
        assert_eq!((found.distance, found.len), (64, 64));

        let mut h6 = primed(BucketMatcher::<true, { 1 << 15 }, 16>::new(0), &data);
        let found = search_at(&mut h6, &data, REPEAT_AT);
        assert_eq!((found.distance, found.len), (64, 64));

        // The deepest bucket quality nine asks for finds the same repeat.
        let mut deep = primed(BucketMatcher::<false, { 1 << 15 }, 256>::new(0), &data);
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
        fn reach<const BLOCK: usize>(data: &[u8]) -> usize {
            let mut matcher = BucketMatcher::<false, { 1 << 15 }, BLOCK>::new(0);
            matcher.prepare(true, data.len(), data, true);
            matcher.store_range(data, usize::MAX, 0, 512);
            let found = search_at(&mut matcher, data, 512);
            assert!(found.is_match());
            found.distance
        }
        assert!(reach::<16>(&data) <= 16);
        assert!(reach::<256>(&data) <= 256);
    }

    #[test]
    fn a_bucket_forgets_all_but_its_newest_sixteen_positions() {
        // Every position hashes to the same bucket, so a store past the
        // sixteenth has to push the oldest one out.
        let data = vec![b'a'; 256];
        let mut matcher = BucketMatcher::<false, { 1 << 14 }, 16>::new(0);
        matcher.prepare(true, data.len(), &data, true);
        matcher.store_range(&data, usize::MAX, 0, 100);
        let found = search_at(&mut matcher, &data, 100);
        assert!(found.is_match());
        assert!(found.distance <= 16);
    }

    #[test]
    fn nothing_is_found_when_the_table_holds_no_candidate() {
        let data = repeated();
        let mut matcher = QuickMatcher::<{ 1 << 16 }, 1, 5, false>::new();
        matcher.prepare(true, data.len(), &data, true);
        assert!(!search_at(&mut matcher, &data, REPEAT_AT).is_match());

        let mut chain = ChainMatcher::<1, 16>::new(Q5_CHAIN);
        chain.prepare(true, data.len(), &data, true);
        assert!(!search_at(&mut chain, &data, REPEAT_AT).is_match());

        let mut bucket = BucketMatcher::<false, { 1 << 14 }, 16>::new(0);
        bucket.prepare(true, data.len(), &data, true);
        assert!(!search_at(&mut bucket, &data, REPEAT_AT).is_match());
    }

    #[test]
    fn a_full_preparation_clears_what_a_previous_stream_stored() {
        let data = repeated();
        let mut matcher = primed(QuickMatcher::<{ 1 << 16 }, 1, 5, false>::new(), &data);
        assert!(search_at(&mut matcher, &data, REPEAT_AT).is_match());
        matcher.prepare(false, 0, &data, true);
        assert!(!search_at(&mut matcher, &data, REPEAT_AT).is_match());

        let mut chain = primed(ChainMatcher::<1, 16>::new(Q5_CHAIN), &data);
        assert!(search_at(&mut chain, &data, REPEAT_AT).is_match());
        chain.prepare(false, 0, &data, true);
        assert!(!search_at(&mut chain, &data, REPEAT_AT).is_match());

        let mut bucket = primed(BucketMatcher::<false, { 1 << 14 }, 16>::new(0), &data);
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
        let mut matcher = QuickMatcher::<{ 1 << 16 }, 1, 5, false>::new();
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
        let mut matcher = QuickMatcher::<{ 1 << 16 }, 1, 5, false>::new();
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
            MatchFinder::H5Q5(_)
        ));
        assert!(matches!(
            MatchFinder::from(HasherPlan::H5(Q9_BUCKET)),
            MatchFinder::H5Q9(_)
        ));
        assert!(matches!(
            MatchFinder::from(HasherPlan::H6(Q9_BUCKET)),
            MatchFinder::H6Q9(_)
        ));
    }

    #[test]
    fn a_matcher_reports_the_cached_distance_count_its_shape_asked_for() {
        assert_eq!(
            BucketMatcher::<false, { 1 << 15 }, 256>::new(0).last_distances_to_check(),
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
            QuickMatcher::<{ 1 << 16 }, 1, 5, false>::new().last_distances_to_check(),
            NUM_REMEMBERED_DISTANCES
        );
    }
}
