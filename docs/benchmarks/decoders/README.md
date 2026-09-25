# Decoder comparison

[Benchmark index](../README.md)

Recorded run: **dec-2026-09-25** (2026-09-25).
[CSV](../decoder-comparison.csv) · [Environment](../decoder-comparison-environment.json) · [Methodology and limits](../decoder-comparison.md).

![Median decompression speed relative to C](charts/overview.svg)

Each value is the median of C mean time / decoder mean time across all 8 datasets,
equally weighted. Higher is faster; 1× matches C.
Qualities are ordered by mbrotli median speed / C. Medians do not imply statistical significance.

All four decoders restore identical C-generated streams. Source quality is an encoder setting,
not a decoder setting. Burli decodes q0–q11; SIMD Brotli shares Rust brotli's decoder and is omitted.
Compressed sizes and ratios are properties of those shared inputs, so there is no decoder size ranking.

| Source quality | Google C | mbrotli | Rust brotli | Burli | mbrotli / Burli |
| --- | ---: | ---: | ---: | ---: | ---: |
| [q4](q4.md) | 1.000× | 3.581× | 0.593× | 3.125× | 1.160× |
| [q2](q2.md) | 1.000× | 3.536× | 0.579× | 3.184× | 1.057× |
| [q1](q1.md) | 1.000× | 3.533× | 0.346× | 3.213× | 1.093× |
| [q0](q0.md) | 1.000× | 3.269× | 0.348× | 3.092× | 1.031× |
| [q8](q8.md) | 1.000× | 3.256× | 0.586× | 3.152× | 1.005× |
| [q7](q7.md) | 1.000× | 3.229× | 0.578× | 3.216× | 1.000× |
| [q9](q9.md) | 1.000× | 3.223× | 0.584× | 3.204× | 1.003× |
| [q3](q3.md) | 1.000× | 3.193× | 0.573× | 2.959× | 1.009× |
| [q6](q6.md) | 1.000× | 3.188× | 0.582× | 3.071× | 1.006× |
| [q11](q11.md) | 1.000× | 3.184× | 0.586× | 3.166× | 1.006× |
| [q5](q5.md) | 1.000× | 3.180× | 0.583× | 3.021× | 1.010× |
| [q10](q10.md) | 1.000× | 3.170× | 0.579× | 3.170× | 1.000× |

The last column takes the median of direct Burli time / mbrotli time ratios;
it does not divide the two medians relative to C.

Open a source quality for all datasets, compressed/restored byte counts,
mean latency, confidence bounds and throughput. Measurements include cold construction,
allocation, decoding and disposal. C receives the known output capacity; Rust brotli
includes its native 4 KiB I/O adapter. See the linked methodology for reproducible commands
and the limits of this single-host comparison.
