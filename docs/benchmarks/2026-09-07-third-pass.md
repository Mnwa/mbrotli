# Third optimization pass: allocation shape, layouts and cast costs

## Decision

**The 95%-of-C target is still not met per case**, but the pass moves the
weakest groups the most: the tiny quality 10 and 11 calls from 13% to 97%,
the repetitive and compressible corpora at qualities 7 to 11 from 26–80% to
120–234%, the flush cases from 76% to 101%, the quality 0/1 incompressible
cases from 80–91% to 96–99%, the tagged bucket matchers' reused binary cases
from 82–84% to 86–93%, and the reused quality 7/8 cases from 71–85% to
91–97% once the deep shapes were allowed to take their dense table on a
compressor's second stream. Every change keeps the output byte-identical: the
harness validates each case's compressed bytes against the C encoder before
timing it, and the full test suite (1001 tests, including the differential
and randomized comparisons against the C library) passes. All source changes
are kept. The results section at the end gives the per-quality and per-group
tallies; the [per-case CSV](2026-09-07-third-pass.csv) holds every
comparison with the previous record's percentage beside it.

## Contract

- Work: the 608 harness-mirrored cases of `benches/compress.rs` (cold,
  reused, preallocated, tiny, streaming, flush; qualities 0–11; the synthetic
  and vendored corpora), as in the
  [previous record](2026-09-06-greedy-runs.md).
- Correctness: output bytes identical to the C encoder for every quality and
  API shape; no `unsafe`; baseline and SIMD paths identical.
- Target: each case at or above 95% of the C throughput
  (`100 * C time / Rust time`).
- Resources: no new dependencies; no new allocation in the search loops.
  The former guarantee that a warmed compressor allocates nothing was dropped
  during the pass on the owner's instruction: a reused deep matcher takes
  its dense table on its second stream and keeps it.

## Method

Same scratch harness as the previous record (source beside it as
[`2026-09-06-greedy-runs-harness.rs.txt`](2026-09-06-greedy-runs-harness.rs.txt);
this pass added a `count` mode for callgrind that runs no warm-up on the C
side), pinned to one performance core, **with glibc trimming and the mmap
threshold disabled** (`MALLOC_TRIM_THRESHOLD_=1000000000 MALLOC_TOP_PAD_=67108864
MALLOC_MMAP_THRESHOLD_=1000000000`) for every case, so the C side's per-call
tables are recycled rather than faulted in. The previous record's sweep ran
without those variables, which is why its quality 7 and 9 percentages (and,
less visibly, its streaming quality 2 ones) were higher than a fair
comparison gives; its trimming-free re-measurements (8 of 41 at quality 7,
33 of 69 at quality 9) are the comparable baseline for those two qualities.

Diagnosis used `perf record -e cpu-clock:u` for the first cut and callgrind
(valgrind 3.24, `--cache-sim=yes`, `--dump-instr=yes` for per-instruction
listings, `--branch-sim=yes` once) for instruction counts, per-function call
counts and basic-block execution counts, on both implementations of the same
case. Two measurement traps found on the way:

- The harness's `count` mode ran one warm-up compression before the timed
  ones for reused groups, so the Rust side's callgrind totals were `(n+1)/n`
  of the truth; it made `population_cost` look like it was called twice as
  often as the reference's and the quality 5 loop look 33% heavier than it
  is. The C side runs no warm-up now, and every Rust total was rechecked.
- With the mmap threshold raised, every `vec![0; big]` is a real `memset`
  while the reference's `malloc` of the same table is not, so the harness
  understates Rust on any cold case that allocates a multi-mebibyte table.
  Criterion (default allocator) is the arbiter for those: cold quality 7 on
  the incompressible corpus measured 82% there against 76% before the pass
  (`taskset -c 6 cargo bench --bench compress -- 'cold/q7/.*/incompressible-256KiB'`;
  the benchmark ids are `cold/q7/<implementation>/<corpus>`).

## Diagnosis

- **Quality 10/11, short and repetitive inputs.** A sixteen-byte call
  allocated and zeroed a 32 MiB binary-tree forest and a mebibyte of literal
  prices; with allocations served from the heap it took 1.3 ms against
  0.15 ms. The literal-cost estimator read every byte through the ring-buffer
  mask with a bounds check, and called the library `log2` at every position
  for a window count that changes rarely; `is_mostly_utf8` copied every
  sequence through a four-byte window. The repetitive corpus spent a third of
  its time in the estimator and a quarter in `memset`.
