# Quality 3 / 4 / 5 benchmark report

Measured comparison against the pinned C reference. **Compression ratio is
identical in every one of the 55 case-and-quality combinations**, so every
throughput number below is a like-for-like comparison; the harness asserts byte
identity with the C encoder before it times anything.

**The throughput gate is not met.** Qualities three, four and five run at
roughly 0.77× to 0.80× of the reference on this machine, against a required
geometric mean of 1.00×. Section 5 says where the time goes and what has been
tried.

## Machine and build manifest

| Item | Value |
| --- | --- |
| Machine | Apple M5 Pro, 18 cores, macOS (Darwin 25.6.0, arm64) |
| SIMD backend | NEON (`Level::new()` on this host) |
| Rust | `rustc 1.97.1 (8bab26f4f 2026-07-14)`, `cargo 1.97.1` |
| C compiler | Apple clang 21.0.0 (clang-2100.1.1.101) |
| Rust profile | Cargo `bench` profile: `opt-level = 3`, `lto` off, `codegen-units = 16`, `panic = "unwind"` |
| C build | `cc` crate driven by the same release profile (`opt-level = 3`); `brotli-ffi/build.rs` passes no architecture flags |
| Native tuning | none on either side — no `-C target-cpu=native`, no `-march=native` |
| Reference | `google/brotli` v1.2.0, commit `028fb5a` (vendored submodule), built without `BROTLI_MAX_SIMD_QUALITY` |
| This crate | commit `a90de08` plus the working tree of this change |
| Instrumentation | none — the `hotpath` features are off, so `#[hotpath::measure]` compiles to nothing |
| Window size | `lgwin = 22` for every case; mode generic; every other parameter at its default |
| Criterion settings | `--warm-up-time 1 --measurement-time 3 --sample-size 10` |
| Date | 2026-08-24 |

Command:

```sh
cargo bench --bench compress -- --warm-up-time 1 --measurement-time 3 --sample-size 10
```

Raw artifacts are in `docs/benchmarks/2026-08-24-apple-m5-pro-q3-q5.{txt,json,csv}`.

The sample count is lower than the quality 0 and 1 report used, because five
qualities across three shapes is a much longer run. Between-run variation on
this machine is roughly ±3% on the mid-sized corpora and larger on the tiny
group, so a single bucket moving by a few percent is noise; the geometric means
are stable to about ±2%.

## 1. Headline results

Ratio is C time divided by this crate's time, so above 1.00 is faster than the
reference.

| Shape | q0 | q1 | q3 | q4 | q5 |
| --- | ---: | ---: | ---: | ---: | ---: |
| End-to-end one-shot | 0.950× | 1.142× | **0.793×** | **0.771×** | **0.799×** |
| Pre-sized output buffer | 1.019× | 1.259× | **0.799×** | **0.765×** | **0.803×** |
| Tiny inputs (16–1024 B) | 0.964× | 0.899× | **0.564×** | **0.576×** | **0.491×** |

Qualities 0 and 1 are unchanged by this work and are reported for context.

## 2. Per case, pre-sized shape

| Case | q3 | q4 | q5 |
| --- | ---: | ---: | ---: |
| binary-256KiB | 0.866× | 0.783× | 0.696× |
| compressible-256KiB | 0.765× | 0.732× | 0.723× |
| incompressible-256KiB | 0.650× | 0.626× | 0.627× |
| text-1KiB | 0.680× | 0.627× | 0.580× |
| text-1MiB | 0.859× | 0.818× | 0.970× |
| vendor-alice29.txt | 0.812× | 0.753× | 0.993× |
| vendor-lcet10.txt | 0.881× | 0.820× | 0.983× |
| vendor-mapsdatazrh | 0.893× | 0.879× | 0.796× |
| vendor-plrabn12.txt | 0.841× | 0.831× | 0.978× |
| vendor-quickfox_repeated | 0.716× | 0.649× | 0.687× |
| vendor-random_org_10k.bin | 0.877× | 0.980× | 0.960× |

Quality five is at parity on English prose, where the search does enough work
per position that the per-position overhead is diluted. The weakest buckets are
the ones where the encoder does the *least* work per byte: incompressible data,
short inputs, and long runs of the same match.

## 3. Per case, tiny group

| Payload | q3 | q4 | q5 |
| --- | ---: | ---: | ---: |
| 16 B | 0.494× | 0.487× | 0.366× |
| 64 B | 0.504× | 0.589× | 0.489× |
| 256 B | 0.622× | 0.635× | 0.546× |
| 1024 B | 0.655× | 0.603× | 0.596× |

This is fixed per-call cost, not per-byte cost, and it is the clearest gap in
the report.

## 4. Compression ratio

| Check | Result |
| --- | --- |
| Cases compared | 55 (11 corpora × 5 qualities) |
| Compressed-size mismatches | **0** |

