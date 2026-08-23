# Repository Guidelines

These instructions apply to all repository-owned code. Treat
`brotli-ffi/vendor/` as upstream source: do not hand-edit it unless the task is
explicitly an upstream-vendor update.

## Architecture and API boundaries

- Keep low-level algorithms and state machines in private submodules named
  `core::*`. For example, `compressor` exposes the public compressor API and
  delegates its implementation to the private `compressor::core` module.
- Follow the same shape for new subsystems: the public module owns ergonomic,
  high-level APIs; its private `core` module owns the implementation details.
- Do not expose `core` modules, implementation types, SIMD types, FFI details,
  or low-level errors from the public API.
- Keep public APIs small and difficult to misuse. Prefer borrowed inputs
  (`&[u8]`, `&str`, and `&T`) when ownership is unnecessary, return owned data
  only when the caller needs it, and use small `Copy` configuration types when
  appropriate.
- Use static dispatch in performance-sensitive code. Use dynamic dispatch only
  when runtime polymorphism is required and its cost is outside the hot path.
- Document every public item. Include `# Errors`, `# Panics`, and `# Safety`
  sections where applicable, plus runnable examples for important APIs.

## Performance and SIMD

- Measure before optimizing. Use `hotpath` on representative release-mode
  workloads to locate CPU and allocation hot paths. Keep instrumentation behind
  the existing `hotpath`, `hotpath-cpu`, and `hotpath-alloc` features.
- Implement confirmed hot paths with `fearless_simd`. Keep a correct baseline
  implementation and select the SIMD level outside inner loops so feature
  detection and dispatch are not repeated per block or element.
- Prefer safe `fearless_simd` abstractions to handwritten architecture-specific
  intrinsics. Any unavoidable `unsafe` code must have a nearby `// SAFETY:`
  comment stating every invariant and tests that exercise boundary cases.
- SIMD and baseline paths must produce identical observable results. Test the
  baseline path explicitly and test every SIMD level available on the current
  host; do not make tests require an instruction set the host lacks.
- Avoid allocations, redundant copies, cloning, bounds checks, and dynamic
  dispatch in hot loops when profiling shows they matter. Do not trade away
  correctness or API clarity without benchmark evidence.
- Run performance measurements with optimized builds and stable, representative
  corpora. Record both speed and compression ratio; a speedup that changes the
  output size or semantics is not an equivalent comparison.

## Errors

- Use `thiserror` for library errors. Fallible library code returns
  `Result<T, E>`; production code must not use `unwrap`, `expect`, or panic for
  recoverable failures.
- Give each low-level subsystem a focused private error type when useful.
  Consolidate those errors into the public high-level error enum with typed
  variants and `#[from]`/`#[source]`, preserving the source chain.
- Public APIs return high-level errors and must not leak private implementation
  errors. Add context at abstraction boundaries; use `#[error(transparent)]`
  only when the higher layer genuinely adds no useful context.
- Keep public error enums `#[non_exhaustive]` when callers may need forward
  compatibility. Test variants, messages that are part of the API contract,
  conversions, and source chains.

## Tests and coverage

- Every new or changed function must be exercised by tests. Maintain 100%
  function coverage for repository-owned Rust code; inspect a coverage report
  rather than assuming a passing test reached the implementation.
- Put focused unit tests beside private `core` logic. Put public API and
  cross-module behavior in integration tests, and use doc tests for public
  examples.
- Give tests behavior-oriented names, cover success, error, empty, boundary,
  and streaming/chunking cases, and keep each test focused on one behavior.
- Add regression tests before fixing a bug. For optimized code, add differential
  tests against the baseline implementation and, where appropriate, Google's C
  Brotli implementation.
- Use `cargo llvm-cov` (or an equivalent Rust coverage tool) to check function
  coverage. Generated coverage reports are local artifacts and should not be
  committed.

## AFL fuzzing

- Add and maintain AFL fuzz targets following the
  [Rust Fuzz Book AFL setup](https://rust-fuzz.github.io/book/afl/setup.html).
  AFL requires a C compiler and `make`; install the runner with
  `cargo install cargo-afl`.
- Keep fuzz targets and seed corpora in a dedicated `fuzz/afl/` tree. Keep the
  fuzz package isolated from normal workspace builds if needed so ordinary
  `cargo test` and `cargo clippy` remain reliable.
- Build targets with `cargo afl build`, then run them with
  `cargo afl fuzz -i <seeds> -o <findings> <target-binary>`.
- Fuzz all byte-consuming and stateful boundaries: one-shot compression,
  streaming readers/writers, parameter parsing, and decompression when it is
  added. Useful oracles include no panic or memory error, round-trip equality,
  streaming/one-shot equivalence, baseline/SIMD equivalence, and compatibility
  with the C implementation.
- Check in small, useful seed corpora. Never commit AFL findings or generated
  output directories. Turn every minimized crash into a deterministic
  regression test before fixing it.

## Benchmarks

- Add Criterion benchmarks for meaningful algorithm and API changes. Benchmarks
  must compare this Rust implementation with the C implementation exposed by
  the `google-brotli-ffi` workspace crate.
- Use identical input bytes, quality/window parameters, API mode, and output
  validation for both implementations. Verify results before timing them.
- Cover representative text, binary, compressible, incompressible, small, and
  large inputs. Measure throughput and record compressed size; include streaming
  benchmarks when changing streaming code.
- Use Criterion throughput metadata and `black_box`, and keep setup, allocation,
  corpus loading, and validation outside the timed region unless the benchmark
  explicitly measures an end-to-end API.
- Treat benchmark results as evidence, not tests. Correctness belongs in the
  test suite, and performance claims must include the command, corpus, target,
  and before/after results.

## Required completion checks

After changing Rust code:

1. Run `cargo fmt --all`, then ensure `cargo fmt --all -- --check` passes.
2. Run `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`.
3. Run `cargo test --workspace --all-features --locked`.
4. Check coverage for every changed function.
5. Run relevant Criterion benchmarks for performance-sensitive changes and the
   relevant AFL target for changes to fuzzed boundaries.

Fix warnings and formatting issues rather than suppressing them. If a Clippy
lint is a demonstrated false positive, use the narrowest possible
`#[expect(clippy::lint_name)]` with a comment explaining why. Do not add a broad
`allow` merely to make checks pass.
