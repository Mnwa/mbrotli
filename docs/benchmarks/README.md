# Benchmark results

Browse by **compression quality** to compare Google C Brotli, mbrotli, Rust
Brotli, SIMD Brotli, and Burli on each dataset. Each page has a dataset summary,
eight speed/size charts, and per-implementation tables with timing confidence
bounds, throughput, output bytes, and compression ratio.

## Results by quality

These pages use the latest recorded five-encoder run,
`path-review-final-after` (2026-09-07), from the
[competitor-guided optimization report](competitor-paths.md). Every chart and
table on a quality page comes from that same run. Burli supports q0–q5 only.

| Compression quality | mbrotli path | Implementations | Datasets | Measurements |
| --- | --- | ---: | ---: | ---: |
| [Quality 0](qualities/q0.md) | Fast, one pass | 5 | 8 | 40 |
| [Quality 1](qualities/q1.md) | Fast, two passes | 5 | 8 | 40 |
| [Quality 2](qualities/q2.md) | Greedy | 5 | 8 | 40 |
| [Quality 3](qualities/q3.md) | Greedy | 5 | 8 | 40 |
| [Quality 4](qualities/q4.md) | Greedy | 5 | 8 | 40 |
| [Quality 5](qualities/q5.md) | Greedy | 5 | 8 | 40 |
| [Quality 6](qualities/q6.md) | Greedy | 4 | 8 | 32 |
| [Quality 7](qualities/q7.md) | Greedy | 4 | 8 | 32 |
| [Quality 8](qualities/q8.md) | Greedy | 4 | 8 | 32 |
| [Quality 9](qualities/q9.md) | Greedy | 4 | 8 | 32 |
| [Quality 10](qualities/q10.md) | High quality | 4 | 8 | 32 |
| [Quality 11](qualities/q11.md) | High quality | 4 | 8 | 32 |

## Datasets and measurement contract

| Dataset | Input bytes | Content |
| --- | ---: | --- |
| empty | 0 | API overhead without input |
| tiny-text | 44 | Quick-brown-fox sentence |
| alice29 | 152,089 | Vendored Alice in Wonderland text |
| text-1m | 1,048,576 | Alice repeated cyclically |
| binary-64k | 65,536 | Structured binary bytes |
| random-64k | 65,536 | Prefix of the pseudorandom corpus |
| random-1m | 1,048,576 | Deterministic xorshift64 bytes |
| repeated-1m | 1,048,576 | Repeated `a` bytes |

The recorded host is an Intel Core i7-13700KF under WSL2, with release builds
pinned to logical CPU 2. Each case uses generic mode, window 22, and cold native
APIs, including encoder construction, allocation, compression, and disposal.
There are 30 samples per case, 0.2 seconds warmup, and at least 0.5 seconds
measurement. All 432 outputs passed C-decoder validation before timing.

Equal quality numbers express each implementation's effort policy, not equal
output size. Rust Brotli and SIMD Brotli include their native 4 KiB I/O adapters.
The lowest recorded mean is not necessarily a statistically significant lead;
confidence bounds do not capture every source of host or allocator variation.
See the [tradeoffs and rechecks](competitor-paths.md#final-measurements), including
the tiny-q1 slowdown and inconclusive random-data before/after results.

## Run reports and raw data

| Record | Contents |
| --- | --- |
| [Optimization follow-up](competitor-paths.md) | Source review, targeted changes, matched before/after results, limitations, and charts across qualities |
| [Latest five-encoder CSV](competitor-paths-comparison.csv) | All 432 measurements used by the quality pages |
| [Latest run environment](competitor-paths-environment.json) | Versions, compiler, machine, commands, source hashes, and binary identities |
| [Matched before/after CSV](competitor-paths-before-after.csv) | Separate 96-case mbrotli comparison; not mixed into quality-page measurements |
| [Original comparison report](implementation-comparison.md) | Earlier 20-sample run and its original charts |
| [Original CSV](implementation-comparison.csv) | Preserved earlier measurements |
| [Original environment](implementation-comparison-environment.json) | Provenance for the earlier run |

## Reproduce the documentation

For running benchmarks, profiling, and checks, use the
[benchmark guide](../benchmarking.md). Rebuild the quality pages and SVG charts
from the recorded data with Matplotlib 3.10.8:

```sh
python3 benchmarks/comparison/quality_docs.py \
  --csv docs/benchmarks/competitor-paths-comparison.csv \
  --environment docs/benchmarks/competitor-paths-environment.json \
  --report docs/benchmarks/competitor-paths.md \
  --output docs/benchmarks/qualities
```

The generator validates the complete supported matrix, then groups it by
quality and dataset. It reads the CSV and environment record without rerunning
benchmarks or replacing the historical reports.
