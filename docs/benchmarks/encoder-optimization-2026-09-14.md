# Encoder optimization — 2026-09-14

This change targets measured encoder work while preserving the encoded bytes.
It changes private greedy and HQ search code, adds no `unsafe`, and keeps the
existing SIMD dispatch and scalar reference paths. The public API is unchanged.

## Measured results and decision

**Keep.** The changes improve several greedy workloads and HQ encoding while
preserving every tested output byte. The strongest consistent result is the
1 MiB single-byte repeat: **1.186–1.205× at q10** and **1.244–1.250× at q11**
across cold, retained-output and writer APIs. Real-text HQ gains are generally
2–4%. These are measured results on this host, not a guarantee for every CPU or
input distribution.

The goal of being universally faster than other libraries is **not reached**.
In the final native-API matrix, mbrotli has the lowest mean in **48 of 96**
corpus/quality combinations, including **20 of 24 at q9–q11**. It has disjoint
95% timing intervals ahead of every competitor in **44** combinations. The
original had 48 lowest means and 43 disjoint leads: this patch improves useful
workloads, but does not establish a broad change in the overall ranking.

### Before/after with identical Rust API boundaries

Cold timings below include construction, allocation and disposal. Speed is the
geometric mean of paired `before / after` ratios; brackets are a bootstrap 95%
interval over the 16 paired batches. Times are arithmetic means. The output
column is identical before and after.

| Input / quality | Before (ms) | After (ms) | Speed [95% interval] | Output (B) |
| --- | --- | --- | --- | --- |
| Single-byte repeat, 1 MiB, q10 | 5.81939 | 4.90954 | 1.186× [1.163–1.206] | 14 |
| Single-byte repeat, 1 MiB, q11 | 9.39880 | 7.55683 | 1.244× [1.229–1.258] | 14 |
| Alice, 152,089 B, q8 | 5.19398 | 4.17960 | 1.243× [1.229–1.257] | 51187 |
| Alice, 152,089 B, q11 | 109.95340 | 105.93449 | 1.038× [1.031–1.044] | 46487 |
| Structured binary, 64 KiB, q9 | 0.25431 | 0.23054 | 1.103× [1.080–1.130] | 1349 |
| Random, 64 KiB, q9 | 0.17203 | 0.15813 | 1.088× [1.069–1.105] | 65540 |
| Tiny text, 44 B, q10 | 0.14765 | 0.14740 | 1.002× [0.987–1.019] | 48 |
| Tiny text, 44 B, q11 | 0.15158 | 0.15139 | 1.001× [0.991–1.014] | 48 |

API mode matters. For example, the large cold q8 Alice gain largely disappears
when its workspace is retained. The structured-binary q9 writer is slightly
slower despite its cold gain.

| Input / quality | Cold speed | Retained-output speed | 4 KiB writer speed |
| --- | --- | --- | --- |
| Single-byte repeat, 1 MiB, q10 | 1.186× | 1.200× | 1.205× |
| Single-byte repeat, 1 MiB, q11 | 1.244× | 1.250× | 1.250× |
| Alice, 152,089 B, q8 | 1.243× | 1.015× | 0.999× |
| Alice, 152,089 B, q11 | 1.038× | 1.031× | 1.043× |
| Structured binary, 64 KiB, q9 | 1.103× | 1.016× | 0.965× |
| Random, 64 KiB, q9 | 1.088× | 1.058× | 1.060× |

The primary matrix's lowest point estimate is **0.956×** for reused q7 random
1 MiB, with interval **0.926–0.989×**. The q9 structured-binary writer is
**0.965× [0.930–0.998×]**, and the 44-byte q5 writer is
**0.965× [0.954–0.975×]**. Every primary point estimate is above 0.95×, but some
intervals admit larger regressions; this is not an uncertainty-aware universal
5% regression guarantee. No samples or cases were removed to hide those losses.

All **330 paired case summaries**, including timings, intervals, bytes and
retained capacity, are in [paired-summary.csv](encoder-optimization-2026-09-14/paired-summary.csv).
The raw [cold](encoder-optimization-2026-09-14/paired-cold.csv),
[retained-output](encoder-optimization-2026-09-14/paired-reused.csv), and
[writer](encoder-optimization-2026-09-14/paired-writer.csv) samples cover every
quality and primary corpus.

### Comparison with other libraries

These are the final cold native-API results. A relative rate above 1 means
mbrotli is faster than C. Counts describe this matrix, whose eight corpora have
equal weight; they are not a production workload distribution.

