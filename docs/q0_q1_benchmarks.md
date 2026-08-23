# Quality 0 / quality 1 benchmark report

Measured comparison against the pinned C reference. Compression ratio is
identical in every case, so every throughput number is a like-for-like
comparison; the harness asserts byte identity with the C encoder before it
times anything.

## Machine and build manifest

| Item | Value |
| --- | --- |
| Machine | Apple M5 Pro, 18 cores, macOS (Darwin 25.6.0, arm64) |
| SIMD backend | NEON (`Level::new()` on this host) |
| Rust | `rustc 1.97.1 (8bab26f4f 2026-07-14)` |
| C compiler | Apple clang 21.0.0 (clang-2100.1.1.101) |
| Rust profile | Cargo `bench` profile: `opt-level = 3`, `lto` off, `codegen-units = 16`, `panic = "unwind"` |
| C build | `cc` crate driven by the same release profile (`opt-level = 3`); `brotli-ffi/build.rs` passes no architecture flags |
| Native tuning | none on either side — no `-C target-cpu=native`, no `-march=native` |
| Reference | `google/brotli` v1.2.0, commit `028fb5a` (vendored submodule) |
| This crate | commit `f77bd37` plus the working tree of this change |
| Instrumentation | none — the `hotpath` features are off, so `#[hotpath::measure]` compiles to nothing |
| Window size | `lgwin = 22` for every case |
| Criterion settings | `--warm-up-time 2 --measurement-time 5 --sample-size 30` |
| Date | 2026-08-24 |

Command:

```sh
cargo bench --bench compress -- --warm-up-time 2 --measurement-time 5 --sample-size 30
```

Raw artifacts are in `docs/benchmarks/`: the full Criterion log
(`.txt`), and the extracted per-case table as `.json` and `.csv`.

The confidence intervals below are Criterion's within-run intervals. Between
runs this machine varies by roughly ±3% on the mid-sized corpora, so a single
bucket moving by a few percent between runs is noise; the geometric means are
stable to about ±1%.

## Corpora

| Bucket | Cases |
| --- | --- |
| Web-like text | `text-1KiB`, `text-1MiB` (generated), `vendor-alice29.txt`, `vendor-lcet10.txt`, `vendor-plrabn12.txt` (Canterbury, via the vendored corpus) |
| Structured binary | `binary-256KiB` (generated), `vendor-mapsdatazrh` |
| Highly compressible | `compressible-256KiB`, `vendor-quickfox_repeated` |
| Incompressible | `incompressible-256KiB`, `vendor-random_org_10k.bin` |
| Tiny payloads | 16, 64, 256 and 1024 bytes, reported separately |

## Measurement shapes

- **`oneshot`** — the full end-to-end API on both sides, including output
  allocation and growth.
- **`presized`** — the same work into a caller-owned buffer sized by the
  compressed-size bound, so output allocation leaves the timed region. Encoder
  workspaces are still built per call on both sides, exactly as each public API
  does it.

## Results

