# mbrotli

Brotli compression in safe Rust, byte-identical to Google's reference encoder.

`mbrotli` implements every Brotli quality but **2** as a port of
[google/brotli] v1.2.0, commit `028fb5a`. For any input and any
combination of encoder parameters, it emits exactly the same bytes the
reference encoder does. That is not an aspiration: it is what the test suite
asserts, on every corpus, on every run.

[google/brotli]: https://github.com/google/brotli/tree/028fb5a

- **No `unsafe`.** Not in the bit writer, not in the match scan, nowhere in
  `src/`. Hot loops shed their bounds checks through `as_chunks`, `first_chunk`
  and const-generic widths instead.
- **SIMD resolved once.** `fearless_simd` picks the instruction set one time per
  compressed block, never inside a loop, and every backend produces identical
  bytes. The match finder is chosen from the caller's parameters alone, so the
  machine cannot change the output.
- **RFC 7932 output.** Verified by round-tripping through Google's C decoder.
- **RFC 9841 Large Window.** A window of up to 62 bits, asked for by name —
  `WindowBits::large(30)` rather than a number that happens to exceed 24, so
  the wider header is never inferred. Declaring a wide window allocates
  nothing: the encoder keeps at most 30 bits of history whatever the header
  says.
- **RFC 9841 shared contexts, without shared ownership.** `SharedContext` owns
  its dictionary bytes outright — no `Arc`, no `Mutex`, no atomic, no global
  cache — and is handed to a call by `&mut`, so the borrow checker is what
  guarantees one context backs one session. Its prepared index is byte-identical
  to the reference's, entry for entry.

## Status

| Feature | State |
| --- | --- |
| Quality 0, 1, 3–11 | implemented, byte-identical to the reference |
| Quality 2 | not implemented — reported as `UnsupportedQuality` |
| Decoder | not implemented |
| One-shot and streaming APIs | implemented |
| Mode, block size, size hint, distance layout, context modelling | implemented |
| RFC 9841 Large Window (qualities 3–11) | implemented — qualities 0 and 1 report `UnsupportedLargeWindow` |
| RFC 9841 shared context: prefix dictionaries, prepared indexes, addressing, search | implemented |
| RFC 9841 shared dictionaries used by an encoder | not implemented — refused with `UnsupportedSharedContextForQuality`, never ignored |
| RFC 9841 serialized dictionaries and framing container | not implemented |
| Custom static dictionaries | not supported; the built-in static dictionary is used |

### Quality guide

| Quality | What it does |
| --- | --- |
| 0 | One pass, static entropy codes — fastest, largest output |
| 1 | Two passes, per-block entropy codes |
| 3 | Greedy matching, one prefix code per stream |
| 4 | Adds block splitting, histogram optimisation, distance parameters |
| 5 | Adds an extensive delayed search and literal context modelling |
| 6–9 | Deepens the search: 32 to 256 bucket candidates, 4 to 16 cached distances, and from 7 the three-context literal model |
| 10 | Replaces greedy matching with a Zopfli search over every match a binary tree can find, and clusters histograms into real context maps |
| 11 | The same, searching harder and re-pricing everything from the commands its first pass produced — slowest, smallest output |

## Usage

```rust
use mbrotli::Brotli;
use mbrotli::compressor::{CompressParams, QualityLevel, WindowBits};

let compressor = Brotli::default().compressor();
let params = CompressParams::new(QualityLevel::Q1, WindowBits::DEFAULT);

let payload = "brotli ".repeat(1000);
let compressed = compressor.compress(params, payload.as_bytes())?;

// Deterministic, and byte-identical to the reference encoder.
assert_eq!(payload.len(), 7000);
assert_eq!(compressed.len(), 41);
```

Into a caller-owned buffer, so the output is not allocated for you:

```rust
let bound = compressor.calculate_bound(&params, payload.len())?;
let mut buffer = vec![0u8; bound];
let written = compressor.compress_to_slice(params, payload.as_bytes(), &mut buffer)?;

assert_eq!(&buffer[..written], compressed.as_slice());
```

Streaming through a `Write`. The stream is only terminated by `finish`, because
`Write` has no closing hook and a fragment boundary need not land on a byte
boundary:

```rust
use std::io::Write;

let mut sink = compressor.compress_writer(params, Vec::new());
for chunk in payload.as_bytes().chunks(512) {
    sink.write_all(chunk)?;
}
let streamed = sink.finish()?;

assert_eq!(streamed, compressed);
```

A matching `Read` adapter, `compress_reader`, yields the compressed form of an
inner reader. All four shapes produce the same bytes.

The whole thing runs as [`examples/compress.rs`](examples/compress.rs):

```sh
cargo run --example compress
```

## API

