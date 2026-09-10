# Decoder implementation comparison — 2026-09-10

[Benchmark index](README.md) · [All source qualities](decoders/README.md)

Google C Brotli, mbrotli, Rust brotli and Burli decode the same 96 C-generated
streams: eight datasets at source qualities q0–q11. All **384 decoder cases**
restore the original payload exactly before any timing starts. SIMD Brotli is
omitted as requested because it uses the Rust `brotli-decompressor` project.
The pinned versions differ: Rust brotli 9.0.0 uses `brotli-decompressor` 6.0.0,
while SIMD Brotli 10.0.1 uses 5.0.3. This run measures only the former; it does
not establish equal decoder performance between those dependency versions.

## Final measurements

![Median decompression speed relative to Google C](decoders/charts/overview.svg)

The [384-case CSV](decoder-comparison.csv) contains mean latency, 95% confidence
bounds on the mean, restored MiB/s, speed relative to C and compressed stream
sizes. The [quality pages](decoders/README.md) retain every dataset, including
empty and tiny input. Burli decodes all source qualities; its encoder's q5
ceiling does not apply here.

Each bar is the median of eight per-dataset ratios, **C mean latency / decoder
mean latency**, with equal dataset weight. Above 1× is faster than C. For eight
values the median averages the fourth and fifth sorted values. This is neither
a median of Criterion samples nor a ratio of pooled times. No confidence bounds
or statistical significance are inferred for these across-dataset medians.
Qualities are ordered by mbrotli median speed / C; dataset panels are ordered by
mbrotli speed relative to the fastest peer. All axes are linear and start at zero.

mbrotli's per-quality median speed / C ranges from **1.185× to 1.779×**.
These medians include the strong API-overhead and repeated-byte cases; individual
datasets can be slower than C. Burli has higher across-dataset medians in this run,
while the text cases show a different ordering. The tables below illustrate both.

On **Alice/q5**, all decoders consume the same **52,809 compressed bytes** and
restore **152,089 bytes**:

| Decoder | Restored MiB/s | 95% mean interval, MiB/s | Mean µs | Speed / C |
| --- | ---: | ---: | ---: | ---: |
| Google C | 499.97 | 493.25–505.98 | 290.11 | 1.000× |
| mbrotli | 530.43 | 527.08–533.69 | 273.44 | 1.061× |
| Rust brotli | 470.50 | 461.27–477.41 | 308.27 | 0.941× |
| Burli | 385.30 | 382.48–388.04 | 376.44 | 0.771× |

Two other q5 examples (each restores 1 MiB):

| Dataset | Compressed bytes | C MiB/s | mbrotli MiB/s | Rust brotli MiB/s | Burli MiB/s |
| --- | ---: | ---: | ---: | ---: | ---: |
| random-1m | 1,048,581 | 14,179.81 | 11,270.72 | 6,828.62 | 47,211.79 |
| repeated-1m | 13 | 942.49 | 15,718.38 | 894.26 | 77,710.77 |

These are recorded means for native cold APIs, not a before/after optimization.
For all source qualities and confidence bounds, use the [complete tables](decoders/README.md).

## Measurement contract

The cold native API contract includes decoder construction, scratch and output
allocation, decoding, output consumption through `black_box`, and disposal.
Corpus generation, C compression and validation happen outside timing. The
exact validated compressed buffers remain alive and immutable during the sweep.
No implementation retains decoder workspace between iterations.

| Decoder | Version | Timed operation |
| --- | --- | --- |
| Google C Brotli | 1.2.0, pinned vendor revision | `BrotliDecoderDecompress` into newly allocated initialized output |
| mbrotli | 0.2.0 at recorded checkout | Construct `Decompressor`, then `decompress` |
| Rust brotli | 9.0.0; decoder 6.0.0 | `BrotliDecompress` into a new Vec; native 4 KiB I/O buffers |
| Burli | 0.3.1 | `burli::decompress` |

C requires caller-supplied output capacity. Its adapter knows the original
length, allocates and zero-initializes that many bytes inside timing (at least
one byte for an empty payload), then truncates to the written length. The Rust
Vec helpers discover output size during decoding. This asymmetry and Rust
brotli's I/O adaptation are native API costs, so the results are not a comparison
of identically allocated decoder kernels. Every implementation selects its
default CPU backend; no harness-side feature dispatch is introduced.

Quality belongs to the **source C encoder**, not the decoder. Streams use generic
mode and requested window 22, with no custom dictionary or flush schedule.
Compression ratios are properties of the shared input streams, so there is no
per-decoder output-size ranking. CSV `input_bytes` means original corpus length,
as in the encoder comparison; `compressed_bytes` means decoder input length.
Throughput counts **restored bytes**. For empty input the CSV records zero
throughput and an undefined compression fraction; tables show latency and “—”.

