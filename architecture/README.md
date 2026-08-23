# Architecture

This directory is the always-current description of what `mbrotli` implements
today. Each specification covers one subsystem: its module boundaries, public
API surface, control and data flow, state machines, SIMD dispatch points, and
error propagation, with Mermaid diagrams for the mechanics it describes.

`specifications/` is a different thing: it holds externally authored source
specifications (the Brotli q0/q1 encoder brief) that describe intended
behavior. This directory describes actual behavior, including what is still
unimplemented.

## Index

| Specification | Summary |
| --- | --- |
| [compressor.md](compressor.md) | Compressor subsystem: SIMD level detection and hand-off, parameter and bound types, one-shot and streaming compression paths, error model, verification topology, and current implementation gaps. |
| [fast-encoder.md](fast-encoder.md) | Quality 0 and quality 1 encoder core: module map, workspace ownership, fragment lifecycle, the two scan state machines, bitstream layer, SIMD dispatch points, and table-bit specialisation. |

## Module map

```mermaid
graph TD
    subgraph public["Public API"]
        lib["mbrotli<br/>(Brotli, SIMD level entry point)"]
        comp["compressor<br/>(Compressor, params, errors)"]
        reader["compressor::reader<br/>(CompressorReader: Read)"]
        writer["compressor::writer<br/>(CompressorWriter: Write)"]
    end

    subgraph private["Private implementation"]
        core["compressor::core"]
        bound["compressor::core::bound<br/>(compressed-size bound)"]
        fast["compressor::core::fast<br/>(FastEncoder, SIMD dispatch)"]
        q0["fast::q0<br/>(one-pass encoder)"]
        q1["fast::q1<br/>(two-pass encoder)"]
        shared["fast::bits, commands,<br/>huffman, histogram, match_len,<br/>tables, workspace"]
    end

    subgraph external["Workspace / dependencies"]
        simd["fearless_simd::Level"]
        ffi["google-brotli-ffi<br/>(dev-only: benchmark oracle)"]
    end

    lib --> comp
    comp --> reader
    comp --> writer
    comp --> core
    core --> bound
    core --> fast
    fast --> q0
    fast --> q1
    q0 --> shared
    q1 --> shared
    lib --> simd
    comp --> simd
    reader --> simd
    writer --> simd
    ffi -.->|benches only| comp

    classDef privateNode fill:#f6e8c3,stroke:#8a6d3b;
    class core,bound,fast,q0,q1,shared privateNode;
```

`core` and everything below it are private: no `core` type, SIMD type detail,
FFI detail, or low-level error escapes the public API. The public modules own
the ergonomic surface; `core` owns the algorithms.

## Repository layout

| Path | Role |
| --- | --- |
| `src/lib.rs` | Crate root; `Brotli` entry point and SIMD level detection. |
| `src/compressor/` | Public compressor API, parameters, error types. |
| `src/compressor/core/` | Private implementation modules. |
| `src/compressor/core/fast/` | Quality 0 and quality 1 encoders, bitstream, Huffman and SIMD dispatch. |
| `docs/` | Port documentation: API binding, design, reference differences, benchmark report. |
| `fuzz/afl/` | AFL fuzz targets, excluded from the workspace. |
| `brotli-ffi/` | Workspace crate binding Google's C Brotli; `vendor/` is upstream source and is not hand-edited. |
| `examples/` | Runnable example mirroring the README, so the documented usage stays compiled. |
| `benches/` | Criterion benchmarks comparing this crate with the C implementation. |
| `tests/` | Integration tests over the public API. |
| `specifications/` | Externally authored source specifications. |
| `architecture/` | This directory. |
