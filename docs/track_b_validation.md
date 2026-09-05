# Track B implementation and validation

Date: 2026-09-05. Host: Apple M5 Pro, `aarch64-apple-darwin`, Rust 1.98.1
(`48a229cea`). Baseline: `a1ff445bc657c53599a72efa7a81bca1ad776f3d`.
The externally authored files under `specifications/` are unchanged.

## Follow-up compression gap fixes (2026-09-05)

The follow-up audit reproduced three regressions before their fixes: the
directory excluded type-8 headers; a 655,897-byte preparation budget accepted
a 721,708-byte observed heap peak; and a prefix/suffix-only custom dictionary
was not searched at q5..9. Those regressions now pass. Preparation accounts for
the live owned description and temporary indexes, and a dedicated system-allocator
test checks both rejection under the old insufficient ceiling and an accepted
build staying below a sufficient ceiling.

Greedy qualities retain the reference shallow search, then search the remaining
transforms through the immutable flat index. Tests cover all transform operations
at q5..9 on scalar and every host SIMD level, verify actual dictionary use, and
decode the streams with C. Long transformed commands are checked at greedy and
HQ qualities. Existing identity-only C fixtures remain unchanged.

`metadata_with_options` adds independent uncompressed/Brotli/Shared Brotli
encoding for original and repeated metadata. `repeat_metadata_fields` chooses
a globally consistent subset before metadata starts. Repeats are pre-encoded,
bounded by retained capacities, and do not retain dictionary borrows. Their
Shared Brotli references must be external IDs; original metadata can use earlier
internal chunks/resources. The directory includes every type 1–8 header.
Fault injection now covers 400 offsets per fault mode with compressed metadata
and repeats. Metadata keep-decoder chains and repeat-to-repeat internal references
are not emitted; every compressed metadata chunk is independently decodable.

Checks and local artifacts for this follow-up:

```sh
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
CARGO_PROFILE_TEST_OPT_LEVEL=1 CARGO_TARGET_DIR=target/track-b-optimized-tests \
  cargo test --workspace --all-features --locked
cargo check --workspace --all-targets --no-default-features --locked
cargo llvm-cov --workspace --all-features --locked --json \
  --output-path target/track-b-gap-final-coverage.json --lib \
  --test serialized_dictionary --test framing --test dictionary_memory \
  --test dictionary --test public_api --test stream_offset --test reuse
```

Function coverage was inspected, including the new description-budget function,
extended greedy probe, metadata adapter and metadata core. This remains a
changed-function claim, not whole-project or branch-completeness certification.

The final workspace run passed **938 tests including doctests**, using the
optimized test profile above. Formatting, workspace Clippy with warnings denied,
and the no-default-features check pass. The duplicate unoptimized full run was
stopped after more than twenty minutes while its writer chunk-size test continued
using a CPU core; it is not recorded as a completed pass. The final targeted
debug/coverage runs pass, including the allocator-backed regression and the
original error-limit contract. The initial final-coverage attempt caught an
internal-budget value escaping into that diagnostic; it was fixed without
weakening the existing assertion, then the report was regenerated successfully.

The final report covers 29/29 static-index functions, 63/63 public dictionary
module functions, 25/25 container-core functions, 3/3 metadata-core functions,
and 22/22 framing-adapter functions. The new serialized-description allocation
bound and its closures all have nonzero execution counts.

From `fuzz/afl`, formatting, Clippy, `cargo afl test`, and release builds of
`framing`/`serialized_dictionary` pass. The framing target now checks directory
completeness and independently decodes compressed metadata in addition to
schedule equivalence; a compressed/selected-repeat seed is committed.

```sh
cargo afl fuzz -i regressions/framing \
  -o /tmp/mbrotli-gap-framing-afl-20260905 -V 60 -c - -- target/release/framing
cargo afl fuzz -i regressions/serialized_dictionary \
  -o /tmp/mbrotli-gap-dictionary-afl-20260905 -V 60 -t 1000 -c - \
  -- target/release/serialized_dictionary
```

The 60-second campaigns completed 137,364 and 140,246 executions respectively,
with zero crashes/hangs and stability 99.94%/99.97%. AFL required approved
unsandboxed execution for shared-memory attachment; no host tuning was performed.
These are bounded smoke runs, not exhaustive proofs.

After the final budget-diagnostic correction, both targets were rebuilt and
smoked again with the same commands, `-V 30`, and fresh output directories
`/tmp/mbrotli-gap-framing-final-afl-20260905` and
`/tmp/mbrotli-gap-dictionary-final-afl-20260905`. The final runs completed
84,516/146,756 executions, zero crashes/hangs, and 99.94%/99.97% stability.

