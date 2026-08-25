# Every quality, measured against the reference

The first complete sweep. Earlier reports covered qualities 0 and 1
([`q0_q1_benchmarks.md`](q0_q1_benchmarks.md)) and 3 to 5
([`q3_q5_benchmarks.md`](q3_q5_benchmarks.md)); quality 2 had no encoder and
qualities 6 to 11 had never been measured at all. Both gaps are closed here.

**Ratio is C time divided by this crate's time, so above 1.00 is faster than
the reference.** Compression ratio is not a variable: every one of the 132
one-shot cases produced byte-identical output to Google Brotli configured the
same way, which the benchmark asserts before it times anything.

## How to reproduce

```sh
cargo bench --bench compress -- --sample-size 10 --warm-up-time 1 --measurement-time 3
```

| | |
| --- | --- |
| Machine | Apple M5 Pro, `aarch64-apple-darwin`, Neon |
| Toolchain | rustc 1.97.1 (8bab26f4f 2026-07-14), release profile |
| Reference | Google Brotli v1.2.0, commit `028fb5a`, built by `google-brotli-ffi` |
| Window | `WindowBits::DEFAULT` (22) for every case |
| Date | 2026-08-25 |
| Raw output | [`benchmarks/2026-08-25-apple-m5-pro-full.txt`](benchmarks/2026-08-25-apple-m5-pro-full.txt), and the same run as [CSV](benchmarks/2026-08-25-apple-m5-pro-full.csv) and [JSON](benchmarks/2026-08-25-apple-m5-pro-full.json) |

The `change:` blocks in the raw log compare against whatever Criterion had
saved in `target/criterion` from earlier local runs, which was an arbitrary
mid-development state. Ignore them; only the absolute numbers are evidence.

Qualities 10 and 11 run at ten samples and three seconds of measurement, which
the benchmark applies itself rather than taking from the command line — a
default `cargo bench` would otherwise spend hours on them. Those are the same
numbers every recorded run already passes on the command line, so no case in
this table was sampled differently from any other.

## 1. Geometric means

Over the eleven corpora: five generated (text, binary, compressible,
incompressible, at 1 KiB to 1 MiB) and six from Google Brotli's own test data.

| Quality | one-shot | pre-sized | tiny (16 B – 1 KiB) |
| --- | --: | --: | --: |
| 0 | 0.925 | 0.997 | 0.952 |
| 1 | **1.150** | **1.272** | 0.912 |
| 2 | 0.774 | 0.776 | 0.397 |
| 3 | 0.795 | 0.793 | 0.573 |
| 4 | 0.765 | 0.763 | 0.568 |
| 5 | 0.756 | 0.759 | 0.478 |
| 6 | 0.748 | 0.749 | 0.348 |
| 7 | 0.612 | 0.601 | 0.142 |
| 8 | 0.536 | 0.521 | 0.081 |
| 9 | **0.377** | **0.369** | **0.040** |
| 10 | 0.863 | 0.863 | 0.488 |
| 11 | 0.901 | 0.902 | 0.544 |

Quality 1 is faster than the reference. Qualities 10 and 11 — the two nobody
had measured, and the two that were expected to be worst — are the *closest* of
the slow qualities at 0.86× and 0.90×. The hole is qualities 7 to 9.

## 2. Where quality 9 actually loses

The geometric mean hides the shape of it. Per corpus, one-shot:

| Corpus | q6 | q7 | q8 | q9 |
| --- | --: | --: | --: | --: |
| text-1MiB | 0.939 | 0.811 | 0.800 | 0.791 |
| vendor-alice29.txt | 0.915 | 0.735 | 0.716 | 0.718 |
| vendor-lcet10.txt | 0.898 | 0.731 | 0.741 | 0.730 |
| vendor-plrabn12.txt | 0.899 | 0.744 | 0.723 | 0.727 |
| binary-256KiB | 0.705 | 0.670 | 0.645 | 0.602 |
| vendor-random_org_10k.bin | 0.910 | 0.611 | 0.442 | 0.291 |
| compressible-256KiB | 0.730 | 0.639 | 0.542 | **0.134** |
| vendor-quickfox_repeated | 0.646 | 0.527 | 0.428 | **0.123** |
| text-1KiB | 0.459 | 0.210 | 0.121 | **0.069** |

