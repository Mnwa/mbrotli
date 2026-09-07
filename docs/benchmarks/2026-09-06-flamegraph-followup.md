# Flamegraph follow-up and rejected reader experiment

## Decision

Both optimization candidates have been reverted. The dense matcher change
showed repeated regressions; a subsequent reader-buffer experiment also failed
the no-regression requirement. Production Rust and architecture files are
byte-identical to revision `731a0becfdc139d2207848f78fb9851ae2617515`.
The independent hotpath-aware allocation-accounting test fix remains.
The **95%-of-C target is still unmet**; no benchmark baseline was replaced.

## CPU sampling

The requested [flamegraph-rs tool](https://github.com/flamegraph-rs/flamegraph)
was installed as version 0.6.13 under `/tmp/mbrotli-profiler-tools`.
On this WSL2 host, own-process `cpu-clock:u` sampling works with the existing
`perf_event_paranoid=2`, even though the earlier hardware-counter attempt did
not. No host settings or system packages were changed. Ubuntu perf 5.15.209
was extracted under `/tmp`; it successfully recorded against the running
6.18.33.2 WSL kernel.

The profiling build uses release optimization, runtime SIMD selection, Rust
line information and frame pointers. C is built with symbols and frame pointers.
The linker uses `--no-rosegment`, as recommended by flamegraph for Rust's lld.
These flags are for attribution, not the separate throughput comparison.

The temporary harness copies the existing benchmark bodies and generators,
restricting reused compression to q6 and streaming to q1, on structured binary
and incompressible 256 KiB inputs. Each selected case is profiled separately
for 15 seconds through Criterion. Validation/setup and Criterion's profiling
warm-up are in the recording; practically all samples are under the selected
compression iteration. Matching output was established by the full baseline
benchmark and streaming assertions. Q6 structured binary produces 94,648 bytes;
q1 incompressible produces 262,149 bytes, identically in Rust and C.

`perf record -e cpu-clock:u -F 499 --call-graph dwarf,16384` captures user CPU
stacks. Rendering uses `--no-inline`: full inline expansion was slow, so the
first completed capture was preserved and rendered without that expansion.
Libc's matching debug package (2.35-0ubuntu3.14, build ID
`22ca0a83a4004122e30a69b597be96e134068616`) was combined with the actual ELF
using `eu-unstrip`, retaining unwind data as well as symbols in a local perf
cache. This avoids leaving memory operations as anonymous libc frames.

The perf collapser weights each sample by its 2,004,008 ns event period.
Every folded weight was verified to be divisible by that fixed period and
normalized to sample counts before SVG rendering, so the graph's sample labels
have the correct units. Percentages below are shares within one recording;
they are not wall-time percentages or cross-implementation speed ratios.

| Recording | Samples | Main finding |
| --- | ---: | --- |
| Rust, reused q6 binary | 7,002 | Backward-reference generation is 84.42% inclusive; the AVX2 search body alone is 78.79%, and `BucketRun::store_range` is 5.58%. |
| C, reused q6 binary | 8,743 | `CreateBackwardReferencesNH58` is 77.82% exclusive. |
| Rust, q1 incompressible reader | 7,403 | Memory copying is 45.43%, clearing 28.47%, and the AVX2 command-generation body 17.24%. |
| C, q1 incompressible stream | 6,821 | Memory copying is 38.48%, clearing 29.73%, and the two-pass fragment body 26.74%. |

The reader's caller stacks further attribute **13.83%** to clearing output
space in `std::io::default_read_to_end`, **9.98%** to clearing the fast encoder's
hash table, and **4.51%** to clearing the source-refill buffer. Copying under
`StreamState::process` accounts for **16.75%**. These findings support
investigating buffer traffic and output growth for the reader; they do not
show that replacing existing memory operations with wider SIMD will help.

Interactive SVGs and normalized folded stacks are local artifacts under
`target/profiles/flamegraph-2026-09-06/`: `q6-rust`, `q6-c`, `q1-reader`, and
`q1-c`. Raw perf recordings, command JSON and rendering logs remain under
`/tmp/mbrotli-flamegraphs/`. The [harness recipe](2026-09-06-flamegraph-harness.py.txt)
and [commands](2026-09-06-flamegraph-commands.txt) document reproduction.

## Reader-buffer experiment: reverted

The hypothesis was to retain initialized input bytes after a full reader
refill, rather than clearing the vector and then zero-filling it again.
The experiment removed that clearing and kept the consumed cursor at the end
until refill. It added no allocation, changed no codec algorithm, and preserved
the benchmark's output assertions. The 4.51% sampled refill clearing suggested
only a small potential benefit on the primary incompressible workload.

A separate standard release harness measured q1/q2/q5 reader and C paths on
three corpus types, with identical bytes and settings. It ran three alternating
baseline/candidate rounds pinned to CPU 2, 25 samples, 200 ms warm-up and
700 ms measurement. No builds or tests ran concurrently. The
[complete pilot CSV](2026-09-06-reader-reuse-rejected.csv) records all nine
cases. Speedup is the median of paired round ratios. The interval envelope
is the minimum/maximum conservative round-wise interval ratio, not a confidence
interval for that median.

| Reader case | Rust speedup | Interval envelope | Before / after % of C |
| --- | ---: | ---: | ---: |
| q1/binary-256KiB | 0.872× | 0.833–1.163× | 129.0 / 115.6 |
| q1/compressible-256KiB | 1.168× | 1.110–1.422× | 106.2 / 123.3 |
| q1/incompressible-256KiB | 1.012× | 0.867–1.161× | 50.8 / 51.1 |
| q2/binary-256KiB | 0.993× | 0.877–1.292× | 79.1 / 80.9 |
| q2/compressible-256KiB | 1.204× | 1.149–1.599× | 504.2 / 582.1 |
| q2/incompressible-256KiB | 1.078× | 1.015–1.434× | 82.2 / 82.2 |
| q5/binary-256KiB | 0.984× | 0.934–1.824× | 63.5 / 65.3 |
| q5/compressible-256KiB | 1.092× | 1.006–1.401× | 294.7 / 302.5 |
| q5/incompressible-256KiB | 1.037× | 0.938–1.824× | 70.6 / 74.6 |

Compressible inputs improved, but q1 binary's median throughput fell by
**12.8%**, and the primary q1 incompressible gain was small and uncertain.
Intervals are wide. This is insufficient evidence for retaining the change
under the user's regression policy, so it was reverted without treating its
wins as compensation for losses. Its source and immutable binaries remain
local experiment artifacts.

## Restored-source validation

Formatting, strict all-feature workspace Clippy, and the full unfiltered
workspace test suite passed on the restored implementation. LLVM coverage
reports **2,175/2,175 library functions (100%)**. A separate report including
test source confirms the retained memory-test function ran and its warm-up
block was exercised for all twelve qualities. Production sources were checked
byte-for-byte against HEAD again after rejecting the reader pilot.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
CARGO_PROFILE_TEST_OPT_LEVEL=1 cargo test --workspace --all-features --locked
```

The coverage collection ran all unit tests and shorter integrations, retaining
the recovery/fork lifecycle cases; normal tests were unfiltered. The inspected
reports are `/tmp/mbrotli-revert-coverage.json` and
`/tmp/mbrotli-revert-with-tests-coverage.json`. No profiler tools, generated
coverage, raw recordings, vendored changes or dependency updates are committed.