| group | q | case | mbrotli MiB/s | c-brotli MiB/s | ratio | 95% CI |
| --- | --- | --- | ---: | ---: | ---: | :---: |
| oneshot | q0 | `binary-256KiB` | 884 | 783 | 1.129 | 1.13–1.13 |
| oneshot | q0 | `compressible-256KiB` | 6,179 | 5,996 | 1.030 | 1.01–1.05 |
| oneshot | q0 | `incompressible-256KiB` | 11,127 | 12,932 | 0.860 | 0.86–0.86 |
| oneshot | q0 | `text-1KiB` | 555 | 552 | 1.004 | 0.99–1.02 |
| oneshot | q0 | `text-1MiB` | 10,909 | 12,089 | 0.902 | 0.89–0.91 |
| oneshot | q0 | `vendor-alice29.txt` | 699 | 817 | 0.855 | 0.84–0.87 |
| oneshot | q0 | `vendor-lcet10.txt` | 613 | 670 | 0.915 | 0.88–0.94 |
| oneshot | q0 | `vendor-mapsdatazrh` | 879 | 724 | 1.214 | 1.19–1.23 |
| oneshot | q0 | `vendor-plrabn12.txt` | 466 | 521 | 0.893 | 0.88–0.91 |
| oneshot | q0 | `vendor-quickfox_repeated` | 8,680 | 9,180 | 0.945 | 0.94–0.95 |
| oneshot | q0 | `vendor-random_org_10k.bin` | 1,014 | 1,090 | 0.931 | 0.93–0.94 |
| oneshot | q1 | `binary-256KiB` | 546 | 385 | 1.419 | 1.41–1.43 |
| oneshot | q1 | `compressible-256KiB` | 22,638 | 20,975 | 1.079 | 1.07–1.09 |
| oneshot | q1 | `incompressible-256KiB` | 11,325 | 11,887 | 0.953 | 0.94–0.97 |
| oneshot | q1 | `text-1KiB` | 405 | 432 | 0.939 | 0.92–0.96 |
| oneshot | q1 | `text-1MiB` | 5,794 | 5,350 | 1.083 | 1.08–1.09 |
| oneshot | q1 | `vendor-alice29.txt` | 500 | 390 | 1.283 | 1.26–1.31 |
| oneshot | q1 | `vendor-lcet10.txt` | 452 | 371 | 1.216 | 1.19–1.24 |
| oneshot | q1 | `vendor-mapsdatazrh` | 694 | 473 | 1.467 | 1.43–1.50 |
| oneshot | q1 | `vendor-plrabn12.txt` | 336 | 299 | 1.126 | 1.10–1.16 |
| oneshot | q1 | `vendor-quickfox_repeated` | 17,559 | 17,263 | 1.017 | 1.01–1.03 |
| oneshot | q1 | `vendor-random_org_10k.bin` | 710 | 384 | 1.848 | 1.83–1.86 |
| presized | q0 | `binary-256KiB` | 882 | 767 | 1.151 | 1.14–1.16 |
| presized | q0 | `compressible-256KiB` | 6,419 | 6,123 | 1.048 | 1.04–1.06 |
| presized | q0 | `incompressible-256KiB` | 11,938 | 12,612 | 0.947 | 0.94–0.96 |
| presized | q0 | `text-1KiB` | 561 | 560 | 1.001 | 1.00–1.01 |
| presized | q0 | `text-1MiB` | 11,743 | 12,043 | 0.975 | 0.97–0.98 |
| presized | q0 | `vendor-alice29.txt` | 717 | 794 | 0.904 | 0.89–0.92 |
| presized | q0 | `vendor-lcet10.txt` | 579 | 737 | 0.785 | 0.77–0.80 |
| presized | q0 | `vendor-mapsdatazrh` | 893 | 735 | 1.216 | 1.20–1.23 |
| presized | q0 | `vendor-plrabn12.txt` | 458 | 503 | 0.910 | 0.89–0.92 |
| presized | q0 | `vendor-quickfox_repeated` | 9,866 | 9,124 | 1.081 | 1.08–1.09 |
| presized | q0 | `vendor-random_org_10k.bin` | 1,030 | 1,081 | 0.952 | 0.94–0.96 |
| presized | q1 | `binary-256KiB` | 553 | 385 | 1.437 | 1.43–1.44 |
| presized | q1 | `compressible-256KiB` | 28,251 | 21,423 | 1.319 | 1.31–1.33 |
| presized | q1 | `incompressible-256KiB` | 12,646 | 12,164 | 1.040 | 1.04–1.04 |
| presized | q1 | `text-1KiB` | 420 | 435 | 0.965 | 0.96–0.97 |
| presized | q1 | `text-1MiB` | 6,049 | 5,334 | 1.134 | 1.13–1.14 |
| presized | q1 | `vendor-alice29.txt` | 512 | 407 | 1.257 | 1.24–1.27 |
| presized | q1 | `vendor-lcet10.txt` | 456 | 378 | 1.204 | 1.18–1.23 |
| presized | q1 | `vendor-mapsdatazrh` | 714 | 487 | 1.466 | 1.43–1.50 |
| presized | q1 | `vendor-plrabn12.txt` | 350 | 306 | 1.144 | 1.14–1.15 |
| presized | q1 | `vendor-quickfox_repeated` | 21,029 | 17,550 | 1.198 | 1.19–1.20 |
| presized | q1 | `vendor-random_org_10k.bin` | 734 | 390 | 1.881 | 1.87–1.89 |

## Geometric means

| Shape | Quality | Geomean ratio | Worst bucket | Best bucket |
| --- | :---: | ---: | ---: | ---: |
| `oneshot` | q0 | **0.965** | 0.855 (`vendor-alice29.txt`) | 1.214 (`vendor-mapsdatazrh`) |
| `oneshot` | q1 | **1.196** | 0.939 (`text-1KiB`) | 1.848 (`vendor-random_org_10k.bin`) |
| `presized` | q0 | **0.991** | 0.785 (`vendor-lcet10.txt`) | 1.216 (`vendor-mapsdatazrh`) |
| `presized` | q1 | **1.257** | 0.965 (`text-1KiB`) | 1.881 (`vendor-random_org_10k.bin`) |

## Compression ratio

Byte-identical to the reference on every case the harness validates, and on the
whole test corpus beyond it. The harness asserts this before timing, so a
speedup cannot come from a changed output.

