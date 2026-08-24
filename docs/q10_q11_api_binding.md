# Quality 10 / 11 API binding

Milestone 0 artifact for the q10–q11 port: the mapping between the conceptual
roles the port specification uses and the symbols in this repository.

These two qualities are a new encoder core, `core::hq`, not a variant of the
greedy one. What they share with it — commands, histograms, block splits, the
ring buffer, the static dictionary, the meta-block writer — was moved into
`core::shared` as part of this change, so neither core depends on the other.

Reference: `google/brotli` v1.2.0, commit `028fb5a`. `BROTLI_MAX_SIMD_QUALITY`
does not reach these qualities, so there is only one semantic profile: both use
the binary-tree matcher `H10` unconditionally.

## Repository audit

| Item | Finding |
| --- | --- |
| Public compression entry point | `mbrotli::compressor::Compressor`. Unchanged. |
| Settings type | `CompressParams`. Unchanged — every parameter these qualities react to was already modelled. |
| Quality specification | `QualityLevel` gained a `Q10` variant. `TryFrom<usize>` now accepts 10, and `ParseQualityLevelError::Unrepresentable` was removed with it. This is the only public API change in the port. |
| Quality routing | `core::driver::Encoder::new` gained an `Hq` arm, reached only when the fast and greedy cores both decline. |
| Ring buffer | `core::shared::ringbuffer::RingBuffer`, moved up from `core::greedy` and now sized by `(rb_bits, lgblock)` rather than by a quality's parameter struct. |
| Command representation | `core::shared::command::Command`, moved up from `core::greedy`, together with `extend_last_command`. |
| Histograms | `core::shared::histogram::Histogram<N>`, moved up and given the `bit_cost` field the reference caches on it for clustering, plus `add_vector`. |
| Block split | `core::shared::block_split::BlockSplit`, split out of the greedy splitters so both cores can produce one. |
| Meta-block shape | `core::shared::metablock::{MetaBlockSplit, optimize_histograms}`, split out of the greedy builder. |
| Bit writer | `core::shared::bitstream::MetaBlockWriter`, moved up. `store_meta_block` gained a `ContextMode` argument; the greedy path passes `Utf8`, which it always used implicitly. |
| Context modes | `core::shared::format::ContextMode`, new: the `CONTEXT_SIGNED` lookup table was added beside the existing UTF-8 one, since quality ten is the first that can select it. |
| Distance alphabet | `core::shared::distance`, split out of `core::greedy::params` so the high-quality builder can re-tune it per meta-block. |
| Population cost | `core::shared::bit_cost::population_cost`, new: `BrotliPopulationCost`, which clustering and the high-quality splitter decide every merge on. |
| Static dictionary | `core::shared::dictionary::all_matches`, new: the exhaustive per-length search `H10` needs, with the two lookup tables it reads carried as binary blobs beside the module. |
| Match finder | `core::hq::h10::BinaryTreeMatcher`, new. |
| Backward references | `core::hq::zopfli`, new: the dynamic program, its node array and its start-position queue. |
| Cost model | `core::hq::cost::ZopfliCostModel` and `core::hq::literal_cost`, new. |
| Block splitter | `core::hq::block_splitter`, new: the high-quality splitter, distinct from the greedy one. |
| Histogram clustering | `core::hq::cluster`, new. |
| Meta-block builder | `core::hq::metablock::MetaBlockBuilder`, new. |
| Workspace abstraction | `core::hq::encoder::HqEncoder` owns every buffer; `ZopfliWorkspace` owns the node array, the match arena and the cost model, and grows only when a larger block arrives. |
| Error model | `BrotliCompressError`. No new variant was needed. |
| Decoder / round-trip | Still Google's C decoder through `google-brotli-ffi`. |
| Differential harness | `brotli-ffi/shim/static_dict_probe.c`, new: four encoder-internal functions with no public header, exposed so each layer of the port can be compared against the reference it was translated from. |
| Fuzz harness | `fuzz/afl`, extended with `q10_roundtrip` and `q11_roundtrip`. |

