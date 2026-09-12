# AFL fuzz targets

This isolated package fuzzes compression, parameters, streaming, dictionaries,
framing, parallel tasks, and native decompression. It is excluded from workspace builds because its
binaries link AFL's runtime. Target bodies also run through committed regression
replay with `cargo afl test`.

## Setup

AFL requires a C compiler and `make`. Run these commands from `fuzz/afl/`:

```sh
cargo install cargo-afl --version 0.18.2 --locked
cargo afl config --build --force
cargo afl build --release
cargo afl build --release --features experimental
```

The default build includes the encoder and native decoder targets. `experimental`
enables `serialized_dictionary`, `framing`, and `decode_serialized` with their
corresponding Rust and C features. Their binaries and regression registry entries are omitted without
that flag; the remaining targets run in both configurations.

Rebuild AFL's runtime when changing Rust toolchains. Some hosts require shared
memory or crash-reporting configuration; `cargo afl system-config` performs
privileged host changes. Consult its output if startup fails.

## Layout and corpora

| Path | Role |
| --- | --- |
| `src/lib.rs` | Input decoding, payload caps, backend enumeration, and C oracles |
| `src/targets.rs` | Target bodies shared by AFL and regression replay |
| `src/bin/` | Thin `afl::fuzz!` adapters |
| `tests/regressions.rs` | Committed corpus replay |
| `regressions/` | Small boundary cases and minimized findings |
| `seeds/` | Generated corpora; local artifacts |
| `findings/` | Campaign results; local artifacts |

Generate seeds from the vendored Brotli test data, then minimize them:

```sh
./prepare-seeds.sh
./minimise-seeds.sh
./prepare-decoder-seeds.sh
```

The generated corpora are `generic`, `params`, `large_window`, and `dictionary`.
`prepare-decoder-seeds.sh` adds `seeds/decoder` for the targets that read a
compressed member directly, from the committed decoder regressions and Google
Brotli's own `*.compressed*` fixtures below 50 KiB. Other targets use their
committed `regressions/<target>/` corpus.
The minimization script preserves the originals in `seeds/*.raw` and disables
the forkserver for `afl-cmin` folder-mode coverage collection.

## Targets

| Target | Input corpus | Checks |
| --- | --- | --- |
| `q0_roundtrip`, `q1_roundtrip`, `q3_roundtrip` through `q11_roundtrip` | `seeds/generic` | Size bound and C decoding |
| `decode_roundtrip` | `seeds/params` (shared with encoder) | C compression at Q0–Q11, native plaintext equality, and chunked decoding equivalence |
| `params_roundtrip` | `seeds/params` | Legal configurations across qualities 0–11, determinism, and C decoding |
| `simd_equivalence` | `seeds/params` | Every available host backend agrees; forced scalar equivalence lives in library unit tests |
| `differential_c` | `seeds/params` | Byte identity with equivalent C streaming settings |
| `streaming_equivalence` | `seeds/params` | Vector, slice, session, reader, and writer identity |
| `output_capacity` | `seeds/params` | Exact and undersized output buffers |
| `parameter_parsing` | `seeds/params` | Numeric validation and rejection paths |
| `large_window` | `seeds/large_window` | Header/quality validation, backend identity, and available C decoding |
| `dictionary` | `seeds/dictionary` | Preparation limits, prefix matching, quality restrictions, and C compatibility |
| `compressor_lifecycle` | `regressions/compressor_lifecycle` | Reuse, trim, reconfiguration, failures, abandonment, and recovery |
| `decompress` | `seeds/decoder` | Arbitrary compressed bytes against the C decoder's typed outcome, plus an unbounded replay of every accepted stream |
| `decode_streaming` | `seeds/decoder` | Chunked sessions against one-shot decoding: exact progress, cumulative counters, and termination |
| `decode_io_limits` | `seeds/decoder` | Output budgets crossed with one retryable sink failure, which may neither duplicate nor drop payload |
| `decode_dictionary` | `regressions/decode_dictionary` | A bounded raw prefix, then attached decoding against C with the same attachment, and reuse determinism |
| `decode_lifecycle` | `regressions/decode_lifecycle` | Forgotten sessions, abandonment, recovery, trim and reconfiguration between decodes |
| `decode_serialized` (`experimental`) | `regressions/decode_serialized` | Serialized attachment parsing and attached decoding against the experimental C decoder |
| `serialized_dictionary` (`experimental`) | `regressions/serialized_dictionary` | Parsing, canonical serialization, bounded preparation, and C decoding |
| `framing` (`experimental`) | `regressions/framing` | Resource/metadata sequences, chunking, directory completeness, and payload decoding |
| `parallel` | `regressions/parallel` | Scheduling, source adapters, staging, fragments, and C decoding |

The common parameter decoder reads six bytes before the payload:

| Byte | Meaning |
| --- | --- |
| 0 | Quality index across 0–11 |
| 1 | Standard window: `10 + value % 15` |
| 2 | Streaming chunk size: `1 << (value % 18)` |
| 3 | Compression mode selection and literal-context flag |
| 4 | Automatic block size at zero; otherwise `16 + value % 9` bits |
| 5 | Distance postfix bits and direct groups |

Common payloads are capped at 128 KiB, and declared stream size equals payload
length. Stateful and format-specific targets have their own input layouts.
The decoder round-trip target caps plaintext at 64 KiB to fit the existing
decoder output budget. It selects one quality per input and shares committed
regression inputs from `regressions/params_roundtrip`. Focused replay tests
exercise all 12 qualities; small generated parameter seeds also span Q0–Q11.
Quality 2 is covered by parameterized targets. Multi-block and larger inputs also
have integration coverage in the root workspace.

