# Burli review: where it beats mbrotli, and what was fixed

[Benchmark index and per-quality results](README.md)

This review re-ran the cases where [Burli](https://github.com/paddor/burli)
beats mbrotli, separated the leads that come from Burli's different
compression policy from the ones that were mbrotli overhead, and fixed the
latter. Burli 0.3.1 is the current release; the clone used here is its `main`
at `d7d1790` (2026-08-19), which carries the same crate version the
comparison harness pins. Burli implements q0–q5 only.

mbrotli's contract is byte identity with the pinned C encoder, so a Burli lead
that comes from encoding the input differently cannot be ported. Every change
below keeps the output identical; the differential test suite against the C
library confirms it.

## What the sweep showed

Comparison harness, cold end-to-end API, window 22, 30 samples, pinned to one
core. Burli was ahead on 13 of 48 q0–q5 cases. The C column is the pinned
reference on the same run and is the yardstick for "overhead": a case where
mbrotli already matches or beats C cannot be closed further without changing
the bytes.

| Corpus | Quality | mbrotli | C | Burli | Verdict |
| --- | ---: | ---: | ---: | ---: | --- |
| tiny-text (44 B) | 0 | 443 ns | 1.80 µs | 211 ns | overhead: fixed |
| tiny-text | 4 | 8.12 µs | 7.59 µs | 4.83 µs | overhead vs C: reduced; rest is policy |
| tiny-text | 5 | 8.40 µs | 8.43 µs | 5.17 µs | policy |
| alice29 | 1 | 740 µs | 669 µs | 697 µs | loop gap vs C: reduced |
| alice29 | 3 | 1.749 ms | 1.674 ms | 1.640 ms | loop gap vs C |
| alice29 | 4 | 2.659 ms | 2.285 ms | 2.460 ms | loop gap vs C |
| alice29 | 5 | 3.717 ms | 3.647 ms | 3.507 ms | policy (Burli's q5 is shallower) |
| text-1m | 0 | 4.41 ms | 8.07 ms | 0.63 ms | policy |
| text-1m | 1 | 1.886 ms | 1.850 ms | 0.783 ms | policy |
| binary-64k | 4 | 101 µs | 99.6 µs | 60.6 µs | policy |
| random-64k | 0 | 16.2 µs | 15.2 µs | 6.9 µs | policy |
| random-1m | 0 | 130 µs | 167 µs | 110 µs | policy |
| repeated-1m | 0 | 209 µs | 259 µs | 60 µs | policy |

Policy leads, with the Burli source that produces them
(`crates/encode/src/encode/{mod,q0,sparse,tune}.rs`):

- **Repeated and cyclic text at q0/q1.** Burli uses meta-blocks as large as
  the window when the input fits, so `text-1m` compresses to 82,461 bytes
  against C's 190,491 and `repeated-1m` to 21 bytes against 712. The speed
  follows from doing one block's work instead of many.
- **Incompressible input at q0.** `sparse::decision` samples 64 KiB blocks
  and stores them verbatim when few six-byte duplicates are found. C hashes
  every position and only decides at `emit_remainder`
  (`ShouldUseUncompressedMode`), and so does mbrotli, at C's speed.
- **`binary-64k` q4 and `alice29` q5.** Burli's q4 and q5 collectors use
  smaller tables and fewer probes than C's `H4` and `H5` (13 bucket bits and
  8 slots at q5 against C's 14 and 16), which is why its output is larger
  (2,593 against 2,554 bytes; 54,231 against 52,809) and faster.
- **Tiny input at q4/q5.** Both C and mbrotli build the literal, command and
  distance prefix codes, then discard them when the stored form is smaller.
  Burli's simpler coding path is cheaper. The remaining mbrotli-versus-C gap
  on this case was overhead, addressed below.

## Changes

All four keep the output byte-identical; callgrind instruction counts
(`Ir`, deterministic) are the evidence for the loop changes because the
machine was running a 24-core AFL campaign during this review, which made
wall-clock timing noisy (±15% between repeats).

1. **Quality 0 arena and table are not built for a verbatim final fragment.**
   A fresh compressor allocated and zeroed the 8.5 KiB one-pass arena, its
   4 KiB Huffman node pool and a 2 KiB hash table before the tiny-final
   shortcut ran. `FastCore::OnePass` now holds `Option<Box<OnePassArena>>`,
   built by the first fragment that scans, and `prepare_table` skips the
   table for a fragment `q0::stores_verbatim` accepts. Cold tiny q0:
   0.44 µs → 0.13 µs (C 1.8 µs, Burli 0.21 µs).
2. **Huffman node pool sized per build.** `MetaBlockWriter::store_meta_block`
   resized the pool to the full command alphabet (1,409 nodes, 11 KiB) on
   every call, and the fast arenas pre-sized theirs to 513 nodes; C's pool is
   uninitialised. `huffman::nodes_for` grows the pool to `2 × used + 1`
   nodes inside the build and never clears it. Cold tiny q4: 110K → 102K
   instructions per call (−7%); the memset share fell from 16% to 6%.
3. **Match length settles the first word before cutting windows.**
   `match_len_at` measured every candidate through the staged
   scalar/vector pipeline, whose slicing cost about 30 instructions before
   the first compare. It now loads eight bytes from each side and answers a
   difference with a trailing-zero count, continuing into the pipeline only
   when the whole word matched. q1 alice29: match-length instructions
   6.3M → 5.1M per ten runs; the same path serves q0 and every greedy
   matcher.
4. **Fast-path hash tables as fixed-width arrays; pass-one buffers owned by
   the scan.** The q0/q1 scans re-sliced the table to `1 << TABLE_BITS` and
   relied on that to fold the bounds checks; inside the backend-specific
   `vectorize` function the compiler did not carry the proof, and every slot
   access kept a compare-and-branch. The table is now passed as
   `&mut [i32; 1 << TABLE_BITS]`. q1 also moves its command and literal
   vectors into locals for the scan, because behind the caller's references
   their headers were reloaded after every table store. q1 alice29 scan:
   40.2M → 35.3M instructions per ten runs (C: 26.6M); q0 alice29 A/B:
   68.9M → 67.4M.

`fuzz/afl` also had a failing replay on `master`: the dictionary target's
oracle asserted that a *requested* attachment count above fifteen is refused,
but a payload shorter than the requested cut yields fewer attachments, which
the builder rightly accepts. The oracle now checks the attached count, which
is also what the error reports. Both replay configurations pass.

## What still separates mbrotli from C

Measured with callgrind on the cold API over alice29; the Criterion figures
in the final table are the timings to quote:

- q1: the scan is still about a third more instructions than C's, from the
  bounds checks on the eight-byte loads (three instructions each, two to
  three per position) and the `Vec` pushes; removing them would need
  unchecked loads.
- q4: the quick matcher runs 82M instructions per five runs against C's
  64M. The extra comes from spills of the by-value `MatchQuery`, the
  `read_u8` bounds checks, and the dictionary probe.
- Tiny q4/q5: at instruction parity with C now; the remaining time gap is
  IPC, and Burli stays ahead on policy.

## Final comparison

Same harness, same settings, saved as Criterion baseline `burli-review-after`
(144 cases: mbrotli, C and Burli at q0–q5 over the eight corpora). The run
shared the machine with the 24-fuzzer AFL campaign, so absolute times are
5–15% slower than the first sweep and single-digit differences between
columns are within its noise. "C / mbrotli" above 1 means mbrotli is faster
than the reference; "mbrotli / Burli" above 1 means mbrotli is faster than
Burli.

| Corpus | Quality | mbrotli | C | Burli | C / mbrotli | mbrotli / Burli |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| empty | 0 | 57 ns | 28 ns | 156 ns | 0.49× | 2.72× |
| empty | 1 | 47 ns | 49 ns | 198 ns | 1.04× | 4.18× |
| empty | 2 | 55 ns | 46 ns | 149 ns | 0.83× | 2.70× |
| empty | 3 | 50 ns | 49 ns | 198 ns | 0.98× | 4.00× |
| empty | 4 | 50 ns | 44 ns | 174 ns | 0.88× | 3.48× |
| empty | 5 | 49 ns | 47 ns | 209 ns | 0.96× | 4.22× |
| tiny-text | 0 | 139 ns | 1.9 µs | 171 ns | 13.31× | 1.23× |
| tiny-text | 1 | 3.5 µs | 2.9 µs | 4.7 µs | 0.85× | 1.34× |
| tiny-text | 2 | 2.4 µs | 2.0 µs | 4.4 µs | 0.83× | 1.86× |
| tiny-text | 3 | 4.3 µs | 3.1 µs | 4.8 µs | 0.73× | 1.11× |
| tiny-text | 4 | 8.4 µs | 8.1 µs | 4.9 µs | 0.97× | 0.58× |
| tiny-text | 5 | 8.4 µs | 8.2 µs | 4.9 µs | 0.98× | 0.58× |
| alice29 | 0 | 526.0 µs | 517.7 µs | 567.3 µs | 0.98× | 1.08× |
| alice29 | 1 | 776.9 µs | 751.6 µs | 704.8 µs | 0.97× | 0.91× |
| alice29 | 2 | 1.530 ms | 1.337 ms | 1.571 ms | 0.87× | 1.03× |
| alice29 | 3 | 1.831 ms | 1.555 ms | 1.616 ms | 0.85× | 0.88× |
| alice29 | 4 | 2.724 ms | 2.415 ms | 2.444 ms | 0.89× | 0.90× |
| alice29 | 5 | 3.985 ms | 3.431 ms | 3.378 ms | 0.86× | 0.85× |
| text-1m | 0 | 4.671 ms | 9.200 ms | 592.9 µs | 1.97× | 0.13× |
| text-1m | 1 | 2.141 ms | 1.905 ms | 765.7 µs | 0.89× | 0.36× |
| text-1m | 2 | 1.805 ms | 2.289 ms | 3.454 ms | 1.27× | 1.91× |
| text-1m | 3 | 1.933 ms | 3.210 ms | 2.598 ms | 1.66× | 1.34× |
| text-1m | 4 | 2.664 ms | 3.045 ms | 2.665 ms | 1.14× | 1.00× |
| text-1m | 5 | 2.797 ms | 3.200 ms | 4.561 ms | 1.14× | 1.63× |
| binary-64k | 0 | 11.9 µs | 17.5 µs | 17.3 µs | 1.47× | 1.45× |
| binary-64k | 1 | 17.5 µs | 20.9 µs | 21.0 µs | 1.19× | 1.20× |
| binary-64k | 2 | 34.1 µs | 50.3 µs | 32.2 µs | 1.47× | 0.94× |
| binary-64k | 3 | 39.4 µs | 53.8 µs | 34.7 µs | 1.37× | 0.88× |
| binary-64k | 4 | 58.3 µs | 58.2 µs | 35.7 µs | 1.00× | 0.61× |
| binary-64k | 5 | 108.2 µs | 86.6 µs | 157.8 µs | 0.80× | 1.46× |
| random-64k | 0 | 9.5 µs | 10.0 µs | 4.3 µs | 1.06× | 0.45× |
| random-64k | 1 | 7.8 µs | 9.4 µs | 196.6 µs | 1.20× | 25.20× |
| random-64k | 2 | 28.0 µs | 31.8 µs | 179.9 µs | 1.14× | 6.43× |
| random-64k | 3 | 38.5 µs | 34.1 µs | 183.6 µs | 0.88× | 4.77× |
| random-64k | 4 | 48.0 µs | 46.8 µs | 188.8 µs | 0.98× | 3.93× |
| random-64k | 5 | 87.2 µs | 71.4 µs | 423.8 µs | 0.82× | 4.86× |
| random-1m | 0 | 73.7 µs | 121.8 µs | 90.9 µs | 1.65× | 1.23× |
| random-1m | 1 | 112.7 µs | 158.8 µs | 1.574 ms | 1.41× | 13.97× |
| random-1m | 2 | 481.2 µs | 542.1 µs | 2.736 ms | 1.13× | 5.69× |
| random-1m | 3 | 642.9 µs | 567.7 µs | 2.755 ms | 0.88× | 4.28× |
| random-1m | 4 | 1.234 ms | 1.175 ms | 3.303 ms | 0.95× | 2.68× |
| random-1m | 5 | 1.533 ms | 1.597 ms | 4.631 ms | 1.04× | 3.02× |
| repeated-1m | 0 | 159.8 µs | 170.7 µs | 34.6 µs | 1.07× | 0.22× |
| repeated-1m | 1 | 40.1 µs | 69.5 µs | 39.0 µs | 1.73× | 0.97× |
| repeated-1m | 2 | 100.9 µs | 477.7 µs | 115.8 µs | 4.73× | 1.15× |
| repeated-1m | 3 | 101.1 µs | 497.9 µs | 177.3 µs | 4.93× | 1.75× |
| repeated-1m | 4 | 211.9 µs | 545.3 µs | 251.9 µs | 2.57× | 1.19× |
| repeated-1m | 5 | 288.9 µs | 927.5 µs | 1.826 ms | 3.21× | 6.32× |

Burli is still ahead on 14 cases. Tiny q0, the one case where its lead
was mbrotli overhead, now runs at 139 ns against Burli's 171 ns and C's
1.9 µs. Every other lead is one of the policy cases listed above, or within
the run's noise (binary-64k q2/q3 and repeated-1m q1 flip between runs; the
first sweep had mbrotli ahead on all three). The alice29 q1–q5 leads remain,
and they are the same size as mbrotli's remaining gap to C on those inputs:
Burli is not faster than C there, so closing them means closing the greedy
and two-pass loop gaps described above.

