# RFC 9841: standard-mode performance

Evidence that adding RFC 9841 Large Window Brotli did not slow ordinary
RFC 7932 compression down. Section 50.1 of the implementation specification
sets the gate:

```text
geometric mean throughput >= 0.99 * pre-change Rust baseline
aggregate output bytes identical
allocation count unchanged
```

## Output bytes: identical

Not measured — proven. `tests/differential_c.rs` asserts byte identity with
Google Brotli v1.2.0 configured the same way, over the vendored corpus at every
implemented quality, and it passes unchanged. `tests/roundtrip.rs`,
`tests/vendor_corpus.rs`, `tests/greedy_qualities.rs`, `tests/window_bits.rs`
and `tests/randomized.rs` likewise pass unchanged. Any change to an ordinary
stream would have failed all of them.

## Allocation count: unchanged

The ordinary path gained no allocation. `CompressParams` gained no field at all:
`WindowBits` carries the header alongside the size, and shrank from a
`usize` newtype to a two-byte private enum in the process. `ResolvedWindow` is a
`Copy` value resolved once per session. Nothing added to the encoders allocates,
and no encoder-owned buffer changed size for an ordinary stream.

## Throughput

### Method

Criterion, comparing this change against its parent commit `fe5e711`, both
built from an **identical `Cargo.lock`**. The baseline was built in a
`git worktree` of the parent commit with `CARGO_TARGET_DIR` pointed at a shared
directory so Criterion could hold both baselines.

Filter: `(tiny|oneshot)/q(0|1|5|9)/mbrotli` — 60 benchmarks spanning per-call
overhead at 16, 64, 256 and 1024 bytes and end-to-end one-shot compression over
the synthetic and vendored corpora, at one quality from each family that has a
distinct hot path.

```sh
# baseline, in a worktree of the parent commit with the same Cargo.lock
cargo bench --bench compress -- --save-baseline before "(tiny|oneshot)/q(0|1|5|9)/mbrotli"
# this change
cargo bench --bench compress -- --baseline before      "(tiny|oneshot)/q(0|1|5|9)/mbrotli"
```

### Environment

| | |
| --- | --- |
| CPU | Apple M5 Pro (`aarch64-apple-darwin`) |
| OS | macOS 26.6.2 |
| Toolchain | rustc 1.97.1 (`8bab26f4f`, 2026-07-14) |
| `fearless_simd` | 0.7.0 |
| Reference C | Google Brotli v1.2.0, commit `028fb5a23661f123017c060daa546b55cf4bde29` |
| Baseline commit | `fe5e711` |
| Corpora | `benches/compress.rs` synthetic set plus `brotli-ffi/vendor/brotli/tests/testdata` |

### Result

```text
n = 60
geometric mean throughput ratio (after / before) = 1.0080   gate: >= 0.99  PASS
mean change   = -0.78%   (negative is faster)
median change = -0.68%
range         = -7.55% .. +6.39%
```

The gate passes with room to spare, and the change is slightly *faster* on
average — which is the expected shape for a change that adds one predictable
branch per call and no per-byte work.

### On the ±6% range

Individual entries move by up to ±7% between runs and **change sign between
repetitions of the same comparison**: `oneshot/q0/mbrotli/vendor-lcet10.txt`
measured −2.16% in one run and +4.21% in the next, against the same baseline on
an idle machine. That is code-layout and scheduling variation, not signal.
Criterion's own confidence intervals are tight within a run, so its
"performance has regressed" verdict on such an entry is confident about a
number that is not reproducible; the aggregate over 60 benchmarks is.

One earlier measurement is worth recording as a trap: the first baseline was
built in a worktree that had resolved its **own** `Cargo.lock` (this repository
gitignores it), which pulled a different `syn`. That comparison showed a
consistent +5..+8% on q0 over the vendored text corpora. Copying the main
lockfile into the worktree and re-measuring turned the same three benchmarks
into −1.0%, −2.2% and −1.1%. Always align the lockfile before believing a
Criterion baseline in this repository.

