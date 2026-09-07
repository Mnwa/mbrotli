# Quality 11 review against SIMD Brotli — 2026-09-07

[Benchmark index](README.md) · [Competitor-guided follow-up](competitor-paths.md)

The [competitor-guided follow-up](competitor-paths.md) left SIMD Brotli
(the `simd-brotli` fork of Rust Brotli, 10.0.1 at `76fea39`, checked out under
`/tmp/mbrotli-competitors/simd-brotli`) ahead of mbrotli on Alice at quality
eleven by about 20%. This review explains that lead from the source and from
instruction profiles, and closes the part of it that does not change the
compressed output. Encoder revision before the changes: `3ea31b4`.

## Where the fork's lead came from

Callgrind on one cold compression of `alice29.txt` at quality eleven, all
three encoders in the same harness binary:

| Encoder | Instructions | Output |
| --- | ---: | ---: |
| Google C Brotli 1.2.0 | 1,918,012,248 | 46,487 B |
| mbrotli before | 2,097,121,464 | 46,487 B |
| SIMD Brotli | 1,735,973,888 | 46,493 B |
| mbrotli after | 1,713,589,482 | 46,487 B |

Three things separated mbrotli from the fork:

1. **Block-splitter refinement, ~110 M instructions.** The fork inherits an
   upstream Rust Brotli policy of three refinement iterations at every
   quality (`iters = if quality <= 11 { 3 } else { 10 }` in its
   `block_splitter.rs`), where the C reference and mbrotli run ten at quality
   eleven. Its splitter costs 33 M instructions to mbrotli's 146 M, and that
   is why its quality-eleven output differs from C's (46,493 versus 46,487
   bytes on Alice, 47,488 versus 47,477 at quality ten). mbrotli keeps C's
   partition, so this part of the lead is not portable; it remains the whole
   of the fork's residual advantage on Alice.
2. **The H10 short backward scan, ~150 M.** At quality eleven the tree search
   first walks the sixty-three positions before the current one, comparing two
   bytes at each. mbrotli did that byte by byte through `Option` compares
   (17 instructions per candidate); the fork compares thirty-two candidates
   per vector. `scan_recent_positions` in `hq/h10.rs` now does the same with
   `u8x32`, visiting agreeing lanes nearest first so the match list is
   unchanged. `find_all_matches` fell from 278 M to 125 M instructions.
