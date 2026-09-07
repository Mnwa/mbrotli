# Greedy matcher runs: register pressure pass

## Decision

**The 95%-of-C target is not met per case.** Counting the scratch-harness
sweep for qualities 0–6, 8, 10 and 11 and Criterion for qualities 7 and 9
(see "Harness caveat"), the final executable passes **332 of 613** (harness: 249 of 498 for the other qualities; Criterion: 83 of 115 for qualities 7 and 9)
paired Rust/C comparisons, against 283 of 658 in the previous
[Criterion record](2026-09-06-per-case.md), which measured the code this
pass started from. Every change keeps the output byte-identical: the harness
and the benchmark validate the compressed bytes of each case against the C
encoder before timing it; the full test suite, the differential randomized
tests and both AFL regression configurations pass. All source changes are
kept.

Files of this record:

- [`2026-09-06-greedy-runs.csv`](2026-09-06-greedy-runs.csv): every
  comparison of the final harness sweep (mean C and Rust nanoseconds,
  percentage, compressed size, pass/fail). Its quality 7 and 9 rows are
  superseded by the Criterion CSV.
- [`2026-09-06-greedy-runs-criterion.csv`](2026-09-06-greedy-runs-criterion.csv):
  the Criterion runs of this record (reused group for qualities 0, 2, 5, 7,
  8, 11; streaming quality 1; every quality 7 and quality 9 case), with the
  percentage of the previous record beside each case.
- [`2026-09-06-greedy-runs-harness.rs.txt`](2026-09-06-greedy-runs-harness.rs.txt):
  the scratch harness source.

## Contract

- Work: the 658 Rust-versus-C cases of `benches/compress.rs` (cold, reused,
  preallocated, tiny, streaming, flush, dictionary, universal; qualities 0–11;
  synthetic text/binary/compressible/incompressible corpora and the vendored
  Brotli test files). The harness mirrors 608 of them; see "Coverage of the
  sweep".
- Correctness: output bytes identical to the C encoder for every quality and
  API shape; no `unsafe` (the crate forbids it); baseline and SIMD paths
  identical.
- Target: each case at or above 95% of the C throughput
  (`100 * C time / Rust time`).
- Resources: no new dependencies, no new allocation in the search loops, code
  size growth accepted where it bought speed (const-generic matcher shapes).

## Method

The harness is a scratch cargo project depending on `mbrotli` and
`google-brotli-ffi` by path. It reproduces the benchmark bodies (same corpora
generators, `LGWIN`, stream chunking, size hint, output validation) and times
both sides with the same loop: `time <rounds> <ms> <case>...` runs each case's
C and Rust bodies alternately for `ms` milliseconds per round and reports the
mean nanoseconds per call and the C/Rust percentage. Everything ran pinned to
one performance core:

```sh
# full sweep (script saved as sweep.sh in the session scratchpad)
taskset -c 2 harness time 1 120 cold/q5/text-1KiB cold/q5/text-1MiB ...   # q0–q9
taskset -c 2 harness time 1 360 cold/q10/text-1KiB ...                     # q10, q11
# spot re-measurements
taskset -c 2 harness time 3 300 cold/q6/incompressible-256KiB ...
```

The sweep is one round of 120 ms per side per case (360 ms for qualities 10
and 11), so a single case is a point estimate with roughly ±3% noise on the
big corpora and more on the microsecond cases. Two sweeps were taken with the
same harness build settings: a mid-pass sweep after the const-generic matcher
shapes and the run visitor had landed but before the register-pressure,
cold-path and `fast_log2` steps, and the final sweep on the final source. Callgrind (valgrind 3.24 built from source) and
`perf record -e cpu-clock:u` supplied the diagnosis; neither is part of the
timing.

Profiling commands and the diagnosis path are summarized in the
[greedy encoder specification](../../architecture/greedy-encoder.md), section
"Known gaps".

## Diagnosis