- **Quality 10/11, binary input.** `population_cost` executed 32 instructions
  per used symbol against the reference's 24: `(log2p + 0.5) as usize` is a
  saturating conversion with two comparisons and a sign fix-up.
- **Quality 0/1, binary and incompressible input.** The scan loop executed
  45 instructions per position against the reference's 25: four
  bounds-checked word loads, a reload of the current word across the table
  store, and a sign test the reference does not make.
- **Quality 7–9 on inputs below half their table.** The on-demand layout's
  store — index entry, generation check, starter-or-full block, encoded
  offset — cost several times the reference's counter-and-slot store, and
  its index-then-block loads are dependent. A quarter-mebibyte incompressible
  input at quality 7 executed 1.7× the reference's instructions.
- **Streams of unknown length on qualities 5/6.** A writer flushing every
  64 KiB stored every position through the same on-demand index: 67% of the
  flush cases' time.
- **Tiny one-shot inputs at qualities 5–9.** The compact map fed starter
  blocks that grew into full blocks; ninety instructions per stored position.
- **Quality 5/6 binary input.** Instruction parity with the reference
  (callgrind, corrected for the warm-up), but 84% of its speed; the tagged
  candidate walk ran as two loops over two masks.

## Changes kept

- `BinaryTreeMatcher::prepare(one_shot, input_size)` sizes the forest to
  `2 * input_size` links for a one-shot stream, as `HashMemAllocInBytes`
  does, and grows it only when a later stream needs more; the buckets are
  written once with the empty marker instead of zeroed and refilled.
  `ZopfliCostModel::reserve` sizes the literal prices per block instead of
  the constructor sizing them for the largest block.
- `literal_cost::contiguous_block` gives the estimators one slice for
  `pos..pos + len` (borrowed when the block does not wrap, copied into the
  arena otherwise); the loops index that slice; a one-entry `Log2Memo` in
  front of `fast_log2` for the window and histogram counts. `is_mostly_utf8`
  scans contiguous runs in place and counts eight ASCII bytes per word.
- `population_cost` narrows the depth through `u8` and clamps with `min`.
- Quality 0/1 scan: the word at the next position is loaded once, hashed and
  kept for the candidate compares of the next iteration; `repeated < ip` is
  tested before the repeat candidate is loaded.
- Bucket matcher: the dense limit of the deep shapes is an eighth of the
  table on a matcher's first stream and a sixty-fourth from its second
  (`streams`; it was half), and a tagged shape treats an unknown length as
  long (`expected_input`). A reused encoder takes a new size hint over
  instead of being rebuilt when the hint resolves to the same shape and
  match-finder variant (`GreedyEncoder::retarget`). The compact one-shot layout links each store into a
  per-bucket chain (`chain: Vec<u64>`) that a search walks newest to oldest
  through the same candidate filter, replacing the starter and full-block
  pools for those streams. The tagged candidate walk is one loop over a
  rotated mask (`rotate_candidates`).
- Quick matcher: `SmallSlots` packs the slot and the position into one word
  (half the clearing) and is only used for a one-shot stream the packing can
  hold; the single-slot shapes read and overwrite their slot with one probe
  (`QuickSlots::replace`).

## Regressions found during the pass and reverted

- Outlining `commit_match` (`#[inline(never)]`) to halve the search loop's
  code size: quality 5 binary fell from 84% to 74%, quality 6 to 74%,
  quality 8 to 81%. Reverted.
- A dense limit of a sixty-fourth of the table for the deep shapes: the
  reused and incompressible cases gained (quality 7 incompressible 71% →
  113%, quality 8 binary 82% → 96%), but every cold call on a short
  compressible input paid for clearing the table (quality 7 quickfox 32%,
  quality 8 compressible 26%). A reuse-aware limit — an eighth on a
  matcher's first stream, a sixty-fourth from its second — kept both, but
  allocated the table on the second stream, which breaks the guarantee that
  a warmed compressor allocates nothing (`tests/compressor_memory.rs`).
  That guarantee was then dropped on request — the crate's one contract is
  the reference's bytes — and the reuse-aware limit kept: reused quality 7
  binary 81% → 97%, quality 7 alice29 78% → 95%, quality 8 binary 82% → 93%;
  the allocator-instrumented tests warm a compressor twice before counting.

## Remaining gaps

