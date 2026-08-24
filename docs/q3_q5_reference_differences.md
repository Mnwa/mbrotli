# Differences from the pinned reference — qualities 3, 4 and 5

Reference: Google Brotli v1.2.0, commit `028fb5a`, vendored at
`brotli-ffi/vendor/brotli`, built without `BROTLI_MAX_SIMD_QUALITY`.

## 1. Bitstream output

**There are none.** For every input, every quality in 3–5, every window size
from 10 to 24, every mode, block size, size hint, distance layout and
context-modelling setting covered by the test suite, this encoder emits bytes
that are identical to the pinned reference configured the same way.

That is asserted rather than claimed:

| Test | Coverage |
| --- | --- |
| `tests/greedy_qualities.rs` | the whole parameter space, plus block and delayed-symbol boundaries, dictionary matches, ring-buffer wrapping and every length from 1 to 300 |
| `tests/differential_c.rs` | structural corpora and every boundary length, all fifteen window sizes |
| `tests/vendor_corpus.rs` | Google Brotli's own test data, including a 12 MiB input |
| `tests/randomized.rs` | seeded random inputs mixing literal runs, back-references and noise |
| `fuzz/afl/src/bin/differential_c.rs` | the same oracle under AFL, with the parameters driven from the fuzz input |

The benchmark harness re-asserts byte identity before every timed run, so a
reported speedup cannot come from a changed output.

## 2. Reference behaviour reproduced on purpose

### 2.1. The ring buffer's filler bytes

Match finding reads whole words past the current position, so what those bytes
are is part of the output. `RingBufferInitBuffer` zeroes a seven-byte margin,
`RingBufferWrite` seeds the last two window bytes with zero and the first tail
byte with the sentinel `241`, and `CopyInputToRingBuffer` clears seven bytes
past the data on the first lap. All four are reproduced, including the
sentinel, which in practice is overwritten by the same write that places it.

### 2.2. The asymmetric forgetful-chain preparation

`HashForgetfulChain`'s partial preparation seeds `head[bucket]` with `0xCCCC`
while its full preparation seeds it with zero (`c/enc/hash_forgetful_chain_inc.h`).
Both are reachable and they are not equivalent, so both are reproduced.

### 2.3. Sixteen-bit bucket counters that wrap

`HashLongestMatch` counts bucket occupancy in a `uint16_t`. After 65 536 stores
into one bucket the counter wraps to zero and the bucket reads as empty until
it fills again. `BucketMatcher` uses `u16::wrapping_add` for the same reason.

### 2.4. The narrowed block-type index

`BlockSplitterFinishBlock` assigns `self->last_histogram_ix_[0] =
(uint8_t)split->num_types`, narrowing to a byte where every other use of the
same value is a `size_t`. It only matters at the very last usable block type;
the port keeps the narrowing.

### 2.5. The unsigned length-code delta in `ExtendLastCommand`

`ExtendLastCommand` reads the copy-length code delta as an unsigned seven-bit
field, while `CommandCopyLenCode` sign-extends the same field. The two agree
everywhere the delta can occur, because only a static-dictionary match sets it
and never to a negative value. The port keeps the unsigned reading, with a
comment saying why it is safe.

### 2.6. The three-context literal map is priced out, not compared

`ChooseContextMap` computes an honest estimate for the three-context model and
then, below quality seven, replaces it with ten times the one-context estimate
so it can never win. That is how the reference keeps a slower model out of the
lower qualities, and the port does the same rather than deleting the branch.

### 2.7. Single-precision logarithm table

`kBrotliLog2Table` is declared as `double[]` but initialised with `float`
literals. Every entropy comparison in the block splitter and the context model
is sensitive to those last bits, so `shared::tables::LOG2_TABLE` reproduces the
widened float values. This is inherited from the quality 0 and 1 port.

### 2.8. `BrotliBitsEntropy`'s unrolled accumulation

`BrotliBitsEntropy` jumps into the middle of a two-at-a-time loop for an odd
length. The order the terms are summed in changes the last bits of the result,
and the splitter compares those results against fixed thresholds, so
`histogram::bits_entropy` reproduces the same order.

## 3. Deliberate departures

Each of these changes nothing about the emitted bytes.

### 3.1. Uninitialised memory is initialised

The reference leaves several allocations uninitialised and relies on a counter
or a sentinel to keep them unread: `HashLongestMatch`'s bucket array,
`HashForgetfulChain`'s slot banks, and the Huffman node pool. This crate has no
`unsafe`, so those allocations are zeroed. Every one of them is provably never
read before it is written, so the zeroing is unobservable — it is a cost, not a
behaviour change.

### 3.2. An unrepresentable distance layout cannot be built

`ChooseDistanceParams` silently falls back to `(0, 0)` when the requested
postfix and direct counts cannot be expressed. `DistanceCodes` rejects such a
pair at construction, so the fallback is unreachable from the public API. The
sanitiser still implements it, and a unit test drives it directly, because font
mode reaches the same code path.

### 3.3. The distance cache is four entries

`PrepareDistanceCache` extends the cache with near-miss variants only when a
matcher checks more than four cached distances. Every matcher these qualities
can select checks exactly four, so the cache is a four-entry array and the
extension is not ported. Qualities seven and above would need it.

### 3.4. Only the `CONTEXT_UTF8` lookup table is carried

`ChooseContextMode` returns `CONTEXT_SIGNED` only at quality ten and above, and
the literal context mode is otherwise always `CONTEXT_UTF8`. Only that quarter
of `_kBrotliContextLookupTable` is embedded.

### 3.5. The static dictionary's transform table is not carried

A dictionary match is emitted as a distance beyond the end of the window; the
decoder applies the transform. The encoder only needs to know which transform
id a given prefix cut maps to, which is a shift of `kCutoffTransforms`. The
transform table itself is decoder-side and is not embedded.

## 4. Unreachable reference configurations

| Reference feature | Why it is unreachable |
| --- | --- |
| Large-window brotli | `WindowBits` stops at 24, so `H35`, `H55` and `H65` are never selected. |
| Tagged `H58` / `H68` | The pinned C baseline is compiled without `BROTLI_MAX_SIMD_QUALITY`. |
| Compound and custom dictionaries | Gated behind the reference's experimental flag. |
| Stream offset | Not exposed, so the poisoned initial distance cache cannot occur. |
| `BrotliBuildMetaBlock` (clustering) | Quality ten and above only. |
| `BrotliSplitBlock` (the non-greedy splitter) | Quality ten and above only. |