Callgrind showed the quality 5–8 bucket loop executing about as many
instructions as the C build but with 1.6× the data reads: a 28 KB inlined
search loop with runtime depth and layout spilled its invariants around every
candidate. Register pressure, not instruction count, was the lever. Cold and
tiny cases were additionally dominated by allocation and zeroing (dense
tables stored as `Vec<[u32; N]>` with `N > 16` are zero-filled element-wise,
not `calloc`'d), and the entropy loop spilled its accumulator around the
out-of-line `log2` fallback.

## Changes kept

- `BucketMatcher<HASH64, BUCKETS, BLOCK>`: depth and table size are const
  generics (ten `MatchFinder` variants); dense tables are `[[u32; BLOCK];
  BUCKETS]` array references, so the probe loop unrolls with no length or
  bounds checks.
- `RunVisitor`: the greedy search loop is compiled once per concrete run type
  (dense, on-demand compact, on-demand sparse, quick table, quick map) instead
  of branching on layout per position.
- Only the filter (distance check plus 4-byte compare) stays inline; measuring
  and scoring live in `#[inline(never)]` helpers returning by value, and the
  running result is written once per search.
- `move` on the `vectorize` closures so captured state is not re-loaded
  through pointers inside the outlined feature functions.
- Lazy `current_window` and `MatchQuery::dictionary_start`.
- Flat dense storage, dense limit of table/16 for tagged shapes, one index
  lookup per position on the on-demand path, reserved starter pools for
  compact streams.
- `fast_log2` libm fallback split into a `#[cold] #[inline(never)]` function.
- Greedy loops compiled only for SSE2 plus scalar (`Selected<S, G>` in
  `core::dispatch`); they only need a byte-compare mask.
- Shared `match_len_at` / `match_len_windows` used by the fast q0/q1 encoders
  and the greedy matchers.

## Results (harness sweep)

Percentages are `100 * C mean time / Rust mean time` for identical work. The
quality 7 and 9 rows of these tables are the harness's and are superseded by
the Criterion section for the reasons given in the harness caveat; the
"final" pass counts below therefore overstate those two qualities.

| Quality | Passing / measured (final) | Median % of C (final) | Weakest case (final) | Passing (mid-pass) | Median (mid-pass) | Passing (Criterion, 2026-09-06 record) | Median (Criterion) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 0 | 22 / 41 | 98.8 | 78.0 (reused/q0/binary-256KiB) | 29 / 41 | 100.2 | 28 / 43 | 99.9 |
| 1 | 28 / 45 | 97.6 | 87.4 (presized/q1/incompressible-256KiB) | 30 / 45 | 97.9 | 47 / 74 | 98.0 |
| 2 | 41 / 69 | 95.5 | 63.1 (tiny/q2/1024) | 12 / 69 | 87.5 | 23 / 72 | 84.7 |
| 3 | 26 / 41 | 96.4 | 76.2 (tiny/q3/64) | 9 / 41 | 90.7 | 12 / 41 | 85.7 |
| 4 | 27 / 41 | 97.2 | 79.5 (tiny/q4/1024) | 7 / 41 | 87.9 | 9 / 41 | 83.7 |
| 5 | 23 / 69 | 88.2 | 65.6 (reader/q5/incompressible-256KiB) | 18 / 69 | 88.0 | 15 / 75 | 76.5 |
| 6 | 14 / 41 | 88.4 | 62.3 (cold/q6/incompressible-256KiB) | 12 / 41 | 87.9 | 10 / 41 | 78.3 |
| 7 | 30 / 41 | 106.5 | 71.8 (tiny/q7/1024) | 28 / 41 | 108.0 | 10 / 41 | 74.4 |
| 8 | 9 / 41 | 91.0 | 71.5 (cold/q8/text-1KiB) | 11 / 41 | 88.7 | 6 / 41 | 77.4 |
| 9 | 68 / 69 | 234.9 | 91.9 (cold/q9/compressible-256KiB) | 69 / 69 | 241.2 | 73 / 73 | 223.4 |
| 10 | 10 / 41 | 90.0 | 69.5 (cold/q10/vendor-quickfox_repeated) | 9 / 41 | 91.0 | 6 / 41 | 89.6 |
| 11 | 49 / 69 | 96.9 | 72.7 (cold/q11/vendor-quickfox_repeated) | 52 / 69 | 99.1 | 44 / 75 | 95.6 |

| API group | Passing / measured (final) | Median % of C (final) | Passing (mid-pass) | Median (mid-pass) |
| --- | ---: | ---: | ---: | ---: |
| Cold one-shot | 66 / 132 | 94.8 | 59 / 132 | 92.7 |
| Reused compressor | 80 / 132 | 97.4 | 67 / 132 | 95.7 |
| Preallocated output | 78 / 132 | 96.7 | 66 / 132 | 95.5 |
| Tiny, cold | 6 / 48 | 87.0 | 6 / 48 | 82.3 |
| Tiny, reused | 38 / 48 | 99.5 | 23 / 48 | 93.9 |
| Streaming writer | 23 / 32 | 98.3 | 19 / 32 | 99.2 |
| Streaming reader | 20 / 32 | 96.8 | 18 / 32 | 97.6 |
| Streaming session | 22 / 32 | 100.2 | 17 / 32 | 97.2 |
| Flush | 14 / 20 | 101.4 | 11 / 20 | 102.2 |

Median percentage per quality and corpus over the cold, reused and
preallocated groups, mid-pass → final:

| Quality | text-1KiB | text-1MiB | binary | compressible | incompressible | alice29 | lcet10 | plrabn12 | mapsdatazrh | random | quickfox |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 0 | 103 → 100 | 108 → 110 | 78 → 80 | 100 → 90 | 79 → 82 | 99 → 102 | 102 → 101 | 102 → 103 | 102 → 107 | 84 → 83 | 97 → 94 |
| 1 | 98 → 98 | 178 → 178 | 96 → 106 | 122 → 99 | 89 → 91 | 92 → 89 | 98 → 94 | 94 → 93 | 98 → 94 | 137 → 141 | 112 → 110 |
| 2 | 71 → 90 | 87 → 98 | 81 → 96 | 744 → 786 | 91 → 115 | 87 → 94 | 88 → 96 | 89 → 95 | 87 → 95 | 100 → 105 | 690 → 699 |
| 3 | 91 → 96 | 89 → 92 | 91 → 94 | 645 → 649 | 79 → 95 | 90 → 98 | 92 → 97 | 92 → 99 | 88 → 96 | 96 → 108 | 643 → 648 |
| 4 | 90 → 94 | 88 → 95 | 88 → 102 | 389 → 429 | 79 → 93 | 87 → 98 | 87 → 98 | 91 → 98 | 88 → 103 | 72 → 89 | 300 → 328 |
| 5 | 77 → 79 | 95 → 99 | 81 → 84 | 354 → 364 | 118 → 106 | 89 → 84 | 84 → 86 | 84 → 86 | 89 → 92 | 81 → 90 | 337 → 346 |
| 6 | 75 → 77 | 135 → 99 | 79 → 85 | 358 → 364 | 116 → 100 | 90 → 88 | 86 → 86 | 92 → 87 | 91 → 94 | 84 → 88 | 328 → 345 |
| 7 | 75 → 77 | 136 → 134 | 107 → 106 | 446 → 449 | 259 → 276 | 131 → 123 | 105 → 103 | 102 → 104 | 115 → 114 | 82 → 87 | 604 → 609 |
| 8 | 78 → 80 | 106 → 94 | 85 → 88 | 305 → 307 | 87 → 82 | 90 → 91 | 93 → 94 | 94 → 94 | 88 → 90 | 88 → 94 | 329 → 340 |
| 9 | 857 → 870 | 280 → 320 | 153 → 155 | 126 → 128 | 902 → 906 | 236 → 236 | 151 → 148 | 130 → 132 | 186 → 191 | 3046 → 3319 | 280 → 291 |
| 10 | 97 → 99 | 88 → 88 | 86 → 87 | 89 → 87 | 90 → 90 | 93 → 92 | 94 → 94 | 93 → 94 | 94 → 95 | 88 → 88 | 76 → 76 |
| 11 | 103 → 100 | 105 → 102 | 91 → 92 | 112 → 110 | 96 → 94 | 99 → 95 | 101 → 95 | 98 → 97 | 100 → 98 | 93 → 94 | 78 → 78 |


## Spot re-measurements

Between the two sweeps, 34 cases dropped by more than eight points. They were
re-measured with three rounds of 300 ms per side; the min/max columns are the
per-round extremes.

| Case | C, µs | Rust, µs | % of C | Round min | Round max |
| --- | ---: | ---: | ---: | ---: | ---: |
| cold/q6/incompressible-256KiB | 419.8 | 423.4 | 99.2 | 96.4 | 127.0 |
| reused/q6/incompressible-256KiB | 423.9 | 308.7 | 137.3 | 121.1 | 137.3 |
| presized/q8/incompressible-256KiB | 622.8 | 760.4 | 81.9 | 79.5 | 81.9 |
| reused/q5/incompressible-256KiB | 301.7 | 375.5 | 80.3 | 80.3 | 116.8 |
| reused/q1/compressible-256KiB | 18.8 | 14.1 | 133.4 | 133.4 | 134.1 |
| cold/q1/compressible-256KiB | 19.0 | 14.2 | 134.0 | 134.0 | 147.3 |
| cold/q6/text-1MiB | 2950.3 | 3228.3 | 91.4 | 91.4 | 94.1 |
| presized/q8/text-1MiB | 2881.0 | 3025.5 | 95.2 | 95.2 | 98.0 |
| reader/q5/incompressible-256KiB | 349.5 | 337.9 | 103.4 | 90.8 | 103.4 |

The incompressible q5/q6 cases swing by 30 points between rounds because the
C encoder's time on them depends on its allocator state (its time moved from
300 to 420 µs across runs while Rust stayed within 10%). Those sweep entries
are drift, not regressions of this pass. The q8 incompressible and q6 text
cases are stable and genuinely below target.

