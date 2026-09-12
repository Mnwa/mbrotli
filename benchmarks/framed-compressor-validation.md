# Framed compressor validation

The implementation replaces the experimental raw-owner writer factory with
`FramedCompressor`. Raw codec algorithms and shared decoder APIs remain unchanged.
`architecture/framing.md` describes ownership, states, wire serialization, budgets
and retry cursors. External source specifications and vendored Brotli are unchanged.

## API and migration

```rust
// Previous experimental construction:
// let mut owner = Compressor::new(encoder_config)?;
// let writer = owner.framed_writer(sink, framing_config)?;

let config = FramedEncodeConfig::default()
    .with_encoder_config(encoder_config)
    .with_framing_config(framing_config);
let mut owner = FramedCompressor::new(config)?;
let writer = owner.framed_writer(sink, FramedEncodeStreamConfig::default())?;
```

The owner exposes `new`, `builder`, configuration/retention/accounting,
`reconfigure`, `trim`, `recover`, `fork_empty`, and:

```text
start(stream) -> Result<FramedEncoderSession<'_>, FramedEncodeError>
compress(FramedInput<'_>) -> Result<Vec<u8>, FramedEncodeError>
compress_into(FramedInput<'_>, &mut Vec<u8>) -> Result<Range<usize>, FramedEncodeError>
compress_to_slice(FramedInput<'_>, &mut [u8]) -> Result<usize, FramedEncodeFailure>
framed_writer<W: Write>(W, stream) -> Result<FramedWriter<'_, W>, FramedEncodeError>
framed_reader(input, stream) -> Result<FramedEncoderReader<'_, '_>, FramedEncodeError>
```

Native container `process(output, FramedEncodeOperation)` consumes zero payload;
resource `process(input, output, Operation)` reports exact acceptance and delivery.
Both return `FramedEncodeProgress`/`FramedEncodeFailure`. Structured commands and
all existing writer operations remain supported. `FramingError` is a compatibility
alias, while std finalization retains the entire writer in `FramingFinishError`.

There are 14 runnable framing examples in the std encoder-only rustdoc profile,
plus two compile-fail borrow checks. The alloc profile runs the 12 examples whose
APIs exist there, plus borrow checks. Examples cover one-shot, append, slice
failure, one-byte native output, metadata/hidden resources, writer, reader,
configuration, reuse, reconfiguration, trim, forgotten-session recovery and a
resource dictionary destroyed before container finalization.

## Reproduction environment

- Host: Apple M5 Pro, `aarch64-apple-darwin`, rustc 1.98.1 (`48a229cea`, LLVM 22.1.8).
- Declared MSRV: Rust 1.89.0, exercised with checks, focused tests and alloc doctests.
- Miri: nightly `0ed41eb414` (2026-09-04).
- cargo-llvm-cov 0.9.0; cargo-afl 0.18.2 / AFL++ 4.40c.
- Baseline source: checkout `910653f9ca6033b6a19be65bafcf89ba3c3c16e1`, archived
  independently under `/tmp/mbrotli-framed-compressor-baseline-910653f`; the
  unchanged C vendor submodule is read through a symlink and the resolved lockfile
  is copied. No reset of the working checkout was performed.
  The baseline harness is `benches/framed_compress.rs` with only owner construction
  changed to raw `Compressor::new(raw_config)` and the second writer argument
  changed to `FramingConfig { repeat_metadata: true, ..Default::default() }`.
  Corpus generation, sinks, raw benchmarks, validation and timing are identical.

## Correctness evidence

Seven complete hex fixtures in `testdata/framing-legacy` were captured before
serializer edits. The existing 12 framing tests compare those fixtures and retain
independent wire assertions, footer/directory checks, C raw-member validation and
sink fault injection. The footerless empty fixture remains an independent literal
byte array. New tests compare native, one-shot, append, fixed slice, writer and
reader, including all input split points of short fixtures, output widths
0/1/2/7/31/4096, exact chunk boundaries, explicit Flush, all reference forms,
metadata encodings/repeat selections, Large Window, hidden and empty resources.
Four reproducible randomized schedules per encoding also exercise a 65,553-byte
binary resource, changing both input splits and output capacity (including zero).

