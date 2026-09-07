# Dense matcher SIMD dispatch — reverted

## Decision and contract

The target is **at least 95% of C throughput in each matched case**, not an
average across qualities. It is not met: the final broad sweep passes
**257 of 658 cases**, versus **279 of 658** in the fresh baseline.
**Decision: reverted after regression review.** The user required reverting
changes with regressions. Repeated measurements showed a roughly 4% cold q3
incompressible slowdown, while flush and q11 regressions remained unresolved.
The specialized matcher also increased the focused executable's text size by
13.04%. The targeted q5/q6 gains do not justify retaining those regressions.

Both greedy implementation files and the corresponding architecture changes
have been restored to the original revision. The independent all-feature
memory-test fix remains: it excludes hotpath's lazy global registrations from
compressor-owned allocation accounting. The measurements below are historical
evidence for the rejected candidate, not performance claims about the current
implementation. The 95% target remains unmet, and the release baseline and gate
have not been changed.

The comparison uses identical input, quality, window, size hint and API mode.
Correctness requires byte identity with the pinned C streaming encoder and
with the scalar backend. Reused measurements include encoding into retained
output storage; Rust retains encoder workspace, while the C adapter recreates
state for each independent stream, as in the existing benchmark contract.
Cold measurements include construction and output allocation. There is no new
public API, allocation policy, ISA requirement or unsafe code.

The [flamegraph follow-up](2026-09-06-flamegraph-followup.md) records the
restored-source checks and subsequent CPU sampling.

## Diagnosis and implementation

**Measured:** the release hotpath example with the Alice corpus attributed
109.25 ms of 160.90 ms (67.90%) to greedy backward-reference generation.
These are inclusive instrumented function times, not sampled CPU shares.
Hardware sampling failed on this WSL2 host; no PMU result is claimed.
Allocation profiling also completed separately from throughput measurements.

**Hypothesis:** the runtime storage-layout enum and bucket depth obscure
invariants in the existing SIMD kernels, adding control work and register
pressure at every search. The retained change chooses the concrete table view
once per block and specializes dense tagged depths four and five with four
cached distances. Other shapes retain runtime depth bounds. `ReferenceRun`
enters `S::vectorize` with that concrete view, which remains concrete through
search and command commit. An eager position load passed to `black_box` has
also been removed: it forced extra load/stack work before the candidate was
needed, including searches whose bucket scan found nothing.

