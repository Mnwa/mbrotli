# Compressor Subsystem

Scope: `src/lib.rs`, `src/compressor/`, and the private `src/compressor/core/`
tree, excluding the three encoder cores themselves, which have their own
specifications in [fast-encoder.md](fast-encoder.md),
[greedy-encoder.md](greedy-encoder.md) and [hq-encoder.md](hq-encoder.md).
This document describes the code as it stands; the [Known gaps](#known-gaps)
section lists what is not implemented.

## 1. Core mechanics

Five layers, each owning one idea:

1. **Configuration** — `EncoderConfig` and the validated values inside it. What
   holds for every stream, and nothing else. No input length, no offset, no
   dictionary, no buffer.
2. **The compressor** — `Compressor` owns a resolved SIMD level, a validated
   configuration, and every buffer the encoders need. Encoding takes `&mut self`.
3. **The dictionary** — `PreparedDictionary` is immutable RFC 9841 knowledge many
   compressors can borrow at once. It is not owned by a compressor.
4. **The session** — `EncoderSession` is the one incremental state machine. The
   `io` adapters are built on it; the one-shot entry points share its encoders
   and its parameter resolution.
5. **The core** — private `compressor::core` modules own the algorithms and the
   bitstream. Nothing from them escapes.

The public types are a redesign; the `core` tree is not. It is still written
against the encoders' own `CompressParams`, `QualityLevel`, `WindowBits` and
`BrotliCompressError`, which now live in the private `compressor::internal`
module. The public configuration *lowers* into those on the way down. Keeping
the two apart is what let the surface change without moving a byte of the
bitstream.

### 1.1. Module map

```mermaid
graph TD
    subgraph public["Public API"]
        lib["mbrotli<br/>(crate root re-exports)"]
        config["compressor::config<br/>(EncoderConfig, Quality, Window,<br/>BlockSize, BlockBits, CompressionMode,<br/>DistanceParams, LiteralContextMode,<br/>ConfigError, SizeOverflow)"]
        enc["compressor::encoder<br/>(Compressor, CompressorBuilder,<br/>RetentionPolicy)"]
        sess["compressor::session<br/>(EncoderSession, StreamConfig, InputSize,<br/>Operation, Progress, EncoderStatus)"]
        err["compressor::error<br/>(EncodeError)"]
        dict["compressor::dictionary<br/>(PreparedDictionary, DictionaryBuilder,<br/>DictionaryLimits, DictionaryError)"]
        io["compressor::io<br/>(EncoderReader, EncoderWriter,<br/>EncoderReaderParts, FinishError)"]
    end

    subgraph bridge["Private bridge"]
        internal["compressor::internal<br/>(CompressParams, QualityLevel, WindowBits,<br/>CompressMode, DistanceCodes,<br/>BrotliCompressError)"]
        sharederr["compressor::shared<br/>(SharedBrotliError)"]
    end

    subgraph private["Private implementation"]
        core["compressor::core"]
        bound["core::bound<br/>(compressed-size bound)"]
        driver["core::driver<br/>(quality routing, EncoderCache,<br/>one-shot engines)"]
        rfc["core::rfc9841<br/>(ResolvedWindow, SharedContextInner,<br/>PrefixSources, PreparedPrefix, search)"]
        shared["core::shared<br/>(bits, huffman, match_len, command,<br/>histogram, ringbuffer, dictionary,<br/>block_split, metablock, bitstream, ...)"]
        fast["core::fast (q0, q1)"]
        greedy["core::greedy (q2 to q9)"]
        hq["core::hq (q10, q11)"]
    end

    lib --> config
    lib --> enc
    lib --> sess
    lib --> err
    lib --> dict
    lib --> io
    io --> sess
    sess --> enc
    enc --> config
    enc --> err
    enc --> dict
    config --> internal
    err --> internal
    err --> sharederr
    dict --> rfc
    enc --> driver
    enc --> bound
    driver --> internal
    driver --> rfc
    driver --> fast
    driver --> greedy
    driver --> hq
    rfc --> greedy
    rfc --> hq
    fast --> shared
    greedy --> shared
    hq --> shared

    classDef privateNode fill:#f6e8c3,stroke:#8a6d3b;
    class core,bound,driver,rfc,shared,fast,greedy,hq,internal,sharederr privateNode;
```

### 1.2. Type relationships

```mermaid
classDiagram
    class EncoderConfig {
        -Quality quality
        -Window window
        -BlockSize block_size
        -CompressionMode mode
        -DistanceParams distance
        -LiteralContextMode literal_context
        +with_quality(Quality) EncoderConfig
        +with_window(Window) EncoderConfig
        +with_block_size(BlockSize) EncoderConfig
        +with_mode(CompressionMode) EncoderConfig
        +with_distance(DistanceParams) EncoderConfig
        +with_literal_context(LiteralContextMode) EncoderConfig
    }
    class Compressor {
        -Level level
        -EncoderConfig config
        -RetentionPolicy retention
        -EncoderCache workspace
        -Vec~u8~ staging
        -Vec~u8~ pending
        -bool active
        +new(EncoderConfig) Result
        +builder(EncoderConfig) CompressorBuilder
        +config() &EncoderConfig
        +reconfigure(EncoderConfig) Result
        +max_compressed_size(usize)$ Result
        +compress(&mut self, src) Result~Vec~u8~~
        +compress_into(&mut self, src, dst) Result~Range~
        +compress_to_slice(&mut self, src, dst) Result~usize~
        +compress_with_dictionary*(&mut self, &dict, ...) Result
        +start(StreamConfig) Result~EncoderSession~
        +start_with_dictionary(&dict, StreamConfig) Result~EncoderSession~
        +writer/reader(&mut self, io, StreamConfig) Result
        +retained_bytes() usize
        +trim(RetentionPolicy)
        +recover()
        +fork_empty() Compressor
    }
    class StreamConfig {
        -InputSize input_size
        -u64 stream_offset
    }
    class EncoderSession {
        -SessionCore core
        +process(input, output, Operation) Result~Progress~
        +is_finished() bool
    }
    class SessionCore {
        <<private>>
        -&mut Compressor compressor
        -Option~&PreparedDictionary~ dictionary
        -StreamState state
        -u64 logical_position
    }
    class StreamState {
        <<private>>
        -Phase phase
        -usize limit
        -bool flint
    }
    class PreparedDictionary {
        <<immutable, Send + Sync>>
        -SharedContextInner inner
        +attachment_count() usize
        +source_bytes() usize
        +retained_bytes() usize
        +backward_distance(u64, u64) Option~u64~
        +prefix_offset(u64, u64) Option~u64~
    }
    class EncoderWriter {
        -EncoderSession session
        -W sink
        -Vec~u8~ outbox
        -usize head
        -usize end
        -State state
        +try_finish() io::Result
        +finish() Result~W, FinishError~
    }
    class EncoderReader {
        -EncoderSession session
        -R source
        -Vec~u8~ input
        -usize head
        +into_parts() EncoderReaderParts
    }
    class CompressParams {
        <<private>>
        -QualityLevel quality
        -WindowBits lgwin
        -Option~BlockBits~ lgblock
        -CompressMode mode
        -Option~usize~ size_hint
        -DistanceCodes distance_codes
        -bool literal_context_modeling
    }

    Compressor *-- EncoderConfig
    Compressor ..> CompressParams : lowers into
    EncoderSession *-- SessionCore
    SessionCore *-- StreamState
    SessionCore --> Compressor : borrows &mut
    SessionCore ..> PreparedDictionary : borrows &
    EncoderWriter *-- EncoderSession
    EncoderReader *-- EncoderSession
    Compressor ..> StreamConfig : starts a session from
```

### 1.3. Where a value is validated

Each configuration value is a type that cannot hold what the format cannot
express, so the validation happens once, where the value is built. What one
value cannot know — whether it agrees with the others — is checked by
`Compressor::new` and `reconfigure`, which are the only places that see the whole
configuration.

```mermaid
flowchart TD
    q["Quality::try_from(u8)"] -->|"> 11"| qe["ConfigError::Quality"]
    w1["Window::standard(bits)"] -->|"outside 10..=24"| we1["ConfigError::StandardWindow"]
    w2["Window::large(bits)"] -->|"outside 10..=62"| we2["ConfigError::LargeWindow"]
    b["BlockBits::try_from(u8)"] -->|"outside 16..=24"| be["ConfigError::BlockBits"]
    d["DistanceParams::explicit(p, n)"] -->|"p > 3"| de1["ConfigError::DistancePostfixBits"]
    d -->|"n > 120"| de2["ConfigError::DirectDistanceCodes"]
    d -->|"not a whole group"| de3["ConfigError::MisalignedDistanceCodes"]

    q --> cfg["EncoderConfig"]
    w1 --> cfg
    w2 --> cfg
    b --> cfg
    d --> cfg
    cfg --> new["Compressor::new / reconfigure"]
    new -->|"Large window at q0, q1 or q2"| cross["ConfigError::LargeWindowUnsupportedForQuality"]
    new -->|otherwise| ok["Compressor"]
```

The reference silently drops a Large Window below quality three, because those
qualities may write distances through a code built for the RFC 7932 alphabet.
This crate refuses it instead, and refuses it before any input has been touched:
a stream that quietly stopped being a Large Window stream is invisible until a
decoder disagrees.

### 1.4. Lowering, and the size hint

`EncoderConfig::lower(size_hint)` produces the private `CompressParams` the
encoders take. The size hint is not part of the configuration — it is what one
*operation* knows about how much input is coming — so it arrives as an argument:

| Entry point | What it declares |
| --- | --- |
| `compress`, `compress_into`, `compress_to_slice` | `Some(src.len())`, which is what `BrotliEncoderCompress` sets `BROTLI_PARAM_SIZE_HINT` to |
| `start`, `writer`, `reader` | `Some(stream.input_size().hint())`, which is `0` for `InputSize::Unknown` — what `BrotliEncoderCompressStream` leaves it at |

Qualities four and five choose their match finder from the hint, so declaring
`InputSize::Exact(n)` is what makes a streamed stream reproduce the same input's
one-shot bytes.

## 2. The compressor and its workspace

`Compressor` owns a `core::driver::EncoderCache`: one retained encoder plus the
SIMD level it was built for. On each operation the cache resets the retained
encoder when the new parameters resolve to the same shape and rebuilds it
otherwise, so reuse can never change a byte:

| Encoder | What "same shape" means |
| --- | --- |
| `Fast` | `FastEncoder::matches` — same quality, fragment limit and stream header |
| `Greedy` | `GreedyParams` compares equal, which covers the matcher, both block sizes and the distance alphabet |
| `Hq` | `HqParams` compares equal |

Two details make the reset correct rather than merely cheap, and both predate
this redesign: `MatchFinder::prepare`'s partial sweep is replayed over the
previous stream's own bytes before the window is dropped, and the ring buffer is
not wiped because a backward reference is bounded by the distance to the start of
the stream. See [greedy-encoder.md](greedy-encoder.md).

A call that fails part-written drops the retained encoder rather than resetting
it, so no half-written stream can reach the next call.

### 2.1. Retention

```mermaid
flowchart TD
    op["an operation finishes"] --> pol{"RetentionPolicy"}
    pol -->|Aggressive| keep["keep everything"]
    pol -->|CurrentConfig| keep
    pol -->|"Bounded { max_bytes }"| cmp{"retained_bytes() > max_bytes?"}
    pol -->|ReleaseAll| drop["drop encoder and staging/pending allocations"]
    cmp -->|yes| drop
    cmp -->|no| keep
    recfg["reconfigure to a different config"] --> pol2{"RetentionPolicy"}
    pol2 -->|CurrentConfig| drop
    pol2 -->|other| keep2["keep until the next call replaces it"]
```

`retained_bytes` counts every owned heap allocation: staging/pending buffers,
boxed arenas, window and matcher tables, commands, entropy-code buffers,
meta-block splits, histogram clusters and HQ cost/search workspace. Shared
dictionary storage and caller-owned output are excluded. Allocator-instrumented
integration tests compare the sum with actual live requested bytes at every
quality. `trim(policy)` applies a policy once without changing the configured
policy. The configured policy runs after one-shot completion and session drop,
including writer/reader completion and abandonment.

### 2.2. Abandoned sessions

A session borrows the compressor exclusively, so nothing else can touch it while
one lives, and `Drop` cleans up. `std::mem::forget` can skip that `Drop`, which
is the one way a compressor can be left holding state no operation has cleaned
up. The compressor keeps an `active` flag for exactly that case.

```mermaid
stateDiagram-v2
    [*] --> Idle
    Idle --> Active: start() sets active = true
    Active --> Idle: session dropped, active = false
    Active --> Abandoned: mem::forget(session)
    Abandoned --> Abandoned: any operation returns AbandonedSession
    Abandoned --> Idle: recover()
```

A session that finished retains its encoder subject to its retention policy.
One abandoned part-way drops the encoder before applying that policy to the
remaining buffers. Final bytes still waiting for delivery keep `is_finished()`
false, even after the encoder has emitted its last block.

## 3. One-shot paths

`compress_into` is the primary entry point; `compress` is it with a fresh `Vec`.
Both use the same encoding contract as sessions, including empty input.

```mermaid
sequenceDiagram
    participant Caller
    participant C as Compressor
    participant Drv as core::driver
    participant State as core::stream::StreamState
    participant Enc as Encoder

    Caller->>C: compress_into(src, dst)
    C->>C: ensure_available()?  (abandoned session)
    C->>C: config.lower(Some(src.len()))
    C->>C: dst.try_reserve(bound(params, src.len()))?
    C->>Drv: compress_to_vec_attached(&mut workspace, level, params, dictionary, src, dst)
    Drv->>Enc: workspace.acquire(level, params), including empty input
    Drv->>State: process(src, Append(dst), Finish)
    loop shared block scheduling, borrowed input
        State->>Enc: encode_block_with(block, is_last, dictionary)
        Enc-->>State: completed bytes, possibly none
        State->>State: append completed bytes
    end
    State-->>Drv: Progress
    Drv-->>C: Ok(())
    C->>C: finish_operation()  (retention policy)
    C-->>Caller: Ok(start..dst.len())
```

On failure `dst` is truncated back to the length it had, so whatever the caller
already had in it survives byte for byte.

`compress_to_slice` follows the same flow but writes each completed block into
the caller's buffer and reports `OutputTooSmall` instead of growing or switching
to another encoding. Exactly the vector's final length is enough. Both destinations use the same
`core::stream::StreamState` block scheduler as sessions. One-shot calls use empty,
unallocated staging/pending vectors because all input and finalization are known.

`Compressor::max_compressed_size` is an associated function, so it needs no
compressor. It is therefore the bound for the *loosest* configuration — a
ten-bit window at a quality that cuts its input at the window, which pays the
per-meta-block reservation most often — and a buffer sized by it fits whatever
the compressor is set to. The tighter, configuration-specific bound is what
`compress_into` reserves internally.

## 4. The session

`EncoderSession::process(input, output, operation)` is the whole streaming API.
It takes what it can, writes what it can, and reports exactly how much of each it
moved.

```mermaid
stateDiagram-v2
    [*] --> Open: start()
    Open --> Open: Process — stage input, emit whole blocks
    Open --> Flushed: Flush — emit the staged block and realign
    Flushed --> Open: any input staged
    Flushed --> Flushed: Flush again emits nothing
    Open --> Finished: Finish — emit the last block with is_last
    Flushed --> Finished: Finish
    Finished --> Finished: drain remaining final output, consume nothing
    Open --> Failed: the encoder reported a failure
    Failed --> Failed: process returns InvalidState
```

The loop inside one `process` call is:

```mermaid
flowchart TD
    start(["process(input, output, op)"]) --> failed{"phase == Failed?"}
    failed -->|yes| inval["Err(InvalidState)"]
    failed -->|no| drain["deliver pending output into `output`"]
    drain --> more{"pending left?"}
    more -->|yes| needout["return NeedsOutput"]
    more -->|no| fin{"phase == Finished?"}
    fin -->|yes| done["return Finished"]
    fin -->|no| tail{"block end or explicit operation known?"}
    tail -->|no| stage["stage undecided tail, return NeedsInput"]
    tail -->|yes| source["borrow complete input block or complete staging"]
    source --> full{"input follows this block?"}
    full -->|yes| encode["encode_block_with(is_last = false)"]
    encode --> drain
    full -->|no| op{"operation"}
    op -->|Process| needin["return NeedsInput"]
    op -->|Flush| flushed{"already Flushed?"}
    flushed -->|yes| needin
    flushed -->|no| doflush["flush_block, phase = Flushed"]
    doflush --> drain
    op -->|Finish| last["encode_block_with(is_last = true), phase = Finished"]
    last --> drain
```

Experimental continuation restarts additionally flush the first two bytes;
the same scheduler handles that bounded restart before normal block scheduling.

Two properties fall out of that shape:

- **A non-final block is only encoded once something is known to follow it.**
  The shared scheduler borrows complete blocks directly when their end is known;
  it stages only a tail that must wait for later input or an operation. That keeps the last block of a
  stream the one carrying `is_last`, and therefore what makes a streamed stream
  reproduce the one-shot bytes for an input that is a whole number of blocks.
- **A call that moved nothing always says why.** Zero consumed and zero produced
  is only ever returned alongside `NeedsInput`, `NeedsOutput` or `Finished`, so
  no caller can spin.

Encoded bytes are copied straight into the caller's slice, and only what does not
fit is held in the compressor's `pending` buffer. Fast encoders can write directly
into a caller slice with enough reservation; other cases use retained encoder
scratch. No extra staging copy is made for a directly borrowed input block.

### 4.1. Universal API byte identity

One-shot and incremental encoding produce the same bytes for equivalent stream
settings, including empty input and incompressible small-window streams. One-shot
calls no longer use C's empty shortcut or whole-stream uncompressed rewrite.
A zero-offset session declaring `InputSize::Exact` and no extra flushes is the
incremental equivalent of a one-shot call. Unknown size, different dictionaries,
explicit flush boundaries or continuation offsets can change encoding decisions.

C streaming FINISH with matching settings is the differential oracle, not native
C one-shot. C's q0/q1 PROCESS calls also emit fragments at caller chunk boundaries,
whereas Rust stages undecided tails. The Criterion streaming adapter normalizes
those C chunks and charges staging to the timed C operation. Its one-shot adapter
borrows the whole input and calls C streaming FINISH without staging or rewrites.
See [universal-encoding.md](universal-encoding.md) for the decision and regressions.

### 4.2. Flushing

`Operation::Flush` mirrors `BROTLI_OPERATION_FLUSH` in two steps: the buffered
input is written out as a meta-block even where the encoder would rather keep
gathering, and the stream is realigned to a byte boundary by
`core::shared::bits::inject_byte_padding`. Nothing is emitted when there was no
buffered input *and* the stream was already aligned, which is what makes a
redundant flush free. A flush carries the attached dictionary, so the bytes after
one are still compressed against it.

Flushing trades ratio for latency and the trade is steep. Measured over 256 KiB
of text, flushing every kibibyte grew the stream 2.4 times at quality 11 and
seventeen times at quality 1.

## 5. The transactional writer

The old writer advanced encoder state before `write_all` had delivered the
resulting block, so a partial sink write followed by an error left no durable
cursor for the unwritten suffix. `EncoderWriter` keeps compressed bytes in a
cursor-addressed buffer until the sink has actually taken them.

The initialized outbox is bounded to 128 KiB and reused without clearing its
contents. Only `head..end` contains pending bytes. A large `write` can accept a
prefix and return its length; it never grows the outbox to buffer the whole
input's output. Flush and finish drain between bounded pulls.

```mermaid
sequenceDiagram
    participant Caller
    participant W as EncoderWriter
    participant S as EncoderSession
    participant Sink as inner writer

    Caller->>W: write(buf)
    W->>Sink: drain outbox[head..end]
    alt the sink fails
        Sink-->>W: Err
        W-->>Caller: Err, zero bytes accepted
        Note over Caller: the caller still owns every byte of `buf`
    else the sink takes them
        W->>S: process(buf, outbox spare room, Process)
        S-->>W: Progress { consumed, produced }
        W->>Sink: drain again, ignoring failure
        W-->>Caller: Ok(consumed)
        Note over W: a failure here waits for the next call,<br/>because the bytes are the adapter's now
    end

    Caller->>W: try_finish()
    W->>Sink: drain outbox[head..end]
    loop until session output is fully delivered
        W->>S: process(&[], bounded room, Finish)
        W->>Sink: drain
    end
    W->>Sink: flush
    W-->>Caller: Ok(())
```

The acceptance rule is the point: **a `write` never both consumes caller input
and returns an error**. Draining comes first, so a failing sink is reported with
nothing accepted and the caller may retry the very same bytes. Once bytes have
been accepted they are the adapter's responsibility, and a sink failure caused by
encoding them surfaces on a later call.

`drain` handles every shape a sink can misbehave in: a short write advances the
cursor, `Interrupted` retries, `WouldBlock` and any other error return with the
exact unwritten suffix preserved, and `Ok(0)` on a non-empty buffer becomes
`WriteZero`.

`try_finish` is retryable: the session's `Finish` is idempotent once finished, so
a sink failure part-way leaves the remaining bytes buffered and the next call
resumes at exactly the byte the sink stopped at. A second terminator is never
written. `finish` hands the adapter back inside `FinishError` rather than
destroying it, so a recoverable failure does not strand the stream.

`Drop` performs no I/O, cannot fail and reports nothing. Dropping an unfinished
writer abandons the stream and returns the compressor to a clean reusable state.

## 6. The reader

`EncoderReader` pulls source bytes into a buffer addressed by a `[head..len]`
cursor — nothing is ever moved to the front — and hands the session a slice of
what it has. Compressed bytes are written straight into the caller's slice, so
there is no intermediate queue at all.

- `read(&mut [])` returns zero without reading the source or initialising
  anything.
- Source `Interrupted` is retried inside `fill`.
- A source error propagates without duplicating anything already encoded.
- End of source switches the operation from `Process` to `Finish`, exactly once.
- `into_parts` returns the source together with the bytes it had read but the
  encoder had not yet accepted, so abandoning the adapter loses no input.

## 7. Error model

```mermaid
graph TD
    subgraph build["When the compressor is built"]
        ce["ConfigError<br/>Quality, StandardWindow, LargeWindow,<br/>BlockBits, DistancePostfixBits,<br/>DirectDistanceCodes, MisalignedDistanceCodes,<br/>LargeWindowUnsupportedForQuality"]
    end
    subgraph dictb["When the dictionary is built"]
        de["DictionaryError<br/>Empty, TooManyAttachments,<br/>TooLarge, PreparationTooLarge"]
    end
    subgraph run["When an operation runs"]
        ee["EncodeError<br/>OutputTooSmall, AllocationFailed, Bound,<br/>DictionaryUnsupportedForQuality,<br/>UnsupportedStreamOffset, StreamPositionOverflow,<br/>AbandonedSession, InvalidState, InternalInvariant"]
    end
    so["SizeOverflow"] -->|"#[from]"| ee
    core["core::BrotliCompressError<br/>(private)"] -->|from_core| ee
    ee -->|"From&lt;EncodeError&gt;"| io["std::io::Error<br/>(kind by variant, original as source)"]
```

Every public error enum is `#[non_exhaustive]`. The split is by *domain*, not by
layer: a configuration that could never work is refused before an operation
exists, a dictionary that could never be built is refused before a stream exists,
and what is left needs an operation in flight to happen at all.

`EncodeError::InternalInvariant` covers the low-level failures a validated
configuration cannot reach — an unimplemented quality, a refused large window, a
scratch buffer overflow. No valid caller input produces one.

## 8. SIMD dispatch contract

```mermaid
graph TD
    A["Compressor::new / CompressorBuilder::build"] -->|"Level::try_detect()"| B["Level stored in the Compressor"]
    B --> C["EncoderCache::acquire(level, params)"]
    C -->|"new encoder only"| D["core::dispatch::select(level)"]
    D --> E["retained Box dyn Kernels containing Selected&lt;S&gt;"]
    E -->|"block-boundary virtual call"| F["S::vectorize → monomorphized scan"]

    classDef once fill:#d9ead3,stroke:#38761d;
    class E once;
```

Feature detection happens when the compressor's opaque `Backend` is selected.
`CompressorBuilder::with_backend` accepts only a host-validated backend, including
`Backend::SCALAR`; no `fearless_simd` type appears in the public API. The single
selection dispatch occurs when the retained encoder is created, not per block.
`Selected<S>` retains the proof token across the operation/session and reuse.
Its feature-enabled kernel calls pass `S` by value into inner loops; no inner
loop has virtual dispatch or feature detection.

The public `EncoderSession` owns a private `core::session::SessionCore` wrapper.
It handles ownership and logical-position checks; the shared `core::stream`
module handles phase transitions, block scheduling and pending delivery for
both one-shot and incremental paths. Completed sessions ignore further input,
including at the maximum logical position.

## 9. Verification topology

```mermaid
graph LR
    src["mbrotli encoder"] --> out["compressed bytes"]
    cref["Google Brotli v1.2.0 encoder"] --> cout["reference bytes"]
    out -->|byte equality| cout
    out --> dec["Google Brotli decoder"]
    dec -->|equality| input["original input"]
    out --> backends["every host SIMD backend"]
    backends -->|byte equality| out
    out --> shapes["compress / compress_into /<br/>compress_to_slice / session /<br/>reader / writer"]
    shapes -->|byte equality| out
```

| Test target | What it pins |
| --- | --- |
| `tests/differential_c.rs` | Byte identity with the pinned C encoder over structural and boundary corpora, every window size, plus reuse and reconfiguration across calls. |
| `tests/greedy_qualities.rs` | Byte identity for every parameter qualities two to nine react to: window, mode, declared size, block size, distance layout, context modelling. Compared through a session, and additionally one-shot where the declared size is the true one. |
| `tests/vendor_corpus.rs` | The same, over Google Brotli's own test data, including a multi-fragment 12 MiB input. |
| `tests/roundtrip.rs` | Independent decoder round-trip, determinism between warm and cold compressors, and the compressed-size bound. |
| `tests/simd_backends.rs` | Byte identity between the scalar fallback and every SIMD backend the host supports. |
| `tests/streaming.rs` | Chunk-size independence, agreement between writer, reader and session, one-shot equivalence when the size is declared, the zero-progress rule, and reader read-ahead recovery. |
| `tests/flush.rs` | Flush semantics against the reference driven with `BROTLI_OPERATION_FLUSH`. |
| `tests/writer_faults.rs` | The transactional proof: a scripted sink failing at **every** byte position of a q0, q1, q5, q9 and q11 stream, short writes of one to sixty-four bytes, `Interrupted`, `WouldBlock`, `Ok(0)`, a failing inner flush, a failing finish handing the writer back, and an abandoned writer. Every schedule has to yield exactly one copy of the one-shot stream. |
| `tests/reuse.rs` | The stateful lifecycle: reuse, appending, deliberate failure, trim, reconfiguration, abandoned and leaked sessions, every retention policy, `fork_empty`. |
| `tests/dictionary.rs` | Byte identity with the reference's compound dictionary, decoder round-trip with the dictionary attached, refusal below quality five, entry-point agreement, attachment order, concurrent sharing. |
| `tests/large_window.rs` | Every RFC 9841 header, the refusal at qualities zero to two, and streams above the C decoder's limit. |
| `tests/public_api.rs` | The public surface from outside the crate: conversions, accessors, errors, `Send`/`Sync`. |
| `tests/randomized.rs` | Seeded property tests mixing literal runs, back-references and noise. |
| Unit tests in `core::*` | Layer-by-layer differential tests against encoder-internal C functions; see [hq-encoder.md](hq-encoder.md) §10. |
| `fuzz/afl/` | Twenty-two AFL targets for the same oracles plus parameter rejection, dictionary preparation and the compressor lifecycle; see [fuzzing.md](fuzzing.md). |

## Known gaps

- **No decoder.** Round-trip verification uses Google's C decoder from the
  `google-brotli-ffi` workspace crate.
- **Stream offsets require experimental quality 2 or above.** See
  [rfc9841-encoding.md](rfc9841-encoding.md) for headerless continuation,
  restart flushing and checked logical-position mechanics.
- **Large window is refused below quality 3.** Qualities 0, 1 and 2 may write
  distances through a code built for the RFC 7932 alphabet, so
  `Compressor::new` refuses the combination. Retained history stops at 30 bits
  however wide the header declares, so the reference's `H35`, `H55` and `H65`
  match finders remain unreachable.
- **A dictionary is refused below quality 5.** The reference compiles its
  compound-dictionary search only for the match finders qualities five and above
  select, and silently ignores the dictionary elsewhere; this crate refuses.
- **Serialized dictionaries and framing are experimental.** See
  [rfc9841-encoding.md](rfc9841-encoding.md) and [framing.md](framing.md).
- **One retained encoder.** The workspace holds exactly one, so alternating
  between two configurations rebuilds on every call. `RetentionPolicy` bounds and
  releases it but does not yet keep one slot per encoder family.
- **Native C API differences are intentional.** Universal Rust API identity
  takes priority over C one-shot empty/fallback shortcuts. C comparisons use
  equivalent streaming settings, as recorded in [universal-encoding.md](universal-encoding.md).
- **Release performance gates remain open.** Lazy bucket payloads and chain
  banks eliminate eager cold payload initialization, and allocator regressions
  enforce zero warmed allocations on text/binary/multi-block cases. They do not
  establish every speed/RSS gate on both AVX2 and NEON hardware.
