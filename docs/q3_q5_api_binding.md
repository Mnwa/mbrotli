# Quality 3 / 4 / 5 API binding

Milestone 0 artifact for the greedy port: the mapping between the conceptual
roles the port specification uses and the symbols in this repository. The
encoder itself is private; what the caller sees is the API that was already
there, with the encoder parameters the reference exposes and this crate did not
yet model.

Reference: `google/brotli` v1.2.0, commit `028fb5a`, built without
`BROTLI_MAX_SIMD_QUALITY`. That build is the pinned semantic profile, and it is
what selects `H5`/`H6` rather than the tagged `H58`/`H68` at quality five.

## Repository audit

| Item | Finding |
| --- | --- |
| Public compression entry point | `mbrotli::compressor::Compressor`, obtained from `Brotli::compressor()`. Unchanged. |
| Settings type | `CompressParams`, `Copy`. Extended with mode, size hint, block size, distance layout and the literal-context-modelling switch. |
| Quality specification | `QualityLevel`, unchanged as a closed enum; now also `Eq`/`Ord` so callers and tests can compare levels. |
| Window size | `WindowBits`, a validated newtype over `10..=24`. Unchanged. Large-window brotli is not exposed, so `H35`, `H55` and `H65` are unreachable. |
| Block size | Was not modelled. Added as `BlockBits`, a validated newtype over `16..=24`, matching `BROTLI_{MIN,MAX}_INPUT_BLOCK_BITS`. |
| Mode | Was not modelled. Added as `CompressMode` (`Generic`, `Text`, `Font`), matching `BrotliEncoderMode`. |
| Size hint | Was not modelled. Added as `CompressParams::with_size_hint`. |
| Distance layout | Was not modelled. Added as `DistanceCodes`, validated at construction. |
| Literal context modelling | Was not modelled. Added as `CompressParams::with_literal_context_modeling`. |
| Ring buffer | None existed; the fast path works on caller slices. Added privately as `core::greedy::ringbuffer::RingBuffer`. |
| Command representation | None existed. Added privately as `core::greedy::command::Command`. |
| Histograms | `core::fast::histogram` counts literals only. Added privately as `core::greedy::histogram::Histogram<N>`. |
| Bit writer | `core::shared::bits::BitWriter`, moved up from `core::fast` and shared. |
| Huffman builder | `core::shared::huffman`, moved up from `core::fast`; `build_and_store_huffman_tree` added for the greedy path. |
| Block splitter / context map | None existed. Added privately as `core::greedy::split` and `core::greedy::metablock`. |
| Static dictionary | None existed. Added privately as `core::greedy::dictionary`. |
| Workspace abstraction | `core::greedy::encoder::GreedyEncoder` owns and reuses every buffer, like `core::fast::FastEncoder` does for the fast path. |
| Error model | `BrotliCompressError`, `#[non_exhaustive]`. No new variant was needed. |
| Decoder / round-trip infrastructure | Still Google's C decoder through the `google-brotli-ffi` workspace crate. |
| Benchmark harness | `benches/compress.rs`, extended to the five implemented qualities. |
| Fuzz harness | `fuzz/afl`, extended with `q3_roundtrip`, `q4_roundtrip` and `q5_roundtrip`, and with a six-byte parameter header. |

## Conceptual role binding

| Conceptual role | Actual repository symbol | Ownership / lifetime | q3 / q4 / q5 usage |
| --- | --- | --- | --- |
| Settings | `compressor::CompressParams` | `Copy`, passed by value per call | read once when a `GreedyEncoder` is built |
| Quality routing | `compressor::QualityLevel` → `core::greedy::params::GreedyQuality` | `Copy` | `TryFrom` accepts `Q3`, `Q4`, `Q5` and refuses the rest |
| Sanitised parameters | `core::greedy::params::GreedyParams` | `Copy`, built once per encoder | window, block size, distance alphabet, matcher plan |
| Hasher plan | `core::greedy::params::HasherPlan` | `Copy`, resolved once | `H3`, `H4`, `H54`, `H40`, `H5`, `H6` |
| Ring buffer | `core::greedy::ringbuffer::RingBuffer` | owned by the encoder | history, tail mirror, wrap behaviour |
| Command buffer | `Vec<core::greedy::command::Command>` | owned by the encoder, reused | LZ77 output for one meta-block |
| Distance cache | `core::greedy::hashers::DistanceCache` in `ReferenceState` | owned by the encoder | reference-exact updates, saved before each meta-block |
| Encoder dictionary | `core::greedy::dictionary` | `static` word and hash tables, plus per-stream `DictionaryStats` | probed by `H4`, `H40`, `H5` and `H6` |
| Bit writer | `core::shared::bits::BitWriter` | borrows the encoder's scratch buffer | every meta-block |
| Block splitter | `core::greedy::split::{BlockSplitter, ContextBlockSplitter}` | built per meta-block | quality four and five |
| Context builder | `core::greedy::context_model` | stateless | quality five only |
| Huffman builder | `core::shared::huffman` | operates on caller-owned scratch | literal, command, distance, block-type and context-map codes |
| Workspace | `core::greedy::encoder::GreedyEncoder`, `core::greedy::bitstream::MetaBlockWriter` | owned by the encoder, reused across blocks | tables, tree pool, storage |
| Error type | `compressor::BrotliCompressError` | returned by value | `UnsupportedQuality`, `OutputTooSmall`, `BufferOverflow`, `BoundOverflow` |
| Streaming state | `CompressorWriter` / `CompressorReader`, each owning a `core::driver::Encoder` | created lazily on first use | carries the partial byte across meta-blocks |
| Benchmark entry | `benches/compress.rs` | — | Rust against C, identical parameters |
| SIMD level | `fearless_simd::Level`, stored in `Brotli` | `Copy`, detected once per process | one `dispatch!` per `encode_block` |