See the results section. The quick-matcher tiny cases (qualities 2–4 on at
most a kibibyte) are diffuse — bounds checks and the map at fifty-five
instructions per position against the reference's eighteen on an
uninitialised table; the quality 0 binary scan keeps three bounds-checked
loads per position the compiler cannot fold; quality 6 binary is at
instruction parity but 86% of the speed; cold calls at qualities 7–9 on
inputs the dense table serves pay for zeroing what the reference reads
uninitialised; quality 10/11 on incompressible input is at 88–94%.

## Results (harness sweep)

One round of 120 ms per side per case (360 ms for qualities 10 and 11);
qualities 7–11 were re-measured after the dense-limit, retargeting and
UTF-8 changes landed (two rounds of 150 ms at 7–9). Percentages are
`100 * C mean time / Rust mean time`. "Before" is the previous record's
sweep, which ran without the allocator settings above; for qualities 7 and
9 its comparable trimming-free counts were 8 of 41 and 33 of 69.
**Passing 366 of 608 (previous sweep on the same cases: 347).**

| Quality | Passing / measured | Median % of C | Weakest case | Passing before | Median before |
| --- | ---: | ---: | --- | ---: | ---: |
| 0 | 29 / 41 | 100.7 | 81.9 (cold/q0/binary-256KiB) | 22 / 41 | 98.8 |
| 1 | 37 / 45 | 99.6 | 90.9 (tiny/q1/64) | 28 / 45 | 97.6 |
| 2 | 34 / 69 | 94.5 | 67.6 (cold/q2/text-1KiB) | 41 / 69 | 95.5 |
| 3 | 25 / 41 | 95.9 | 78.5 (tiny/q3/64) | 26 / 41 | 96.4 |
| 4 | 28 / 41 | 96.7 | 82.8 (tiny/q4/1024) | 27 / 41 | 97.2 |
| 5 | 28 / 69 | 93.2 | 81.2 (cold/q5/vendor-random_org_10k.bin) | 23 / 69 | 88.2 |
| 6 | 15 / 41 | 92.3 | 79.6 (presized/q6/vendor-random_org_10k.bin) | 14 / 41 | 88.4 |
| 7 | 18 / 41 | 93.6 | 77.7 (cold/q7/vendor-mapsdatazrh) | 30 / 41 | 106.5 |
| 8 | 21 / 41 | 95.2 | 79.4 (cold/q8/binary-256KiB) | 9 / 41 | 91.0 |
| 9 | 39 / 69 | 96.1 | 80.8 (cold/q9/vendor-quickfox_repeated) | 68 / 69 | 234.9 |
| 10 | 31 / 41 | 97.5 | 92.4 (cold/q10/vendor-plrabn12.txt) | 10 / 41 | 90.0 |
| 11 | 61 / 69 | 99.1 | 92.1 (reused/q11/incompressible-256KiB) | 49 / 69 | 96.9 |

| API group | Passing / measured | Median % of C | Passing before | Median before |
| --- | ---: | ---: | ---: | ---: |
| Cold one-shot | 69 / 132 | 95.2 | 66 / 132 | 94.8 |
| Reused compressor | 86 / 132 | 97.8 | 80 / 132 | 97.4 |
| Preallocated output | 87 / 132 | 97.8 | 78 / 132 | 96.7 |
| Tiny, cold | 15 / 48 | 91.7 | 6 / 48 | 87.0 |
| Tiny, reused | 42 / 48 | 102.0 | 38 / 48 | 99.5 |
| Streaming writer | 19 / 32 | 95.8 | 23 / 32 | 98.3 |
| Streaming reader | 12 / 32 | 94.2 | 20 / 32 | 96.8 |
| Streaming session | 18 / 32 | 95.3 | 22 / 32 | 100.2 |
| Flush | 18 / 20 | 100.9 | 14 / 20 | 101.4 |

The raw sweep output is in
[`2026-09-07-third-pass-sweep.txt`](2026-09-07-third-pass-sweep.txt) and the
harness source in
[`2026-09-07-third-pass-harness.rs.txt`](2026-09-07-third-pass-harness.rs.txt).

Cases below 95%, by quality (the five weakest of each):

