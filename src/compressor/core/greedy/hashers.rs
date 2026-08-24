//! Match finders for qualities three to five.
//!
//! Ports `hash_longest_match_quickly_inc.h` (H3, H4, H54),
//! `hash_longest_match_inc.h` (H5), `hash_longest_match64_inc.h` (H6) and
//! `hash_forgetful_chain_inc.h` (H40) from the pinned reference
//! (`google/brotli` v1.2.0, commit `028fb5a`).
//!
//! Which of them runs is decided once, from the caller's parameters, by
//! [`super::params::choose_hasher`]. Each is a separate type so the bucket
//! geometry is a compile-time constant inside the probe loop, and the enum that
//! selects between them is matched once per block rather than once per
//! candidate.
//!
//! The reference extends the distance cache with near-miss variants when a
//! matcher checks more than four cached distances. Every matcher reachable
//! from qualities three to five checks exactly four, so the cache here is the
//! plain four-entry one and `PrepareDistanceCache` has nothing to do.

use fearless_simd::Simd;

use super::dictionary::{self, DictionaryStats};
use super::params::HasherPlan;
use super::score::{
    SearchResult, backward_reference_penalty_using_last_distance, backward_reference_score,
    backward_reference_score_using_last_distance,
};
use crate::compressor::core::shared::constants::HASH_MUL32;
use crate::compressor::core::shared::match_len::find_match_length;

/// Sixty-four-bit hash multiplier (`kHashMul64`).
const HASH_MUL64: u64 = 0x1FE3_5A7B_D357_9BD3;

/// The four distances the encoder keeps as short codes.
pub(crate) type DistanceCache = [i32; 4];

/// Distance cache the reference starts every stream with.
pub(crate) const INITIAL_DISTANCE_CACHE: DistanceCache = [4, 11, 15, 16];

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

/// A match finder over the ring buffer.
pub(crate) trait Matcher {
    /// Bytes a candidate needs available to be hashed (`HashTypeLength`).
    const HASH_TYPE_LENGTH: usize;

    /// Bytes a store needs available (`StoreLookahead`).
    const STORE_LOOKAHEAD: usize;

    /// Clears the table before the first block (`Prepare`).
    fn prepare(&mut self, one_shot: bool, input_size: usize, data: &[u8]);

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

    fn prepare(&mut self, one_shot: bool, input_size: usize, data: &[u8]) {
        // Clearing only the slots a short input can reach is far cheaper than
        // wiping the whole table, and reaches exactly the same slots the
        // search will later look at.
        let partial_prepare_threshold = Self::BUCKET_SIZE >> 5;
        if one_shot && input_size <= partial_prepare_threshold {
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
                }
            }
            return;
        }

        let mut keys = [0usize; 4];
        for (sweep, slot) in keys.iter_mut().enumerate().take(Self::SWEEP) {
            *slot = (key + (sweep << 3)) & Self::BUCKET_MASK;
        }
        let key_out = keys[(query.cur_ix & Self::SWEEP_MASK) >> 3];
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

        if USE_DICTIONARY && min_score == out.score {
            dictionary::search(
                stats,
                data.get(cur_ix_masked..).unwrap_or_default(),
                query.max_length,
                query.dictionary_distance,
                query.max_distance,
                out,
                true,
            );
        }
        buckets[key_out & Self::BUCKET_MASK] = query.cur_ix as u32;
    }
}

/// Slots one bucket of the H5 and H6 matchers holds (`1 << block_bits`).
///
/// Quality five fixes `block_bits` at four; a higher quality would have to make
/// this a parameter again.
const BUCKET_BLOCK_BITS: u32 = 4;

/// Number of positions one bucket remembers.
const BUCKET_BLOCK_SIZE: usize = 1usize << BUCKET_BLOCK_BITS;

/// Mask that turns a bucket counter into a slot index.
const BUCKET_BLOCK_MASK: u16 = (BUCKET_BLOCK_SIZE - 1) as u16;

/// Cached distances the bucket and chain matchers probe first.
///
/// Quality five fixes this at four, which is also why the distance cache never
/// needs the reference's near-miss extension.
const NUM_LAST_DISTANCES_TO_CHECK: usize = 4;

/// Bucketed match finder keeping the last sixteen positions per hash.
///
/// `HASH64` selects the H6 variant, which hashes eight bytes instead of four
/// and pre-filters candidates on their first four bytes.
pub(crate) struct BucketMatcher<const HASH64: bool, const BUCKET_BITS: u32> {
    num: Vec<u16>,
    buckets: Vec<u32>,
}

impl<const HASH64: bool, const BUCKET_BITS: u32> BucketMatcher<HASH64, BUCKET_BITS> {
    /// Number of buckets in the table.
    const BUCKET_SIZE: usize = 1usize << BUCKET_BITS;