Criterion commands:

```sh
cargo bench --bench track_b --features experimental --locked -- \
  --sample-size 10 --warm-up-time 1 --measurement-time 2
cargo bench --bench track_b --features experimental --locked -- \
  track-b/metadata --sample-size 10 --warm-up-time 1 --measurement-time 2
```

The metadata benchmark verifies identical complete framing bytes against C
one-shot compression plus an independently constructed RFC envelope, then C
decoding before timing. Rust includes field serialization, container creation
and finalization; C receives pre-serialized identical fields and constructs the
same envelope. Neither side times corpus generation or validation. Means below
are microseconds, from Criterion's `new/estimates.json` on the host above:

| Metadata corpus / quality | Rust | C with envelope | Compressed bytes |
| --- | ---: | ---: | ---: |
| Small / q5 | 4.56 | 4.55 | 26 |
| Small / q9 | 4.60 | 4.59 | 26 |
| Small / q11 | 95.31 | 80.28 | 17 |
| Repetitive text / q5 | 8.04 | 7.64 | 51 |
| Repetitive text / q9 | 8.43 | 7.53 | 51 |
| Repetitive text / q11 | 434.50 | 367.08 | 59 |
| Deterministic binary / q5 | 73.28 | 77.90 | 16,393 |
| Deterministic binary / q9 | 97.89 | 98.57 | 16,393 |
| Deterministic binary / q11 | 20,276.53 | 19,638.06 | 16,393 |

Serialized input sizes are 22, 16,805 and 16,389 bytes. The binary corpus is a
fixed xorshift sequence, not ambient randomness. These short measurements
overlapped test work. They document the new functionality, not a claim that all
performance targets are met. The existing quality minima, physical-history
ceiling, independent decoding above 30 bits, cross-architecture evidence, and
full release performance gates remain as described below and in architecture.

Rust and AFL skill guidance informed the allocation-backed regression,
deterministic candidate ordering, and strengthened framing fuzz oracle.

## Delivered behavior

- Existing strict serialized parsing and canonical serialization now feed the
  same immutable prepared dictionary used by prefix compression. Custom word,
  transform and context-combination indexes participate at qualities 5..11.
- Expanded HQ words retain their RFC base copy length even above the C encoder's
  transformed-word limit. Index preparation has explicit expansion ceilings.
- Experimental nonzero stream offsets work at qualities 2..11, with checked
  63-bit logical positions, suppressed headers, restart flushing and no invented
  prior history.
- A separate experimental container writer emits chunk types 0..10 over a
  non-seekable sink, with bounded resources, explicit references and IDs,
  metadata, repeated metadata, central directory and a checked fixed-point footer.
- Resource and container writes retain unwritten suffixes across sink errors;
  finalization is retryable and destructors perform no I/O.
- No decoder, hidden hash policy, ready-made Brotli dependency or vendor edit
  was introduced.

Mechanics and diagrams: [custom encoding](../architecture/rfc9841-encoding.md),
[framing](../architecture/framing.md), and the updated
[architecture index](../architecture/README.md).

## Correctness and coverage commands

From the workspace root:

```sh
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo check --workspace --all-targets --no-default-features --locked
```

The unoptimized full suite passed. Its existing one-byte writer schedule takes
over twenty minutes in debug code because `EncoderWriter::pump` zero-fills a
128 KiB temporary slice on each call. A two-second process sample attributed
the active test to `Vec::resize`, not a stalled encoding state machine. Final
full-suite repetitions use optimization without changing assertions or test
inputs:

```sh
CARGO_PROFILE_TEST_OPT_LEVEL=1 CARGO_TARGET_DIR=target/track-b-optimized-tests \
  cargo test --workspace --all-features --locked
```

Focused coverage was inspected with:

```sh
cargo llvm-cov --workspace --all-features --locked --json \
  --output-path target/track-b-coverage.json --lib \
  --test serialized_dictionary --test framing --test dictionary \
  --test public_api --test stream_offset --test reuse
```

The new static-index module has 23/23 covered functions. The framing public
adapter, container core and resource core have 20/20, 20/20 and 6/6 respectively,
including focused footer-width/overflow unit tests. The changed dictionary,
session, command-packing and HQ-parameter functions are exercised. This is
function coverage evidence, not a claim of 100% branch coverage or whole-project
coverage from the focused subset. Reports remain ignored local artifacts.

