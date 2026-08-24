# Qualities 6 to 11: differences from the reference

Every divergence from `google/brotli` v1.2.0, commit `028fb5a`, in the q6–q11
port — including the quirks reproduced on purpose. None of them changes a byte
of output: `tests/differential_c.rs` compares all eleven implemented qualities
against the C library over the structural, boundary and vendor corpora at every
window size from 10 to 24.

## 1. Deliberate structural differences

### 1.1. The tagged match finders are not built

On this workspace's platforms the reference defines `BROTLI_MAX_SIMD_QUALITY`
and therefore selects `H58` and `H68` — tagged variants of `H5` and `H6` — at
qualities five and six. This port builds only the untagged pair.

They cannot produce a different stream:

- The tagged `HashBytes` keeps eight more low bits, which the key shifts
  straight back off, so both select the same bucket.
- Both walk the bucket newest to oldest.
- A tag is a function of the hashed bytes, so two positions whose first four
  bytes agree always share a tag. A slot the tag mask drops therefore differs in
  those four bytes, and such a candidate can never pass the reference's
  `len >= 4` acceptance test.
- Both break at the first candidate beyond `max_backward`, and stored positions
  grow monotonically along the ring, so both stop having seen the same prefix.

The differential tests check the consequence rather than the argument: qualities
six and seven are compared against a C library that really is using the tagged
matchers.

What this gives up is the tag mask itself, which is the reference's main SIMD
opportunity on the greedy path.

### 1.2. Candidate depth is a field, not a const generic

`BucketMatcher` takes `bucket_bits` and the hash width as const generics, so the
hash is a fixed shift, but `block_bits` and `last_distances` are ordinary
fields. Qualities five to nine ask for five different bucket depths; making each
a separate monomorphisation would cost more instruction cache than a
loop bound is worth. The same applies to `ChainMatcher`'s hop limit.

### 1.3. `saved_dist_cache` stays four wide

The reference's live distance cache is sixteen entries but its saved copy is
four, and the restore after an uncompressed fallback copies only those four.
This port makes that explicit with a four-element array rather than copying
sixteen and relying on the derived entries being rebuilt. The derived entries
are recomputed by `prepare_distance_cache` before any search reads them either
way, so the two are equivalent.

### 1.4. The Zopfli node union is a `u32`

`ZopfliNode` in the reference is a union of `float cost`, `uint32_t shortcut`
and `uint32_t next`, reused in that order as the dynamic program progresses.
This port stores a plain `u32` and reads or writes the cost through
`f32::{to_bits, from_bits}`, which keeps the exact aliasing with no `unsafe`.

### 1.5. The match arena grows instead of being pre-sized

The reference sizes a fixed `BackwardMatch` buffer from `MAX_NUM_MATCHES_H10`
and grows a separate arena for quality eleven with `BROTLI_ENSURE_CAPACITY`.
This port uses one growable `Vec` for each, reserved once per stream and reused.
`MAX_NUM_MATCHES` survives only as a compile-time assertion that the bound still
matches the reference's 128.

### 1.6. `core::shared` grew, and `score` moved into it

Commands, histograms, block splits, the ring buffer, the static dictionary, the
meta-block writer, the distance alphabet, context modes and reference scoring
moved out of `core::greedy` into `core::shared`, so `core::hq` could use them
without either encoder core depending on the other. `score` in particular lives
beside the static dictionary because the dictionary probe scores its own
candidates; qualities ten and eleven never call it.

## 2. Reference quirks reproduced on purpose

### 2.1. The short backward scan skips its oldest position

`FindAllMatches` scans back with `for (i = cur_ix - 1; i > stop; --i)`, so the
position at `stop` itself is never examined. At `cur_ix = 0` the loop variable
wraps to `SIZE_MAX` and the backward limit — zero there — is what stops it. Both
are reproduced, the second with `wrapping_sub`.

### 2.2. `ComputeDistanceCache` refills from the beginning

When the shortcut chain supplies fewer than four distances, the reference fills
the rest with `dist_cache[idx++] = *starting_dist_cache++` — a *separate*
pointer that starts at the saved cache's first entry. Indexing both sides in
parallel puts the wrong distance behind every derived short code, which shows up
as a copy spelled out in full where the reference used a short code. The port
had this bug and it is now covered by
`hq::encoder::tests::the_collision_regression_matches_the_c_encoder`.

### 2.3. `StoreRange` can skip the middle of a range entirely

`H10`'s range store keeps the last 63 positions dense and strides by eight over
the prefix — but only once the range spans at least 512. A range between 63 and
575 long has everything before its last 63 positions dropped outright.

### 2.4. The distance-parameter search carries an index between rows

`BrotliBuildMetaBlock` walks postfix bits outward and direct-code counts upward,
and `ndirect_msb` is *not* reset between rows: after each it is decremented and
halved. Which combinations get examined at all depends on that carry.

### 2.5. `BrotliPopulationCostDistance` always prices the full alphabet

The distance-parameter search calls it on candidate alphabets of different
widths, but it always uses `BROTLI_NUM_HISTOGRAM_DISTANCE_SYMBOLS` (544), not
the candidate's own `alphabet_size_limit`. Narrowing it makes a wide alphabet
look cheaper than the reference thinks it is. The port had this wrong initially.

### 2.6. NUL is not treated as text

`BrotliParseAsUTF8` falls through its ASCII case for a zero byte, so NUL lands in
the "not UTF-8" bucket alongside a stray continuation byte. This affects both the
literal-cost model and, at quality ten and above, the choice of context mode.

### 2.7. The start-position queue's eviction is unspecified

`StartPosQueuePush` writes into a ring slot and restores order with adjacent
swaps, so which candidate is displaced when the queue is full depends on where
those swaps have moved things. The reference makes no promise about it; the port
reproduces the mechanism rather than any particular eviction rule, and its tests
assert only the size bound and the ordering.

### 2.8. `ExtendLastCommand` reads the length-code delta unsigned

Recomputing the command symbol after growing a copy, the reference reads the
packed length-code modifier as an unsigned field. That agrees with the signed
reading everywhere it can occur — only a dictionary match sets it, and never
negative — so the port keeps the reference's arithmetic.

## 3. Numerical contract

Qualities ten and eleven decide on `f32` comparisons, so the arithmetic is part
of the output:

- `FastLog2` returns a `double` and is narrowed to `f32` at each use, so the
  intermediate is computed at full width and rounded once.
- The cumulative literal costs use the reference's explicit carry. Over 200,000
  additions of a value with no exact binary representation, a naive `f32` sum is
  more than two orders of magnitude further from the true total than the carried
  one; the dynamic program compares those totals directly.
- No `mul_add` is substituted anywhere the reference uses separate operations,
  and no reduction is reassociated.
- Nothing tie-sensitive is vectorised. `find_match_length` is the only SIMD
  kernel on this path, and it returns an exact first mismatch.

## 4. What is not implemented

- **Large-window brotli.** `WindowBits` stops at 24, so `H35`, `H55`, `H65` and
  the extended distance alphabet are unreachable.
- **Shared and compound dictionaries.** The reference's compound-dictionary
  branches in `UpdateNodes` and `FindAllMatches` therefore have no counterpart;
  `gap` is zero throughout, which makes the "gray area" branch of the
  cached-distance loop unreachable.
- **Stream offset.** Always zero; the reference uses it only for shared
  dictionaries.
- **Quality 2.** The only quality the format defines that has no encoder here.
- **The `BROTLI_EXPERIMENTAL` trie** in the static dictionary search.