## Regressions found during the pass and reverted

- Outlining the whole `find_longest_match`: −5 to −15% on q2–q8 (call cost
  with a large by-reference query). Reverted.
- A `get(offset..).and_then(first_chunk)` word loader: q0/q1 binary fell from
  79% to 61% in the real loop. The `offset & u32::MAX` plus `get(o..o+8)`
  form is kept.
- A word-only match-length scan: compressible and text-1MiB corpora regressed;
  the native-vector loop in `match_len_windows` is restored with the SIMD
  token threaded through the accept helpers.

## Remaining gaps

Of the 281 failing cases: 102 are bucket-matcher cases at
qualities 5–8 (binary 79–88%, text 84–94%, quality 7 the weakest at 66–87%);
72 are inputs of at most 1 KiB (tiny and text-1KiB groups,
63–87%), which pay for allocating and zeroing scratch and for the compact
map's roughly ninety instructions per stored position; 43 are
q10/q11 (quickfox_repeated 72–80%, otherwise 87–95%); 28 are q0/q1
(binary and incompressible at 47–91%, the streaming q1 incompressible cases
unchanged from before); the remaining 36 are q2–q4 and q9 cases in
the 90–95% band. The next levers, in order of expected effect, are the
remaining outer-loop spills in the bucket loop (quality 7's sixty-four-deep
buckets most of all), per-call allocation and zeroing on the cold and
high-quality paths, and `GreedyParams` equality including `size_hint` (a
reused compressor rebuilds its encoder whenever the input length changes).

