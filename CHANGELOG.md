# Changelog

## [Unreleased]

- Cut cold decoder allocation and memory traffic further. The block-switch
  state, context maps, prefix-code groups, distance layout and dictionary
  scratch a compressed meta-block needs now live in a workspace created on the
  first compressed meta-block, so an empty, stored or metadata-only stream never
  allocates or initializes it and `Decompressor::new` copies a small state.
  Huffman table building sizes its second-level storage to the exact number of
  entries a code appends instead of a worst-case bound, so a cold build
  zero-fills only slots it overwrites. The common standard-window distance
  layout uses a shared constant split table, so a fresh decoder neither fills
  nor allocates it. A unit-distance run that grows the history ring now fills
  the new region with the repeated byte in one pass rather than zeroing it and
  overwriting it. Constructors are inlinable so the returned decoder is built in
  place. Empty and stored-member decodes reach parity with Burli and small
  compressed and repeated streams narrow the gap; text, binary, dictionary and
  large inputs stay ahead of Burli and several times faster than Google C.


- Fix a decoder panic on a legal stream: a built-in dictionary word whose
  transform consumes the whole word decodes to nothing, and RFC 7932 only
  rejects that below distance code 121. As a member's first command it left the
  ring buffer unallocated, and the ring write derived its mask as `len() - 1`
  before noticing it had nothing to store. Found by AFL in `decompress`,
  `decode_dictionary` and `decode_io_limits`; covered by a hand-built wire
  fixture and three committed regression inputs.

- Fuzz the decoder surface on its own schedule: `fuzz/afl/decoder-campaign.sh`
  runs one worker per decoder target in both feature builds, and
  `fuzz/afl/prepare-decoder-seeds.sh` materialises `seeds/decoder` from the
  committed regressions and Google Brotli's compressed fixtures.

- Batch context-free decoder literals three at a time and recognize the common
  three-byte stored header directly, retaining bounded scalar tails and full
  parser fallback. Publish direct median ratios to Burli on the original eight
  equally weighted inputs at q0–q11, including empty and tiny data.

- Refresh all 384 decoder comparison measurements after the owned-output
  optimization, including q0–q11 quality pages, SVG charts, raw CSV and source
  provenance. Preserve the previous CSV and environment as historical evidence.

- Reduce cold decoder allocation and memory traffic: recognize complete stored
  members, transfer fresh non-wrapping history into owned results, and initialize
  raw/repeated growth with final bytes. Window-sized collection falls back to
  ordinary streaming before wrap; retained workspaces and appends keep reuse.
  Preserve limits, dictionary and concatenation behavior. Return speculative
  bytes at output pauses so a pending copy cannot strand a following member.
  Add differential boundary tests and bounded AFL replay of the default owned
  path. See `architecture/decoder-owned-output.md` for Burli comparison evidence.

- Add an isolated four-decoder Criterion comparison for mbrotli, Google C,
  Rust brotli and Burli on identical C streams at q0–q11. Validate all 384
  cases before timing, export timings and shared stream sizes separately,
  and publish decoder overview/quality charts, raw results and run provenance.
  SIMD Brotli is omitted because it shares Rust brotli’s decoder.

- Specialize decoder command loops for the configured CPU backend and use safe
  `fearless_simd` snapshots for 16- and 32-byte history copies. Dispatch stays
  outside command loops; headers and raw blocks share scalar code. Add
  differential fallback/host-SIMD tests at ring boundaries and hotpath anchors
  for command execution, overlapping copies, and Huffman table construction.
  See `architecture/decoder-simd.md` for measured results and limitations.