| Dataset | Restored bytes | Content |
| --- | ---: | --- |
| empty | 0 | Empty payload |
| tiny-text | 44 | Quick-brown-fox sentence |
| alice29 | 152,089 | Vendored Alice in Wonderland |
| text-1m | 1,048,576 | Alice repeated cyclically |
| binary-64k | 65,536 | Structured `((i / 16) XOR i) mod 256` bytes |
| random-64k | 65,536 | Prefix of deterministic xorshift64 corpus |
| random-1m | 1,048,576 | Xorshift64, seed `0x243f6a8885a308d3` |
| repeated-1m | 1,048,576 | Repeated `a` bytes |

## Environment and reproduction

Decoder revision: `f4dba7c47c363286291abf948d6b324b783bbb1a`. Only comparison tooling and
documentation changed in this task; production codec sources are unchanged.
The [environment record](decoder-comparison-environment.json) includes compiler
versions, source/lockfile and binary hashes, commands, run times and archive path.
The old encoder reports retain their original measurements and vendor revision.

| Setting | Value |
| --- | --- |
| Host | Intel Core i7-13700KF, x86_64, WSL2 |
| Affinity | Logical CPU 2 |
| Rust | 1.98.1; release defaults; runtime SIMD dispatch |
| C compiler | GCC 11.4.0; vendored build release defaults |
| C source | `4508218e7fef90fa4273286f7a415065946f2c43` |
| Sampling | 30 samples; 0.2 s warmup; at least 0.5 s measurement per case |
| Plotting | Matplotlib 3.10.8 |
| Baseline | `decoder-comparison-2026-09-10-060318` |

```sh
cargo bench --manifest-path benchmarks/comparison/Cargo.toml --bench decoders --locked -- --test
taskset -c 2 cargo bench --manifest-path benchmarks/comparison/Cargo.toml --bench decoders --locked -- --save-baseline decoder-comparison-2026-09-10-060318 --sample-size 30 --warm-up-time 0.2 --measurement-time 0.5 --noplot
python3 benchmarks/comparison/report.py --decoding --baseline decoder-comparison-2026-09-10-060318 --csv docs/benchmarks/decoder-comparison.csv
python3 benchmarks/comparison/decoder_docs.py \
  --csv docs/benchmarks/decoder-comparison.csv \
  --environment docs/benchmarks/decoder-comparison-environment.json \
  --report docs/benchmarks/decoder-comparison.md \
  --output docs/benchmarks/decoders
```

Use a fresh baseline name for another run. Criterion can extend a measurement
to collect the requested samples. Decoder estimates and raw samples live in the
standalone package's ignored `target/criterion-decoders/`, independent of encoder
and root API benchmarks. Archive its `sizes.csv` immediately alongside the named
baseline: the manifest describes the latest preflight, and sizes alone cannot
detect a mixed revision. This run's manifest, samples and log are archived under
`benchmarks/comparison/target/run-archives/decoder-comparison-2026-09-10-060318/`.
See the [benchmark guide](../benchmarking.md#decoder-implementation-comparison)
and [harness specification](../../architecture/benchmark-comparison.md#decoder-comparison).

## Validation

- Root `cargo fmt --all -- --check` and all-target/all-feature Clippy passed.
- `cargo test --workspace --all-features --locked`: 921 tests and doc tests passed.
- Standalone formatting and all-target/all-feature Clippy passed; five unit tests passed.
- Decoder Criterion test mode restored all 384 cases before timing; encoder test mode retained all 432 cases.
- Thirteen Python tests passed, including complete decoder export, q11 Burli,
  invalid/missing data, shared compressed-size checks, medians and SVG generation.
- LLVM coverage reports 100% function coverage in the new decoder module
  (8/8 functions), and both public comparison drivers (2/2). The `decoders`
  benchmark `main` also executed. Decoder module line coverage is 94.59%.

Coverage command:

```sh
cargo llvm-cov --manifest-path benchmarks/comparison/Cargo.toml --release --locked --benches --no-report -- --test
cargo llvm-cov report --manifest-path benchmarks/comparison/Cargo.toml --release --json --output-path /tmp/decoder-comparison-coverage.json
```

The benchmark entry point was inspected in LLVM's function records because
cargo-llvm-cov omits `benches/` from its default displayed file summary. Coverage
and Criterion artifacts are local and are not committed. No production codec
or fuzzed public boundary changed; the existing AFL targets remain unchanged.

## Interpretation limits

This is an exploratory run on one WSL2 host, not a universal performance claim.
Affinity does not fix frequency, temperature or host scheduling. Decoder order
is fixed. Confidence bounds describe sampling variation, not every source of
host or allocator bias. No tests, compilation or profiling run concurrently with
the timed sweep. Timing intervals in dataset charts are transformed to throughput
by inverting their latency bounds.

Only cold serial decoding of standard-window streams from the pinned C encoder
is measured. This does not establish streaming, retained-workspace, caller-slice,
dictionary, other-encoder-stream or malformed-input performance. The root
`benches/decompress.rs` retains its separate API/backend contracts and results.
The earlier warm decoder measurements and historical encoder runs must not be
pooled with these cold measurements. There is no before/after codec change here.
