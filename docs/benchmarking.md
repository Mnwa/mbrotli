# Benchmarks and profiling

Criterion benchmarks compare Rust compression with the pinned C implementation
in the `google-brotli-ffi` workspace crate. Each harness validates output before
timing and records compressed sizes alongside throughput.

Initialize the corpus submodule from the repository root:

```sh
git submodule update --init --recursive
```

## Serial APIs

```sh
cargo bench --bench compress --locked
```

| Group | Measurement |
| --- | --- |
| `cold` | Compressor construction and encoding |
| `reused` | Successive operations with retained workspace |
| `presized` | Encoding into a preallocated destination |
| `tiny` | Short inputs and setup costs |
| `streaming` | Reader, writer, and session APIs |
| `flush` | Explicit intermediate flushes |
| `dictionary` | Attached prefix compression |
| `universal` | Empty and incompressible inputs under the serial byte-identity contract |

The serial C oracle uses matching streaming settings. The streaming adapter
normalizes fast-quality block scheduling; its staging costs are charged to C.
Native C one-shot output can differ and is not an interchangeable baseline.

## Parallel and experimental APIs

```sh
cargo bench --bench parallel --locked
cargo bench --bench track_b --features experimental --locked
```

The parallel harness compares task counts, source adapters, and serial C/Rust
compression. Parallel and serial streams have different history policies;
their sizes must be reported separately. The experimental harness covers custom
dictionaries, framing, and metadata. C framing comparisons wrap C-compressed
payloads in an independently constructed container envelope; C has no container
writer API.

Use Criterion's test mode to run validation without collecting timing samples:

```sh
cargo bench --bench compress --locked -- --test
cargo bench --bench parallel --locked -- --test
cargo bench --bench track_b --features experimental --locked -- --test
```

## Profiling

The optional `hotpath` features instrument release builds:

```sh
cargo run --release --features hotpath-cpu --example profile_compressor -- brotli-ffi/vendor/brotli/tests/testdata/alice29.txt
cargo run --release --features hotpath-alloc --example profile_compressor -- brotli-ffi/vendor/brotli/tests/testdata/alice29.txt
```

Keep CPU and allocation profiling separate from throughput measurements.

## Reading results

See the [complete per-case assessment](benchmarks/2026-09-05-per-case.md) for the
95% target and every measured pass/fail. The earlier
[Intel i7-13700KF greedy SIMD comparison](benchmarks/2026-09-05-intel-i7-13700kf.md)
provides a recorded before/after run with C controls, compressed sizes, confidence
intervals, and validation results.

Compare identical input bytes, quality, window, dictionary, declared size, and
flush schedule. Record the command, revision, compiler, target, CPU, corpus,
API mode, compressed sizes, and timing intervals. Include allocation and setup
costs only when the named benchmark measures them.

Criterion reports live under `target/criterion/`. Generated reports and profiles
are local artifacts. Short runs and shared CI hosts provide limited timing
evidence; they do not establish a general speed claim across qualities or
machines.

## Per-case throughput target

The current optimization target is at least 95% of C throughput for **every
individual matched case**, computed as `100 * C time / Rust time`. A geometric
mean does not satisfy that target. Use a unique baseline name for a complete run
so stale Criterion `new` estimates cannot enter the comparison:

```sh
taskset -c 2 cargo bench --bench compress --locked -- --save-baseline candidate
python3 scripts/compare_benchmarks.py --baseline candidate --expected-cases 658 --csv /tmp/candidate.csv
```

The full `compress` harness has 658 Rust/C pairs when all six vendor corpus files
are present. A cold-only sweep has 132 pairs; pass `--group cold/ --expected-cases
132` when checking that subset. The dictionary-free control uses different
settings and is excluded from pairing. The script reports every failing case,
exports both estimates and conservative interval bounds, and exits unsuccessfully
for a missed threshold or an unexpected pair count. Output identity and compressed
size are validated by the harness before timing; timing files alone cannot prove
those properties. C recreates encoder state in the reused and presized comparisons,
whereas Rust retains its workspace, as described above.

The gate's regression checks cover the exact threshold, a slow case hidden by a
fast case, missing pairs, missing C controls, and mismatched input lengths:

```sh
python3 -m unittest discover -s scripts -p 'test_*.py'
```