## Coverage of the sweep

The harness mirrors 608 of the 658 Criterion cases. Not mirrored: the three
dictionary cases, the eight universal cases, one extra streaming corpus per
streaming quality (twelve cases), and the 27 streaming q1 cases, because the
harness's C streaming wrapper lacks the benchmark's window-sized staging for
qualities below 2 and rejects the size mismatch. Criterion covers those.

## Harness caveat: quality 7 and 9 C times depend on the allocator

Cross-checking the harness against Criterion exposed a measurement error in
the harness at quality 7. Rust times agree between the two tools within
noise, but the harness's C times at quality 7 are 1.2–4× longer than
Criterion's (incompressible 256 KiB: 2.0 ms against 0.51 ms). `perf stat`
shows about 2,200 minor page faults per C call in the harness process and
`strace` five `brk` calls per call: glibc trims the reference's 8 MB
per-call hasher after every free in that process, and the next call faults
it back in. With trimming disabled the harness reproduces Criterion:

```sh
MALLOC_TRIM_THRESHOLD_=1000000000 MALLOC_TOP_PAD_=67108864 MALLOC_MMAP_THRESHOLD_=1000000000 \
  taskset -c 4 harness time 2 300 reused/q7/incompressible-256KiB
# C 518 µs, Rust 738 µs, 70.2% (Criterion: 511 µs, 751 µs, 68.0%)
```

