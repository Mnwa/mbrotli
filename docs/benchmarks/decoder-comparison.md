# Decompression comparison — 2026-09-10

[Benchmark index](README.md) · [All quality pages](decoders/README.md) ·
[Published CSV](decoder-comparison.csv)

The published 384 rows cover four decoders on eight identical C-produced inputs
at each source quality. mbrotli's median speed relative to Burli is **1.013×**
across all 96 corpus/quality pairs. Empty and tiny inputs have the same weight as
large inputs. Per-workload results include cases where mbrotli is slower.

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

Each library column is the median of eight C mean time / decoder mean time
ratios. The last column uses direct Burli time / mbrotli time ratios. The overall
value takes the median of all 96 direct ratios, not an average of quality medians.
Higher is faster; medians do not establish statistical significance.

## Measurement contract

Cold native APIs include construction, allocation, decoding and disposal. C gets
the known output capacity; Rust brotli includes 4 KiB I/O adaptation. Throughput
counts restored bytes. The shared input sizes belong to the source streams,
not to a decoder. Every case must restore the original bytes before timing.

The corpus is empty input, tiny text, Alice, cyclic Alice at 1 MiB, structured
binary at 64 KiB, random bytes at 64 KiB and 1 MiB, and repeated `a` at 1 MiB.
Parameters are generic mode and window 22 on an i7-13700KF/WSL2 host. The reported
sampling settings are 30 samples, 0.2 s warmup and at least 0.5 s measurement.

## Provenance and limits

The CSV, quality tables and charts agree. The checked-in
[environment record](decoder-comparison-environment.json) has a different CSV hash
and summary, so it cannot authenticate the source revision, binary or exact run
identity of these published timings. Treat the results as exploratory; a fresh
run with matching provenance is needed for a reproducible performance claim.

One shared host, fixed implementation order and uncontrolled clocks/thermals can
bias results beyond sampling intervals. These data cover cold serial decoding
of standard-window C streams; retained state, streaming, dictionaries and other
producers require separate measurements.

## Reproduce

```sh
cargo bench --manifest-path benchmarks/comparison/Cargo.toml --bench decoders --locked -- --test
cargo bench --manifest-path benchmarks/comparison/Cargo.toml --bench decoders --locked -- --save-baseline my-decoders --sample-size 30 --warm-up-time 0.2 --measurement-time 0.5 --noplot
python3 benchmarks/comparison/report.py --decoding --baseline my-decoders --csv /tmp/my-decoders.csv
```

Use a fresh baseline name and archive its size manifest, source/binary identity,
commands and environment together. [Benchmarking](../benchmarking.md) covers
report generation and the separate root API benchmarks.
