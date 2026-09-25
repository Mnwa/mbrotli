# Decompression comparison — 2026-09-25

[Benchmark index](README.md) · [All quality pages](decoders/README.md) ·
[Published CSV](decoder-comparison.csv)

The published 384 rows cover four decoders on eight identical C-produced inputs
at each source quality, recorded as `dec-2026-09-25` with Rust brotli 9.0.0
(decoder `brotli-decompressor` 6.0.1, was 6.0.0) and Burli 0.3.2 (was 0.3.1).
mbrotli's median speed relative to Burli is **1.007×**
across all 96 corpus/quality pairs. Empty and tiny inputs have the same weight as
large inputs. Per-workload results include cases where mbrotli is slower.

| Source quality | Google C | mbrotli | Rust brotli | Burli | mbrotli / Burli |
| --- | ---: | ---: | ---: | ---: | ---: |
| q0 | 1.000× | 3.269× | 0.348× | 3.092× | 1.031× |
| q1 | 1.000× | 3.533× | 0.346× | 3.213× | 1.093× |
| q2 | 1.000× | 3.536× | 0.579× | 3.184× | 1.057× |
| q3 | 1.000× | 3.193× | 0.573× | 2.959× | 1.009× |
| q4 | 1.000× | 3.581× | 0.593× | 3.125× | 1.160× |
| q5 | 1.000× | 3.180× | 0.583× | 3.021× | 1.010× |
| q6 | 1.000× | 3.188× | 0.582× | 3.071× | 1.006× |
| q7 | 1.000× | 3.229× | 0.578× | 3.216× | 1.000× |
| q8 | 1.000× | 3.256× | 0.586× | 3.152× | 1.005× |
| q9 | 1.000× | 3.223× | 0.584× | 3.204× | 1.003× |
| q10 | 1.000× | 3.170× | 0.579× | 3.170× | 1.000× |
| q11 | 1.000× | 3.184× | 0.586× | 3.166× | 1.006× |

Each library column is the median of eight C mean time / decoder mean time
ratios. The last column uses direct Burli time / mbrotli time ratios. The overall
value takes the median of all 96 direct ratios, not an average of quality medians.
Higher is faster; medians do not establish statistical significance. Against
the September 10 publication, compressed inputs are identical and every library's
per-quality medians moved by at most about 5%, within host variation; Burli's
lead narrowed at q7 (mbrotli / Burli 1.032× → 1.000×).

## Measurement contract

Cold native APIs include construction, allocation, decoding and disposal. C gets
the known output capacity; Rust brotli includes 4 KiB I/O adaptation. Throughput
counts restored bytes. The shared input sizes belong to the source streams,
not to a decoder. Every case must restore the original bytes before timing.

The corpus is empty input, tiny text, Alice, cyclic Alice at 1 MiB, structured
binary at 64 KiB, random bytes at 64 KiB and 1 MiB, and repeated `a` at 1 MiB.
Parameters are generic mode and window 22 on an i7-13700KF/WSL2 host. Sampling uses the
harness defaults: 20 samples, 1 s warmup and at least 3 s measurement per case.

## Provenance and limits

The [environment record](decoder-comparison-environment.json) hashes this CSV,
the lockfiles, harness sources and the timed executable, and names the baseline
commit. Decoder sources are unchanged from that commit; the working tree's only
source change is in the greedy encoder. The run's
[size manifest](decoder-comparison-2026-09-25/sizes.csv) is archived. Encoder
documentation was regenerated on other cores while this sweep ran on CPU 2.

One shared host, fixed implementation order and uncontrolled clocks/thermals can
bias results beyond sampling intervals. These data cover cold serial decoding
of standard-window C streams; retained state, streaming, dictionaries and other
producers require separate measurements.

## Reproduce

```sh
cargo bench --manifest-path benchmarks/comparison/Cargo.toml --bench decoders --locked -- --test
cargo bench --manifest-path benchmarks/comparison/Cargo.toml --bench decoders --locked -- --save-baseline my-decoders
python3 benchmarks/comparison/report.py --decoding --baseline my-decoders --csv /tmp/my-decoders.csv
```

Use a fresh baseline name and archive its size manifest, source/binary identity,
commands and environment together. [Benchmarking](../benchmarking.md) covers
report generation and the separate root API benchmarks.