| Item | Role |
| --- | --- |
| `Brotli` | Entry point; resolves the SIMD level once |
| `Compressor` | Compression entry points, bound to a level |
| `CompressParams` | Every encoder parameter, `Copy`, built by chained `with_*` |
| `QualityLevel` | Closed enum, `Q0`–`Q11` |
| `WindowBits` | The window and the header it selects: `standard(10..=24)` or `large(10..=62)` |
| `BlockBits` | Validated newtype over `16..=24` |
| `CompressMode` | `Generic`, `Text`, `Font` |
| `DistanceCodes` | Validated postfix-bit and direct-code pair |
| `CompressorWriter` / `CompressorReader` | Streaming adapters |
| `BrotliCompressError` | `#[non_exhaustive]` error type |

Everything below that surface is private. No encoder internal, SIMD type or FFI
detail escapes the public API.

## Performance

Measured against the same reference encoder the output is compared with, on an
Apple M5 Pro (NEON), portable builds on both sides, `lgwin = 22`. Compressed
size is **exactly identical**, so these are like-for-like comparisons.

| Shape | q0 | q1 | q3 | q4 | q5 |
| --- | ---: | ---: | ---: | ---: | ---: |
| End-to-end one-shot, geometric mean | 0.950× | 1.142× | 0.793× | 0.771× | 0.799× |
| Pre-sized output buffer, geometric mean | 1.019× | 1.259× | 0.799× | 0.765× | 0.803× |

Quality 1 is ahead of the reference and quality 0 is at parity in the pre-sized
shape. **The greedy qualities are not there yet**: they run at roughly 0.77× to
0.80×, and short inputs are worse still at about 0.5×, because this crate pays
initialisation costs the reference skips. Where the time goes, what has been
tried and what would close the gap are all in
[`docs/q3_q5_benchmarks.md`](docs/q3_q5_benchmarks.md).

**Qualities 6 to 11 have not been benchmarked yet.** Their compressed size is
exactly the reference's, because their output is byte-identical, but no
throughput measurement has been taken and no performance claim is made for
them. `benches/compress.rs` covers them; the numbers are simply not in yet.

Per-case numbers, confidence intervals, the machine manifests and the raw
Criterion logs are in [`docs/q0_q1_benchmarks.md`](docs/q0_q1_benchmarks.md)
and [`docs/q3_q5_benchmarks.md`](docs/q3_q5_benchmarks.md), including what is
*not* claimed — no x86-64 host was available, so the AVX2 and AVX-512 paths
compile and are dispatch-covered but unmeasured.

## Requirements

- Rust 1.89 or newer (set by the pinned `fearless_simd = "=0.7.0"`), edition 2024.
- A C compiler and `git` submodules for the development dependencies: the
  benchmark and differential-test oracle is Google's C Brotli, vendored at
  `brotli-ffi/vendor/brotli`.

```sh
git submodule update --init --recursive
```