A full sweep under those variables changed the C-side medians of every
quality but 7 and 9 by at most 2% (Rust medians by at most 2% everywhere),
so the harness figures for the other qualities stand. Quality 7 falls from
30 to 8 passing cases of 41 (median 85.3%). Quality 9 falls from 68 to 33 of
69 (median 93.7%): the reference allocates a 32 MB table per call at that
quality, which glibc always maps fresh, so it pays the faults in every
process, including Criterion's, while this crate sizes its table from the
input. The Criterion numbers below are the authoritative ones for both
qualities; the harness CSV rows for them are kept for the Rust times only.

The trimming-free sweep also showed a Rust cost that the default allocator
hides: with all allocations served from the heap, the quality 10 and 11
one-shot calls on 16-byte inputs took 1.2 ms against 0.15 ms for C, because
the high-quality encoder allocates and zeroes its full tables regardless of
input size. That is an existing gap, not a change of this pass.

## Criterion

`taskset -c 2 cargo bench --bench compress -- '<filter>'`, filters
`reused/q(0|2|5|7|8|11)/`, `streaming/q1/`, `/q7/` and `/q9/`, on the final
source; Criterion's own settings (ten samples, one second warm-up, three
seconds of measurement, longer at qualities 10 and 11). "Before" columns are
the previous record.

### Reused group, qualities 0, 2, 5, 7, 8, 11

| Quality | Passing / measured | Median % of C | Weakest corpus | Passing in the 2026-09-06 Criterion record | Median then |
| --- | ---: | ---: | ---: | ---: | ---: |
| 0 | 6 / 11 | 96.1 | 78.3 (incompressible-256KiB) | 8 / 11 | 100.2 |
| 2 | 6 / 11 | 102.1 | 89.8 (binary-256KiB) | 4 / 11 | 82.5 |
| 5 | 4 / 11 | 89.7 | 78.3 (text-1KiB) | 3 / 11 | 76.8 |
| 7 | 2 / 11 | 84.5 | 68.0 (incompressible-256KiB) | 2 / 11 | 73.8 |
| 8 | 2 / 11 | 90.6 | 81.6 (text-1KiB) | 2 / 11 | 80.8 |
| 11 | 6 / 11 | 95.7 | 80.1 (vendor-quickfox_repeated) | 6 / 11 | 95.2 |

### Streaming, quality 1

| Group | Quality | Implementation | Passing / measured | Median % of C | Weakest | Passing before | Median before |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: |
| streaming | 1 | mbrotli-reader | 6 / 9 | 96.1 | 47.3 (incompressible-256KiB) | 5 / 9 | 95.7 |
| streaming | 1 | mbrotli-session | 6 / 9 | 97.9 | 62.5 (incompressible-256KiB) | 6 / 9 | 98.8 |
| streaming | 1 | mbrotli-writer | 6 / 9 | 98.8 | 67.0 (incompressible-256KiB) | 6 / 9 | 97.7 |

Streaming quality 1 passes 18 of 27 cases.

### Every quality 7 case