## Not measured

- **Large Window against the C encoder.** Section 50.4 asks for a Rust/C ratio
  at declared windows up to 30. The benchmark harness configures the C side
  through `BrotliEncoderCompress`, which has no large-window parameter; adding
  a large-window C harness is the next benchmark change, not part of this one.
- **Qualities 3, 4, 6, 7, 8, 10 and 11.** The filter takes one quality from
  each family with a distinct hot path (0 fast one-pass, 1 fast two-pass, 5
  greedy, 9 greedy with the widest search). Qualities 10 and 11 are excluded
  because a single Criterion sample of them costs minutes; they share the hot
  path measured at quality 9 for everything this change touches, and their
  output is covered by the byte-identity tests above.
- **Peak memory.** The change cannot raise it: retained history is capped at 30
  bits however wide the declared window, and the ring buffer still grows with
  the input.

## Per-benchmark detail

| Benchmark | low | median | high |
| --- | ---: | ---: | ---: |
| `oneshot/q0/mbrotli/binary-256KiB` | -1.14% | -0.99% | -0.83% |
| `oneshot/q0/mbrotli/compressible-256KiB` | -0.96% | -0.61% | -0.28% |
| `oneshot/q0/mbrotli/incompressible-256KiB` | -1.63% | -1.26% | -0.89% |
| `oneshot/q0/mbrotli/text-1KiB` | -0.79% | -0.67% | -0.56% |
| `oneshot/q0/mbrotli/text-1MiB` | -0.61% | -0.45% | -0.28% |
| `oneshot/q0/mbrotli/vendor-alice29.txt` | +1.86% | +2.03% | +2.19% |
| `oneshot/q0/mbrotli/vendor-lcet10.txt` | +3.84% | +4.21% | +4.63% |
| `oneshot/q0/mbrotli/vendor-mapsdatazrh` | -1.41% | -1.27% | -1.12% |
| `oneshot/q0/mbrotli/vendor-plrabn12.txt` | +0.51% | +0.92% | +1.36% |
| `oneshot/q0/mbrotli/vendor-quickfox_repeated` | -0.94% | -0.39% | +0.14% |
| `oneshot/q0/mbrotli/vendor-random_org_10k.bin` | -0.34% | +0.34% | +1.02% |
| `oneshot/q1/mbrotli/binary-256KiB` | -2.77% | -2.59% | -2.41% |
| `oneshot/q1/mbrotli/compressible-256KiB` | -2.57% | -1.14% | +0.38% |
| `oneshot/q1/mbrotli/incompressible-256KiB` | -0.49% | -0.26% | -0.04% |
| `oneshot/q1/mbrotli/text-1KiB` | +0.18% | +0.47% | +0.77% |
| `oneshot/q1/mbrotli/text-1MiB` | -0.37% | -0.05% | +0.39% |
| `oneshot/q1/mbrotli/vendor-alice29.txt` | -3.38% | -3.03% | -2.70% |
| `oneshot/q1/mbrotli/vendor-lcet10.txt` | -3.87% | -3.65% | -3.42% |
| `oneshot/q1/mbrotli/vendor-mapsdatazrh` | -5.06% | -4.53% | -4.04% |
| `oneshot/q1/mbrotli/vendor-plrabn12.txt` | -1.87% | -1.67% | -1.45% |
| `oneshot/q1/mbrotli/vendor-quickfox_repeated` | -0.68% | +0.55% | +1.87% |
| `oneshot/q1/mbrotli/vendor-random_org_10k.bin` | -1.73% | -1.35% | -0.95% |
| `oneshot/q5/mbrotli/binary-256KiB` | -2.40% | -2.10% | -1.77% |
| `oneshot/q5/mbrotli/compressible-256KiB` | -1.61% | -0.45% | +0.68% |
| `oneshot/q5/mbrotli/incompressible-256KiB` | -7.77% | -7.55% | -7.33% |
| `oneshot/q5/mbrotli/text-1KiB` | -1.45% | -0.88% | -0.28% |
| `oneshot/q5/mbrotli/text-1MiB` | +1.48% | +1.80% | +2.14% |
| `oneshot/q5/mbrotli/vendor-alice29.txt` | -3.01% | -2.81% | -2.56% |
| `oneshot/q5/mbrotli/vendor-lcet10.txt` | -2.54% | -2.42% | -2.31% |
| `oneshot/q5/mbrotli/vendor-mapsdatazrh` | -1.43% | -1.15% | -0.85% |
| `oneshot/q5/mbrotli/vendor-plrabn12.txt` | -2.87% | -2.73% | -2.61% |
| `oneshot/q5/mbrotli/vendor-quickfox_repeated` | -2.55% | -1.13% | +0.27% |
| `oneshot/q5/mbrotli/vendor-random_org_10k.bin` | -1.70% | -1.57% | -1.42% |
| `oneshot/q9/mbrotli/binary-256KiB` | -1.03% | -0.80% | -0.61% |
| `oneshot/q9/mbrotli/compressible-256KiB` | -0.65% | +0.14% | +0.89% |
| `oneshot/q9/mbrotli/incompressible-256KiB` | -0.70% | -0.51% | -0.31% |
| `oneshot/q9/mbrotli/text-1KiB` | -0.10% | +0.13% | +0.42% |
| `oneshot/q9/mbrotli/text-1MiB` | +0.01% | +0.13% | +0.25% |
| `oneshot/q9/mbrotli/vendor-alice29.txt` | -0.36% | -0.16% | +0.06% |
| `oneshot/q9/mbrotli/vendor-lcet10.txt` | -0.95% | -0.70% | -0.50% |
| `oneshot/q9/mbrotli/vendor-mapsdatazrh` | -1.22% | -1.09% | -0.97% |
| `oneshot/q9/mbrotli/vendor-plrabn12.txt` | -0.83% | -0.69% | -0.55% |
| `oneshot/q9/mbrotli/vendor-quickfox_repeated` | -0.24% | +0.57% | +1.72% |
| `oneshot/q9/mbrotli/vendor-random_org_10k.bin` | +1.54% | +6.39% | +11.56% |
| `tiny/q0/mbrotli/1024` | -2.83% | -1.95% | -1.16% |
| `tiny/q0/mbrotli/16` | -0.45% | -0.25% | -0.04% |
| `tiny/q0/mbrotli/256` | +0.11% | +0.29% | +0.51% |
| `tiny/q0/mbrotli/64` | +0.28% | +0.39% | +0.50% |
| `tiny/q1/mbrotli/1024` | +0.40% | +0.77% | +1.11% |
| `tiny/q1/mbrotli/16` | -1.78% | -1.36% | -0.99% |
| `tiny/q1/mbrotli/256` | -0.65% | -0.37% | -0.06% |
| `tiny/q1/mbrotli/64` | -0.62% | -0.20% | +0.16% |
| `tiny/q5/mbrotli/1024` | -0.97% | -0.36% | +0.24% |
| `tiny/q5/mbrotli/16` | -5.93% | -4.30% | -2.70% |
| `tiny/q5/mbrotli/256` | -0.96% | -0.15% | +0.65% |
| `tiny/q5/mbrotli/64` | -3.64% | -2.41% | -1.19% |
| `tiny/q9/mbrotli/1024` | -2.27% | -1.43% | -0.93% |
| `tiny/q9/mbrotli/16` | -2.42% | -1.62% | -0.83% |
| `tiny/q9/mbrotli/256` | -2.67% | -0.95% | +0.31% |
| `tiny/q9/mbrotli/64` | -0.95% | +0.36% | +1.90% |