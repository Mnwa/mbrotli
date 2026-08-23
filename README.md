# mbrotli

Brotli compression in safe Rust, byte-identical to Google's reference encoder.

`mbrotli` implements the two fast Brotli qualities — **0** (one-pass) and **1**
(two-pass) — as a port of [google/brotli] v1.2.0, commit `028fb5a`. For any
input, quality and window size, it emits exactly the same bytes the reference
encoder does. That is not an aspiration: it is what the test suite asserts, on
every corpus, on every run.

[google/brotli]: https://github.com/google/brotli/tree/028fb5a

- **No `unsafe`.** Not in the bit writer, not in the match scan, nowhere in
  `src/`. Hot loops shed their bounds checks through `as_chunks`, `first_chunk`
  and const-generic widths instead.
- **SIMD resolved once.** `fearless_simd` picks the instruction set one time per
  compression, never inside a loop, and every backend produces identical bytes.
- **RFC 7932 output.** Verified by round-tripping through Google's C decoder.

## Status

| Feature | State |
| --- | --- |
| Quality 0 and 1 | implemented, byte-identical to the reference |
| Quality 2–11 | not implemented — reported as `UnsupportedQuality` |
| Decoder | not implemented |
| One-shot and streaming APIs | implemented |
| Large window (`lgwin > 24`) | not supported, as in the reference fast path |

## Usage

```rust
use mbrotli::Brotli;
use mbrotli::compressor::{BrotliCompressParams, BrotliQualityLevel, BrotliWindowBits};

let compressor = Brotli::default().compressor();
let params = BrotliCompressParams::new(BrotliQualityLevel::Q1, BrotliWindowBits::DEFAULT);

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
| `BrotliCompressor` | Compression entry points, bound to a level |
| `BrotliCompressParams` | Quality and window size, `Copy` |
| `BrotliQualityLevel` | Closed enum, `Q0`–`Q9` and `Q11` |
| `BrotliWindowBits` | Validated newtype over `10..=24` |
| `BrotliCompressorWriter` / `BrotliCompressorReader` | Streaming adapters |
| `BrotliCompressError` | `#[non_exhaustive]` error type |

Everything below that surface is private. No encoder internal, SIMD type or FFI
detail escapes the public API.

## Performance

Measured against the same reference encoder the output is compared with, on an
Apple M5 Pro (NEON), portable builds on both sides, `lgwin = 22`. Compressed
size is **exactly identical**, so these are like-for-like comparisons.

| Shape | q0 | q1 |
| --- | ---: | ---: |
| End-to-end one-shot, geometric mean | 0.965× | 1.196× |
| Pre-sized output buffer, geometric mean | 0.991× | 1.257× |

Quality 1 is ahead of the reference; quality 0 is at parity in the pre-sized
shape and slightly behind end-to-end, with English prose the weakest bucket.
Per-case numbers, confidence intervals, the machine manifest and the raw
Criterion logs are in [`docs/q0_q1_benchmarks.md`](docs/q0_q1_benchmarks.md),
including what is *not* claimed — no x86-64 host was available, so the AVX2 and
AVX-512 paths compile and are dispatch-covered but unmeasured.

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
| `src/compressor/core/fast/` | Quality 0 and 1 encoders, bitstream, Huffman, SIMD dispatch |
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
| [`architecture/fast-encoder.md`](architecture/fast-encoder.md) | Encoder core: scans, bitstream, dispatch, specialisation |
| [`docs/q0_q1_api_binding.md`](docs/q0_q1_api_binding.md) | How the port maps onto the existing API, and what changed |
| [`docs/q0_q1_design.md`](docs/q0_q1_design.md) | Design record and the reasoning behind it |
| [`docs/q0_q1_reference_differences.md`](docs/q0_q1_reference_differences.md) | Every divergence from the reference, including the quirks reproduced on purpose |
| [`docs/q0_q1_benchmarks.md`](docs/q0_q1_benchmarks.md) | Measured results, methodology, and the gates not met |
| [`docs/q0_q1_ci.md`](docs/q0_q1_ci.md) | Commands for checks, backend matrix, coverage, fuzzing |

## Attribution

The quality 0 and quality 1 algorithms, their constant tables and the bitstream
layout are ported from [google/brotli] v1.2.0 (commit `028fb5a`), which Google
distributes under the MIT licence; see
`brotli-ffi/vendor/brotli/LICENSE`. Translated tables carry an upstream
reference in their source comments and are pinned by golden checksums.

The format itself is [RFC 7932](https://datatracker.ietf.org/doc/html/rfc7932).

`mbrotli` does not declare a licence of its own yet.