| Quality | Lowest mean before / 8 | Lowest mean after / 8 | Disjoint leads after / 8 | Median rate vs C after |
| --- | --- | --- | --- | --- |
| 0 | 4 | 3 | 2 | 1.129× |
| 1 | 4 | 4 | 4 | 1.224× |
| 2 | 4 | 4 | 3 | 1.102× |
| 3 | 3 | 3 | 3 | 1.099× |
| 4 | 3 | 4 | 3 | 0.998× |
| 5 | 3 | 3 | 2 | 0.976× |
| 6 | 3 | 2 | 2 | 0.963× |
| 7 | 2 | 2 | 2 | 0.929× |
| 8 | 2 | 3 | 3 | 0.898× |
| 9 | 7 | 7 | 7 | 2.555× |
| 10 | 7 | 6 | 6 | 1.124× |
| 11 | 6 | 7 | 7 | 1.228× |

[Before](encoder-optimization-2026-09-14/comparison-before.csv) and
[after](encoder-optimization-2026-09-14/comparison-after.csv) contain all 432
native measurements, their intervals, throughput and compressed sizes. Output
sizes match the before run in every row, including the unchanged competitors.
Native timings are separate processes and can drift with the host; use the
paired trials above to assess this patch's effect. For example, final native
tiny-HQ times are higher than the earlier baseline while their C controls also
rose; the contemporaneous paired tiny-HQ results are essentially unchanged.

The root Criterion suite additionally compares **matching C streaming FINISH
semantics**, exact bytes and API mode. Selected final results are:

| Case | C (ms) | mbrotli (ms) | Rate vs C | Output (B) |
| --- | --- | --- | --- | --- |
| Cold single-byte repeat, 1 MiB, q10 | 8.635 | 4.871 | 1.773× | 14 |
| Cold single-byte repeat, 1 MiB, q11 | 11.891 | 7.782 | 1.528× | 14 |
| Alice writer, q9 | 11.521 | 4.685 | 2.459× | 51054 |
| Alice writer, q11 | 132.601 | 107.500 | 1.234× | 46487 |
| Cold random, 1 MiB, q8 | 3.331 | 5.218 | 0.638× | 1048581 |
| Cold random, 1 MiB, q9 | 12.155 | 16.537 | 0.735× | 1048581 |

The large random cases make the remaining gap concrete: C wins at q8 and q9.
[All 16 root Criterion measurements](encoder-optimization-2026-09-14/root-criterion.csv)
include intervals and throughput. HQ groups use their repository policy of
10 samples, 1 second warmup and a 3 second measurement target; other selected
groups use 20 samples and the command-line 0.1/0.3 second targets.

### Additional corpora

Six unmodified vendor files were checked with 12 alternating cold pairs each at
q5–q11. Every output matched the baseline bytes. Selected speed ratios follow;
all qualities and intervals remain in the summary and
[raw additional-corpus samples](encoder-optimization-2026-09-14/paired-heldout.csv).

| Corpus | Input (B) | q7 | q8 | q9 | q10 | q11 |
| --- | --- | --- | --- | --- | --- | --- |
| lcet10.txt | 426754 | 1.077× | 1.051× | 1.009× | 1.024× | 1.035× |
| plrabn12.txt | 481861 | 1.038× | 1.038× | 1.066× | 1.032× | 1.030× |
| mapsdatazrh | 285886 | 1.083× | 1.127× | 1.117× | 1.039× | 1.031× |
| random_org_10k.bin | 10000 | 1.032× | 1.005× | 1.023× | 0.992× | 0.992× |
| quickfox_repeated | 176128 | 0.985× | 0.988× | 1.006× | 1.005× | 1.004× |
| compressed_file | 50096 | 1.058× | 1.095× | 1.089× | 0.990× | 0.971× |

The maps corpus improves **1.127× [1.104–1.151×] at q8** and
**1.117× [1.089–1.148×] at q9**. Already-compressed data at q11 regresses to
**0.971× [0.962–0.980×]**. The repeated phrase is essentially unchanged at HQ;
the much larger single-byte-repeat gain should not be generalized to every
repetitive input.

### Memory and final profiles

Measured retained capacity is unchanged for greedy cases. The HQ primary cases
add **6,144 bytes** for non-tiny inputs and **zero** for cold tiny inputs; the
10,000-byte held-out input adds 5,888 bytes. Larger capacity remains retained
when a later block is shorter. Retained capacity counts requested library
storage, not resident physical pages.

