# Native decompressor

The `decompression` feature enables the public `decompressor` module, which owns configuration, reusable decoder ownership,
operation sessions, typed errors, and synchronous adapters. Its private `core`
owns wire parsing and regeneration. Production decoding uses safe Rust and
`alloc`, without a C or third-party decoder dependency. `no_std` disables I/O
adapters and profiling. Serialized/custom dictionaries require `experimental`;
RAW prefixes, built-in words/transforms, large windows, and ordinary context maps
need only `decompression`. Shared window, backend and retention types remain
available without the encoder. Prepared dictionary views additionally require
`compression`; see [codec features](codec-features.md).

```mermaid
graph TD
    API[decompressor: config, decoder, session, error] --> Core[private decompressor::core]
    IO[decompressor::io: reader / writer] --> API
    Facade[mbrotli::io] --> IO
    Facade --> Finish[private finish_error: shared FinishError]
    Facade -->|compression enabled| EncoderIO[compressor::io]
    Core --> Bits[bits / header]
    Core --> Entropy[huffman / block / context_map]
    Core --> Regen[stream / distance / dictionary]
    Core --> Memory[memory: fallible live workspace accounting]
    Regen --> Shared[private crate::shared: wire tables, dictionary bytes, transforms]
    Dict[dictionary: DecodeDictionary / PreparedDictionary] --> View[DictionaryRef borrowed view]
    View --> API
    Dict --> Store[shared::decode_dictionary / experimental decode_serialized]
```

## Public ownership and APIs

`Decompressor::new` and `builder` create an allocation-free workspace. A validated
`Backend` is selected once at construction; currently every backend uses the
same scalar decoder. There are no inner-loop feature checks or decoder SIMD
kernels. `fork_empty` copies policy/backend into an independent empty workspace.
`config`, `retention`, `retained_bytes`, `trim`, `recover`, and `reconfigure`
manage reuse without exposing core storage.

`DecoderConfig` combines `WindowLimit`, `MemberMode`, and `DecodeLimits`.
Default is `Single`, extended windows up to 62 bits, and no numeric budgets.
`DecodeStreamConfig` optionally specifies `OutputSize::Exact(u64)`, which is a
validation contract, never an allocation hint. Limits count accepted compressed
bytes, regenerated output, and live decoder-owned requested heap bytes. Input
and output totals span all concatenated members. Caller destinations, immutable
borrowed dictionaries, and fixed adapter buffers are excluded from workspace.

A bounded policy example:

```rust
use mbrotli::{DecodeLimits, DecoderConfig, Decompressor, WindowLimit};
let config = DecoderConfig::default()
    .with_window_limit(WindowLimit::standard(24)?)
    .with_limits(DecodeLimits::default()
        .with_max_input_bytes(Some(1 << 20))
        .with_max_output_bytes(Some(8 << 20))
        .with_max_workspace_bytes(Some(32 << 20)));
let mut decoder = Decompressor::new(config)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`decompress` returns a Vec, `decompress_into` appends and returns its range, and
`decompress_to_slice` fills the caller's initialized slice. Any Vec append is
rolled back on failure; a slice preserves the written prefix and untouched
suffix. `Single` one-shot calls reject tails. Dictionary variants accept
`impl Into<DictionaryRef<'dict>>` before the compressed source. Sessions borrow
the decoder and dictionary independently, without copying/rebuilding indexes.

## Session lifecycle and exact progress

```mermaid
stateDiagram-v2
    Idle --> Active: start / start_with_dictionary
    Active --> Active: Process / NeedsInput / NeedsOutput
    Active --> Finishing: first Finish latches total_in + input.len
    Finishing --> Finishing: Finish with exact unconsumed suffix
    Active --> Complete: validated Single member
    Finishing --> Complete: validated member and final input boundary
    Active --> Failed: format / resource error
    Finishing --> Failed: error / truncation / changed final-input contract
    Active --> Abandoned: mem::forget
    Finishing --> Abandoned: mem::forget
    Complete --> Abandoned: mem::forget
    Failed --> Abandoned: mem::forget
    Active --> Idle: Drop
    Finishing --> Idle: Drop
    Complete --> Idle: Drop
    Failed --> Idle: Drop
    Abandoned --> Idle: recover / successful reconfigure
```