| Quality | Corpus | Compressed bytes (both) |
| --- | --- | ---: |
| q0 | `vendor-alice29.txt` | 65,795 |
| q0 | `vendor-lcet10.txt` | 173,662 |
| q0 | `vendor-mapsdatazrh` | 187,191 |
| q1 | `vendor-alice29.txt` | 60,292 |
| q1 | `vendor-lcet10.txt` | 154,908 |
| q1 | `vendor-mapsdatazrh` | 181,028 |

The `Rust / C` compressed-size ratio is exactly `1.000` for every case, so the
ratio gate is satisfied by construction rather than by margin.

## Per-call overhead, including SIMD dispatch

Reported separately, because on tiny payloads the fixed cost dominates.

| payload | quality | mbrotli ns/call | C ns/call | delta |
| ---: | :---: | ---: | ---: | ---: |
| 16 B | q0 | 750 | 698 | +52 ns |
| 64 B | q0 | 968 | 885 | +83 ns |
| 256 B | q0 | 1,366 | 1,350 | +15 ns |
| 1024 B | q0 | 1,787 | 1,750 | +36 ns |
| 16 B | q1 | 1,649 | 1,444 | +204 ns |
| 64 B | q1 | 1,864 | 1,632 | +232 ns |
| 256 B | q1 | 2,235 | 2,079 | +156 ns |
| 1024 B | q1 | 2,388 | 2,256 | +132 ns |

The delta is flat across payload sizes, which is what a fixed per-call cost
looks like: workspace construction plus one `dispatch!`. Quality 1 pays more
because its state includes the 512 KiB command buffer and the 128 KiB literal
buffer. Dispatch itself is one `Level` match per fragment; there is no
per-block, per-command or per-match dispatch anywhere in the encoder.

## Allocations and memory

| Property | Status |
| --- | --- |
| Allocations in the match scan | none |
| Allocations in the command replay | none |
| Allocations after workspace construction | only output growth owned by the caller's API |
| Workspace growth with total input | bounded: hash table by quality, quality 1 buffers by the 128 KiB block size |
| Hash table reset | active range only; unused capacity is never touched |
| Peak workspace | q0 ≈ 137 KiB, q1 ≈ 1.15 MiB, plus `2 × fragment + 511 B` of scratch only when the destination is too tight to encode into directly |

## Assessment against the acceptance gate

| Gate | q0 | q1 |
| --- | --- | --- |
| Steady-state (`presized`) geomean ≥ 1.00 on AArch64 NEON | **0.991 — not met** | 1.257 — met |
| End-to-end (`oneshot`) geomean ≥ 1.00 | **0.965 — not met** | 1.196 — met |
| No bucket below 0.95× | **not met** (see below) | **not met** (`text-1KiB` at 0.939, `presized` 0.965) |
| No bucket below 0.90× | **not met** (`vendor-alice29.txt` 0.855, `incompressible-256KiB` 0.860, `vendor-lcet10.txt` 0.785 in `presized`) | met |
| Compression ratio ≤ 1.001× | met (exactly 1.000) | met (exactly 1.000) |
| No hot-loop allocations | met | met |
| Dispatch overhead measured | met | met |
| Raw artifacts retained | met | met |
| x86-64 AVX2 measurements | **not run** — no x86 host available | **not run** |

### Buckets that are behind, and why

- **`vendor-alice29.txt` / `vendor-lcet10.txt` / `vendor-plrabn12.txt` at
  quality 0 (0.79–0.92).** English prose produces many short matches with short
  literal runs, so the per-command work dominates. The reference indexes the
  hash table and the literal arrays through raw pointers; this port reaches the
  same shape through `as_chunks`, const-generic table widths and symbol masking,
  which removes most but not all of the bounds checks. What is left is the
  per-command residue on the densest command stream in the corpus.
- **`incompressible-256KiB` at quality 0 (0.86 one-shot, 0.95 pre-sized).**
  This path is dominated by the verbatim copy of the uncompressed meta-block.
  The one-shot number also carries the zeroed allocation of the destination
  vector, which the pre-sized number does not — the gap between the two rows is
  exactly that.
- **`text-1KiB` at quality 1 (0.94).** A single 1 KiB payload is close to the
  per-call floor measured above; the ratio is the fixed +130 ns rather than a
  throughput difference.

### What is not claimed

The x86-64 AVX2 half of the hardware matrix was not measured: no x86 host was
available for this change. The AVX2 and AVX-512 vector loops compile and are
covered by the const-lane dispatch, but their throughput is unmeasured, so no
claim is made for them. The results above are AArch64 NEON only.

## Reproducing

```sh
git submodule update --init --recursive
cargo bench --bench compress -- --warm-up-time 2 --measurement-time 5 --sample-size 30
```

Every case validates both encoders against the C decoder and asserts byte
identity with the C encoder before timing, so a failed parity check aborts the
run rather than producing a misleading number.