## Campaigns

```sh
cargo afl fuzz -i seeds/params -o findings/differential -- target/release/differential_c
cargo afl build --release --features experimental --bin framing
cargo afl fuzz -i regressions/framing -o findings/framing -- target/release/framing
```

Resume an existing output directory with `-i -`. For multiple workers, use
unique names and one shared directory:

```sh
cargo afl fuzz -M main -i seeds/params -o findings/differential -- target/release/differential_c
cargo afl fuzz -S worker1 -i seeds/params -o findings/differential -c - -- target/release/differential_c
cargo afl whatsup -s findings/differential
```

CmpLog is enabled by default; `-c -` disables it for a worker. Use fresh output
directories after changing instrumentation or target semantics. Record the
revision, toolchain, corpus, duration, executions, stability, crashes, and hangs.
Bounded smoke runs only provide evidence for the inputs they execute.

`campaign.sh` fuzzes encoder targets and `decode_roundtrip` in both feature
configurations, one worker per target, each from its own seed corpus into its own output directory:

```sh
./prepare-seeds.sh
cargo afl build --release --no-default-features --target-dir target/stable
TARGET_DIR=target/stable/release ./minimise-seeds.sh
CAMPAIGN_PARALLEL=1 ./campaign.sh findings/campaign 7200
```

Its arguments are a findings root, which must not already exist, the seconds
each phase runs, and an optional execution timeout in milliseconds, 30000 by
default. The stable phase fuzzes 22 targets of a `--no-default-features`
build; the experimental phase fuzzes 24 targets of a
`--features experimental` build, because the feature reaches into the encoder.
The phases run one after the other so every worker owns a hardware thread.
`CAMPAIGN_PARALLEL=1` runs them together instead: the campaign then costs one
phase of wall clock and oversubscribes the host, which lowers executions per
second per worker. Run both scripts together only when host resources permit.
Record results with revision, corpus and feature configuration; see
[validation limits](../../docs/correctness.md).

A saved hang needs standalone reproduction against the instrumented binary.
Inspect elapsed/user time, resident memory and page faults before classifying it
as search cost, allocation work or a non-terminating loop.

## Triage and required checks

Minimize a finding and commit a deterministic regression before fixing it:

```sh
cargo afl tmin -i findings/differential/default/crashes/id:000000,... \
  -o regressions/differential_c/crash-short-description.bin \
  -- target/release/differential_c
cargo afl test --features experimental
```

Confirm that the regression fails before the fix and passes afterward. Details
are in [the regression guide](regressions/README.md).

After changes in this package or to a public API its targets call, run:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --no-default-features -- -D warnings
cargo clippy --all-targets --no-default-features --features experimental -- -D warnings
cargo afl test --no-default-features
cargo afl test --no-default-features --features experimental
```

Plain `cargo test` cannot link the fuzz binaries' AFL runtime. Workspace checks
at the repository root do not reach this package. See
[the fuzzing specification](../../architecture/fuzzing.md) for ownership,
per-iteration state, dispatch, and oracle limitations.

## Decoder round-trip campaign

Reuse the encoder corpus, generated by `./prepare-seeds.sh`; no separate decoder
round-trip seeds are required. From `fuzz/afl/`:

```sh
cargo afl build --release --no-default-features --bin decode_roundtrip
cargo afl fuzz -i seeds/params -o findings/decode-roundtrip -V 60 -t 1000 -c - -- target/release/decode_roundtrip
```

Build with `--features experimental` and choose a fresh findings directory to
repeat that profile. `../../scripts/fuzz_decoder.sh base 60` includes this target
alongside the arbitrary-byte decoder targets. The same crash replay and
minimization procedure above applies; decoder round-trip regressions share
`regressions/params_roundtrip` with the encoder.

`decoder-campaign.sh` is the decoder counterpart of `campaign.sh`: one worker
per decoder target in both feature builds, thirteen in all, both phases at once.

```sh
./prepare-decoder-seeds.sh
./decoder-campaign.sh findings/decoder 10800
```

Its arguments are a findings root, which must not already exist, the seconds
every worker runs, and an optional execution timeout in milliseconds, 5000 by
default. Set `SEED_ROOT` to carry an earlier campaign's exploration forward:
when `$SEED_ROOT/<config>-<target>` exists it replaces that worker's corpus,
which is where a `cargo afl cmin` reduction of a previous queue belongs.
Record the revision, corpus, feature configuration and results with each run.

`decompress` and `decode_streaming` additionally consume arbitrary bytes directly.
Their shared synthetic random-byte fixtures are described in
`regressions/decoder-provenance.md`; format errors are expected, while panics
and violated progress or C-equivalence assertions fail the target.


### Native framed encoder

`framed_encode` requires `--features experimental`. It compares native tiny-output
schedules with writer output, one-shot and Read (for matching flush schedules),
and decoded payload. The 1024-byte cap bounds work, not accepted byte values.

```sh
cargo afl build --release --no-default-features --features experimental --bin framed_encode
cargo afl fuzz -i regressions/framed_encode -o artifacts/framed-encode -S smoke -V 60 -c - -- target/release/framed_encode
```

Seeds in `regressions/framed_encode` select plain, flushed, stored, shared and
hidden/checksummed cases. Preserve findings; replay the minimized input through
`framed_targets::framed_encode` and add a deterministic regression before fixing.
