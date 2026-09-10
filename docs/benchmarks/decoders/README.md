# Decoder comparison

[Benchmark index](../README.md)

Recorded run: **decoder-comparison-2026-09-10-060318** (2026-09-10).
[CSV](../decoder-comparison.csv) · [Environment](../decoder-comparison-environment.json) · [Methodology and limits](../decoder-comparison.md).

![Median decompression speed relative to C](charts/overview.svg)

Each value is the median of C mean time / decoder mean time across all eight datasets,
equally weighted, including empty and tiny input. Higher is faster; 1× matches C.
Qualities are ordered by mbrotli median speed / C. Medians do not imply statistical significance.

All four decoders restore identical C-generated streams. Source quality is an encoder setting,
not a decoder setting. Burli decodes q0–q11; SIMD Brotli shares Rust brotli's decoder and is omitted.
Compressed sizes and ratios are properties of those shared inputs, so there is no decoder size ranking.

| Source quality | Google C | mbrotli | Rust brotli | Burli |
| --- | ---: | ---: | ---: | ---: |
| [q11](q11.md) | 1.000× | 1.779× | 0.605× | 2.988× |
| [q2](q2.md) | 1.000× | 1.766× | 0.601× | 3.063× |
| [q4](q4.md) | 1.000× | 1.681× | 0.596× | 3.105× |
| [q10](q10.md) | 1.000× | 1.638× | 0.603× | 3.002× |
| [q3](q3.md) | 1.000× | 1.443× | 0.601× | 2.939× |
| [q5](q5.md) | 1.000× | 1.440× | 0.608× | 3.042× |
| [q6](q6.md) | 1.000× | 1.424× | 0.603× | 2.964× |
| [q9](q9.md) | 1.000× | 1.411× | 0.602× | 3.020× |
| [q7](q7.md) | 1.000× | 1.400× | 0.592× | 2.960× |
| [q8](q8.md) | 1.000× | 1.389× | 0.604× | 3.066× |
| [q1](q1.md) | 1.000× | 1.362× | 0.372× | 3.021× |
| [q0](q0.md) | 1.000× | 1.185× | 0.369× | 2.998× |

Open a source quality for all eight datasets, compressed/restored byte counts,
mean latency, confidence bounds and throughput. Measurements include cold construction,
allocation, decoding and disposal. C receives the known output capacity; Rust brotli
includes its native 4 KiB I/O adapter. See the linked methodology for reproducible commands
and the limits of this single-host comparison.