New integration tests cover long transforms, C-identical identity dictionaries
on scalar and available SIMD backends, context combinations, input chunking,
preparation limits, offset overflow, tiny continuations and joined-stream C
decoding. Framing tests independently parse every emitted chunk form and inject
short writes, interruption, zero and `WouldBlock` at 150 offsets per fault mode.
The C library does not implement the framing container, so full-container tests
use RFC fixtures; compressed resource payloads use its decoder.

## AFL

From `fuzz/afl`:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo afl test
cargo afl build --release --bin framing --bin serialized_dictionary
cargo afl fuzz -i regressions/framing -o /tmp/mbrotli-track-b-framing-verified-afl \
  -V 30 -c - -- target/release/framing
cargo afl fuzz -i regressions/serialized_dictionary \
  -o /tmp/mbrotli-track-b-serialized-corrected-afl -V 120 -t 1000 -c - \
  -- target/release/serialized_dictionary
```

Runner: cargo-afl 0.18.2, bundled AFL++ 4.40c. The final framing smoke completed
95,324 executions with zero crashes/timeouts and 99.93% stability. The small
persistent calibration variance is recorded rather than treated as 100% stable.

A serialized-dictionary campaign found two oracle assertion failures. Both were
minimized before changing the harness and preserved as 8-byte and 25-byte corpus
files. They encode redundant six-byte prefix-length varints: RFC 9841 allows up
to nine bytes, while C's `ReadVarint32` rejects a continuation in byte five.
Rust already handled them correctly. The harness now exempts only that exact
structural C limitation; it still requires canonical bytes to parse with C and
prepared compressed payloads to decode with C. A deterministic integration test
also pins the RFC behavior. No production parser behavior was weakened.

The corrected 120-second run completed 182,203 executions with zero crashes or
timeouts and 99.97% stability. Rust best-practices and AFL skill guidance informed
the private-core ownership boundaries, bounded preparation, and the
minimize-before-fix regression workflow.

## Criterion observations

```sh
cargo bench --bench track_b --features experimental --locked -- \
  --sample-size 10 --warm-up-time 1 --measurement-time 2
```

Corpus: the fixed 4,248-byte phrase payload in `benches/track_b.rs`, using the
same serialized identity dictionary, quality, default window and unknown input
size for both encoders. Prepared dictionary construction, payload generation and
byte-for-byte validation are outside timing. Timings include stream/resource
creation, encoding and finalization; destination capacity is reused. C creates
its stream state each iteration; Rust retains its compressor workspace.

Mean times from Criterion's `new/estimates.json` (microseconds):

| Mode / quality | Rust | C | Raw compressed bytes |
| --- | ---: | ---: | ---: |
| Custom stream q5 | 7.82 | 7.75 | 43 |
| Custom stream q9 | 8.53 | 6.66 | 43 |
| Custom stream q11 | 230.90 | 175.26 | 45 |
| Single-resource framing q5 | 6.85 | 6.78 | 43 |
| Single-resource framing q9 | 7.39 | 6.74 | 43 |
| Single-resource framing q11 | 221.12 | 165.09 | 45 |

The framing C case adds an explicit RFC envelope around C's compressed stream;
it is not represented as a C container API. The complete framed outputs are
identical before timing. Envelope overhead is 46 bytes, and the Rust prepared
dictionary retains 131,785 bytes. The raw writer and framing resource writer
have different staging mechanisms, so their rows are separate API measurements,
not a claim that framing itself speeds up compression.

Standard-path probe, run in the clean baseline and current worktree:

```sh
cargo bench --bench compress --locked -- \
  'reused/q(5|9|11)/.*/text-1MiB' \
  --sample-size 10 --warm-up-time 1 --measurement-time 2
```

| Quality | Baseline mean | Current mean | Output bytes (both) |
| --- | ---: | ---: | ---: |
| q5 | 1.511 ms | 1.532 ms | 8,254 |
| q9 | 1.998 ms | 1.961 ms | 7,542 |
| q11 | 1.572 s | 1.517 s | 8,038 |

These selected measurements completed, but the harness subsequently failed an
existing `benches/compress.rs:855` q1 streamed-reference equality assertion in
**both** baseline and current trees. It validates unrelated cases even when the
Criterion name filter excludes their timing. Neither command is reported as a
successful complete benchmark run.

These short measurements overlapped correctness work and are preliminary.
In particular, q5's observed latency increase is about 1.4%; this does not prove
the specification's under-1% regression gate. No before/after memory campaign
or x86 target measurement was performed. The full cross-corpus, cross-API,
cross-architecture throughput and memory gates remain open; functional Track B
implementation is not a claim that every release acceptance gate is complete.
