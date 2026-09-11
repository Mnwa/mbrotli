# Decoder comparison

[Benchmark index](../README.md)

Published measurements: **2026-09-10**. See the report for provenance limits.
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
| [q4](q4.md) | 1.000× | 3.710× | 0.611× | 3.181× | 1.176× |
| [q1](q1.md) | 1.000× | 3.542× | 0.363× | 3.143× | 1.120× |
| [q2](q2.md) | 1.000× | 3.483× | 0.581× | 3.069× | 1.080× |
| [q0](q0.md) | 1.000× | 3.362× | 0.367× | 3.175× | 1.042× |
| [q5](q5.md) | 1.000× | 3.254× | 0.591× | 3.102× | 1.015× |
| [q6](q6.md) | 1.000× | 3.251× | 0.611× | 3.104× | 1.006× |
| [q10](q10.md) | 1.000× | 3.236× | 0.604× | 3.225× | 1.006× |
| [q8](q8.md) | 1.000× | 3.189× | 0.583× | 3.095× | 1.011× |
| [q7](q7.md) | 1.000× | 3.181× | 0.579× | 3.009× | 1.032× |
| [q9](q9.md) | 1.000× | 3.156× | 0.607× | 3.115× | 1.013× |
| [q11](q11.md) | 1.000× | 3.131× | 0.582× | 3.137× | 1.020× |
| [q3](q3.md) | 1.000× | 3.115× | 0.584× | 2.799× | 1.006× |

The last column takes the median of direct Burli time / mbrotli time ratios;
it does not divide the two medians relative to C.

Open a source quality for all datasets, compressed/restored byte counts,
mean latency, confidence bounds and throughput. Measurements include cold construction,
allocation, decoding and disposal. C receives the known output capacity; Rust brotli
includes its native 4 KiB I/O adapter. See the linked methodology for reproducible commands
and the limits of this single-host comparison.
