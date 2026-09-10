# Decoder implementation comparison

[Benchmark index](README.md) · [All decoder quality pages](decoders/README.md)

With the original eight inputs unchanged, the current mbrotli median speed
relative to Burli is **1.013×** across all 96 corpus/quality pairs. The
previously recorded value was **0.967×**. Every per-quality median is now at or
above **1.006×**, so mbrotli matches or leads Burli's median at every source
quality; median speed relative to Google C ranges from **3.1× to 3.7×**. Each
corpus/quality pair has equal weight, including empty and tiny inputs.

The complete run **decoder-workspace-split-2026-09-10** measures all 384 decoder
cases with exactly the previous corpora, q0–q11, generic mode, requested window
22 and cold native APIs. Empty input and tiny text retain equal weight in the
median. Every decoder restored the exact original bytes before timing.

| Source quality | Google C | mbrotli | Rust brotli | Burli | mbrotli / Burli |
| --- | ---: | ---: | ---: | ---: | ---: |
| q0 | 1.000× | 3.362× | 0.367× | 3.175× | 1.042× |
| q1 | 1.000× | 3.542× | 0.363× | 3.143× | 1.120× |
| q2 | 1.000× | 3.483× | 0.581× | 3.069× | 1.080× |
| q3 | 1.000× | 3.115× | 0.584× | 2.799× | 1.006× |
| q4 | 1.000× | 3.710× | 0.611× | 3.181× | 1.176× |
| q5 | 1.000× | 3.254× | 0.591× | 3.102× | 1.015× |
| q6 | 1.000× | 3.251× | 0.611× | 3.104× | 1.006× |
| q7 | 1.000× | 3.181× | 0.579× | 3.009× | 1.032× |
| q8 | 1.000× | 3.189× | 0.583× | 3.095× | 1.011× |
| q9 | 1.000× | 3.156× | 0.607× | 3.115× | 1.013× |
| q10 | 1.000× | 3.236× | 0.604× | 3.225× | 1.006× |
| q11 | 1.000× | 3.131× | 0.582× | 3.137× | 1.020× |

The Google C, mbrotli, Rust brotli and Burli columns are the median across the
eight datasets of C mean time / that decoder's mean time (higher is faster; 1×
matches C). The final column is the median of the per-corpus Burli time /
mbrotli time ratios, which is not the ratio of the two C-relative medians.

Each value takes Burli mean time / mbrotli mean time for each matching corpus,
then the median. For eight values this averages the fourth and fifth sorted
ratios. The overall value uses all 96 ratios; it does not average the twelve
quality medians or divide two medians relative to C. These are separate runs,
not paired trials: differences also include host variation. The controlled
[78-case before/after experiment](decoder-literal-paired.csv) reports code effects.

![Median decoder speed relative to C](decoders/charts/overview.svg)

> The SVG charts and per-quality pages under `decoders/` still render the
> preceding **decoder-synthetic-final-2026-09-10-090040** run; regenerating them
> needs the Matplotlib-equipped benchmarking environment (`decoder_docs.py`).
> The table above and [`decoder-comparison.csv`](decoder-comparison.csv) are the
> current source of truth for this run.

The [CSV](decoder-comparison.csv),
[environment](decoder-comparison-environment.json) and
[quality pages](decoders/README.md) retain every measurement,
including cases where Burli is faster. The preceding run remains in the
[historical CSV](decoder-comparison-before-literal-batching.csv).

## Unchanged measurement contract

The eight inputs are empty, 44-byte tiny text, Alice, cyclic Alice at 1 MiB,
structured binary at 64 KiB, random bytes at 64 KiB and 1 MiB, and repeated
`a` at 1 MiB. The same deterministic generator and C encoder prepare the same
streams. All four decoders use their cold native APIs, including construction,
allocation and disposal. C receives known output capacity; Rust brotli includes
its 4 KiB I/O adapter. Source preparation and validation remain outside timing.
Burli covers every source quality. Both comparison suites use the shared
`core::corpora` generator. The decoder benchmark writes to
`target/criterion-decoders`.

Intel Core i7-13700KF, WSL2, logical CPU 2; Rust 1.98.1; normal release flags;
30 samples, 0.2 s warmup and at least 0.5 s measurement per case. Compilation,
tests and profiles finish before timing. Sampling intervals do not capture
all host scheduling, clock, thermal or allocator variation. No significance
claim is inferred for the across-corpus medians.

## Reproduce

```sh
cargo bench --manifest-path benchmarks/comparison/Cargo.toml --bench decoders --locked -- --test
cargo bench --manifest-path benchmarks/comparison/Cargo.toml --bench decoders --locked -- --save-baseline my-decoders --sample-size 30 --warm-up-time 0.2 --measurement-time 0.5 --noplot
python3 benchmarks/comparison/report.py --decoding --baseline my-decoders --csv /tmp/my-decoders.csv
python3 benchmarks/comparison/decoder_docs.py \
  --csv docs/benchmarks/decoder-comparison.csv \
  --environment docs/benchmarks/decoder-comparison-environment.json \
  --report docs/benchmarks/decoder-comparison.md \
  --output docs/benchmarks/decoders
```

Use a fresh baseline name for another run. This run's raw samples, size manifest,
estimates and logs are archived locally in `benchmarks/comparison/target/run-archives/decoder-synthetic-final-2026-09-10-090040/`.

The recorded timings are reused unchanged. Restoring the benchmark default
changes corpus selection and reporting only; the decoder implementation and
timed adapters are unchanged. The environment record retains the exact command
and hashes from the measured run.

## Publication checks

The restored default passed all 384 Criterion preflight cases; every corpus,
source quality, decoder and input/compressed size matches the published CSV.
Formatting, strict workspace and comparison Clippy, 930 workspace tests and
doctests, comparison tests and 14 Python report tests passed. Instrumented
preflight covered all five compiled functions/closures in the decoder benchmark
module (100% function coverage). All 13 SVGs and 179 local documentation links
were checked. Coverage output remains a local artifact.
