# CI commands

Everything below runs from the repository root. The vendored Brotli submodule
is required by the dev-dependency, so a clean checkout starts with:

```sh
git submodule update --init --recursive
```

## Required checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
```

The test profile enables `fearless_simd/force_support_fallback` through a
dev-dependency, so the scalar backend is part of every test run without being
part of the distributable build.

## Backend matrix

`tests/simd_backends.rs` already compares the scalar fallback against every
backend the host supports, by downgrading the detected token through the
`Level::as_*` accessors. On an AVX2 machine that covers scalar, SSE2, SSE4.2
and AVX2 in one run; on AArch64 it covers scalar and NEON.

To pin a specific backend for a whole run, disable the ones above it:

```sh
# x86-64: force the SSE2 baseline
RUSTFLAGS='--cfg fearless_simd_disable_dispatch_avx512 \
           --cfg fearless_simd_disable_dispatch_avx2 \
           --cfg fearless_simd_disable_dispatch_sse4_2' \
  cargo test --workspace --all-features --locked

# x86-64: force AVX2 as the ceiling
RUSTFLAGS='--cfg fearless_simd_disable_dispatch_avx512' \
  cargo test --workspace --all-features --locked
```

Cross-compilation checks, for the targets the port claims to support:

```sh
cargo check --target x86_64-unknown-linux-gnu --all-features --locked
cargo check --target aarch64-unknown-linux-gnu --all-features --locked
cargo check --target wasm32-unknown-unknown --lib --locked
```

## Profiling

The encoder carries `#[hotpath::measure]` anchors on the fragment entry points,
the two scans, the prefix-code builders and the histogram accumulator. The
attribute compiles to nothing unless one of the `hotpath` features is on, so
the distributable build is unaffected:

```sh
cargo bench --bench compress --features hotpath-cpu
cargo bench --bench compress --features hotpath-alloc
```

Anchors are deliberately placed outside the innermost loops: a timer inside the
match scan would dominate what it measures.

## Coverage

```sh
cargo llvm-cov --package mbrotli --all-features --summary-only
```

The gate is 100% function coverage for repository-owned Rust code. Reports are
local artifacts and are not committed.

## Benchmarks

```sh
cargo bench --bench compress
```

Each case validates both encoders against the C decoder, and asserts byte
identity between this crate and the pinned C encoder, before anything is timed.

## Fuzzing

```sh
cargo install cargo-afl
cargo afl system-config          # shared memory and crash reporting, needs sudo
fuzz/afl/prepare-seeds.sh
cd fuzz/afl && cargo afl build --release
cargo afl fuzz -V 300 -i seeds/params -o findings/differential \
    target/release/differential_c
```

A CI smoke run can skip AFL entirely and just replay the seed corpus through
each target, which is what a build-only check does:

```sh
cd fuzz/afl && cargo afl build --release
for target in target/release/*_roundtrip target/release/differential_c \
              target/release/simd_equivalence target/release/streaming_equivalence \
              target/release/output_capacity; do
    for seed in seeds/params/*; do "$target" < "$seed" > /dev/null || exit 1; done
done
```

## Not available on this toolchain

- **Miri** requires a nightly toolchain (`rustup +nightly component add miri`).
  It is worth running when one is available, though `src/` contains no `unsafe`
  code, so there is no unsound construct for it to find.
- **Sanitizers** (`-Zsanitizer=address`) likewise require nightly. The C side
  can be built with ASan/UBSan independently through the `google-brotli-ffi`
  build script's `CFLAGS`.