- Bring decoder throughput to at least 95% of the reference C decoder for
  every quality and benchmark corpus, measured warm against C's create/destroy
  slice shape (see the compatibility specification for the table). The command
  fast path now keeps every hot quantity in a local and writes state back only
  when it pauses: the bit reservoir, an input cursor over the precomputed
  acceptable prefix, the ring position, the remaining meta-block length, the
  recent-distance cache and the resumable command fields. Decoded bytes are
  delivered when the ring wraps or the loop pauses rather than after every
  command, the output space is a running count, and the ring grows once per
  call to what the call can still produce. A padded 1024-entry command table
  yields both length bases, extra widths, the implicit flag and the distance
  context in one masked lookup; a per-meta-block distance symbol table (reused
  when the layout repeats) resolves a distance with one lookup and no `u128`;
  the recent-distance cache is a push-indexed ring so an implicit or code-0
  distance costs one slot write and no branch; and literal, command and
  distance tables plus the literal context slice and its trivial-context flag
  are refreshed at block switches instead of per command. Literal runs are
  bounded once by every loop-invariant limit so the run checks only the
  reservoir, a run that paused on output resumes in the same loop, and short
  copies are 16- or 32-byte word moves. Huffman table entries are padding-free
  words holding absolute second-level offsets; each group keeps its roots and
  its second-level tables in one vector whose length is a grow-only high-water
  mark, so warm meta-blocks write no fill and a cold decode zero-fills once as
  a memory set. The description reader keeps an intrusive linked list per code
  length, so building a table walks only coded symbols, counts canonical codes
  upward and reverses them through a byte table, and reads code lengths,
  context maps and block switches with whole-word refills. Accounted workspace
  buffers grow geometrically, and the `Vec` decoding shapes parse up to the
  first output byte before reserving what the meta-block declares, so large
  outputs get one reservation instead of a chain of copies. No `unsafe` was
  added. The cold shape on payloads of a kilobyte and below remains
  allocation-bound (see the compatibility specification). Fixes a latent
  fast-path bug found by the vendored `zerosukkanooa` fixture during this
  work: a copy paused on a full output window resumed without the
  distance-cache push, and a regression test now covers it.

- Expand public API documentation with runnable decoder, dictionary, parallel
  source, and framing examples. Clarify partial output, final-input retries,
  resource limits, reader read-ahead, writer finalization, and dictionary
  transform boundary behavior; codec behavior and public signatures are unchanged.

- Move the vendored Google Brotli reference from the v1.2.0 tag (`028fb5a`) to
  upstream `master` at `4508218e` (2026-09-01, 153 commits, library version
  still 1.2.0). The bindings gain the `BLOCK_SWITCH` decoder error code, the
  `BROTLI_PARAM_BASE64_MODE`, `BROTLI_PARAM_MAX_BASE64_REGIONS` and
  `BROTLI_PARAM_SIMD_HASHER` encoder parameters with their enums, and
  `SHARED_BROTLI_MAX_RAW_DICT_SIZE`; the test shim follows the new
  `BrotliBuildMetaBlock` signature. Every new parameter defaults to the
  previous behaviour, and the byte-identity, round-trip and AFL regression
  suites pass unchanged against the new reference.

- Rebuild the decompressor benchmark on the compressor benchmark's inputs. The
  corpora move to a shared `benches/support/corpora.rs` module, so both
  benchmarks measure identical bytes: the deterministic text, binary,
  compressible and incompressible inputs plus the six vendored Google Brotli
  test files. Every corpus is encoded by the C one-shot encoder at all twelve
  qualities and validated on both decoders before timing, and the groups
  mirror the compressor's `cold`, `reused`, `presized`, `tiny`, `streaming`
  and `universal` shapes under a `decompress/` prefix, plus `small-chunks`,
  `dictionary` and `metadata` groups for the decoder's own boundary cases.

- Optimize the decoder. Replace the per-bit `u128` reservoir with a whole-word
  refill, canonical per-bit Huffman lookup with two-level lookup tables stored in
  flat per-kind groups, and per-byte regeneration with a power-of-two history
  ring and bulk copies. A command fast path decodes whole commands while a
  whole-word refill and output room allow, and raw blocks, copies, prefix runs
  and metadata skip regenerate in bulk while the byte-exact resumable stages back
  every chunking and limit case. Standard-window distances resolve in `u64`, the
  built-in dictionary stringlet lookup is `O(1)`, and Huffman roots index a
  fixed-size array. Behavior and byte identity are unchanged; no public API or
  `unsafe` was added. Warm decoding is faster than the reference C
  create/destroy shape on copy-heavy, tiny and metadata inputs and reaches
  roughly parity across the vendored test corpus overall; high-entropy text
  decoded at roughly 0.6x of C at that point, a gap the entry above closes.

- Add an AFL C-encoder-to-Rust-decoder round-trip target with one-shot and
  streaming checks, shared encoder seeds, Q0–Q11 regression coverage, and
  base/experimental campaigns. Extend small parameter seed headers to Q5–Q11
  and add arbitrary-byte decoder seeds with panic/progress regression checks.