| Group | Quality | Implementation | Passing / measured | Median % of C | Weakest | Passing before | Median before |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: |
| cold | 7 | mbrotli | 3 / 11 | 80.6 | 66.5 (text-1KiB) | 6 / 11 | 96.4 |
| presized | 7 | mbrotli | 2 / 11 | 83.3 | 74.5 (incompressible-256KiB) | 2 / 11 | 71.3 |
| reused | 7 | mbrotli | 2 / 11 | 83.9 | 69.6 (incompressible-256KiB) | 2 / 11 | 73.8 |
| tiny | 7 | mbrotli | 0 / 4 | 86.0 | 73.1 (1024) | 0 / 4 | 70.2 |
| tiny | 7 | mbrotli-reused | 2 / 4 | 98.1 | 78.8 (1024) | 0 / 4 | 82.5 |

Quality 7 passes 9 of 41 cases in Criterion; the harness sweep had counted 30 of 41.

### Every quality 9 case

| Group | Quality | Implementation | Passing / measured | Median % of C | Weakest | Passing before | Median before |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: |
| cold | 9 | mbrotli | 11 / 11 | 199.7 | 99.9 (compressible-256KiB) | 11 / 11 | 200.9 |
| dictionary | 9 | mbrotli | 1 / 1 | 275.8 | 275.8 (alice29-half) | 1 / 1 | 277.6 |
| dictionary | 9 | mbrotli-no-dictionary | 1 / 1 | 390.9 | 390.9 (alice29-half) | 0 / 0 | – |
| flush | 9 | mbrotli | 4 / 4 | 291.9 | 195.8 (256) | 4 / 4 | 248.3 |
| presized | 9 | mbrotli | 11 / 11 | 234.1 | 127.5 (vendor-plrabn12.txt) | 11 / 11 | 225.6 |
| reused | 9 | mbrotli | 11 / 11 | 238.7 | 128.5 (vendor-plrabn12.txt) | 11 / 11 | 219.0 |
| streaming | 9 | mbrotli-reader | 9 / 9 | 184.6 | 120.7 (compressible-256KiB) | 9 / 9 | 153.7 |
| streaming | 9 | mbrotli-session | 9 / 9 | 184.7 | 128.1 (vendor-plrabn12.txt) | 9 / 9 | 154.4 |
| streaming | 9 | mbrotli-writer | 9 / 9 | 184.6 | 128.7 (vendor-plrabn12.txt) | 9 / 9 | 156.1 |
| tiny | 9 | mbrotli | 4 / 4 | 861.8 | 600.6 (16) | 4 / 4 | 739.7 |
| tiny | 9 | mbrotli-reused | 4 / 4 | 956.8 | 761.5 (16) | 4 / 4 | 853.6 |

Quality 9 passes 74 of 74 cases in Criterion, where the reference pays the page faults of its 32 MB per-call table; the harness sweep had counted 68 of 69. Under the trimming-free allocator settings above the same quality passes 33 of 69.

Notes on the Criterion runs: the reused subset ran while a coverage
collection was active on other cores; quality 7 and 9 ran on an otherwise
idle machine. The three streaming quality 1 incompressible cases (47–67%)
were 50–78% in the previous record; that path was not changed and is
outside this pass.


## Verification

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo llvm-cov --workspace --all-features --json   # function coverage of the changed files
cd fuzz/afl && cargo fmt --all -- --check \
  && cargo clippy --all-targets --no-default-features -- -D warnings \
  && cargo clippy --all-targets --no-default-features --features experimental -- -D warnings \
  && cargo afl test --no-default-features \
  && cargo afl test --no-default-features --features experimental
```

## Environment

Intel Core i7-13700KF under WSL2 (Linux 6.18.33.2-microsoft-standard-WSL2),
rustc 1.98.1, release profile of the workspace, runtime SIMD selection (the
greedy loops run on the SSE2 level, the rest on AVX2), C reference built by
`google-brotli-ffi` from the vendored sources with the default optimization
flags. One performance core via `taskset -c 2`. No other benchmark or build job of
this session ran during the two sweeps; the Criterion subset below ran while
a coverage collection was active on other cores.
