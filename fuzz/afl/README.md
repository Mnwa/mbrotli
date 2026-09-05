# AFL fuzz targets

This isolated package fuzzes compression, parameters, streaming, dictionaries,
framing, and parallel tasks. It is excluded from workspace builds because its
binaries link AFL's runtime. Target bodies also run through committed regression
replay with `cargo afl test`.

## Setup

AFL requires a C compiler and `make`. Run these commands from `fuzz/afl/`:

```sh
cargo install cargo-afl --version 0.18.2 --locked
cargo afl config --build --force
cargo afl build --release
```

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
```

The generated corpora are `generic`, `params`, `large_window`, and `dictionary`.
Other targets use their committed `regressions/<target>/` corpus.
The minimization script preserves the originals in `seeds/*.raw` and disables
the forkserver for `afl-cmin` folder-mode coverage collection.

## Targets

| Target | Input corpus | Checks |
| --- | --- | --- |
| `q0_roundtrip`, `q1_roundtrip`, `q3_roundtrip` through `q11_roundtrip` | `seeds/generic` | Size bound and C decoding |
| `params_roundtrip` | `seeds/params` | Legal configurations across qualities 0–11, determinism, and C decoding |
| `simd_equivalence` | `seeds/params` | Scalar and every available host backend agree |
| `differential_c` | `seeds/params` | Byte identity with equivalent C streaming settings |
| `streaming_equivalence` | `seeds/params` | Vector, slice, session, reader, and writer identity |
| `output_capacity` | `seeds/params` | Exact and undersized output buffers |
| `parameter_parsing` | `seeds/params` | Numeric validation and rejection paths |
| `large_window` | `seeds/large_window` | Header/quality validation, backend identity, and available C decoding |
| `dictionary` | `seeds/dictionary` | Preparation limits, prefix matching, quality restrictions, and C compatibility |
| `compressor_lifecycle` | `regressions/compressor_lifecycle` | Reuse, trim, reconfiguration, failures, abandonment, and recovery |
| `serialized_dictionary` | `regressions/serialized_dictionary` | Parsing, canonical serialization, bounded preparation, and C decoding |
| `framing` | `regressions/framing` | Resource/metadata sequences, chunking, directory completeness, and payload decoding |
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
Quality 2 is covered by parameterized targets. Multi-block and larger inputs also
have integration coverage in the root workspace.

## Campaigns

```sh
cargo afl fuzz -i seeds/params -o findings/differential -- target/release/differential_c
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

## Triage and required checks

Minimize a finding and commit a deterministic regression before fixing it:

```sh
cargo afl tmin -i findings/differential/default/crashes/id:000000,... \
  -o regressions/differential_c/crash-short-description.bin \
  -- target/release/differential_c
cargo afl test
```

Confirm that the regression fails before the fix and passes afterward. Details
are in [the regression guide](regressions/README.md).

After changes in this package or to a public API its targets call, run:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo afl test
```

Plain `cargo test` cannot link the fuzz binaries' AFL runtime. Workspace checks
at the repository root do not reach this package. See
[the fuzzing specification](../../architecture/fuzzing.md) for ownership,
per-iteration state, dispatch, and oracle limitations.