- Align the crate-level API guide with the README, covering both codecs, memory
  reuse, reader/writer adapters, parallel compression, decoder limits, and features.

- Add independent `compression` and `decompression` features, both enabled by
  default. Disabling either removes its public API and implementation while
  retaining common configuration and dictionary types needed by the other codec.

- Run AFL fuzzing, Miri and AddressSanitizer workflows on every pushed version tag,
  while retaining manual dispatch.
- Add native Brotli decompression: reusable Vec/slice APIs, incremental sessions,
  concatenated members, exact-size validation, explicit resource budgets, and
  retryable synchronous reader/writer adapters.
- Support standard and extended windows, metadata, built-in transforms and RAW
  dictionaries in std and alloc-only profiles. Add decode-only external
  dictionary loading; serialized/custom mappings remain experimental.
- Move private common codec primitives from `compressor::core::shared` to root
  `shared`; share dictionary transform application across both codecs.
- Add independent C and RFC fixtures, decoder fuzz targets, allocation checks,
  benchmarks, feature-gate consumers and heavy history/counter verification.

- Only enable `libm` and `fearless_simd/libm` with `no_std`, removing `libm`
  from ordinary std builds.

- Add the opt-in `no_std` feature for alloc-backed compression, incremental
  sessions, and dictionaries. It disables I/O adapters, parallel compression,
  experimental framing, and profiling instrumentation, and uses compile-time
  SIMD selection and portable logarithms. Disable default features for a fully
  std-free dependency tree; ordinary builds retain their existing behavior.

## [v0.2.0](https://github.com/Mnwa/mbrotli/releases/tag/v0.2.0) - 2026-09-08

- Stop forcing the scalar SIMD fallback into production builds. Keep `Backend`
  public, make `Backend::SCALAR` private to unit tests, and retain the portable
  fallback automatically on targets without supported SIMD.

## [v0.1.5](https://github.com/Mnwa/mbrotli/releases/tag/v0.1.5) - 2026-09-08

- Add a runnable parallel compression example to the crate-level documentation.

## [v0.1.4](https://github.com/Mnwa/mbrotli/releases/tag/v0.1.4) - 2026-09-08

- Add parallel compression examples using scoped threads and Rayon, plus a quick-start example in the README.
- Fix parallel compression documentation examples so they compile and run with default features.

[Full diff](https://github.com/Mnwa/mbrotli/compare/v0.1.3...v0.1.4)

## [v0.1.3](https://github.com/Mnwa/mbrotli/releases/tag/v0.1.3) - 2026-09-08

- Add `BatchConfig::auto` to size in-memory staging for parallel compression automatically while respecting configured memory limits.
- Add a complete staging-memory estimate to help callers choose explicit memory budgets.

[Full diff](https://github.com/Mnwa/mbrotli/compare/v0.1.2...v0.1.3)

## [v0.1.2](https://github.com/Mnwa/mbrotli/releases/tag/v0.1.2) - 2026-09-08

- Improve compression performance for small inputs, high-quality encoding, and repeated use of a compressor while preserving output bytes.
- Add benchmark comparisons with other Brotli encoders and a guide to compatibility, memory-safety, and fuzzing checks.

[Full diff](https://github.com/Mnwa/mbrotli/compare/v0.1.1...v0.1.2)

## [v0.1.1](https://github.com/Mnwa/mbrotli/releases/tag/v0.1.1) - 2026-09-07

- Speed up compression through more efficient matching, SIMD processing, and buffer reuse while preserving output bytes.
- Add automated API compatibility checks and separate fuzzing coverage for stable and experimental features.

[Full diff](https://github.com/Mnwa/mbrotli/compare/v0.1.0...v0.1.1)

## [v0.1.0](https://github.com/Mnwa/mbrotli/releases/tag/v0.1.0) - 2026-09-05

- Initial release: Brotli compression in safe Rust at quality levels 0–11, requiring Rust 1.89 or later.
- Support reusable compressors, streaming readers and writers, and caller-scheduled parallel compression with memory or disk staging.
- Support Large Window Brotli and prepared prefix dictionaries, with serialized dictionaries, stream continuations, and Shared Brotli framing behind the `experimental` feature.
