# Compression comparison — 2026-09-26

[Benchmark index](README.md) · [Results by quality](encoders/README.md)

The [432-case CSV](encoder-comparison.csv) and
[environment record](encoder-comparison-environment.json) describe the working
tree based on `3916af75`, recorded as `enc-2026-09-26b`. The tree's
uncommitted changes are encoder-only: the quick matchers' out-of-line
match-length tail, the inline empty-bucket answer of the shallow dictionary
probe, the ring buffer's append-and-reserve growth and the smaller one-shot
destination reservation at qualities 2-11. Competitors are unchanged: Rust
brotli 9.0.0, simd-brotli 10.0.1 and Burli 0.3.3. The
[size manifest](encoder-comparison-2026-09-26/sizes.csv) is archived with the
run and is identical to that of the earlier September 26 sweep
(`enc-2026-09-26`), which this publication replaces; the environment records
source and executable hashes.

## Final measurements

![Median speed and output relative to C by quality](encoders/charts/overview.svg)

Each quality summarizes eight equally weighted datasets. Speed / C is C mean
latency divided by the encoder's mean latency for the same input; output / C is
encoder bytes divided by C bytes. The summary takes the median of those eight
ratios, including empty and tiny input. These are not median Criterion sample
times. Higher speed and lower output are better.

| Quality | mbrotli median speed / C ↑ | mbrotli median output / C ↓ |
| --- | ---: | ---: |
| 0 | 1.147× | 1.000× |
| 1 | 1.216× | 1.000× |
| 2 | 1.129× | 1.000× |
| 3 | 1.145× | 1.000× |
| 4 | 0.984× | 1.000× |
| 5 | 0.984× | 1.000× |
| 6 | 0.974× | 1.000× |
| 7 | 0.936× | 1.000× |
| 8 | 0.894× | 1.000× |
| 9 | 2.605× | 1.000× |
| 10 | 1.110× | 1.000× |
| 11 | 1.260× | 1.000× |

mbrotli has a lower mean latency than C in 58 of 96 shared cases and the lowest
mean among supported implementations in 49 of 96 cases. These counts compare
point estimates, not statistical significance. In 45 cases, mbrotli's 95% mean
timing interval is wholly below every competitor's interval. The medians also
retain qualities where mbrotli is slower than C.

For Alice at q4, mbrotli measures 91.07 MiB/s against C's 93.01 MiB/s, both
producing 55,504 bytes; Burli measures 93.94 MiB/s with 57,813 bytes. At q5,
mbrotli measures 58.57 MiB/s against C's 63.92 MiB/s, both producing 52,809
bytes. At q11, mbrotli measures 1.451 MiB/s against C's 1.111 MiB/s, both
producing 46,487 bytes; SIMD Brotli measures 1.349 MiB/s with 46,493 bytes.

Against the earlier September 26 sweep, every output size is unchanged. The
encoder changes show where they apply: at q4 the median speed / C rose from
0.955× to 0.984× and at q3 from 1.076× to 1.145×; q4 means fell on Alice
(1653 → 1593 µs), cyclic 1 MiB text (1640 → 1583 µs), random 1 MiB
(1249 → 1160 µs) and repeated 1 MiB (212 → 182 µs). Other qualities moved by
1–5% in both directions (q2 1.184× → 1.129×, q9 2.501× → 2.605×), which is
the size of this host's run-to-run and allocator-state variation: the lower
count of cases faster than C (60 → 58) is within it.

The [quality pages](encoders/README.md) retain all datasets, all implementations,
and exact timings with 95% mean confidence bounds. Burli supports q0–q5 only.
Equal quality numbers describe each encoder's effort policy and do not imply
equal output size.

mbrotli and Google C produce equal output lengths in all 96 shared cases in
this run. Equal lengths alone do not establish compressed byte identity; the
greedy differential tests check byte identity with Google C separately.

## Measurement contract

| Setting | Recorded value |
| --- | --- |
| Host | Intel Core i7-13700KF, x86_64, WSL2 |
| Affinity | Logical CPU 2 |
| Rust | rustc 1.98.1; release defaults; runtime SIMD dispatch |
| C compiler | GCC 11.4.0; vendored release defaults |
| C Brotli | 1.2.0; commit `4508218e7fef90fa4273286f7a415065946f2c43` |
| Rust brotli / simd-brotli / burli | 9.0.0 / 10.0.1 / 0.3.3 |
| Compression | Generic mode, window 22, complete known input, no dictionary or explicit flush |
| API | Cold native serial helpers; construction, allocation, compression, and disposal timed |
| Sampling | 20 samples; 1 s warmup; 3 s requested measurement per case |
| Validation | All 432 outputs decoded to the original input through Google C Brotli before timing |

Inputs are empty, 44-byte text, vendored `alice29.txt` (152,089 bytes), cyclic
Alice text at 1 MiB, structured binary at 64 KiB, deterministic pseudorandom
bytes at 64 KiB and 1 MiB, and 1 MiB of repeated `a` bytes. Corpus construction
and validation occur outside timing. The environment record hashes the corpus
generator, Alice source, lockfiles, and benchmark executable.

No encoder retains workspace between iterations. Rust Brotli and SIMD Brotli
include their native 4 KiB I/O adapters. The C encoder uses its native one-shot
API, including its output rewrites. Round-trip equality is required; compressed
byte identity across implementations is not. Empty input has latency and output
size but no meaningful throughput or compressed/input fraction.

## Reproduction

Run from the repository root; use a fresh baseline name when repeating timings:

```sh
cargo bench --manifest-path benchmarks/comparison/Cargo.toml --bench implementations --locked -- --test
taskset -c 2 cargo bench --manifest-path benchmarks/comparison/Cargo.toml --bench implementations --locked -- --save-baseline enc-2026-09-26b
python3 benchmarks/comparison/report.py --baseline enc-2026-09-26b --csv docs/benchmarks/encoder-comparison.csv
python3 benchmarks/comparison/quality_docs.py --csv docs/benchmarks/encoder-comparison.csv --environment docs/benchmarks/encoder-comparison-environment.json --report docs/benchmarks/encoder-comparison.md --output docs/benchmarks/encoders
python3 benchmarks/comparison/plot.py --csv docs/benchmarks/encoder-comparison.csv --output docs/benchmarks/encoders/charts --subtitle 'i7-13700KF; working tree; 20 samples; 2026-09-26'
```

Plotting uses Matplotlib 3.10.8. Criterion extends slow cases to collect the
requested samples. The exporter rejects incomplete matrices and mismatched
input lengths. Export immediately after timing and archive `sizes.csv` with
the named baseline: a later preflight replaces that shared manifest. This
run's [size manifest](encoder-comparison-2026-09-26/sizes.csv) is checked in.
Raw samples remain local under `benchmarks/comparison/target/criterion/`.
Chart regeneration reuses the CSV; it does not rerun timings.

## Interpretation limits

This is a single sweep on one WSL2 host. Affinity restricts CPU
migration but does not control frequency, thermal state, or host scheduling.
Implementation order is fixed. The lowest mean does not necessarily imply a
statistically significant lead; confidence bounds do not capture every source
of host or allocator variation. No builds, tests, fuzzing or profiling ran alongside
the timed sweep.

The suite measures cold serial compression. Streaming, retained workspace,
parallel tasks, dictionaries, and experimental formats have separate root API
benchmarks and are not represented by these results. See the
[benchmark guide](../benchmarking.md) and [harness specification](../../architecture/benchmark-comparison.md).