`DecodeProgress` and `DecodeFailure` both report exact per-call consumed/produced
prefix lengths. Counters include failing-call progress. A failed session remains
terminal; a completed session returns zero progress and `Finished` again. Single
sessions stop exactly at the member boundary. Concatenated sessions reset window,
history, distances, contexts and entropy state between members, but keep aggregate
counters and policy; a boundary is `NeedsInput` until final EOF confirms success.
At least one member is required.

A session stores the dictionary borrow itself. The reusable workspace never
stores that reference, so forgetting a session cannot leave a dereferenceable
stale pointer. The owner active flag survives `trim`; only explicit recovery or
successful reconfiguration clears abandonment. Drop clears operation state and
applies the configured retention policy without I/O. Aggressive retains capacity;
CurrentConfig additionally releases it on configuration changes; Bounded releases
all workspace above its threshold; ReleaseAll releases on each operation drop.

## Parsing and regeneration

```mermaid
flowchart TD
    Window[Demand-read window header] --> Header[Transactional meta-block header]
    Header -->|metadata| Skip[Consume aligned bytes without allocation/output]
    Header -->|uncompressed| Raw[Regenerate bytes into caller output and history]
    Header -->|compressed| Types[Three independent block streams]
    Types --> Maps[Context modes and RLE / inverse-MTF maps]
    Maps --> Trees[Resumable simple / complex Huffman descriptions]
    Trees --> Command[Insert/copy command and extra bits]
    Command --> Literals[Context-dependent literal symbols]
    Literals --> Distance[Short cache / direct / extra-bit distance]
    Distance --> History[Overlapping history copy]
    Distance --> Prefix[Borrowed ordered RAW segments]
    Distance --> Static[Resolve static word then apply transform]
    Prefix -->|crossing reference| History
    History --> Command
    Static --> Command
    Raw --> End[Validate block end and padding]
    Skip --> End
    Command -->|remaining = 0| End
    End -->|more blocks| Header
    End -->|last| Member[Complete member]
```

`Bits` uses a demand-driven 64-bit reservoir. The slow path accepts one byte at
a time and never accepts a byte beyond the field that needs it, so limits and
per-call progress stay byte-exact; it reads fields up to 56 bits and splits a
62-bit distance extra field across two reads. Small headers are parsed
transactionally; partial peeks retain bits, and a validated field commits by
dropping exactly its width. Padding must be zero.

The hot path uses a whole-word `refill`, which loads a full little-endian word
while at least eight acceptable bytes remain before the input limit, and a
matching `unread`, which returns every whole speculatively buffered byte. The
reservoir persists in the stream across calls, so an input or output pause keeps
its buffered bytes; only a member boundary, where the session resets the
workspace, unreads them so a following member reads them again.

Huffman codes are two-level lookup tables. A 256-entry root is indexed by the
next eight stream bits; a code longer than eight bits selects a second-level
table through a root entry that carries the combined width and the table offset.
Complete codes fill every entry, so `decode_fast` needs no per-symbol validity
check, and a single-symbol code fills the root with zero-width entries and
consumes no bits. A `Group` stores every code of one kind for a meta-block in
one flat vector at a fixed per-code stride sized from the alphabet, so the tables
are reused between operations. The resumable `Builder` reads a simple or complex
description, retaining unfinished symbol lists and repeats, then `build` or
`build_slot` fills an owned table or a group slot; `reset` abandons a partial
description.

The stream keeps a power-of-two history ring. `run` first attempts a fast path
that decodes whole commands while a whole-word refill and output room allow: it
reads the command, literal run and distance from the reservoir, writes literals
and copies straight into the ring, and delivers ring bytes to the caller between
commands so `space` reflects true output room. The fast path returns to the
byte-exact stages at every point where input or output can run short, and those
stages produce identical results and errors for any chunking. Raw blocks, copies
and prefix runs regenerate in bulk ring pieces rather than one byte at a time;
metadata is skipped in bulk. A copy near the end of a small, highly expanding
input still bulk-copies because `Stage::Copy` copies a whole run.

Regenerated bytes update caller output and the ring together; the two most recent
output bytes for literal context are read back from the ring. Raw blocks update
the same ring as compressed blocks. The ring grows on actual output, doubling up
to the window size, so declaring a large window does not allocate it eagerly, and
a full ring wraps rather than growing. Distances and cumulative positions use
`u64`; arithmetic uses checked operations and `u128` for wire distances. The
theoretical large alphabet is `16 + NDIRECT + (124 << NPOSTFIX)` even for small
large-window headers. Every reachable distance symbol must have a maximum
represented distance no larger than `(1 << 63) - 4`.