On a mebibyte of text quality 9 runs at 0.79×, in line with every other greedy
quality. It collapses on inputs that are *small* or *finish early* — a 1 KiB
text, a payload that compresses to fifty-one bytes, a 256 KiB run of zeros that
compresses to nineteen. Those are the cases where the encoder spends almost all
of its time before it looks at a byte.

## 3. The cause: a per-call setup floor

The `tiny` group makes it unambiguous. Absolute times, not ratios:

| Quality | 16 B | 1 KiB | C at 16 B | C at 1 KiB |
| --- | --: | --: | --: | --: |
| 0 | 0.78 µs | 1.72 µs | 0.70 µs | 1.71 µs |
| 5 | 8.1 µs | 15.0 µs | 2.9 µs | 8.6 µs |
| 9 | **145.9 µs** | **146.8 µs** | 2.9 µs | 9.7 µs |

Quality 9 costs the same for sixteen bytes as for a thousand. That is not a
search cost — it is a **constant ≈146 µs of setup**, against the reference's
≈2.9 µs, and it is fifty times the whole job at that size. Quality 9 selects
the largest match finders the crate builds: `H42`'s five hundred and twelve
banks, or `H6` with fifteen bucket bits and two hundred and fifty-six slots
each. Every call allocates and clears them.

This is the same effect [`q3_q5_benchmarks.md`](q3_q5_benchmarks.md) named as
"initialisation the reference skips". It is much larger than that report could
see, because the qualities where it dominates had not been measured.

## 4. It is already fixed, in an API this run also measures

`CompressWorkspace` retains the encoder between calls. From
[`api_benchmarks.md`](api_benchmarks.md):

| Quality | payload | fresh | reused | speed-up |
| --- | --: | --: | --: | --: |
| 9 | 256 B | 0.054 | **0.892** | 16.6× |
| 9 | 4 KiB | 0.084 | **0.915** | 11.0× |
| 9 | 64 KiB | 0.365 | 0.852 | 2.3× |
| 9 | 1 MiB | 0.874 | 0.923 | 1.1× |

A retained workspace takes quality 9 from eighteen times slower than the
reference to within eleven per cent of it. The floor is the whole gap.

## 5. What this says about the throughput gate

The gate in the port specification is a geometric mean of 1.00× per quality.
Against that:

| | |
| --- | --- |
| Met | quality 1 (1.15×) |
| Close | quality 0 (0.93×), quality 11 (0.90×), quality 10 (0.86×) |
| Behind, search-bound | qualities 2 to 6 (0.75× – 0.79×) |
| Behind, setup-bound | qualities 7 to 9 (0.38× – 0.61× one-shot; 0.85× – 0.92× per call once the workspace removes the floor) |

The order of work this suggests:

1. **Make the setup cheap, not just reusable.** The workspace helps a caller
   who repeats; a caller who compresses once still pays 146 µs at quality 9.
   The reference reaches ≈2.9 µs by clearing only what it is about to touch.
   `MatchFinder::prepare` already has that shortcut for short one-shot inputs —
   quality 9's tables are large enough that its `partial_prepare_threshold` is
   rarely reached, so the full wipe runs instead.
2. **Then the search itself**, which is what qualities 2 to 6 are limited by,
   and what the block-splitter allocation described in
   [`q3_q5_benchmarks.md`](q3_q5_benchmarks.md) §5 is part of.

Neither is attempted here. This document is the measurement that says which one
to do first, which is the thing that was missing.

## Known gaps in this measurement

- **One machine, one target.** Every number is `aarch64-apple-darwin` with
  Neon. Nothing here says what the x86-64 backends do.
- **Ten samples.** Enough to separate a two-fold difference from noise, not
  enough to resolve a few per cent. Criterion's intervals are in the raw
  output; a change small enough to need them should be measured with a longer
  run and a saved baseline.
- **One window.** Every case uses `lgwin` 22. The window changes which match
  finder qualities 4 and above select, so a different one would move the
  table — `tests/greedy_qualities.rs` covers the selection, not its speed.
- **No allocation counts.** The setup floor is inferred from a flat time
  against payload size, which is strong evidence but not a profile. The
  `hotpath-alloc` feature exists to confirm it and has not been run here.