Byte identity is asserted before every timed run, so the ratio gate — at most
0.2% worse than the reference in total, at most 0.5% per category — is met with
zero margin used.

## 5. Where the time goes

Profiled with `sample(1)` over `alice29.txt`, release build, 6 s at 1 ms.

### Quality 3, 3 329 samples

| Region | Share |
| --- | ---: |
| `create_backward_references`, match finder inlined | 65% |
| `Command::length_code` from command construction | 4% |
| `store_meta_block_trivial`, symbol writing | 17% |
| `Command::extra_bits` | 5% |
| Huffman construction and serialisation | 4% |
| Everything else (ring buffer, allocation, driver) | 5% |

### Quality 5, 4 813 samples

| Region | Share |
| --- | ---: |
| `Matcher::find_longest_match` | 67% |
| `dictionary::search` | 4% |
| `create_backward_references` itself | 12% |
| Meta-block build and store | 16% |

The shape is the same on both: the search loop dominates, and it is where the
reference is ahead.

## 6. What was tried

| Change | q3 | q4 | q5 | Kept |
| --- | ---: | ---: | ---: | --- |
| `#[inline(always)]` on `Matcher::find_longest_match` and `store` | +8.5% | +8.4% | +7.0% | yes |
| Hoisting the quick matcher's bucket bounds check | +0.6% | +0.5% | +1.3% | yes |
| `#[inline(always)]` on the command length and extra-bit helpers | ±0% | ±0% | ±0% | yes (harmless) |
| Passing the match query by value rather than by reference | ±0% | ±0% | ±0% | yes (clearer) |
| Fat LTO, one codegen unit | −3% | −2% | −5% | no |
| Pure scalar match-length scan instead of the hybrid | −11% | −1% | −2% | no |
| Unchecked reads in the match finder (measurement only, `unsafe`) | +9% | +1% | +1% | no — this crate has no `unsafe` |

The first row is the one that mattered. Before it, the match finder was a real
call per position, so the query and the result travelled through memory on
every position; the reference marks the same function `BROTLI_INLINE`.

## 7. Why the gate is not met, and what would close it

Three distinct costs, in the order they are worth attacking:

1. **Per-call setup dominates short inputs.** The tiny group at 0.37×–0.66×
   says the encoder pays a fixed cost the reference does not. The largest known
   contributors are allocations this crate must initialise and the reference
   does not: the Huffman node pool (1 409 nodes, ~11 KiB written on the first
   meta-block) and the per-meta-block histogram vectors in the block splitters
   (up to ~190 KiB written per 64 KiB meta-block at qualities four and five,
   against roughly 1 KiB in the reference, which clears only the histogram it is
   about to use). Moving both into a reusable arena owned by the encoder, and
   clearing only the entries the splitter actually touches, is a contained
   change that should recover most of the tiny-input gap and part of the
   quality four and five gap. It was not attempted here because it inverts the
   ownership between the splitters and `MetaBlockSplit`.

2. **Bounds checks in the match finder's byte reads.** Measured at about 9% for
   quality three by temporarily switching to unchecked reads. Recovering that
   without `unsafe` needs the reads restructured so one check covers several,
   which means carrying a window type with a proven length rather than a bare
   `&[u8]`.

3. **The remaining per-position gap in the search loop**, roughly 20% for
   quality three after the two above, is not yet explained. It needs a
   side-by-side look at the generated code for
   `QuickMatcher::find_longest_match` against the reference's `FindLongestMatch`
   on the same input, which has not been done.

## 8. Gates

| Gate | Required | Measured | Met |
| --- | --- | --- | :-: |
| Compression ratio, total | within 0.2% of the reference | identical | yes |
| Compression ratio, per category | within 0.5% | identical | yes |
| Compression ratio, per fixture | within 1.0% | identical | yes |
| Throughput, q3, geometric mean | ≥ 1.00× | 0.799× | **no** |
| Throughput, q4, geometric mean | ≥ 1.00× | 0.765× | **no** |
| Throughput, q5, geometric mean | ≥ 1.00× | 0.803× | **no** |
| Throughput, per category | ≥ 0.97× | 0.63×–0.99× | **no** |
| Tiny inputs, geometric mean | ≥ 0.97× | 0.49×–0.58× | **no** |
| Backend identity | byte identical | byte identical | yes |
| Hot-loop allocations | none | none after the first meta-block | yes |

## 9. Not claimed

- **No x86-64 host was available.** The SSE2, SSE4.2, AVX2 and AVX-512 paths
  compile and are covered by the backend-identity tests through
  `Level::fallback()` and the host's own accessors, but they are unmeasured.
  The AVX2 throughput gate is therefore untested, not met.
- **No allocation or peak-workspace numbers.** The `hotpath-alloc` feature can
  produce them; this report does not.
- **No flamegraphs.** The profiles in section 5 are `sample(1)` call trees, not
  rendered graphs.
- **Streaming throughput is not measured.** Only the one-shot entry points are.
