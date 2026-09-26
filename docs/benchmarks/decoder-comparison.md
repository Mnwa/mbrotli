# Decompression comparison — 2026-09-26

[Benchmark index](README.md) · [All quality pages](decoders/README.md) ·
[Published CSV](decoder-comparison.csv)

The published 384 rows cover four decoders on eight identical C-produced inputs
at each source quality, recorded as `dec-2026-09-26` with Rust brotli 9.0.0
(decoder `brotli-decompressor` 6.0.1) and Burli 0.3.3 (was 0.3.2).
mbrotli's median speed relative to Burli is **1.001×**
across all 96 corpus/quality pairs; Burli is faster in 48 of them. Empty and tiny
inputs have the same weight as large inputs. Per-workload results include cases
where mbrotli is slower.

| Source quality | Google C | mbrotli | Rust brotli | Burli | mbrotli / Burli |
| --- | ---: | ---: | ---: | ---: | ---: |
| q0 | 1.000× | 3.381× | 0.347× | 3.154× | 0.982× |
| q1 | 1.000× | 3.554× | 0.347× | 3.473× | 0.997× |
| q2 | 1.000× | 3.587× | 0.580× | 3.610× | 1.005× |
| q3 | 1.000× | 3.039× | 0.578× | 3.047× | 0.998× |
| q4 | 1.000× | 3.716× | 0.578× | 3.707× | 1.008× |
| q5 | 1.000× | 3.151× | 0.576× | 3.123× | 0.996× |
| q6 | 1.000× | 3.141× | 0.579× | 3.120× | 0.987× |
| q7 | 1.000× | 3.116× | 0.576× | 3.110× | 0.989× |
| q8 | 1.000× | 3.213× | 0.579× | 3.211× | 0.990× |
| q9 | 1.000× | 3.170× | 0.579× | 3.154× | 0.992× |
| q10 | 1.000× | 3.195× | 0.582× | 3.204× | 1.008× |
| q11 | 1.000× | 3.207× | 0.590× | 3.204× | 1.023× |

Each library column is the median of eight C mean time / decoder mean time
ratios. The last column uses direct Burli time / mbrotli time ratios. The overall
value takes the median of all 96 direct ratios, not an average of quality medians.
Higher is faster; medians do not establish statistical significance.

Against the September 25 publication, compressed inputs are identical. mbrotli's
decoder changed (unmasked whole-word refill, byte-exact literal tail runs, an
inline code-length table): per corpus its geometric-mean speed rose 7% on Alice
and cyclic Alice, 4.5% on structured binary and 1% on tiny text, and stayed
within host variation elsewhere. Burli 0.3.3 is 12% faster than 0.3.2 by the
same measure, so the direct median moved from 1.007× to 1.001×. By dataset,
mbrotli leads on Alice and cyclic Alice (median 1.19× and 1.23× Burli's speed),
is level on random input, and trails on empty input (0.67×), repeated bytes
(median 0.98×, 0.75× at q1) and structured binary at q5–q9 (0.88–0.91×). Tiny text at
q3 and q5–q9, where a first decode builds context-modelled tables, remains
Burli's (down to 0.72×). This suite times only `decompress`; the one-shot
`decompress_to_slice` change is measured by the root `decompress` benchmark.

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
commit. The working tree adds the uncommitted decoder changes described above
to that commit. The run's
[size manifest](decoder-comparison-2026-09-26/sizes.csv) is archived. Nothing
else ran on the host during the sweep; the encoder sweep ran before it.

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