Prefix addresses traverse attachments in order, including empty slots. A copy
crossing the prefix end continues through the history present at command start.
If its distance exceeds retained window length, reusable `prefix_history` scratch
preserves original history before emitted prefix bytes overwrite it; overlapping
copies repeat the original virtual prefix/history sequence. This follows
[RFC 9841 §3.2](https://www.rfc-editor.org/rfc/rfc9841.html#section-3.2); C 1.2.0
rejects references crossing the prefix end, so this path has independent fixture
and unit-test evidence. Static addresses are
resolved before emitting, and transformed byte count reduces the meta-block's
remaining output. Zero-output references with distance <= 120 are invalid, avoiding
zero-bit command loops. Dictionary references do not enter the history distance
cache; normal/prefix references follow short-code cache update rules.

## Allocation and dictionary boundaries

`Memory::resize` checks arithmetic and workspace policy before `try_reserve_exact`.
Growth conservatively includes both old and replacement buffers, since an
allocator may implement realloc as allocate/copy/free. Reported retained bytes
count current owned capacities. The history ring, context maps, flat Huffman
groups, and rare prefix-crossing scratch reuse capacity across operations. There
is no allocation per output byte or transform. Header-only and metadata-only
streams need no heap workspace.

`DecodeDictionary` copies RAW payloads without encoder search structures. Source
budget counts all supplied attachment bytes; owned budget includes peak storage
while replacing serialized descriptions. RAW is never magic-sniffed. Empty RAW
attachments consume one of 15 slots; an empty embedded serialized prefix does not.
Experimental `Description` owns source bytes and compact offsets/layouts, applies
C attachment replacement rules, validates list/transform/context indexes, and
resolves combinations through the borrowed view. The old custom mapping remains
live until a successful replacement; prefix-only descriptions reset it to built-in.
Prepared dictionaries resolve through their existing immutable static index and
prefix representation. Common transform application lives in `shared::dictionary`.

## I/O and error propagation

```mermaid
sequenceDiagram
    participant Caller
    participant Writer as DecoderWriter
    participant Session
    participant Sink
    Caller->>Writer: write(compressed)
    Writer->>Sink: drain pending outbox
    Writer->>Session: process(input, outbox, Process)
    Session-->>Writer: progress or failure with counts
    Writer->>Sink: write output, track exact cursor
    Writer-->>Caller: Ok(consumed) if input accepted; defer error
    Caller->>Writer: try_finish / retry
    Writer->>Session: Finish(empty), once and without new input
    Writer->>Sink: drain suffix, flush
    Writer-->>Caller: success or recoverable sink failure
```

Reader owns an 8 KiB compressed-input buffer and writes directly into caller output.
Empty reads do nothing. Interrupted retries; WouldBlock preserves state; physical
EOF declares Finish. An error after produced bytes is deferred to the next
nonempty read. Codec errors remain terminal; no false EOF follows them. Single
mode preserves read-ahead; `into_parts` returns source, unread suffix and verified
finished state.

Writer owns an 8 KiB output outbox with cursors. An operation accepting input
returns `Ok(n)` even if a codec or sink error follows; that error is deferred.
Produced bytes survive errors until delivered. Delivery of a failed codec's
outbox can retry without re-entering the codec. Interrupted retries, short writes
advance only their prefix, and write-zero is an error. Flush does not declare
EOF. Once finalization begins, new input is refused; only sink delivery/flush
can retry. `finish` uses the original `io::FinishError<Self>` type and retains the
adapter on failure. Drop performs no I/O.

`thiserror` public non-exhaustive errors categorize wire violations and resource,
size, policy, tail and lifecycle failures. Core returns these high-level categories
without exposing private implementation errors. `DecodeFailure` preserves the
source chain; conversion to `std::io::Error` retains the concrete codec error.

## Known gaps

The decoder uses scalar regeneration with two-level table Huffman lookup, a
whole-word bit reservoir, and bulk ring copies; every selected backend runs the
same scalar code and there is no decoder SIMD kernel. The prefix-crossing history
path and short static-dictionary words still emit byte by byte, which does not
affect the primary corpora. Compatibility evidence and remaining acceptance gaps
are tracked separately in
[decompressor compatibility](decompressor-compatibility.md). There is no container
parser, authentication, seek, or async runtime in this raw decoder.