3. **The `update_nodes` distance-cache loop, ~250 M.** Sixteen probes for each
   of up to five start positions at every byte — 24 M iterations on Alice —
   ran at 33 instructions per probe with seven stack reloads. The loop now
   loads one packed probe, admits the window distances with a single unsigned
   compare, reads the ring through a mask-bounded slice so the out-of-window
   test and the bounds check are one `get`, and prices the reached lengths by
   iterating the node slice. It runs at 23 instructions per probe; the C
   reference's own loop is 30. Details and the equivalence argument are in
   [the HQ specification](../../architecture/hq-encoder.md#4-the-dynamic-program).

## Matched Criterion results

`benches/compress.rs`, `reused/q10` and `reused/q11` groups, both encoders,
pinned to logical CPU 4, ten samples, 0.5 s warm-up, 2 s measurement. The
machine (i7-13700KF, WSL2) was running a 21-process AFL campaign throughout,
so absolute times are higher than the records in this directory and the
C column shows the run-to-run drift: C is unchanged code, so its ratio is the
noise floor of each pair.

| Case | C before | C after | C ratio | mbrotli before | mbrotli after | mbrotli ratio |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| q11 text-1KiB | 2.429 ms | 2.171 ms | 1.12 | 2.398 ms | 2.098 ms | 1.14 |
| q11 text-1MiB | 2815.6 ms | 2716.6 ms | 1.04 | 3139.2 ms | 2696.5 ms | 1.16 |
| q11 binary-256KiB | 700.8 ms | 687.5 ms | 1.02 | 563.7 ms | 508.5 ms | 1.11 |
| q11 compressible-256KiB | 9.436 ms | 10.002 ms | 0.94 | 7.086 ms | 7.119 ms | 1.00 |
| q11 incompressible-256KiB | 60.41 ms | 60.65 ms | 1.00 | 65.02 ms | 45.92 ms | 1.42 |
| q11 alice29.txt | 192.4 ms | 193.1 ms | 1.00 | 192.2 ms | 168.1 ms | 1.14 |
| q11 lcet10.txt | 570.8 ms | 593.5 ms | 0.96 | 563.7 ms | 488.5 ms | 1.15 |
| q11 plrabn12.txt | 611.7 ms | 618.4 ms | 0.99 | 635.8 ms | 548.0 ms | 1.16 |
| q11 mapsdatazrh | 545.2 ms | 554.4 ms | 0.98 | 382.9 ms | 335.5 ms | 1.14 |
| q11 random_org_10k.bin | 15.05 ms | 15.03 ms | 1.00 | 9.200 ms | 8.917 ms | 1.03 |
| q11 quickfox_repeated | 5.612 ms | 5.775 ms | 0.97 | 4.637 ms | 4.668 ms | 0.99 |
| q10 text-1MiB | 132.7 ms | 127.9 ms | 1.04 | 160.9 ms | 149.9 ms | 1.07 |
| q10 incompressible-256KiB | 37.80 ms | 35.03 ms | 1.08 | 39.76 ms | 33.94 ms | 1.17 |
| q10 alice29.txt | 71.08 ms | 68.47 ms | 1.04 | 72.47 ms | 66.89 ms | 1.08 |
| q10 lcet10.txt | 231.2 ms | 223.8 ms | 1.03 | 223.8 ms | 205.9 ms | 1.09 |
| q10 mapsdatazrh | 204.3 ms | 192.1 ms | 1.06 | 145.6 ms | 127.0 ms | 1.15 |

Quality eleven gains **10–16% on text and 42% on incompressible input** with
C flat; the two cases at 1.00 are dominated by work outside the search.
Quality ten's before run overlapped the test suite, so its C column drifts
by 3–8%; net of that drift the quality-ten text gain is about 4%, and the
incompressible gain about 9%. Every compressed size is unchanged and every
case still validates through the C decoder before timing.

## Scratch harness sweep against the fork

A paired harness (one process, C, mbrotli and the fork interleaved, best of
1.5 s per case, glibc trimming disabled, logical CPU 2, same loaded machine)
over every quality and the comparison corpora, after the change:
[raw table](hq-q11-review-sweep.txt). Against the fork, mbrotli is now
faster or equal everywhere except:

| Corpus | Quality | mbrotli | SIMD Brotli | Fork lead | Cause |
| --- | ---: | ---: | ---: | ---: | --- |
| alice29 | 11 | 171.5 ms | 160.4 ms | 7% | Three splitter iterations against ten; output differs |
| tiny (44 B) | 10 | 0.195 ms | 0.185 ms | 5% | Cold allocation; mbrotli executes fewer instructions (3.9 M against 4.3 M per call) |
| tiny (44 B) | 11 | 0.222 ms | 0.211 ms | 5% | As above |
| alice29 | 9 | 7.66 ms | 7.43 ms | 3% | Within this box's ±3% noise; mbrotli executes 98.7 M instructions to the fork's 109.3 M |

The quality-nine and tiny rows were re-timed with eight-second runs and
profiled; neither shows a code path the fork does more cheaply, so no change
was made for them. Before this pass the same harness had the fork ahead on
Alice q11 by 16%, on 1 MiB text q11 by 11%, and on random 64 KiB q11 by 22%;
those are now 7% behind, 4% ahead and 63% ahead respectively.

## Reproduction

```sh
# Matched Criterion pairs (build the before binary from 3ea31b4 in a worktree,
# then run both with the same options).
cargo bench --bench compress --locked --no-run
taskset -c 4 target/release/deps/compress-<hash> --bench 'reused/q1[01]/' \
  --sample-size 10 --warm-up-time 0.5 --measurement-time 2 --noplot

# Instruction counts (valgrind 3.24 built from source, see the profiling note).
valgrind --tool=callgrind --dump-instr=yes --dump-line=no <harness> run alice29 11 rust 1
```

## Correctness

- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets
  --all-features --locked -- -D warnings`, and `cargo test --workspace
  --all-features --locked` pass, including the C byte-identity suites.
- New test `the_vector_short_scan_agrees_with_the_byte_walk_on_every_backend`
  compares the vector scan against the transcribed reference walk on every
  host backend, with and without ring wrap, over five corpora, every
  position, six scan lengths and three backward limits.
- `cargo llvm-cov` over the library and the dictionary, differential and
  round-trip suites: every function in `hq/h10.rs`, `hq/zopfli.rs` and
  `hq/nodes.rs` executes.
- No AFL run was added: the fuzzed public boundaries are unchanged, and the
  host was already running the standing campaign in `fuzz/afl/campaign.sh`.
