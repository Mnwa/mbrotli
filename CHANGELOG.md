# Changelog

## [Unreleased]

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
  decodes at roughly 0.6x of C, where safe bounds-checked table lookups cannot
  match an unchecked-pointer decoder.

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
