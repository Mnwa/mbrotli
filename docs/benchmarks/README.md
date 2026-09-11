# Benchmark results

| Compare | Results | Data |
| --- | --- | --- |
| Compression speed and size | [Qualities 0–11](encoders/README.md) | [CSV](encoder-comparison.csv), [conditions](encoder-comparison.md) |
| Decompression speed on identical streams | [Source qualities 0–11](decoders/README.md) | [CSV](decoder-comparison.csv), [conditions](decoder-comparison.md) |

## Compression

![Median compression speed and size relative to C](encoders/charts/overview.svg)

Five libraries, 432 cases. Burli supports encoding at q0–q5; the other encoders
cover q0–q11. Each speed/size bar is the median of eight per-dataset ratios to C.
Higher speed and lower size are better. Equal quality numbers do not imply equal
output size. [Exact results and confidence bounds](encoders/README.md).

## Decompression

![Median decompression speed relative to C](decoders/charts/overview.svg)

Four decoders restore the same C-produced streams in 384 cases. Quality is the
source encoder's setting. Each bar is the median of eight C-time/decoder-time
ratios; higher is faster. Burli decodes every quality. SIMD Brotli re-exports the
Rust brotli decoder project and is omitted. [Exact results](decoders/README.md).

## How to interpret the results

Published measurements use an Intel Core i7-13700KF under WSL2, generic mode,
window 22, and cold native APIs including construction, allocation and disposal.
Compression is dated 2026-09-07; decompression is dated 2026-09-10. See each
report for provenance and limits; these are measurements of recorded builds.

Empty and tiny inputs have equal weight with large inputs. Medians describe this
corpus, not a production workload distribution. C decoding knows the output
capacity; Rust brotli includes its native I/O buffers. The lowest mean is not
necessarily a statistically significant lead.

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

## Reproduce

Use [benchmarking and profiling](../benchmarking.md) for validation, fresh timings
and report-generation commands. Published CSVs contain all measured rows;
quality pages expose the individual cases rather than only aggregate scores.