    /// Creates an empty table.
    pub(crate) fn new() -> Self {
        Self {
            num: vec![0u16; Self::BUCKET_SIZE],
            // The reference leaves this uninitialised: a slot is only read
            // after the counter that guards it has been incremented. Zeroing
            // costs one pass and removes the question entirely.
            buckets: vec![0u32; Self::BUCKET_SIZE * BUCKET_BLOCK_SIZE],
        }
    }

    /// Returns the bucket of the bytes at `offset` (`HashBytes`).
    #[inline(always)]
    fn hash(data: &[u8], offset: usize) -> usize {
        if HASH64 {
            // H6 tunes the multiplier to a five-byte match and always takes
            // fifteen bits, whatever the bucket count is.
            let hash_mul = HASH_MUL64 << (64 - 5 * 8);
            (read_u64(data, offset).wrapping_mul(hash_mul) >> (64 - 15)) as usize
        } else {
            (read_u32(data, offset).wrapping_mul(HASH_MUL32) >> (32 - BUCKET_BITS)) as usize
        }
    }
}

impl<const HASH64: bool, const BUCKET_BITS: u32> Matcher for BucketMatcher<HASH64, BUCKET_BITS> {
    const HASH_TYPE_LENGTH: usize = if HASH64 { 8 } else { 4 };
    const STORE_LOOKAHEAD: usize = Self::HASH_TYPE_LENGTH;

    fn prepare(&mut self, one_shot: bool, input_size: usize, data: &[u8]) {
        let partial_prepare_threshold = Self::BUCKET_SIZE >> 6;
        if one_shot && input_size <= partial_prepare_threshold {
            for offset in 0..input_size {
                let key = Self::hash(data, offset);
                if let Some(count) = self.num.get_mut(key) {
                    *count = 0;
                }
            }
        } else {
            self.num.fill(0);
        }
    }

