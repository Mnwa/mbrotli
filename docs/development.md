# Development

Requires Rust 1.89 or later. The workspace includes `google-brotli-ffi`, which
builds vendored C Brotli for tests and benchmarks and requires a C compiler.

```sh
git submodule update --init --recursive
cargo run --example compress
cargo doc --workspace --all-features --no-deps --locked
```

## Workspace checks

Run from the repository root:

```sh
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
```

For faster execution of the compression-heavy suite, the test profile can be
optimized while retaining debug assertions and overflow checks:

```sh
CARGO_PROFILE_TEST_OPT_LEVEL=1 cargo test --workspace --all-features --locked
```

`--all-features` activates `no_std`, which removes std-only adapters and parallel
APIs. Also exercise the std surface and experimental extensions:

```sh
cargo test --workspace --locked
cargo test --workspace --features experimental --locked
```

## Coverage

The repository requires every changed Rust function to be exercised and targets
100% function coverage for repository-owned Rust code.

```sh
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov --version 0.9.0 --locked
cargo llvm-cov clean --workspace
CARGO_INCREMENTAL=1 CARGO_PROFILE_TEST_OPT_LEVEL=1 cargo llvm-cov --workspace --features experimental,diagnostics,hotpath-cpu,hotpath-alloc --locked --no-report
CARGO_INCREMENTAL=1 CARGO_PROFILE_TEST_OPT_LEVEL=1 cargo llvm-cov --workspace --all-features --locked --lib --test no_std --no-report
cargo llvm-cov report --workspace --html --fail-under-functions 100
```

The explicit incremental setting matches CI. Disabling it at optimization
level 1 can lose coverage counters for small public APIs during automatic
cross-crate inlining, even when tests call them. Clean workspace coverage artifacts before the first run; keep the profiles
from both feature configurations for the combined report.

Inspect the report in `target/llvm-cov/html/`; passing tests alone do not show
which functions executed. Coverage reports are local artifacts.

## Fuzzing

The AFL package is excluded from the workspace and has its own checks. After
changes there, or to a public API its targets call, run from `fuzz/afl/`:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --no-default-features -- -D warnings
cargo clippy --all-targets --no-default-features --features experimental -- -D warnings
cargo afl test --no-default-features
cargo afl test --no-default-features --features experimental
```

`cargo afl test` links the AFL runtime and replays committed regressions.
See the [AFL guide](../fuzz/afl/README.md) for installation, corpora, campaigns,
and crash minimization.

## Repository map

| Path | Contents |
| --- | --- |
| `src/` | Public codec APIs and private implementations |
| `tests/` | Public API, compatibility, and integration tests |
| `examples/` | Runnable usage and profiling examples |
| `benches/` | Criterion comparisons with C |
| `benchmarks/comparison/` | Isolated, pinned comparison of five Brotli implementations |
| `brotli-ffi/` | C bindings, build, and test shims |
| `fuzz/afl/` | Isolated fuzz package and regression corpus |
| `docs/` | User and contributor guides |
| `architecture/` | Current subsystem specifications and diagrams |
| `specifications/` | Externally authored source specifications |

Repository rules are in [AGENTS.md](../AGENTS.md). Public APIs own ergonomic
configuration and errors; private `core` modules own algorithms and state.
Changes to these boundaries include corresponding architecture updates.
Vendored upstream files and externally authored specifications are maintained
separately.

Routine CI runs formatting, lint, documentation, packaging, tests, and AFL
regression replay. Separate workflows cover coverage and benchmarks; release tags
also trigger Miri, sanitizers, fuzz campaigns and heavy decoder checks. See [CI mechanics](../architecture/ci.md) and
[benchmark commands](benchmarking.md).

Codec feature isolation is checked with `python3 scripts/check_codec_features.py`.
It compiles independent consumers for all codec combinations in std and no_std,
with and without experimental APIs, including imports that must fail. Run
`python3 scripts/check_decoder_features.py` for prepared/decode-only dictionary
interoperability gates. When disabling defaults, explicitly select each codec
under test alongside `std` or `no_std`.
