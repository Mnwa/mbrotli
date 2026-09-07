# Benchmarks and profiling

The independent [implementation comparison](../benchmarks/comparison/Cargo.toml)
measures `mbrotli`, Google C Brotli, Rust `brotli`, `simd-brotli`, and `burli`.
See the [recorded results](benchmarks/implementation-comparison.md) for the machine,
commands, throughput, compressed sizes, and limitations.

## New implementation comparison

Run from the repository root:

```sh
git submodule update --init --recursive
cargo bench --manifest-path benchmarks/comparison/Cargo.toml --bench implementations --locked -- --test
cargo bench --manifest-path benchmarks/comparison/Cargo.toml --bench implementations --locked -- --save-baseline my-comparison
python3 benchmarks/comparison/report.py --baseline my-comparison --csv /tmp/my-comparison.csv
```

Use a fresh baseline name for each complete run. The standalone package owns its
lockfile and writes to `benchmarks/comparison/target/criterion/`. Competitor crates
are dependencies only of this unpublished package. Existing harnesses, their
dependency resolution, and root `target/criterion/` results remain independent.

The suite contains **432 cases**: eight corpora, q0–q11 for four implementations,
and q0–q5 for Burli. Unsupported qualities are omitted, never clamped. Every case
uses generic mode, window bits 22, automatic block size, a complete known input,
no custom dictionary, and no explicit flushes. Equal numeric quality describes
each encoder's effort policy; it does not guarantee equal output size.

| Implementation | Version | Timed API |
| --- | --- | --- |
| Google C Brotli | 1.2.0, pinned submodule | `BrotliEncoderCompress` |
| mbrotli | Local checkout | Construct `Compressor`, then `compress` |
| [Rust brotli](https://docs.rs/brotli/9.0.0/brotli/) | 9.0.0 | `BrotliCompress` |
| [simd-brotli](https://docs.rs/simd-brotli/10.0.1/simd_brotli/) | 10.0.1 | `BrotliCompress` |
| [burli](https://github.com/paddor/burli) | 0.3.1 | `compress_with_options` |

This is **cold end-to-end compression**: configuration, construction, scratch and
destination allocation, encoding, and disposal occur inside each iteration.
The two `BrotliCompress` helpers include their native 4 KiB I/O adaptation.
No implementation retains workspace. Native C one-shot output rewrites are
included; validation requires decoded equality, not compressed byte identity.

The corpora are empty input, a 44-byte text, vendored `alice29.txt`, Alice repeated
to 1 MiB, structured 64 KiB binary, deterministic pseudorandom bytes at 64 KiB and
1 MiB, and 1 MiB of repeated `a` bytes. Construction and C-decoder validation of
all 432 outputs finish before timing starts. Missing vendor data fails the build.
`sizes.csv` records the validated output lengths.

Defaults are 20 samples, one second of warmup, and three seconds of measurement
per case. Criterion options override them. A focused timing run is:

```sh
cargo bench --manifest-path benchmarks/comparison/Cargo.toml --bench implementations --locked -- 'alice29/q5/'
```

Filtered runs still validate the full matrix. The exporter rejects incomplete
timing sweeps and checks input lengths. Export immediately after the matching run:
`sizes.csv` describes the most recent preflight. Archive that file with the named
baseline for later analysis. CSV columns include mean nanoseconds and 95%
confidence bounds, MiB/s, `C time / encoder time`, compressed bytes, and
`compressed / input`. For empty input, throughput is zero and the compressed
fraction is undefined; compare latency.

Generate standalone SVG diagrams from a complete exported CSV using Matplotlib:

```sh
python3 -m venv /tmp/brotli-plots
/tmp/brotli-plots/bin/python -m pip install matplotlib==3.10.8
/tmp/brotli-plots/bin/python benchmarks/comparison/plot.py --csv /tmp/my-comparison.csv --output /tmp/my-comparison-charts --subtitle 'My CPU and run settings'
```

The charts show throughput with confidence bands, compressed bytes, and the
speed/size tradeoff across six nontrivial corpora. Empty and tiny inputs remain
in the CSV, where their per-call latency is easier to assess.

## Existing API benchmarks

The original harnesses retain their names, settings, and result locations:

```sh
cargo bench --bench compress --locked
cargo bench --bench parallel --locked
cargo bench --bench track_b --features experimental --locked
```

`compress` covers cold, reused, presized, tiny, streaming, flush, dictionary,
and universal cases against a matched C streaming oracle. `parallel` covers
task counts and source adapters. `track_b` covers experimental formats.
Their contracts differ from this new native one-shot comparison. The existing
per-case 95%-of-C gate and its Python tests remain available:

```sh
cargo bench --bench compress --locked -- --save-baseline candidate
python3 scripts/compare_benchmarks.py --baseline candidate --expected-cases 658 --csv /tmp/candidate.csv
python3 -m unittest discover -s scripts -p 'test_*.py'
```

## Checks and profiling

The standalone package needs its own checks:

```sh
cargo fmt --manifest-path benchmarks/comparison/Cargo.toml --all -- --check
cargo clippy --manifest-path benchmarks/comparison/Cargo.toml --all-targets --locked -- -D warnings
cargo test --manifest-path benchmarks/comparison/Cargo.toml --release --locked
python3 -m unittest discover -s benchmarks/comparison -p 'test_*.py'
cargo llvm-cov --manifest-path benchmarks/comparison/Cargo.toml --release --locked --benches -- --test
```

Keep instrumented profiling separate from timing:

```sh
cargo run --release --features hotpath-cpu --example profile_compressor -- brotli-ffi/vendor/brotli/tests/testdata/alice29.txt
cargo run --release --features hotpath-alloc --example profile_compressor -- brotli-ffi/vendor/brotli/tests/testdata/alice29.txt
```

Record revision, lockfile, compilers, CPU, target flags, affinity, corpus identity,
settings, API mode, sizes, and timing intervals. Generated Criterion and coverage
files are local artifacts. Short runs on shared hosts provide exploratory
evidence, not universal performance claims.
