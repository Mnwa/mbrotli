# Compressor Subsystem

Scope: `src/lib.rs`, `src/compressor/`, and the private `src/compressor/core/`
tree, excluding the fast encoder core itself, which has its own specification
in [fast-encoder.md](fast-encoder.md). This document describes the code as it
stands; the [Known gaps](#known-gaps) section lists what is not implemented.

## 1. Core mechanics

The subsystem is a three-layer funnel:

1. **Level layer** — `Brotli` resolves the SIMD instruction set once, at
   construction time, and carries it as a `Copy` value.
2. **API layer** — `Compressor` pairs that level with per-call
   `CompressParams` and exposes four entry points: bound calculation,
   one-shot to a `Vec`, one-shot into a caller slice, and the two streaming
   adapters.
3. **Core layer** — private `compressor::core` modules own the algorithms:
   `core::bound` computes the compressed-size upper bound, and `core::fast`
   owns the quality 0 and quality 1 encoders plus the single runtime SIMD
   dispatch.

SIMD selection is hoisted out of every inner loop: it is decided once in
`Brotli::default()` and then threaded, by value, into whatever implementation
runs. Nothing below the API layer re-detects features.

### 1.1. Ownership and data flow

```mermaid
graph LR
    detect["Level::try_detect()<br/>fallback: Level::baseline()"]
    brotli["Brotli { level }"]
    compressor["Compressor { level }"]
    params["CompressParams<br/>{ quality, lgwin }"]

    oneshot["compress / compress_to_slice"]
    rd["CompressorReader<br/>{ reader, level, params, encoder }"]
    wr["CompressorWriter<br/>{ writer, level, params, encoder }"]
    bound["core::bound::bound(&params, input_size)"]
    fast["core::fast<br/>(FastEncoder, dispatch)"]

    detect --> brotli
    brotli -->|"From&lt;Brotli&gt;"| compressor
    compressor --> oneshot
    compressor -->|compress_reader| rd
    compressor -->|compress_writer| wr
    params --> oneshot
    params --> rd
    params --> wr
    oneshot --> bound
    oneshot --> fast
    rd --> fast
    wr --> fast
```

### 1.2. Type relationships

```mermaid
classDiagram
    class Brotli {
        -Level level
        +compressor() Compressor
    }
    class Compressor {
        -Level level
        +calculate_bound(params, usize) BrotliResult~usize~
        +compress(params, src) BrotliResult~Vec~u8~~
        +compress_to_slice(params, src, dst) BrotliResult~usize~
        +compress_writer(params, w) CompressorWriter
        +compress_reader(params, r) CompressorReader
    }
    class CompressParams {
        -QualityLevel quality
        -WindowBits lgwin
        +new(quality, lgwin)
        +quality() QualityLevel
        +lgwin() WindowBits
    }
    class WindowBits {
        <<newtype usize>>
        +MIN = 10
        +MAX = 24
        +DEFAULT = 22
    }
    class QualityLevel {
        <<enum Q0..Q9, Q11>>
    }
    class FastEncoder {
        <<private>>
        -Level level
        -FastCore core
        -usize block_size_limit
        -u16 last_bytes
        -u32 last_bytes_bits
        +encode_block(input, is_last) BrotliResult~&[u8]~
    }

    Brotli --> Compressor : From
    Compressor --> CompressParams : uses
    CompressParams *-- QualityLevel
    CompressParams *-- WindowBits
    Compressor ..> FastEncoder : drives
    CompressorReader *-- FastEncoder
    CompressorWriter *-- FastEncoder
```

### 1.3. Parameter validation

Both parameter types make the invalid state unrepresentable rather than
validating at use:

- `WindowBits` is only constructible through `TryFrom<usize>`, which
  rejects anything outside `10..=24`, or through the `MIN` / `MAX` / `DEFAULT`
  associated constants.
- `QualityLevel` is a closed enum. `TryFrom<usize>` rejects values above
  eleven and reports quality 10, which the enum does not model, as
  `ParseQualityLevelError::Unrepresentable`.

Quality routing happens once, when a `FastEncoder` is built: qualities 0 and 1
enter the fast path and everything else is refused with
`BrotliCompressError::UnsupportedQuality`.

## 2. One-shot compression path

`compress` and `compress_to_slice` reproduce the reference one-shot entry
point, including its two special cases.

```mermaid
sequenceDiagram
    participant Caller
    participant API as Compressor
    participant Fast as core::fast
    participant Enc as FastEncoder

    Caller->>API: compress(params, src)
    API->>API: calculate_bound(&params, src.len())?
    API->>Fast: compress_to_vec(level, &params, src, &mut out)
    alt src is empty
        Fast-->>API: out = [0x06]
    else
        Fast->>Enc: FastEncoder::new(level, params)?
        loop each 1 << lgwin fragment
            Fast->>Enc: encode_block(fragment, is_last)
            Enc-->>Fast: completed bytes
            Fast->>Fast: out.extend_from_slice(bytes)
        end
        opt out longer than max_compressed_size(src.len())
            Fast->>Fast: rewrite as uncompressed meta-blocks
        end
    end
    Fast-->>API: Ok(())
    API-->>Caller: Ok(out)
```

`compress_to_slice` follows the same flow but copies each completed fragment
into the caller's buffer, and reports `OutputTooSmall` instead of growing.
Because the uncompressed fallback can still shrink the result, a buffer that
was too small for the compressed form is only fatal once the fallback has been
ruled out.

## 3. Streaming paths

Both adapters own a `FastEncoder` created lazily on first use, so a quality the
encoder does not implement surfaces as an `io::Error` at the first write or
read rather than at construction.

```mermaid
stateDiagram-v2
    [*] --> Idle: compress_writer / compress_reader
    Idle --> Buffering: first write / read
    Buffering --> Buffering: input below one fragment
    Buffering --> Emitting: more than one fragment buffered
    Emitting --> Buffering: fragment encoded, is_last = false
    Buffering --> Finished: finish() / inner reader at EOF
    Emitting --> Finished: final fragment, is_last = true
    Finished --> [*]
```

The writer only emits a fragment once **more** than a whole fragment is
buffered, so the final call always has data for the terminating meta-block.
`Write::flush` flushes the inner writer without terminating the stream, because
a fragment boundary need not fall on a byte boundary;
`CompressorWriter::finish` writes the final meta-block and returns the
inner writer.

The reader keeps one byte more than a fragment buffered, which is what lets it
tell a full fragment apart from the last one. That makes its output identical
to the one-shot path for the same window size, aside from the one-shot special
cases.

## 4. Error model

```mermaid
graph TD
    io["std::io::Error"] -->|"#[from]"| err["BrotliCompressError"]
    unsup["UnsupportedQuality(usize)"] --> err
    small["OutputTooSmall"] --> err
    overflow["BufferOverflow"] --> err
    boundovf["BoundOverflow"] --> err
    err -->|"From&lt;BrotliCompressError&gt;"| io2["std::io::Error<br/>(streaming adapters)"]
```

`BrotliCompressError` is `#[non_exhaustive]`. The conversion back into
`std::io::Error` unwraps a nested IO error rather than boxing it twice, so an
IO failure that entered through a streaming adapter comes back out with its
original kind.

Private encoder errors do not exist as a separate type: the fast encoder's only
failure modes are the ones above, and its internal buffer overflow is reported
through `BufferOverflow`, which no correct input can reach.

## 5. SIMD dispatch contract

```mermaid
graph TD
    A["Brotli::default()"] -->|"Level::try_detect()"| B["Level stored by value"]
    B --> C["Compressor { level }"]
    C --> D["FastEncoder { level }"]
    D -->|"once per fragment"| E["dispatch!(level, simd => encode_fragment)"]
    E --> F["q0 / q1 scan, generic over S: Simd"]
    F --> G["find_match_length(simd, ...)"]

    classDef once fill:#d9ead3,stroke:#38761d;
    class E once;
```

Feature detection happens once per process, inside `Level::new()`. The
`dispatch!` macro is expanded exactly once per fragment — that is, once per
`1 << lgwin` bytes — and never per meta-block, command, match or vector.

## 6. Verification topology

```mermaid
graph LR
    src["mbrotli encoder"] --> out["compressed bytes"]
    cref["Google Brotli v1.2.0 encoder"] --> cout["reference bytes"]
    out -->|byte equality| cout
    out --> dec["Google Brotli decoder"]
    dec -->|equality| input["original input"]
    out --> backends["every host SIMD backend"]
    backends -->|byte equality| out
```

| Test target | What it pins |
| --- | --- |
| `tests/differential_c.rs` | Byte identity with the pinned C encoder over structural and boundary corpora, every window size. |
| `tests/vendor_corpus.rs` | The same, over Google Brotli's own test data, including a multi-fragment 12 MiB input. |
| `tests/roundtrip.rs` | Independent decoder round-trip, determinism, and the compressed-size bound. |
| `tests/simd_backends.rs` | Byte identity between the scalar fallback and every SIMD backend the host supports. |
| `tests/streaming.rs` | Chunk-size independence, writer/reader agreement, one-shot equivalence. |
| `tests/randomized.rs` | Seeded property tests mixing literal runs, back-references and noise. |
| `fuzz/afl/` | AFL targets for the same oracles, seeded from the vendored Brotli test data. |

## Known gaps

- **Qualities 2 through 11 are not implemented.** `compress` returns
  `BrotliCompressError::UnsupportedQuality` for them. `QualityLevel` has
  no `Q10` variant, and `TryFrom<usize>` reports 10 as unrepresentable.
- **No decoder.** Round-trip verification uses Google's C decoder from the
  `google-brotli-ffi` workspace crate.
- **No large-window support.** The fast path never sets the large-window bit,
  matching the reference, so `lgwin` above 24 is not representable.
- **`Write::flush` does not terminate the stream.** Callers must use
  `CompressorWriter::finish`; dropping the adapter discards buffered
  input.
- **No `Send`/`Sync` guarantees are documented** beyond what the fields imply.