Fresh-process peak RSS, measured with GNU `time` over retained-output calls,
rises by **0.16–0.51 MiB** across the 15 measured cases (median of three runs
per side). This includes executable, runtime and allocator effects and is not
attributed solely to the price cache. Peak live requested bytes and post-release
allocator RSS were not isolated. Selected median process peaks are:

| Case | Before peak RSS (MiB) | After peak RSS (MiB) |
| --- | --- | --- |
| random-1m, q8 | 40.75 | 40.98 |
| random-1m, q9 | 71.99 | 72.48 |
| repeated-1m, q9 | 5.39 | 5.90 |
| random-1m, q10 | 21.93 | 22.26 |
| random-1m, q11 | 22.84 | 23.18 |

[All process-memory samples](encoder-optimization-2026-09-14/peak-rss.csv) and the
paired capacity columns preserve the distinction between these measurements.
The [final all-quality profiles](encoder-optimization-2026-09-14/profiles-after.csv)
still concentrate in search: q9 backward references occupy 58.58% of inclusive
profiled elapsed time, versus 59.33% before; q10 shortest-path search occupies
62.94%, versus 61.54%. These time-budgeted profiles redistribute work and call
counts as speed changes. Their percentages do not establish a speedup; the
un-instrumented timings above do.

The decision is to keep the measured text/search and long-copy improvements,
with the small regressions and memory costs above recorded. Future changes
should rerun this matrix without automatically replacing its baseline, and
should measure the caller's actual corpus and deployment CPU. q4–q8 and large
incompressible q9 inputs remain useful optimization targets.

## Measurement contract

The baseline is `090db88b4efb8836fe4dde74444bbfba63091c9a`; the candidate is the
working-tree change accompanying this report. The target is an Intel Core
i7-13700KF under WSL2, x86-64 Linux, rustc 1.98.1 / LLVM 22. Release measurements
use default features, the system allocator, runtime SIMD selection, one serial
encoder, and CPU affinity 2. No `target-cpu=native` or host tuning was added.
[Build hashes and environment](encoder-optimization-2026-09-14/environment.json)
identify the actual executables and source files.

The eight deterministic corpora are empty input, 44-byte text, `alice29.txt`
(152,089 bytes), Alice repeated to 1 MiB, structured binary (64 KiB), xorshift64
random bytes (64 KiB and 1 MiB), and one repeated byte (1 MiB). Random generation
starts at `0x243f6a8885a308d3`. Every quality from 0 through 11 uses generic mode
and window 22. Exact corpus generation and the native adapter boundaries live in
[`benchmarks/comparison/src/core.rs`](../../benchmarks/comparison/src/core.rs).

Two complementary measurements are used:

- The existing Criterion suite measures 432 cold native-API cases across mbrotli,
  Google C Brotli 1.2.0, Rust brotli 9.0.0, simd-brotli 10.0.1, and Burli 0.3.1.
  Burli supports q0–q5. Each iteration includes construction, output allocation,
  encoding and disposal. Input construction and C round-trip validation happen
  outside timing. Equal quality numbers can produce different compressed sizes;
  every row records size as well as time.
- A local paired harness links the original and candidate Rust implementations
  into one process. It validates byte identity before timing and alternates
  AB/BA/BA/AB batches. Cold construction, retained `compress_into`, and retained
  writers using exact input sizes and 4 KiB chunks are measured separately.
  This reduces allocator-state and scheduling differences between processes.
  It does not remove code-placement effects or shared-host noise.

Timings run separately from builds, correctness tests, allocation instrumentation,
and fuzzing. Bootstrap intervals describe the collected samples; they do not
account for every machine effect. Per-case results matter more than an aggregate
score. Sustained slowdowns above 5% were investigated during selection, including
the rejected allocation and small-input price-table variants below.

## Profile and diagnosis

Release-mode `hotpath` 0.25.0 profiles cover all twelve qualities and all eight
corpora. Both inclusive function timing and exclusive requested allocation bytes
were collected. [All-quality profile rows](encoder-optimization-2026-09-14/profiles-before.csv)
preserve the functions, call counts and measured shares. These duration-based
runs give each corpus time, not equal call counts; aggregate allocation totals
are not per-compression memory usage. Inclusive timing shares must not be added
across nested functions.