## Integration rule

```text
quality == 0 or 1        -> core::fast          (unchanged)
quality == 3, 4 or 5     -> core::greedy
quality == 2, 6..9, 11   -> BrotliCompressError::UnsupportedQuality
```

`core::driver::Encoder` owns that routing, together with the empty-input
shortcut and the uncompressed fallback both families share. Routing still goes
through the existing `QualityLevel`; no second way to specify a quality was
added.

## Changes made to the existing public API

Nothing was removed or re-signed. What was added is the set of encoder
parameters the reference already has and this crate did not model, each behind
a validated type so an unrepresentable configuration cannot be built:

| Addition | Why |
| --- | --- |
| `CompressMode` and `CompressParams::{with_mode, mode}` | Font mode changes the distance layout at quality four and above. |
| `BlockBits`, `ParseBlockBitsError`, `CompressParams::{with_block_bits, lgblock}` | The input block size is a reference parameter and changes meta-block boundaries. |
| `CompressParams::{with_size_hint, size_hint}` | Qualities four and five choose their match finder from it. |
| `DistanceCodes`, `ParseDistanceCodesError`, `CompressParams::{with_distance_codes, distance_codes}` | Postfix bits and direct codes are a reference parameter. |
| `CompressParams::{with_literal_context_modeling, literal_context_modeling}` | Quality five models literal contexts unless told not to. |
| `QualityLevel: Eq + Ord + Hash`, `CompressParams: Eq` | Comparison was missing; nothing else changed about them. |

### The size hint and the streaming adapters

`BrotliEncoderCompress` sets `BROTLI_PARAM_SIZE_HINT` to the input length;
`BrotliEncoderCompressStream` leaves it at zero unless the caller sets it. This
crate reproduces both: the one-shot entry points substitute the input length
for a missing hint, the streaming adapters treat a missing hint as zero. A
caller who needs the two to agree byte for byte sets the hint explicitly.

This is the only documented way in which streaming output can differ from
one-shot output for the same input.

## Reachability of the reference's configuration space

| Reference feature | Status here | Why |
| --- | --- | --- |
| Large-window brotli (`lgwin > 24`) | Not exposed | `WindowBits` stops at 24, so `H35`, `H55` and `H65` are unreachable. |
| Tagged matchers `H58` / `H68` | Not built | The pinned C baseline is compiled without `BROTLI_MAX_SIMD_QUALITY`, which selects `H5` / `H6`. |
| Compound and custom dictionaries | Not exposed | The reference gates them behind `BROTLI_EXPERIMENTAL`; the built-in static dictionary is used. |
| Stream offset | Not exposed | No public parameter, so the reference's poisoned distance cache is unreachable. |
| `CONTEXT_SIGNED` literal mode | Unreachable | `ChooseContextMode` only returns it at quality ten and above. |
| Three-context literal map | Unreachable at these qualities | `ChooseContextMap` prices it out below quality seven, exactly as the reference does. |

## Verification

| Check | Where |
| --- | --- |
| Byte identity with the C encoder, default parameters | `tests/differential_c.rs`, `tests/vendor_corpus.rs`, `tests/randomized.rs` |
| Byte identity across the whole parameter space | `tests/greedy_qualities.rs` |
| Backend identity | `tests/simd_backends.rs`, `tests/roundtrip.rs` |
| Streaming equals one-shot | `tests/streaming.rs` |
| Round-trip through an independent decoder | every suite above, using Google's C decoder |
| Continuous fuzzing | `fuzz/afl`, eleven targets |
