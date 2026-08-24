# Quality 6 / 7 / 8 / 9 API binding

Milestone 0 artifact for the q6–q9 port: the mapping between the conceptual
roles the port specification uses and the symbols in this repository.

These four qualities reuse the greedy encoder wholesale. Nothing about the
public API changed for them, and no new module was created: the search, the
meta-block builder and the bit writer are the ones qualities three to five
already used, with the depths and thresholds the reference varies by quality now
resolved from the quality rather than fixed.

Reference: `google/brotli` v1.2.0, commit `028fb5a`. On this workspace's
platforms that build defines `BROTLI_MAX_SIMD_QUALITY = 6`, so it selects the
tagged `H58`/`H68` at quality six and the untagged `H5`/`H6` at seven and above.
This port builds only the untagged pair, which is byte-for-byte equivalent; see
`architecture/greedy-encoder.md` §2.2 for why, and `tests/differential_c.rs` for
the check against the real library.

## Repository audit

| Item | Finding |
| --- | --- |
| Public compression entry point | `mbrotli::compressor::Compressor`. Unchanged. |
| Settings type | `CompressParams`. Unchanged — every parameter these qualities react to was already modelled for q3–q5. |
| Quality specification | `QualityLevel::Q6` … `Q9` already existed and were rejected at routing; they now resolve. |
| Quality routing | `core::driver::Encoder::new`, unchanged in shape: `GreedyQuality::try_from` accepts four more levels. |
| Ring buffer | `core::shared::ringbuffer::RingBuffer`, moved up from `core::greedy` so the high-quality encoder can share it. Its constructor now takes `(rb_bits, lgblock)` instead of a `GreedyParams`. |
| Command representation | `core::shared::command::Command`, moved up from `core::greedy`. `extend_last_command` moved with it, since every quality above one performs it. |
| Distance cache | Widened from four entries to sixteen (`core::shared::hashers::DistanceCache`), with `prepare_distance_cache` deriving the near-miss entries qualities seven and up probe. |
| Match finders | `core::greedy::hashers`. `BucketMatcher` gained runtime `block_bits` and `last_distances`; `ChainMatcher` became generic over `NUM_BANKS`/`BANK_BITS` so `H42` can spread its chains over 512 banks. |
| Context modelling | `core::greedy::context_model`, gained an `hq_contexts` flag that stops pricing the three-context model out of reach at quality seven and above. |
| Block size | `compute_lgblock` gained the `lgwin` argument quality nine's `min(18, lgwin)` default needs. |
| Sparse search | `GreedyParams::random_heuristics_window_size`, now 512 at quality nine. |
| Error model | `BrotliCompressError`. Unchanged. |
| Benchmark harness | `benches/compress.rs`, driven by the shared quality list. |
| Fuzz harness | `fuzz/afl`, extended with `q6_roundtrip` … `q9_roundtrip`. |

## Conceptual role binding

| Conceptual role | Existing repository symbol | Ownership / lifetime | Required use |
| --- | --- | --- | --- |
| Settings | `mbrotli::compressor::CompressParams` | `Copy`, per call | quality and encoder parameters |
| Quality routing | `core::driver::Encoder::new` → `core::greedy::params::GreedyQuality::try_from` | per encoder | routes q6–q9 onto the greedy path and nothing else |
| Resolved parameters | `core::greedy::params::GreedyParams` | `Copy`, owned by the encoder | every quality-dependent depth and threshold, resolved once |
| Hasher plan | `core::greedy::params::HasherPlan`, `BucketShape`, `ChainShape` | `Copy`, resolved once | selected from quality, `lgwin` and `size_hint` alone |
| Ring buffer | `core::shared::ringbuffer::RingBuffer` | owned by `GreedyEncoder` | history and wrap behaviour |
| Command buffer | `Vec<core::shared::command::Command>` | owned by `GreedyEncoder`, reused | LZ77 output |
| Distance cache | `core::greedy::hashers::DistanceCache` + `prepare_distance_cache` | inside `ReferenceState` | sixteen entries, four remembered across meta-blocks |
| Encoder dictionary | `core::shared::dictionary` | `'static` tables | static dictionary only |
| Bit writer | `core::shared::bits::BitWriter` | borrows the encoder's scratch buffer | RFC-compliant storage |
| Block splitter | `core::greedy::split::{BlockSplitter, ContextBlockSplitter}` | per meta-block | greedy splitting |
| Context builder | `core::greedy::context_model::decide_over_literal_context_modeling` | per meta-block | one, two, three or thirteen contexts |
| Huffman builder | `core::shared::huffman` | scratch owned by `MetaBlockWriter` | deterministic trees |
| Workspace | `core::greedy::encoder::GreedyEncoder` | one per stream | owns and reuses every buffer |
| Error type | `mbrotli::compressor::BrotliCompressError` | — | no new public error model |
| Streaming state | `GreedyEncoder` fields | one per stream | history and finalisation |
| Benchmark entry | `benches/compress.rs` | — | Rust/C comparison |

## What the quality actually changes

Everything below is a function of the quality alone, resolved before any loop
runs. Nothing about the running machine takes part.

| Decision | Where | q6 | q7 | q8 | q9 |
| --- | --- | ---: | ---: | ---: | ---: |
| Bucket candidates | `BucketShape::block_bits` | 32 | 64 | 128 | 256 |
| Bucket bits (`H5`) | `BucketShape::bucket_bits` | 14 | 15 | 15 | 15 |
| Cached distances probed | `BucketShape::last_distances` | 4 | 10 | 10 | 16 |
| Small-window matcher | `HasherPlan::Chain` | `H40` | `H41` | `H41` | `H42` |
| Chain hops | `ChainShape::max_hops` | 32 | 56 | 112 | 224 |
| Chain banks | `ChainShape::num_banks` | 1 | 1 | 1 | 512 |
| Three-context model | `GreedyQuality::hq_context_modeling` | no | yes | yes | yes |
| Sparse-search threshold | `random_heuristics_window_size` | 64 | 64 | 64 | 512 |
| Default `lgblock` | `compute_lgblock` | 16 | 16 | 16 | `min(18, lgwin)` |

## Integration rules honoured

- The implementation is private behind the existing high-level API.
- Qualities 0, 1 and 3 to 5 produce byte-identical output to before the change;
  `tests/differential_c.rs` covers all of them against the same C baseline.
- No public compatibility layer was added to mirror the C API.
- Dispatch, workspace selection and quality routing all happen before the hot
  match loop: one `dispatch!` per `encode_block`, and the `MatchFinder` enum is
  matched once per input block rather than once per candidate.
- No new public error variant, and no change to the `std`/allocator/MSRV
  contracts.