| Quality | Main measured encoder work | Inclusive share of profiled elapsed time |
| --- | --- | ---: |
| 0 | Fragment compression | 70.24% |
| 1 | Command creation / command storage | 37.43% / 25.42% |
| 2 | Greedy backward references / meta-block output | 35.87% / 16.09% |
| 3 | Greedy backward references / meta-block output | 40.24% / 17.29% |
| 4 | Greedy backward references | 42.47% |
| 5 | Greedy backward references | 47.28% |
| 6 | Greedy backward references | 51.98% |
| 7 | Greedy backward references | 53.79% |
| 8 | Greedy backward references | 56.39% |
| 9 | Greedy backward references | 59.33% |
| 10 | Shortest-path search | 61.54% |
| 11 | HQ backward references, including two price passes | 72.05% |

A separate q5 Alice run attributed about 81% of elapsed time to backward
references. Allocation profiling also placed substantial q8 allocation traffic
inside backward-reference construction, where sparse bucket pools grow. Finer
temporary timing hooks pointed to candidate measurement, HQ `update_nodes`, and
the start-position queue. Their overhead perturbs these very short routines, so
they were diagnostic only and were removed from the final implementation.

Hardware CPU sampling was unavailable: `samply` could not obtain perf events with
the host's `perf_event_paranoid=2`. No host setting was changed. Consequently this
report does not claim measured instruction counts, branch misses or cache misses.
Disassembly does confirm that accepted greedy matches now return through
registers, with the positive score encoding the optional result; the baseline
used a separate return buffer and option tag.

## Kept changes and tradeoffs

1. **Greedy match measurement.** Successful scores are strictly positive, so a
   `NonZeroUsize` niche removes the extra optional-result tag. Physical ring
   indices and block lengths cross the outlined helper boundary as `u32`, making
   their existing bounds visible to the optimizer. Slice access remains safe.
2. **Sparse-pool growth.** Once 1024 buckets outgrow their four-slot starters,
   reserve from the input-size hint. Round down and cap the reservation at 16384
   blocks and the bucket count. The pool can grow further on demand. Positions,
   tags, generation resets and candidate order are unchanged.
3. **HQ command prices.** Append rows indexed by insert code, last-distance
   policy and copy code to the existing symbol-price vector. Only insert codes
   reachable in the block are rebuilt. Blocks below 128 bytes use one stack row
   instead. Failed probes and matches with no new copy lengths skip row setup.
   Copy loops borrow one row and avoid repeatedly constructing command symbols.
   The original `f32` values and addition order are preserved. The final paired
   matrix retains 6,144 extra bytes on large HQ inputs and none on tiny inputs;
   no extra vector or separately retained allocation is introduced. Growing the
   symbol buffer can still reallocate it once. Capacity after reuse depends on
   growth.
4. **HQ candidate queue.** Shift only the cheaper prefix on insertion and stop at
   the first equal or higher cost. The finite-cost invariant preserves exact
   reference tie order and eviction after ring wraps. The operation is `const`.

The [greedy](../../architecture/greedy-encoder.md),
[HQ](../../architecture/hq-encoder.md), and
[workspace](../../architecture/encoder-workspace.md) specifications describe the
ownership, data flow, dispatch and invariants. No new ISA requirement, public
error, dependency or approximation was introduced.

Experiments rejected during selection:

- A safe SIMD filter for nearby cached distances slowed structured binary at
  q7/q8 by roughly 13–15% in the exploratory run.
- Alternate Huffman bit-reversal implementations did not yield a broad win.
- Uncapped sparse reservation improved the paired 1 MiB random case by about
  23% at q8 and 19% at q9, but slowed q9 large text by about 26% and retained an
  extra 16 MiB there. The final reservation is capped.
- A separate command-price vector slowed native cold tiny q10/q11 calls by
  about 9–11%, even when only reachable insert-code rows were built. Packing
  rows into existing storage and skipping the cache below 128 bytes removed
  that setup cost. Limiting each stack fill to a requested copy-code range was
  also rejected: it improved tiny inputs but slowed random HQ inputs by up to
  16% in its paired trial. The final code fills a complete row after a useful
  match, keeping the larger-input calling convention small.

Cross-process exploratory timings were noisier than paired trials and are not
used for the final optimization claims.

## Reproduction

Run the native comparison at both revisions, saving separate named baselines:

```sh
cargo bench --manifest-path benchmarks/comparison/Cargo.toml \
  --bench implementations --locked --no-run
taskset -c 2 target/encoder-optimization/comparison-before --bench \
  --save-baseline encoder-before-20260914 --sample-size 20 \
  --warm-up-time 0.1 --measurement-time 0.3 --noplot
taskset -c 2 target/encoder-optimization/comparison-final --bench \
  --save-baseline encoder-final-20260914 --sample-size 20 \
  --warm-up-time 0.1 --measurement-time 0.3 --noplot
python3 benchmarks/comparison/report.py --baseline encoder-final-20260914 \
  --csv target/encoder-optimization/comparison-after.csv
```

