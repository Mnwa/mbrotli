# Decoder comparison

[Benchmark index](../README.md)

Recorded run: **dec-2026-09-26** (2026-09-26).
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
| [q4](q4.md) | 1.000× | 3.716× | 0.578× | 3.707× | 1.008× |
| [q2](q2.md) | 1.000× | 3.587× | 0.580× | 3.610× | 1.005× |
| [q1](q1.md) | 1.000× | 3.554× | 0.347× | 3.473× | 0.997× |
| [q0](q0.md) | 1.000× | 3.381× | 0.347× | 3.154× | 0.982× |
| [q8](q8.md) | 1.000× | 3.213× | 0.579× | 3.211× | 0.990× |
| [q11](q11.md) | 1.000× | 3.207× | 0.590× | 3.204× | 1.023× |
| [q10](q10.md) | 1.000× | 3.195× | 0.582× | 3.204× | 1.008× |
| [q9](q9.md) | 1.000× | 3.170× | 0.579× | 3.154× | 0.992× |
| [q5](q5.md) | 1.000× | 3.151× | 0.576× | 3.123× | 0.996× |
| [q6](q6.md) | 1.000× | 3.141× | 0.579× | 3.120× | 0.987× |
| [q7](q7.md) | 1.000× | 3.116× | 0.576× | 3.110× | 0.989× |
| [q3](q3.md) | 1.000× | 3.039× | 0.578× | 3.047× | 0.998× |

The last column takes the median of direct Burli time / mbrotli time ratios;
it does not divide the two medians relative to C.

Open a source quality for all datasets, compressed/restored byte counts,
mean latency, confidence bounds and throughput. Measurements include cold construction,
allocation, decoding and disposal. C receives the known output capacity; Rust brotli
includes its native 4 KiB I/O adapter. See the linked methodology for reproducible commands
and the limits of this single-host comparison.