## Conceptual role binding

| Conceptual role | Existing repository symbol | Ownership / lifetime | Required use |
| --- | --- | --- | --- |
| Settings | `mbrotli::compressor::CompressParams` | `Copy`, per call | quality and encoder parameters |
| Quality routing | `core::driver::Encoder::new` → `core::hq::params::HqQuality::try_from` | per encoder | routes q10–q11 and nothing else |
| Resolved parameters | `core::hq::params::HqParams` | `Copy`, owned by the encoder | search limits, block size, starting distance alphabet |
| Ring buffer | `core::shared::ringbuffer::RingBuffer` | owned by `HqEncoder` | history and wrap behaviour |
| Command buffer | `Vec<core::shared::command::Command>` | owned by `HqEncoder`, reused | LZ77 output |
| Distance cache | `core::hq::zopfli::ZopfliState::dist_cache` | per stream | four entries, reference-compatible updates |
| Encoder dictionary | `core::shared::dictionary::all_matches` | `'static` tables | static dictionary only |
| Bit writer | `core::shared::bits::BitWriter` | borrows the encoder's scratch buffer | RFC-compliant storage |
| Block splitter | `core::hq::block_splitter::BlockSplitter` | owned by `MetaBlockBuilder`, reused | high-quality splitting |
| Context builder | `core::hq::metablock::MetaBlockBuilder` + `HqParams::choose_context_mode` | per meta-block | per-context histograms and their clustering |
| Huffman builder | `core::shared::huffman` | scratch owned by `MetaBlockWriter` | deterministic trees |
| Workspace / allocator | `core::hq::zopfli::ZopfliWorkspace` | one per stream | node array, match arena, cost model |
| Error type | `mbrotli::compressor::BrotliCompressError` | — | no new public error model |
| Streaming state | `HqEncoder` fields, `ZopfliState` | one per stream | history and finalisation |
| Benchmark entry | `benches/compress.rs` | — | Rust/C comparison |

## The one public API change

`QualityLevel` previously had no `Q10`, and `TryFrom<usize>` reported 10 as
`ParseQualityLevelError::Unrepresentable`. Quality ten is now implemented, so:

```rust
pub enum QualityLevel { Q0, /* … */ Q9, Q10, Q11 }
```

`TryFrom<usize>` accepts `0..=11` and rejects only 12 and above, with
`UpperBound`. `ParseQualityLevelError::Unrepresentable` is gone with it.

Both changes are breaking for a downstream caller that matches exhaustively:
`QualityLevel` is a plain enum, so adding `Q10` breaks a `match` without a
wildcard arm, and removing `Unrepresentable` breaks one that names it — although
`ParseQualityLevelError` is `#[non_exhaustive]`, so such a `match` needed a
wildcard arm already. The crate is at `0.1.0` and the alternative was to leave
quality ten permanently unreachable, which the port specification rules out.

Quality two remains the only quality the format defines that this crate does not
implement, and it is still reported as `UnsupportedQuality(2)`.

## Integration rules honoured

- The implementation is private behind the existing high-level API; no
  `core::hq` type, SIMD type or low-level error escapes it.
- Qualities 0 to 9 produce byte-identical output to before the change. The
  module moves into `core::shared` were verified against the same C baseline
  before any high-quality code was written.
- `core::hq` does not depend on `core::greedy`, nor the reverse. What both need
  lives in `core::shared`.
- A routing assertion is unnecessary: `HqQuality::try_from` accepts only ten and
  eleven, and `GreedyQuality::try_from` only three to nine, so neither core can
  be reached by the other's qualities.
- Dispatch, workspace selection and quality routing all happen before the hot
  loop: one `dispatch!` per `encode_block`.
- No new public error variant, and no change to the `std`/allocator/MSRV
  contracts.
