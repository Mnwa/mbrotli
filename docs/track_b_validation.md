# Track B implementation and validation

Date: 2026-09-05. Host: Apple M5 Pro, `aarch64-apple-darwin`, Rust 1.98.1
(`48a229cea`). Baseline: `a1ff445bc657c53599a72efa7a81bca1ad776f3d`.
The externally authored files under `specifications/` are unchanged.

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