    #[inline(always)]
    fn store(&mut self, data: &[u8], mask: usize, ix: usize) {
        let key = Self::hash(data, ix & mask);
        let Some(count) = self.num.get_mut(key) else {
            return;
        };
        let minor_ix = usize::from(*count & BUCKET_BLOCK_MASK);
        *count = count.wrapping_add(1);
        if let Some(slot) = self.buckets.get_mut(minor_ix + (key << BUCKET_BLOCK_BITS)) {
            *slot = ix as u32;
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
        let bucket_base = key << BUCKET_BLOCK_BITS;

        out.len = 0;
        out.len_code_delta = 0;

        for index in 0..NUM_LAST_DISTANCES_TO_CHECK {
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
        let total = usize::from(count);
        let down = total.saturating_sub(BUCKET_BLOCK_SIZE);
        let mut index = total;
        while index > down {
            index -= 1;
            let slot = bucket_base + usize::from((index as u16) & BUCKET_BLOCK_MASK);
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
            let slot = bucket_base + usize::from(*counter & BUCKET_BLOCK_MASK);
            if let Some(entry) = self.buckets.get_mut(slot) {
                *entry = query.cur_ix as u32;
            }
            *counter = counter.wrapping_add(1);
        }

        if min_score == out.score {
            dictionary::search(
                stats,
                data.get(cur_ix_masked..).unwrap_or_default(),
                query.max_length,
                query.dictionary_distance,
                query.max_distance,
                out,
                false,
            );
        }
    }
}

/// Slots one forgetful-chain bank holds (`BANK_SIZE`).
const CHAIN_BANK_SIZE: usize = 1 << 16;

/// Number of buckets the forgetful chain hashes into.
const CHAIN_BUCKET_SIZE: usize = 1 << 15;

/// Address value that terminates a chain after its first node.
///
/// Positions never reach three gibibytes plus sixty-four mebibytes, so a
/// bucket seeded with this always produces a delta larger than any window.
const CHAIN_EMPTY_ADDR: u32 = 0xCCCC_CCCC;

/// Head value the partial preparation seeds a bucket with.
const CHAIN_EMPTY_HEAD: u16 = 0xCCCC;

/// Chain hops quality five follows (`max_hops`).
const CHAIN_MAX_HOPS: usize = 16;

/// One node of a forgetful chain.
#[derive(Copy, Clone, Debug, Default)]
struct ChainSlot {
    delta: u16,
    next: u16,
}

/// Forgetful-chain match finder (`HashForgetfulChain`, H40).
///
/// Chains share one storage bank, so old nodes are overwritten rather than
/// freed and several chains may end up sharing a tail. A one-byte truncated
/// hash rejects cached-distance candidates before they are compared.
pub(crate) struct ChainMatcher {
    addr: Vec<u32>,
    head: Vec<u16>,
    tiny_hash: Vec<u8>,
    slots: Vec<ChainSlot>,
    free_slot_idx: u16,
}

impl ChainMatcher {
    /// Creates an empty chain table.
    pub(crate) fn new() -> Self {
        Self {
            addr: vec![CHAIN_EMPTY_ADDR; CHAIN_BUCKET_SIZE],
            head: vec![0u16; CHAIN_BUCKET_SIZE],
            tiny_hash: vec![0u8; 1 << 16],
            slots: vec![ChainSlot::default(); CHAIN_BANK_SIZE],
            free_slot_idx: 0,
        }
    }

    /// Returns the bucket of the bytes at `offset` (`HashBytes`).
    #[inline(always)]
    fn hash(data: &[u8], offset: usize) -> usize {
        (read_u32(data, offset).wrapping_mul(HASH_MUL32) >> (32 - 15)) as usize
    }
}

impl Matcher for ChainMatcher {
    const HASH_TYPE_LENGTH: usize = 4;
    const STORE_LOOKAHEAD: usize = 4;

    fn prepare(&mut self, one_shot: bool, input_size: usize, data: &[u8]) {
        let partial_prepare_threshold = CHAIN_BUCKET_SIZE >> 6;
        if one_shot && input_size <= partial_prepare_threshold {
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
        self.free_slot_idx = 0;
    }

    #[inline(always)]
    fn store(&mut self, data: &[u8], mask: usize, ix: usize) {
        let key = Self::hash(data, ix & mask);
        let idx = usize::from(self.free_slot_idx) & (CHAIN_BANK_SIZE - 1);
        self.free_slot_idx = self.free_slot_idx.wrapping_add(1);
        let previous = self.addr.get(key).copied().unwrap_or(CHAIN_EMPTY_ADDR);
        let delta = ix.wrapping_sub(previous as usize);
        if let Some(slot) = self.tiny_hash.get_mut(ix as u16 as usize) {
            *slot = key as u8;
        }
        let delta = if delta > 0xFFFF { 0xFFFF } else { delta as u16 };
        let head = self.head.get(key).copied().unwrap_or(0);
        if let Some(slot) = self.slots.get_mut(idx) {
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

        for index in 0..NUM_LAST_DISTANCES_TO_CHECK {
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

        let mut backward = 0usize;
        let mut delta = query
            .cur_ix
            .wrapping_sub(self.addr.get(key).copied().unwrap_or(CHAIN_EMPTY_ADDR) as usize);
        let mut slot = usize::from(self.head.get(key).copied().unwrap_or(0));
        for _ in 0..CHAIN_MAX_HOPS {
            let last = slot;
            backward = backward.wrapping_add(delta);
            if backward > query.max_backward {
                break;
            }
            let prev_ix = (query.cur_ix.wrapping_sub(backward)) & mask;
            let node = self.slots.get(last).copied().unwrap_or_default();
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
            dictionary::search(
                stats,
                data.get(cur_ix_masked..).unwrap_or_default(),
                query.max_length,
                query.dictionary_distance,
                query.max_distance,
                out,
                false,
            );
        }
    }
}

/// The match finder a stream is using, chosen once from its parameters.
pub(crate) enum MatchFinder {
    /// Quality 3.
    H3(QuickMatcher<16, 1, 5, false>),
    /// Quality 4, small inputs.
    H4(QuickMatcher<17, 2, 5, true>),
    /// Quality 4, large inputs.
    H54(QuickMatcher<20, 2, 7, false>),
    /// Quality 5, small windows.
    H40(ChainMatcher),
    /// Quality 5, ordinary inputs.
    H5(BucketMatcher<false, 14>),
    /// Quality 5, large inputs and wide windows.
    H6(BucketMatcher<true, 15>),
}

impl From<HasherPlan> for MatchFinder {
    /// Allocates the match finder a plan calls for.
    fn from(plan: HasherPlan) -> Self {
        match plan {
            HasherPlan::H3 => Self::H3(QuickMatcher::new()),
            HasherPlan::H4 => Self::H4(QuickMatcher::new()),
            HasherPlan::H54 => Self::H54(QuickMatcher::new()),
            HasherPlan::H40 => Self::H40(ChainMatcher::new()),
            HasherPlan::H5 { .. } => Self::H5(BucketMatcher::new()),
            HasherPlan::H6 { .. } => Self::H6(BucketMatcher::new()),
        }
    }
}

impl MatchFinder {
    /// Clears the table before the first block of a stream (`Prepare`).
    pub(crate) fn prepare(&mut self, one_shot: bool, input_size: usize, data: &[u8]) {
        match self {
            Self::H3(matcher) => matcher.prepare(one_shot, input_size, data),
            Self::H4(matcher) => matcher.prepare(one_shot, input_size, data),
            Self::H54(matcher) => matcher.prepare(one_shot, input_size, data),
            Self::H40(matcher) => matcher.prepare(one_shot, input_size, data),
            Self::H5(matcher) => matcher.prepare(one_shot, input_size, data),
            Self::H6(matcher) => matcher.prepare(one_shot, input_size, data),
        }
    }

    /// Records the positions spanning the previous block boundary.
    pub(crate) fn stitch_to_previous_block(
        &mut self,
        num_bytes: usize,
        position: usize,
        data: &[u8],
        mask: usize,
    ) {
        match self {
            Self::H3(matcher) => matcher.stitch_to_previous_block(num_bytes, position, data, mask),
            Self::H4(matcher) => matcher.stitch_to_previous_block(num_bytes, position, data, mask),
            Self::H54(matcher) => matcher.stitch_to_previous_block(num_bytes, position, data, mask),
            Self::H40(matcher) => matcher.stitch_to_previous_block(num_bytes, position, data, mask),
            Self::H5(matcher) => matcher.stitch_to_previous_block(num_bytes, position, data, mask),
            Self::H6(matcher) => matcher.stitch_to_previous_block(num_bytes, position, data, mask),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

        let mut h5 = primed(BucketMatcher::<false, 14>::new(), &data);
        let found = search_at(&mut h5, &data, REPEAT_AT);
        assert_eq!((found.distance, found.len), (64, 64));

        let mut h6 = primed(BucketMatcher::<true, 15>::new(), &data);
        let found = search_at(&mut h6, &data, REPEAT_AT);
        assert_eq!((found.distance, found.len), (64, 64));
    }

    #[test]
    fn the_chain_matcher_finds_a_repeat_it_has_stored() {
        let data = repeated();
        let mut matcher = primed(ChainMatcher::new(), &data);
        let found = search_at(&mut matcher, &data, REPEAT_AT);
        assert_eq!((found.distance, found.len), (64, 64));
    }

    #[test]
    fn a_bucket_forgets_all_but_its_newest_sixteen_positions() {
        // Every position hashes to the same bucket, so a store past the
        // sixteenth has to push the oldest one out.
        let data = vec![b'a'; 256];
        let mut matcher = BucketMatcher::<false, 14>::new();
        matcher.prepare(true, data.len(), &data);
        matcher.store_range(&data, usize::MAX, 0, 100);
        let found = search_at(&mut matcher, &data, 100);
        assert!(found.is_match());
        assert!(found.distance <= BUCKET_BLOCK_SIZE);
    }

    #[test]
    fn nothing_is_found_when_the_table_holds_no_candidate() {
        let data = repeated();
        let mut matcher = QuickMatcher::<16, 1, 5, false>::new();
        matcher.prepare(true, data.len(), &data);
        assert!(!search_at(&mut matcher, &data, REPEAT_AT).is_match());

        let mut chain = ChainMatcher::new();
        chain.prepare(true, data.len(), &data);
        assert!(!search_at(&mut chain, &data, REPEAT_AT).is_match());

        let mut bucket = BucketMatcher::<false, 14>::new();
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

        let mut chain = primed(ChainMatcher::new(), &data);
        assert!(search_at(&mut chain, &data, REPEAT_AT).is_match());
        chain.prepare(false, 0, &data);
        assert!(!search_at(&mut chain, &data, REPEAT_AT).is_match());

        let mut bucket = primed(BucketMatcher::<false, 14>::new(), &data);
        assert!(search_at(&mut bucket, &data, REPEAT_AT).is_match());
        bucket.prepare(false, 0, &data);
        assert!(!search_at(&mut bucket, &data, REPEAT_AT).is_match());
    }

    #[test]
    fn every_backend_agrees_on_the_match_it_finds() {
        let data = repeated();
        let mut results = Vec::new();
        for level in [Level::new(), Level::baseline(), Level::fallback()] {
            let mut matcher = primed(BucketMatcher::<false, 14>::new(), &data);
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
            MatchFinder::from(HasherPlan::H40),
            MatchFinder::H40(_)
        ));
        assert!(matches!(
            MatchFinder::from(HasherPlan::H5 {
                bucket_bits: 14,
                block_bits: 4,
                last_distances: 4
            }),
            MatchFinder::H5(_)
        ));
        assert!(matches!(
            MatchFinder::from(HasherPlan::H6 {
                bucket_bits: 15,
                block_bits: 4,
                last_distances: 4
            }),
            MatchFinder::H6(_)
        ));
    }
}
