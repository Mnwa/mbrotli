# Changelog

## [Unreleased]

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