## Development

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo bench --bench compress
```

The test profile enables `fearless_simd/force_support_fallback`, so the scalar
backend is compared against every SIMD backend on every run.

| Suite | What it pins |
| --- | --- |
| `tests/differential_c.rs` | Byte identity with the C encoder, all boundary lengths, every window size |
| `tests/vendor_corpus.rs` | The same over Google Brotli's own test data, including a 12 MiB multi-fragment input |
| `tests/roundtrip.rs` | Independent decoder round-trip, determinism, size bound |
| `tests/simd_backends.rs` | Scalar fallback == every SIMD backend, byte for byte |
| `tests/streaming.rs` | Chunk-size independence, writer/reader agreement |
| `tests/randomized.rs` | Seeded property tests over generated inputs |
| `tests/public_api.rs` | Constructors, conversions, accessors, error model |
| `fuzz/afl/` | AFL targets for the same oracles |

Fuzzing, seeded from the vendored Brotli test data:

```sh
cargo install cargo-afl
fuzz/afl/prepare-seeds.sh
cd fuzz/afl && cargo afl build --release
cargo afl fuzz -i seeds/params -o findings/differential target/release/differential_c
```

Function coverage is held at 100% for repository-owned code:

```sh
cargo llvm-cov --package mbrotli --all-features --summary-only
```

## Repository layout

| Path | Role |
| --- | --- |
| `src/compressor/` | Public API, parameters, error types |
| `src/compressor/core/shared/` | Bit writer, Huffman builders, match-length scan, format constants |
| `src/compressor/core/fast/` | Quality 0 and 1 encoders and their SIMD dispatch |
| `src/compressor/core/greedy/` | Quality 3 to 9 encoder: match finders, greedy search, greedy meta-blocks |
| `src/compressor/core/hq/` | Quality 10 and 11 encoder: binary-tree matcher, Zopfli search, high-quality meta-blocks |
| `brotli-ffi/` | Bindings to Google's C Brotli; `vendor/` is upstream source and is not hand-edited |
| `architecture/` | Always-current description of what the code does |
| `docs/` | Port record: API binding, design, reference differences, benchmarks, CI |
| `specifications/` | Externally authored source specifications |
| `benches/`, `tests/`, `fuzz/afl/` | Criterion benchmarks, integration tests, fuzz targets |

## Documentation

| Document | Contents |
| --- | --- |
| [`architecture/README.md`](architecture/README.md) | Index and module map |
| [`architecture/compressor.md`](architecture/compressor.md) | API layer, streaming state machines, error model |
| [`architecture/fast-encoder.md`](architecture/fast-encoder.md) | Quality 0 and 1 core: scans, bitstream, dispatch, specialisation |
| [`architecture/greedy-encoder.md`](architecture/greedy-encoder.md) | Quality 3 to 9 core: hasher plan, commands, greedy meta-blocks |
| [`architecture/hq-encoder.md`](architecture/hq-encoder.md) | Quality 10 and 11 core: binary tree, Zopfli search, clustering, numerical determinism |
| [`architecture/shared-brotli.md`](architecture/shared-brotli.md) | RFC 9841: Large Window selection, declared window versus retained history, the widened distance alphabet, the shared context and its prefix search |
| [`docs/rfc9841_api_binding.md`](docs/rfc9841_api_binding.md) | How RFC 9841 maps onto the existing API, symbol by symbol |
| [`docs/rfc9841_interop_decisions.md`](docs/rfc9841_interop_decisions.md) | Every ambiguity in RFC 9841 and which reading this encoder implements |
| [`docs/rfc9841_wire_map.md`](docs/rfc9841_wire_map.md) | Every RFC 9841 field written, its width, its validation and its implementing function |
| [`docs/rfc9841_context_lifecycle.md`](docs/rfc9841_context_lifecycle.md) | Who owns a `SharedContext`, what a call does to it, and what reuse guarantees |
| [`docs/rfc9841_security.md`](docs/rfc9841_security.md) | What an attacker can influence through RFC 9841, and what is done about it |
| [`docs/rfc9841_benchmarks.md`](docs/rfc9841_benchmarks.md) | Evidence that Large Window support did not slow ordinary compression |
| [`docs/q6_q9_api_binding.md`](docs/q6_q9_api_binding.md) | How qualities 6–9 map onto the greedy encoder |
| [`docs/q10_q11_api_binding.md`](docs/q10_q11_api_binding.md) | How the high-quality encoder maps onto the existing API, and the one public change |
| [`docs/q6_q11_reference_differences.md`](docs/q6_q11_reference_differences.md) | Every divergence from the reference in the q6–q11 port, and the quirks reproduced on purpose |
| [`docs/q0_q1_api_binding.md`](docs/q0_q1_api_binding.md) | How the port maps onto the existing API, and what changed |
| [`docs/q0_q1_design.md`](docs/q0_q1_design.md) | Design record and the reasoning behind it |
| [`docs/q0_q1_reference_differences.md`](docs/q0_q1_reference_differences.md) | Every divergence from the reference, including the quirks reproduced on purpose |
| [`docs/q0_q1_benchmarks.md`](docs/q0_q1_benchmarks.md) | Measured results, methodology, and the gates not met |
| [`docs/q0_q1_ci.md`](docs/q0_q1_ci.md) | Commands for checks, backend matrix, coverage, fuzzing |
| [`docs/q3_q5_api_binding.md`](docs/q3_q5_api_binding.md) | How the greedy port maps onto the API, and what was added |
| [`docs/q3_q5_design.md`](docs/q3_q5_design.md) | Design record for the greedy port |
| [`docs/q3_q5_reference_differences.md`](docs/q3_q5_reference_differences.md) | Every divergence from the reference at qualities 3 to 5 |
| [`docs/q3_q5_benchmarks.md`](docs/q3_q5_benchmarks.md) | Measured results for qualities 3, 4 and 5, and the gates not met |

## Attribution

The encoder algorithms, their constant tables, the built-in static dictionary
and the bitstream layout are ported from [google/brotli] v1.2.0 (commit
`028fb5a`), which Google distributes under the MIT licence; see
`brotli-ffi/vendor/brotli/LICENSE`. Translated tables carry an upstream
reference in their source comments and are pinned by golden checksums; the
dictionary blobs under `src/compressor/core/greedy/dictionary/` are extracted
verbatim from the same source.

The format itself is [RFC 7932](https://datatracker.ietf.org/doc/html/rfc7932),
extended by [RFC 9841](https://www.rfc-editor.org/rfc/rfc9841.html).

`mbrotli` does not declare a licence of its own yet.
