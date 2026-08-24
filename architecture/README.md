# Architecture

This directory is the always-current description of what `mbrotli` implements
today. Each specification covers one subsystem: its module boundaries, public
API surface, control and data flow, state machines, SIMD dispatch points, and
error propagation, with Mermaid diagrams for the mechanics it describes.

## Index

| Specification | Summary |
| --- | --- |
| [compressor.md](compressor.md) | Compressor subsystem: SIMD level detection and hand-off, parameter and bound types, quality routing, one-shot and streaming compression paths, error model, verification topology, and current implementation gaps. |
| [fast-encoder.md](fast-encoder.md) | Quality 0 and quality 1 encoder core: module map, workspace ownership, fragment lifecycle, the two scan state machines, bitstream layer, SIMD dispatch points, and table-bit specialisation. |
| [greedy-encoder.md](greedy-encoder.md) | Quality 3, 4 and 5 encoder core: parameter resolution and the deterministic hasher plan, ring buffer layout, greedy and lazy command generation, meta-block splitting and context modelling, the static dictionary, and the single SIMD dispatch. |
| [fuzzing.md](fuzzing.md) | AFL fuzzing subsystem: package isolation, the engine-neutral target layer, input model and payload cap, the eleven targets and their oracles, backend deduplication, and the crash-to-regression lifecycle. |

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
        bound["core::bound<br/>(compressed-size bound)"]
        driver["core::driver<br/>(quality routing, one-shot entry points)"]
        shared["core::shared<br/>(bits, huffman, match_len,<br/>fast_log, tables, constants)"]
        fast["core::fast<br/>(FastEncoder, SIMD dispatch)"]
        q0["fast::q0<br/>(one-pass encoder)"]
        q1["fast::q1<br/>(two-pass encoder)"]
        greedy["core::greedy<br/>(GreedyEncoder, SIMD dispatch)"]
        gsearch["greedy::hashers,<br/>backward_references, dictionary"]
        gblock["greedy::metablock, split,<br/>context_model, bitstream"]
    end

    subgraph external["Workspace / dependencies"]
        simd["fearless_simd::Level"]
        ffi["google-brotli-ffi<br/>(dev-only: benchmark and test oracle)"]
    end

    lib --> comp
    comp --> reader
    comp --> writer
    comp --> core
    core --> bound
    core --> driver
    driver --> fast
    driver --> greedy
    fast --> q0
    fast --> q1
    greedy --> gsearch
    greedy --> gblock
    q0 --> shared
    q1 --> shared
    gsearch --> shared
    gblock --> shared
    lib --> simd
    comp --> simd
    reader --> simd
    writer --> simd
    ffi -.->|benches and tests| comp

    classDef privateNode fill:#f6e8c3,stroke:#8a6d3b;
    class core,bound,driver,shared,fast,q0,q1,greedy,gsearch,gblock privateNode;
```

`core` and everything below it are private: no `core` type, SIMD type detail,
FFI detail, or low-level error escapes the public API. The public modules own
the ergonomic surface; `core` owns the algorithms.

## Repository layout

| Path | Role |
| --- | --- |
| `src/lib.rs` | Crate root; `Brotli` entry point and SIMD level detection. |
| `src/compressor/` | Public compressor API, parameters, error types. |
| `src/compressor/core/` | Private implementation modules: bound, quality routing. |
| `src/compressor/core/shared/` | Primitives every quality shares: bit writer, Huffman builders, match-length scan, reference logarithms, format constants and entropy tables. |
| `src/compressor/core/fast/` | Quality 0 and quality 1 encoders and their SIMD dispatch. |
| `src/compressor/core/greedy/` | Quality 3, 4 and 5 encoder: ring buffer, match finders, static dictionary, meta-block builder, bitstream writer and its SIMD dispatch. |
| `docs/` | Port documentation: API binding, design, reference differences, benchmark report. |
| `fuzz/afl/` | AFL fuzz targets and their regression corpus, excluded from the workspace. |
| `brotli-ffi/` | Workspace crate binding Google's C Brotli; `vendor/` is upstream source and is not hand-edited. |
| `examples/` | Runnable example mirroring the README, so the documented usage stays compiled. |
| `benches/` | Criterion benchmarks comparing this crate with the C implementation. |
| `tests/` | Integration tests over the public API. |
| `architecture/` | This directory. |
