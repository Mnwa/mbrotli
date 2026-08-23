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
- Prefer standard library traits over inherent constructors and accessors. Use
  `From`/`Into` for infallible conversions, `TryFrom`/`TryInto` for validated
  ones, `Default` for the canonical value, and `Display`, `FromStr`, `AsRef`,
  `Deref`, and the operator traits where they fit. Add an inherent `new`, `get`,
  or `to_*` method only when no trait expresses the operation, or when a
  `const fn` is genuinely required; expose associated constants instead of
  const constructors for well-known values.
- Use dynamic dispatch at the top level of `core` module and pass chose simd to low level abstractions.
- Document every public item. Include `#Examples`, `# Errors`, `# Panics`, and `# Safety`
  sections where applicable, plus runnable examples for important APIs.
- Do not use ready brotli dependencies.

## Architecture documentation

- Keep the project architecture current: whenever a change adds, removes, or
  reshapes a module, a public API, a state machine, a data flow, or a
  dispatch boundary, update the architecture documentation in `architecture/`
  in the same change. Documentation drift is treated as an incomplete change.
- After the code change lands, write or refresh a specification under
  `architecture/` that describes the core mechanics of the affected subsystem:
  module and ownership boundaries, public API surface, control and data flow,
  state transitions, SIMD dispatch points, error propagation, and the
  invariants each layer relies on.
- Every specification must contain Mermaid diagrams (fenced ```mermaid blocks)
  for the mechanics it describes. Use the diagram type that fits: `graph` for
  module and dependency maps, `classDiagram` for type relationships,
  `sequenceDiagram` for call and data flow, `stateDiagram-v2` for state
  machines and streaming lifecycles, `flowchart` for decision logic.
- Keep `architecture/README.md` as the index: one entry per specification with
  a one-line summary, plus the current high-level module map.
- Describe what the code actually does today. Mark unimplemented paths
  explicitly and keep a short "known gaps" section instead of documenting
  intended behavior as if it already exists.
- `specifications/` holds externally authored source specifications; do not
  rewrite them. `architecture/` holds this repository's own, always-current
  description of the implementation.

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
- Mark functions as const where it's possible.

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
5. Update the affected specifications and diagrams in `architecture/`, and the
   `architecture/README.md` index, so they match the code as changed.
6. Run relevant Criterion benchmarks for performance-sensitive changes and the
   relevant AFL target for changes to fuzzed boundaries.
7. Do not use `#[allow(..)]` to fix clippy warnings.

Fix warnings and formatting issues rather than suppressing them. If a Clippy
lint is a demonstrated false positive, use the narrowest possible
`#[expect(clippy::lint_name)]` with a comment explaining why. Do not add a broad
`allow` merely to make checks pass.