The large structured text corpus exercises distinct Unknown/Exact raw size-hint
outputs and proves that a known aggregate size does not rewrite the resource hint.
Tests cover output-pending command rejection, wrong final suffix, partial progress
on late chunk failure, aggregate/resource size contracts, aborted/forgotten guards,
invalid reconfigure, compatible capacity retention, raw source chains, append
rollback, short/Interrupted/WouldBlock/zero/oversized sink writes and flush retry.

Allocator tests inject each allocation failure in an uncompressed structured
metadata/resource/padding/repeat/directory container until a full run succeeds.
Every refusal is typed and append is atomic. Measured owner retention equals
requested live heap bytes for stored and dictionary-compressed resources, excluding
external dictionary storage. For the 100,000-byte stored workload with 4 KiB
chunks, retained framing storage is 16,136 bytes; peak requested heap including
the returned output is 217,678 bytes. Recovery and exact/below-boundary trimming
are checked. These are heap allocation measurements, not hard RSS claims.

The targeted llvm-cov report covers 159/159 functions across all framed encoder
files and the shared raw session driver. The changed private `Compressor::begin`
is also exercised. Reports are local artifacts under `target/`, not committed.
The command sequence is:

```sh
cargo llvm-cov --no-default-features --features std,compression,decompression,experimental --lib --no-report -- compressor::framing
cargo llvm-cov --no-clean --no-default-features --features std,compression,decompression,experimental --test streaming -- a_finished_session_stays_finished
cargo llvm-cov --no-clean --no-default-features --features std,compression,decompression,experimental --test framing --test framed_encoder --test framed_encoder_memory --json --output-path target/framed-encoder-coverage-completion.json
```

## Check record

| Check | Result |
| --- | --- |
| `cargo fmt --all` and `cargo fmt --all -- --check` | Pass |
| Workspace all-target/all-feature Clippy, locked, `-D warnings` | Pass |
| Workspace std/both-codec/experimental Clippy, locked, `-D warnings` | Pass |
| `cargo test --workspace --all-features --locked` | 1,071 pass, 2 pre-existing stress tests ignored |
| Workspace std/both-codec/experimental tests | 1,272 pass, same 2 stress tests ignored |
| Final std/both-codec focused API/wire/allocation tests | 34 pass |
| Final alloc encoder-only API/allocation tests | 21 pass |
| Decoder-only framed integration tests | 27 pass |
| Encoder-only, decoder-only and alloc doctests | Pass |
| `scripts/check_codec_features.py` | All 16 isolated profiles pass, including negative imports |
| Rust 1.89 std/alloc encoder checks, focused tests and alloc doctests | Pass |
| Targeted function coverage | 100%, 159/159 |
| AFL package format, stable/experimental Clippy and `cargo afl test` | Pass |
| Native AFL build and final 60-second campaign | 29,844 executions, 0 crashes, 0 hangs; 99.97% stability |
| Migrated writer AFL, 30 seconds | 104,910 executions, 0 crashes, 0 hangs; 99.95% stability |
| Migrated framed round-trip AFL, 30 seconds | 18,282 executions, 0 crashes, 0 hangs; 99.97% stability |
| Miri queue/boundaries, lifecycle, dictionary forgetting, metadata, allocation failures | Pass |
| AddressSanitizer native/writer tests on aarch64 macOS | 29 pass |
| Scalar plus every available host SIMD backend | Pass; scalar and NEON exercised |

The native campaign command, from `fuzz/afl`, is:

```sh
cargo afl build --release --no-default-features --features experimental --bin framed_encode
AFL_SKIP_CPUFREQ=1 AFL_NO_AFFINITY=1 AFL_I_DONT_CARE_ABOUT_MISSING_CRASHES=1 \
  cargo afl fuzz -i regressions/framed_encode -o ../../target/framed-encoder-afl-final-native \
  -S smoke -V 60 -c - -- target/release/framed_encode
```

The writer and round-trip campaigns use `--bin framing` / `--bin framed_roundtrip`,
their corresponding `regressions/` seed directories and `-V 30`. All three binaries
were rebuilt from the final production code before these campaigns.

Sandboxed shared-memory creation initially failed; the campaign was rerun outside
the sandbox without modifying system configuration. Saved artifacts remain local.
Miri commands select the focused `framed_encoder`/`framed_encoder_memory` tests with
`--no-default-features --features no_std,compression,experimental`. ASan uses
`RUSTFLAGS=-Zsanitizer=address cargo +nightly test -Zbuild-std --target
aarch64-apple-darwin --no-default-features --features std,compression,experimental
--test framed_encoder --test framing` and a separate target directory.