The two named executables above are copies of the Criterion executable printed
by Cargo for each build. Archive each run's `sizes.csv` before starting another
preflight. The locked comparison workspace pins competitors independently of
the production workspace. The ignored root lockfile needed only its local
mbrotli package version refreshed from 0.2.0 to 0.4.1; dependency versions did
not change.

The local probe was built separately with `hotpath-cpu` and `hotpath-alloc`:

`environment.json` embeds the local probe and paired-harness sources, manifests,
lockfiles and statistics script. The paired harness's `base` directory contains
the baseline commit's `src`, `benches`, `examples`, `Cargo.toml` and
`README.md` (the extra target files satisfy Cargo's manifest validation); its saved manifest
renames the package version to `0.4.1-baseline`, isolates the workspace, and
points its test-only C dependency to the existing vendored workspace crate.

```sh
cargo build --manifest-path target/encoder-optimization/probe/Cargo.toml \
  --release --offline --target-dir target --features hotpath-cpu
HOTPATH_OUTPUT_FORMAT=json HOTPATH_OUTPUT_PATH=target/encoder-optimization/cpu-final-q8-all.json \
  taskset -c 2 target/encoder-optimization/profile-final 8 all cold 0.25
```

For AFL, build the five targets with `cargo afl build --release
--no-default-features`, then run each for 60 seconds using `-s 197 -t 2000 -m
none`. `differential_c`, `simd_equivalence`, and `streaming_equivalence` use
`seeds/params`; q10/q11 round trips use `seeds/generic`. Campaigns set
`AFL_NO_UI=1`, `AFL_SKIP_CPUFREQ=1`,
`AFL_I_DONT_CARE_ABOUT_MISSING_CRASHES=1`, `AFL_FAST_CAL=1`, and
`AFL_CMPLOG_ONLY_NEW=1`. CmpLog is enabled on `differential_c` and q11, and disabled
with `-c -` on the other workers. Separate output directories and CPUs
8/10/12/14/16 prevent workers from sharing a queue accidentally.

All five final-source runs completed: **438,562 executions, zero saved crashes, zero saved
hangs**, with 99.97–99.99% reported stability.
[Per-target statistics and binary hashes](encoder-optimization-2026-09-14/afl.json)
are retained. These are bounded smoke campaigns, not exhaustive fuzzing.


## Correctness and portability

The final workspace checks pass with warnings denied in both default and
all-feature Clippy configurations. The all-feature test run passes 1,081 tests;
two pre-existing decoder stress tests remain ignored. They require a stream
above 4 GiB or a 2 GiB history and are unrelated to the changed encoder paths.
Tests and coverage use optimization level 1 for the dev and test profiles;
performance measurements use the ordinary release profile.

Focused tests compare queue insertion against the original adjacent-swap
algorithm across 4,096 insertions, ties and ring wraps. Command-price tests check
exact floating-point bits across both model initializers, short/large block
transitions, and the 127/128/129-byte cache boundary. Greedy tests cover accepted
scores, rejected ties and short/invalid spans on every available backend, plus
sparse-pool promotion, tags and reservation limits. Full integration tests
exercise C byte identity, round trips, modes, windows, dictionaries, reuse,
streaming and experimental paths.

The host exercises scalar, SSE2, SSE4.2 and AVX2 backends. AArch64 NEON and 32-bit
execution remain unvalidated here. Existing feature selection stays outside
inner loops; the greedy encoder's existing SSE2 policy is unchanged. No new
unsafe operations need a pointer or alignment proof.

The exported coverage report reaches **2,980 / 2,980 functions (100%)** in
cargo-llvm-cov's library-source report scope, including inline unit tests.
Changed benchmark functions were checked separately in the raw counters:
`bench_cold` and its four closures all ran (625 aggregate executions).
The optional combined HTML rendering warns about 53 mismatched function
mappings across the collected objects; the canonical JSON export completes
without warnings. The changed-function counters were inspected explicitly.
The benchmark test run passes all **1,229 validation entries**.
Coverage reports remain local under `target/encoder-optimization/`; the
[verification record](encoder-optimization-2026-09-14/verification.json) records
commands and outcomes. The architecture and changelog were refreshed with the
implementation.