The scalar backend still uses its independent unfiltered candidate scan.
The existing bounded loads, SIMD tails, match order, score comparisons,
wrapping counters, history and error propagation are unchanged. The operation
owns exclusive borrows for one block; it adds no heap allocation or copy of
the tables. The [current mechanics specification](../../architecture/greedy-encoder.md#23-storage-layouts-runs-and-sweeps) describes the restored implementation; the specialization described here was reverted.

**Measured assembly:** the AVX2 `ReferenceRun::run<DenseRun<false, 14, 4>>`
body contains inline `vpcmpeqb`/`vpmovmskb` tag filtering and YMM match scans.
No feature detection occurs in that body. The runtime ISA proof remains the
encoder's retained `fearless_simd` token. The final focused executable's body
is 11,112 bytes, down from 11,552 before removing the eager position load.
It contains eight byte-equality comparisons, eight mask extractions and no
`cpuid` instruction. Assembly and disassembly are local artifacts.

Experiments rejected before the measured candidate:

- Entering the SIMD scan without a scalar prefix did not improve the tested
  workloads consistently.
- Replacing the scalar-prefix chunk loop with two explicit word comparisons
  did not give a consistent gain.
- Gathering eight rejection words and filtering deep candidates with SIMD
  was slower on the six q7/q8 text/binary pilot cases (0.80–0.94× Rust speed).
- Selecting a concrete layout without fixing dense depth gave small gains but
  regressed q6 incompressible input in repeated trials.
- Specializing all on-demand depths increased code size further and regressed
  several cases. The retained specialization is limited to dense q5/q6 shapes.

The focused benchmark executable's `size` text column grew from 7,253,170 to
8,198,978 bytes (**derived: 13.04%**). Specializing all on-demand depths grew
it to 11,255,770 bytes and was rejected. These executable sizes include the
same benchmark harness and dependencies; they are not library-only sizes.

## Reproduction

- Baseline: `731a0becfdc139d2207848f78fb9851ae2617515`.
- CPU: Intel Core i7-13700KF; throughput runs pinned to logical CPU 2.
- OS: Linux 6.18.33.2-microsoft-standard-WSL2.
- Rust: 1.98.1 (`48a229cea`, LLVM 22.1.8), x86_64-unknown-linux-gnu.
- GCC: 11.4.0; pinned C Brotli commit `028fb5a23661f123017c060daa546b55cf4bde29`.
- Default optimized build flags, system allocator, runtime SIMD dispatch.
- Native backends exercised: fallback, SSE2, SSE4.2, AVX2. AArch64 and AVX-512
  execution were not available; no cross-target speed claim is made.
- Inputs and deterministic generators are those in `benches/compress.rs`.
  Vendor inputs are unchanged from the pinned submodule. Alice SHA-256:
  `7467306ee0feed4971260f3c87421154a05be571d944e9cb021a5713700c38f0`.

```sh
cargo run --release --locked --features hotpath-cpu --example profile_compressor -- brotli-ffi/vendor/brotli/tests/testdata/alice29.txt
cargo run --release --locked --features hotpath-alloc --example profile_compressor -- brotli-ffi/vendor/brotli/tests/testdata/alice29.txt
cargo rustc --release --locked --lib -- --emit=asm
cargo bench --bench compress --locked --no-run
```

The initial broad pilot had overlapping measurements and builds; it is not
used as acceptance evidence. Later focused layout trials used three alternating
baseline/candidate rounds, no concurrent builds, 15 samples, 100 ms warm-up
and 350 ms measurement. The subsequent depth pilot motivated final validation;
it does not by itself establish the improvement across the full harness.
Raw Criterion samples remain in `target/criterion/`.

The focused executable is a temporary copy of `benches/compress.rs` with
`QUALITIES` limited to q2–q9 and only `bench_cold` / `bench_reused` registered.
It is compiled with `rustc --edition=2024 -C opt-level=3 --crate-name compress`,
linking the default-feature release `mbrotli`, `criterion` and
`google_brotli_ffi` rlibs produced by Cargo, with `CARGO_MANIFEST_DIR` set to
the repository root. The API bodies, corpus generators and validation are
unchanged. The final filter is:

```text
^reused/q[5-8]/(mbrotli|c-brotli)/(binary-256KiB|vendor-alice29.txt|incompressible-256KiB|text-1KiB)$
```

Its repeated trials use `--warm-up-time 0.1 --measurement-time 0.35
--sample-size 15 --nresamples 10000`, alternating baseline/final order across
three rounds. The full matrix and regression repeats use the unmodified
Cargo benchmark harness.

The final broad comparison runs all 658 matched cases sequentially, first
baseline and then final, pinned to the same core. The harness overrides the
short sampling settings for qualities ten and eleven with one-second warm-up
and three-second measurement windows. Fixed executables prevent rebuilding
from changing either measurement during the run:

```sh
taskset -c 2 /tmp/mbrotli-compress-before --bench --warm-up-time 0.05 --measurement-time 0.1 --sample-size 10 --nresamples 10000 --save-baseline dense-full-before
taskset -c 2 /tmp/mbrotli-compress-complete --bench --warm-up-time 0.05 --measurement-time 0.1 --sample-size 10 --nresamples 10000 --save-baseline dense-full-after
python3 scripts/compare_benchmarks.py --baseline dense-full-before --expected-cases 658 --csv /tmp/mbrotli-dense-full-before.csv
python3 scripts/compare_benchmarks.py --baseline dense-full-after --expected-cases 658 --csv /tmp/mbrotli-dense-full-after.csv
```

The baseline executable SHA-256 is
`e1de764e4f1855a0b19d536ad659d8d6a873ff415f037d9dbbcf54d6b02a07d9`;
the final executable SHA-256 is
`0f2ba071bafabf8085dc2e44ddbffa5a5e48f60593400cb0fd0af63175309ee0`.

## Correctness and resource evidence

The new unit test compares the selected operation with the unspecialized
`BucketRun` on every host backend, compact/sparse/dense layouts, depths four
through six, nonstandard cache counts and repeated resets. Existing tests
cover byte identity with C, empty and boundary inputs, ring wrapping, streaming,
attached dictionaries and independent fragments.

The all-feature memory test initially counted hotpath 0.25's lazily registered
profiler state as compressor-owned memory. It failed at unchanged quality zero
when run alone. The test now warms profiler registrations outside the measured
lifetime and still constructs a fresh compressor for the exact ownership and
trim assertions. All seven memory tests then passed. Warmed compression still
allocates nothing on the existing tested workloads. Peak and post-burst RSS
were not measured as a before/after pair.

The AFL smoke corpus uses qualities five and six, window 22, the Alice input
and lengths around tiny-input, compact-table, block and 128 KiB boundaries.
The existing target caps inputs at 128 KiB, so q6's larger dense-table threshold
is exercised by unit/integration tests rather than this smoke campaign.
The host has a piped core handler; the bounded campaign uses the process-local
`AFL_I_DONT_CARE_ABOUT_MISSING_CRASHES=1` bypass. Crashes may be reported as
hangs on this host; both finding categories must be inspected.

## Per-case results

The [paired CSV](2026-09-06-dense-simd-paired.csv) records all 16 focused
cases: three alternating baseline/final rounds, 15 samples per measurement,
100 ms warm-up, 350 ms measurement, core 2, no concurrent builds or tests.
Speedup is the median of the three before/after time ratios. Each percentage
of C is the median of the round's matched C/Rust ratios, so it need not equal
a ratio formed from separately rounded median times. Sizes matched before
and after and against C in every validation.

| Reused case | Before, µs | After, µs | Rust speedup | After, % of C | Compressed bytes |
| --- | ---: | ---: | ---: | ---: | ---: |
| q5/binary-256KiB | 4245.67 | 3610.53 | 1.167× | 76.8 | 94,650 |
| q5/incompressible-256KiB | 368.48 | 317.04 | 1.162× | 94.9 | 262,149 |
| q5/text-1KiB | 14.54 | 14.15 | 1.027× | 69.2 | 146 |
| q5/vendor-alice29.txt | 2886.76 | 2718.85 | 1.062× | 85.4 | 52,809 |
| q6/binary-256KiB | 5098.50 | 4288.14 | 1.181× | 79.8 | 94,648 |
| q6/incompressible-256KiB | 432.05 | 387.28 | 1.086× | 98.4 | 262,149 |
| q6/text-1KiB | 14.87 | 14.47 | 1.039× | 69.8 | 146 |
| q6/vendor-alice29.txt | 3441.36 | 3417.17 | 1.009× | 77.4 | 51,967 |
| q7/binary-256KiB | 8345.59 | 7625.59 | 1.090× | 73.9 | 94,208 |
| q7/incompressible-256KiB | 749.99 | 745.42 | 1.006× | 73.0 | 262,149 |
| q7/text-1KiB | 14.60 | 14.66 | 0.996× | 71.2 | 146 |
| q7/vendor-alice29.txt | 3987.67 | 3881.81 | 1.016× | 80.5 | 51,451 |
| q8/binary-256KiB | 10184.81 | 9274.96 | 1.106× | 79.0 | 94,208 |
| q8/incompressible-256KiB | 752.68 | 737.12 | 1.022× | 90.9 | 262,149 |
| q8/text-1KiB | 15.46 | 15.22 | 1.029× | 72.1 | 146 |
| q8/vendor-alice29.txt | 4329.32 | 4122.89 | 1.050× | 86.1 | 51,187 |

All 16 median before/after ratios exceed 0.99. The CSV's lower/upper columns
are the extrema of the conservative round-wise interval ratios, not a
confidence interval for the median. The q7 Alice lower envelope is below
0.95; a small regression there cannot be excluded. Only one focused case
exceeds 95% of C at its median, so these gains do not satisfy the overall
performance target.

## Complete matrix

The [baseline CSV](2026-09-06-dense-simd-before.csv) and
[final CSV](2026-09-06-dense-simd-after.csv) each contain exactly 658 matched
Rust/C cases. The [comparison CSV](2026-09-06-dense-simd-comparison.csv)
preserves Rust before/after time ratios, conservative interval ratios,
movement in the C timings and both gate results. All
[printed output sizes](2026-09-06-dense-simd-sizes.txt) match between runs;
the harness also asserts exact bytes and successful decoding before timing.

The baseline passes **279/658** cases at the mean point estimate and
**241/658** at the conservative lower interval bound. The final passes
**257/658** and **227/658**, respectively. Both 95% gate commands exit with
status 1. This is a performance-target failure, not an incomplete matrix or
a correctness failure.

| Quality | Cases | Before passes | After passes | Before median % of C | After median % of C |
| --- | ---: | ---: | ---: | ---: | ---: |
| 0 | 43 | 31 | 33 | 99.5 | 99.6 |
| 1 | 74 | 50 | 45 | 97.9 | 97.9 |
| 2 | 72 | 26 | 25 | 90.2 | 89.1 |
| 3 | 41 | 10 | 10 | 89.5 | 91.2 |
| 4 | 41 | 8 | 9 | 86.0 | 85.2 |
| 5 | 75 | 13 | 17 | 81.6 | 85.8 |
| 6 | 41 | 7 | 10 | 79.1 | 84.1 |
| 7 | 41 | 10 | 12 | 78.0 | 76.9 |
| 8 | 41 | 6 | 6 | 78.8 | 79.2 |
| 9 | 73 | 73 | 72 | 229.0 | 239.9 |
| 10 | 41 | 3 | 0 | 89.4 | 87.4 |
| 11 | 75 | 42 | 18 | 96.8 | 92.7 |

| API group | Cases | Before passes | After passes |
| --- | ---: | ---: | ---: |
| cold | 132 | 53 | 50 |
| reused | 132 | 52 | 51 |
| presized | 132 | 52 | 48 |
| tiny | 96 | 27 | 26 |
| streaming | 135 | 78 | 65 |
| flush | 20 | 11 | 12 |
| dictionary | 3 | 1 | 1 |
| universal | 8 | 5 | 4 |

The median Rust before/after speed ratio over all cases is **0.999×**.
Twenty-five cases show more than 10% higher throughput and 26 show more than
10% lower throughput (ratio below `1 / 1.1`). These summary statistics are
not substitutes for the per-case gate. The q5/q6 pass counts improve, while
quality eleven accounts for 24 fewer passes despite no edits to its encoder.
The median C speed ratio is 0.997×, but individual C timings move much more;
for example, the presized q6 incompressible C run slows to 0.748×. Thus a
nearly stable overall C median does not establish stable conditions for each
measurement. Host load after the sweep was 4.62/4.51/3.19; no other builds or
tests from this task ran during the throughput trials.

The broad sweep alone cannot establish that the apparent regressions are
caused by the change, nor can they simply be dismissed as noise. Alternating
follow-up measurements are recorded separately below without replacing any
of the full-sweep results.

## Follow-up of apparent regressions

The [follow-up CSV](2026-09-06-dense-simd-regressions.csv) records three
alternating baseline/final rounds on 11 cases: the six largest apparent
slowdowns in changed qualities q2–q9, plus cold q2 maps, cold q3 incompressible,
reused q8 large text, reused q5 incompressible, and an unchanged q1 control.
These use the full, fixed executables, 15 samples, 100 ms warm-up, 350 ms
measurement and core 2. The [exact commands](2026-09-06-dense-simd-repeat-commands.txt)
include the filters and named baselines. As in the initial paired trials,
interval columns are the extrema of round-wise conservative ratios, not a
confidence interval for the median.

| Case | Median Rust speedup | Interval envelope |
| --- | ---: | ---: |
| flush/q5/mbrotli/1 | 1.044× | 1.003–1.089× |
| presized/q6/mbrotli/incompressible-256KiB | 1.107× | 0.804–1.584× |
| flush/q5/mbrotli/32 | 1.048× | 0.988–1.169× |
| flush/q9/mbrotli/32 | 0.939× | 0.766–1.112× |
| flush/q9/mbrotli/1 | 1.033× | 0.945–1.206× |
| flush/q2/mbrotli/32 | 0.932× | 0.823–1.049× |
| cold/q2/mbrotli/vendor-mapsdatazrh | 0.996× | 0.818–1.045× |
| cold/q3/mbrotli/incompressible-256KiB | 0.962× | 0.912–1.009× |
| reused/q8/mbrotli/text-1MiB | 0.998× | 0.828–1.120× |
| reused/q5/mbrotli/incompressible-256KiB | 1.183× | 0.869–1.425× |
| reused/q1/mbrotli/vendor-random_org_10k.bin | 0.978× | 0.902–1.047× |

The large q8 text regression does not recur in the median; q5 flush shows
4–5% higher throughput. Presized q6 and reused q5 incompressible inputs have
higher medians but wide spread. Cold q3 incompressible remains about **3.8%
slower**: all three point ratios are between 0.949 and 0.964, with the first
two round intervals below 1. The q2 and q9 flush medians are **6.8%** and
**6.1%** lower, but their rounds straddle 1 and the envelopes are wide.
These are retained limitations, not evidence that every workload improves.

The [quality eleven follow-up](2026-09-06-dense-simd-controls.csv) repeats
two outliers whose encoder source did not change. It uses three alternating
rounds and the full harness's q11 policy of ten samples, one-second warm-up
and three-second measurement windows:

| Case | Median Rust speedup | Interval envelope |
| --- | ---: | ---: |
| streaming/q11/mbrotli-writer/vendor-quickfox_repeated | 0.995× | 0.843–1.079× |
| presized/q11/mbrotli/incompressible-256KiB | 0.929× | 0.834–1.016× |

The writer outlier does not recur in the median. Presized incompressible q11
still has a **7.1% lower median**, with a wide envelope that includes 1;
unchanged source alone does not establish that code layout or other binary
effects are irrelevant. Its regression remains unresolved. No follow-up is
used to turn a failed or inconclusive case into a gate pass.

Any future replacement requires resolving the q3/flush/q11 concerns and repeating
the required per-case comparison under sufficiently stable conditions. The
q1 incompressible reader remains near 50% of C in both full runs, independently
of these matcher gains. No result in this report establishes the user's
95%-in-every-case target.

## Validation commands and results

```sh
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
CARGO_PROFILE_TEST_OPT_LEVEL=1 cargo test --workspace --all-features --locked
```

These checks passed on the measured candidate before its revert. Coverage was first collected over the
full suite. After removing the eager load, a fresh collection ran all unit
tests and the shorter integration cohorts, followed by the fork/recovery
lifecycle cases. The full normal test suite still ran without filtering.
The final inspected report covers **2,185/2,185 functions (100%)** and
97.38% of lines; both changed core files have 100% function coverage.
The [coverage command](2026-09-06-dense-simd-coverage-command.txt) records the
exact selection. This avoids repeating expensive integration stress loops
under coverage instrumentation after they have passed in the normal suite.

AFL regression replay passed. Final binaries completed two 60-second smoke
campaigns: **6,153 executions** for `simd_equivalence` and **19,531** for
`differential_c`, with **zero saved crashes and zero saved hangs** in both.
Both reported 99.98% stability. These are bounded smoke results, not a safety
proof. The commands used the existing target bodies and 26 local seeds:

```sh
cd fuzz/afl
cargo afl test
cargo afl build --release --bin simd_equivalence --bin differential_c
AFL_SKIP_CPUFREQ=1 AFL_NO_AFFINITY=1 AFL_NO_UI=1 AFL_I_DONT_CARE_ABOUT_MISSING_CRASHES=1 cargo afl fuzz -c - -V 60 -t 2000 -m none -i /tmp/mbrotli-dense-simd-seeds -o findings/dense-simd-final-simd_equivalence -- target/release/simd_equivalence
AFL_SKIP_CPUFREQ=1 AFL_NO_AFFINITY=1 AFL_NO_UI=1 AFL_I_DONT_CARE_ABOUT_MISSING_CRASHES=1 cargo afl fuzz -c - -V 60 -t 2000 -m none -i /tmp/mbrotli-dense-simd-seeds -o findings/dense-simd-final-differential_c -- target/release/differential_c
```

Coverage, assembly, raw timings, generated seeds and AFL findings remain local
artifacts. No vendored source or dependency versions were changed.