## Performance evidence

`benches/framed_compress.rs` measures end-to-end framing with a reused owner and
caller output capacity retained outside the timed region. It covers one large
resource, 256 small resources, compressed metadata/repeats, stored data, a borrowed
dictionary, a one-byte sink, and 256 KiB of deterministic incompressible binary
data. Configuration is quality 5/window 22 with 64 KiB
chunks and metadata repetition; there are no explicit Flush calls. Setup and full
baseline-byte equality checks are outside timing. C is only an oracle and
comparison for raw Brotli, never a whole-container encoder.

```sh
MBROTLI_FRAMED_BASELINE_WRITE="$PWD/target/framed-bench-baseline" \
  cargo bench --offline --manifest-path /tmp/mbrotli-framed-compressor-baseline-910653f/Cargo.toml \
  --bench framed_compress --no-default-features --features std,compression,experimental --locked \
  -- --sample-size 10 --measurement-time 1 --warm-up-time 1
MBROTLI_FRAMED_BASELINE_READ="$PWD/target/framed-bench-baseline" \
  cargo bench --bench framed_compress --no-default-features \
  --features std,compression,experimental --locked \
  -- --sample-size 10 --measurement-time 1 --warm-up-time 1
```

Both binaries assert identical complete bytes for all seven framed scenarios. Raw
one-shot and native session bytes equal the C encoder outside timing. Native raw session performance
is recorded separately from the C one-shot API; these are different API modes.
The following paired runs were sequential, after the workspace tests and AFL
campaigns completed. Values are Criterion central time estimates in microseconds;
positive delta means slower. Samples are intentionally short (10 samples, one
second measurement and warm-up), so small changes are not general speed claims.

| Workload | Payload bytes | Wire bytes, both versions | Before µs | After µs | Time delta |
| --- | ---: | ---: | ---: | ---: | ---: |
| One large resource | 1,040,000 | 527 | 109.790 | 108.110 | −1.53% |
| 256 small resources | 66,560 | 16,140 | 1,545.600 | 1,521.300 | −1.57% |
| Metadata and repeats | 66,560 | 40,623 | 3,733.700 | 3,758.900 | +0.67% |
| Uncompressed resource | 1,040,000 | 1,040,271 | 60.061 | 89.246 | +48.59% |
| Borrowed dictionary | 6,656 | 112 | 4.4994 | 4.3879 | −2.48% |
| One-byte sink | 6,656 | 76 | 6.1353 | 6.0981 | −0.61% |
| Incompressible binary | 262,144 | 262,259 | 200.700 | 204.750 | +2.02% |
| Raw Rust one-shot | 638,976 | 46 | 31.004 | 30.957 | −0.15% |
| Raw Rust native session | 638,976 | 46 | 30.996 | 31.104 | +0.35% |
| Raw C one-shot | 638,976 | 46 | 340.530 | 340.770 | +0.07% |

The uncompressed writer regression is material. Its 95% time interval is
58.496–61.193 µs before and 84.029–94.674 µs after. The new independent native
engine copies queued bytes into the adapter's bounded 8 KiB transport buffer
before the sink receives them; the former core wrote its queue directly to the
sink. Stored data exposes this extra copy and repeated transport calls most
strongly. This explains a concrete added cost, but the fraction attributable to
each operation was not isolated by profiling. This change makes no claim of
improved framing throughput and does not optimize the raw algorithm.

Old `Compressor::retained_bytes()` excludes the old writer's framing allocations,
whereas the new owner's metric includes them. The benchmark's printed retention
values therefore cannot be compared as a before/after memory regression; use the
allocator tests above for actual accounting. Complete logs and Criterion reports
remain local under `target/` and the isolated baseline target directory.

## Limits of this evidence

The two pre-existing decoder stress tests (>4 GiB cumulative output and up to
2 GiB history) remain ignored by the ordinary workspace command; this refactor
does not alter them. No execution is claimed on x86/AVX or other absent targets.
No C container oracle exists. The AFL run is a smoke campaign rather than an
exhaustive proof. Prepared checksums/references remain caller-supplied; there is
no checksum verification, dictionary resolver or extraction API in the encoder.
Reader input is borrowed complete slices; streaming sources use native resources
or the writer. Directory and retained repeats scale with command descriptions.