```text
Still failing: 242
  q0: 12  cold/binary-256KiB 82, reused/binary-256KiB 84, cold/vendor-random_org_10k.bin 85, presized/binary-256KiB 86, presized/vendor-random_org_10k.bin 86
  q1: 8  tiny/64 91, tiny/256 92, tiny/16 92, flush/256 92, reused/vendor-plrabn12.txt 93
  q2: 35  cold/text-1KiB 68, tiny/1024 68, tiny/256 73, tiny/64 79, tiny/16 83
  q3: 16  tiny/64 78, tiny/16 79, cold/text-1KiB 82, tiny/1024 83, tiny/256 83
  q4: 13  tiny/1024 83, cold/vendor-random_org_10k.bin 85, tiny/256 86, tiny/16 87, tiny/64 88
  q5: 41  cold/vendor-random_org_10k.bin 81, tiny/1024 84, cold/text-1KiB 84, presized/vendor-random_org_10k.bin 86, cold/binary-256KiB 87
  q6: 26  presized/vendor-random_org_10k.bin 80, cold/vendor-quickfox_repeated 82, presized/text-1MiB 84, cold/binary-256KiB 84, tiny/1024 85
  q7: 23  cold/vendor-mapsdatazrh 78, cold/vendor-alice29.txt 78, cold/vendor-random_org_10k.bin 78, cold/binary-256KiB 78, cold/vendor-lcet10.txt 80
  q8: 20  cold/binary-256KiB 79, cold/vendor-mapsdatazrh 83, cold/vendor-alice29.txt 85, cold/vendor-random_org_10k.bin 85, cold/vendor-lcet10.txt 88
  q9: 30  cold/vendor-quickfox_repeated 81, cold/compressible-256KiB 81, cold/vendor-lcet10.txt 82, cold/vendor-alice29.txt 82, cold/vendor-plrabn12.txt 85
  q10: 10  cold/vendor-plrabn12.txt 92, cold/vendor-lcet10.txt 93, presized/vendor-alice29.txt 93, cold/text-1MiB 93, cold/vendor-alice29.txt 93
  q11: 8  reused/incompressible-256KiB 92, session/incompressible-256KiB 92, cold/incompressible-256KiB 93, session/vendor-lcet10.txt 94, reader/vendor-plrabn12.txt 94
```

Cold quality 7–9 cases (the sparse layout's store, and the table clear that
the reference skips by reading uninitialised memory) and the quality 0
binary cases (82–86%) are the largest groups left besides the tiny
quick-matcher calls; see "Remaining gaps".

## Criterion cross-check

`taskset -c 6 cargo bench --bench compress -- 'reused/q(5|7|8)/|tiny/q10/|cold/q7/.*/(incompressible|compressible|vendor-quickfox)'`,
default allocator, Criterion's own settings, taken before the reuse-aware
dense limit was reinstated (so its reused quality 7/8 rows show the sparse
layout; with the table the harness measures those cases at 91–97%). Median
times; the percentage is `100 * C / Rust`.

| Case | C, µs | Rust, µs | % of C |
| --- | ---: | ---: | ---: |
| cold/q7/compressible-256KiB | 102.9 | 43.8 | 235.0 |
| cold/q7/incompressible-256KiB | 528.7 | 687.0 | 77.0 |
| cold/q7/vendor-quickfox_repeated | 62.6 | 28.8 | 217.6 |
| reused/q5/binary-256KiB | 2638.1 | 3013.6 | 87.5 |
| reused/q5/incompressible-256KiB | 341.1 | 285.0 | 119.7 |
| reused/q5/text-1MiB | 2623.7 | 2690.4 | 97.5 |
| reused/q5/vendor-alice29.txt | 2294.9 | 2445.6 | 93.8 |
| reused/q5/vendor-plrabn12.txt | 8058.8 | 8652.8 | 93.1 |
| reused/q7/binary-256KiB | 5361.0 | 6600.1 | 81.2 |
| reused/q7/incompressible-256KiB | 598.3 | 730.6 | 81.9 |
| reused/q7/text-1MiB | 2441.1 | 2811.0 | 86.8 |
| reused/q7/vendor-lcet10.txt | 8651.4 | 10212.0 | 84.7 |
| reused/q8/binary-256KiB | 6830.1 | 8410.9 | 81.2 |
| reused/q8/incompressible-256KiB | 688.6 | 728.7 | 94.5 |
| reused/q8/text-1MiB | 3095.3 | 3511.2 | 88.2 |
| reused/q8/vendor-plrabn12.txt | 14619.0 | 16295.0 | 89.7 |

Criterion agrees with the harness on the compressible and repetitive
corpora and reads the quality 5 binary case lower (87.5% against the
harness's 93%). The previous Criterion record had reused quality 7 at
68–87% and quality 8 at 82–91%.
