# Quality 0 / quality 1 API binding

Milestone 0 artifact: the mapping between the conceptual roles the port
specification uses and the symbols that already existed in this repository. No
public type was invented for the port; the fast encoders are private
implementation details behind the API that was already there.

## Repository audit

| Item | Finding |
| --- | --- |
| Public compression entry point | `mbrotli::compressor::Compressor`, obtained from `Brotli::compressor()`. |
| Settings type | `CompressParams { quality, lgwin }`, `Copy`, built with `new`. |
| Quality specification | `QualityLevel`, a closed enum `Q0..Q9`, `Q11` (no `Q10`). |
| Window size | `WindowBits`, a validated newtype over `10..=24`. |
| Mode / size hint | Not modelled. The reference default (`BROTLI_MODE_GENERIC`, no size hint) is what the fast path uses anyway. |
| One-shot entry points | `compress` (to `Vec<u8>`) and `compress_to_slice`. |
| Streaming entry points | `compress_writer` (`Write`) and `compress_reader` (`Read`). |
| Output sink | `Vec<u8>` for `compress`, caller slice for `compress_to_slice`, inner `Write`/`Read` for the adapters. |
| Allocator / workspace abstraction | None existed. Added privately as `core::fast::FastEncoder`, which owns and reuses every buffer. |
| Error model | `BrotliCompressError`, `#[non_exhaustive]`, with `BrotliResult<T>`. |
| Bit writer | None existed. Added privately as `core::fast::bits::BitWriter`. |
| Huffman / prefix-code builder | None existed. Added privately as `core::fast::huffman`. |
| Decoder / round-trip infrastructure | None in this crate. Google's C decoder through the `google-brotli-ffi` workspace crate. |
| Feature flags | `hotpath`, `hotpath-cpu`, `hotpath-alloc`. Unchanged. |
| MSRV | Not declared. Edition 2024 implies 1.85 or newer; `slice::as_chunks` raises the effective floor to 1.88. |
| Benchmark harness | `benches/compress.rs`, Criterion, already comparing against the C encoder. |

## Conceptual role binding

| Conceptual role | Actual repository symbol | Ownership / lifetime | q0 / q1 usage |
| --- | --- | --- | --- |
| Encoder settings | `compressor::CompressParams` | `Copy`, passed by value per call | read once when a `FastEncoder` is built |
| Quality routing | `compressor::QualityLevel` → `core::fast::FastQuality` | `Copy` | `TryFrom` accepts `Q0` and `Q1`, refuses the rest |
| Window size | `compressor::WindowBits` | `Copy`, validated at construction | fragment size is `1 << lgwin`; the stream header advertises `max(lgwin, 18)` |
| Input fragment | `&[u8]` slice of the caller's input | borrowed for the call | `FastEncoder::encode_block` |
| Output sink | `Vec<u8>` / `&mut [u8]` / inner `Write` | caller-owned | fragments are appended as they complete |
| Workspace / allocator | `core::fast::FastEncoder` (`table`, `storage`, `FastCore`) | owned by the encoder, reused across fragments | allocated once, cleared per fragment |
| Bit writer | `core::fast::bits::BitWriter` | borrows the encoder's scratch buffer | every meta-block |
| Huffman builder | `core::fast::huffman` | operates on arena-owned scratch | literal, command and distance codes |
| Error type | `compressor::BrotliCompressError` | returned by value | `UnsupportedQuality`, `OutputTooSmall`, `BufferOverflow`, `BoundOverflow` |
| Streaming state | `CompressorWriter` / `CompressorReader`, each owning a `FastEncoder` | created lazily on first use | carries `last_bytes` across fragments |
| SIMD level | `fearless_simd::Level`, stored in `Brotli` | `Copy`, detected once per process | one `dispatch!` per fragment |

## Integration rule

```text
quality == 0  -> core::fast q0 one-pass
quality == 1  -> core::fast q1 two-pass
quality >= 2  -> BrotliCompressError::UnsupportedQuality
```

Routing uses the existing `QualityLevel`; no second way to specify a
quality was added.

## Changes made to the existing public API

The port avoided inventing a parallel API, but three existing items were
unusable as written and had to change. All three were `todo!()` or provably
broken before this change.

| Symbol | Before | After | Why |
| --- | --- | --- | --- |
| `Compressor::compress_to_slice` | `-> BrotliResult<()>` | `-> BrotliResult<usize>` | The caller could not learn how many bytes were written, which makes the entry point unusable. |
| `Compressor::calculate_bound` | `-> usize`, `todo!()` | `-> BrotliResult<usize>` | The bound can overflow `usize`; saturating would return a value that no longer bounds anything. |
| `QualityLevel: TryFrom<usize>` | `todo!()` | implemented | Panicking on a public conversion is not acceptable. |

Additions that do not change existing signatures:

- `CompressorWriter::finish` and `get_ref`, `CompressorReader::get_ref`.
  A `Write` implementation has no terminating hook, so the stream cannot be
  closed without one.
- New variants on the two `#[non_exhaustive]` error enums:
  `BrotliCompressError::{UnsupportedQuality, OutputTooSmall, BufferOverflow, BoundOverflow}`
  and `ParseQualityLevelError::Unrepresentable`.
- `impl From<BrotliCompressError> for std::io::Error`, so the streaming
  adapters can report encoder errors.

## Dependency and MSRV report

| Dependency | Version | Note |
| --- | --- | --- |
| `fearless_simd` | pinned `=0.7.0` | requires Rust 1.89 or newer; `libm` feature kept, `std` not enabled beyond what was already there |
| `thiserror` | `2` | unchanged |
| `hotpath` | `0.24` | unchanged, still behind its feature flags |
| `google-brotli-ffi` | workspace path | dev-dependency only; vendored Brotli is pinned to v1.2.0, commit `028fb5a` |
| `afl` | `0.18` | fuzz package only, excluded from the workspace |

The crate declares no `rust-version`. The effective floor is Rust 1.89, set by
the pinned `fearless_simd`; `slice::as_chunks` (1.88) and edition 2024 (1.85)
are below it. No MSRV was raised silently, because none was declared.

Tests additionally enable `fearless_simd/force_support_fallback` through a
dev-dependency, so `Level::fallback()` is available to the backend equality
tests without putting it in the distributable build.

## Reusable primitives found

None. The repository had no bit writer, no prefix-code builder and no shared
constant tables before this change, so all three were added under
`core::fast/`.
