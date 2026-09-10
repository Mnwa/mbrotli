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
    Core --> Stored[stored: borrow a validated complete raw member]
    Stored --> Bits[bits / header]
    Core --> Bits
    Core --> Entropy[huffman / block / context_map]
    Core --> Regen[stream / distance / dictionary]
    Core --> Memory[memory: fallible live workspace accounting]
    Regen --> Dispatch[validated backend: command loop or ring-copy stage]
    Dispatch --> Copies[fearless_simd: 16 / 32-byte history copies]
    Regen --> Shared[private crate::shared: wire tables, dictionary bytes, transforms]
    Dict[dictionary: DecodeDictionary / PreparedDictionary] --> View[DictionaryRef borrowed view]
    View --> API
    Dict --> Store[shared::decode_dictionary / experimental decode_serialized]
```

## Public ownership and APIs

`Decompressor::new` and `builder` create an allocation-free workspace. A validated
`Backend` is selected once at construction. The scalar state machine dispatches
the command loop and bulk ring-copy stages through that validated token. The
command loop passes its concrete `fearless_simd::Simd` token to 16- and 32-byte
history-copy helpers; there are no feature checks or dynamic calls per command
or vector. Headers, raw blocks, and dictionary resolution outside the command
loop remain shared scalar code. `fork_empty` copies policy/backend into an independent empty workspace.
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
`decompress_to_slice` fills the caller's initialized slice. Eligible complete
stored members use the borrowed-payload shortcut described below. Other Vec shapes first
run the session with no destination at all, which parses up to the first byte
of output, then reserve what the meta-block declares (`declared_remaining`, at
least a small geometric step, at most 16 MiB) so a stream usually gets one
reservation rather than a chain of copies; the unused tail is trimmed. Any Vec append is
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
`Input` computes its acceptable prefix (`fast_end`) once at construction, and the
command loop refills through `refill_from` over that prefix with the cursor in a
local, so the hot loop keeps it in a register. Input pauses retain the reservoir for incomplete fields. Output pauses and
member boundaries return whole buffered bytes accepted during that call.
Older bytes belonging to a partial field remain buffered. Returning read-ahead
at output pauses prevents a long pending copy from stranding the next member
in a previous call when it later completes without consuming more input. Every description reader (`Builder`,
`ContextMap`, `Block`) decodes symbols through `decode_refilling`, which takes a
whole word when the input allows and otherwise falls back to byte-exact loading.

Huffman codes are two-level lookup tables. A 256-entry root is indexed by the
next eight stream bits; a code longer than eight bits selects a second-level
table through a root entry that carries the combined width and the absolute
offset of that table in the group's storage. An entry is one padding-free
`u32` (width in the low byte, symbol or offset above), so storage zero-fills
as a plain memory set. Complete codes fill every entry, so `decode_fast` needs
no per-symbol validity check, and a single-symbol code fills the root with
zero-width entries and consumes no bits. A `Group` keeps every code of one
kind for a meta-block in one vector: the roots first, one per code, then the
second-level tables appended as codes are built, so only tables that exist are
written. The vector's length is a high-water mark that only grows (zeroed only
when it grows) and `used` is the logical end, so a warm meta-block writes no
fill at all; `prepare` restarts the logical end without touching storage.
`Tables` borrows the whole group once so a loop that selects a code per symbol
pays one bounds check on the root. A single-slot group is an owned `Huffman`.
The resumable `Builder` reads a simple or complex description into an intrusive
linked list per code length, which is already canonical order, so building a
table walks only the symbols with a code rather than the alphabet: `fill`
counts canonical codes upward, turns each into its table key with a byte
bit-reversal table, and replicates it across the root or the current
second-level table, having grown the vector once to the alphabet's bound
(`second_bound`) when needed. `build` or `build_slot` fills an owned table or a
group slot; `reset` abandons a partial description.

```mermaid
classDiagram
    class Code {
        u32: width low byte, symbol or offset above
        bits() u32
        value() usize
    }
    class Group {
        codes: Vec~Code~ roots then second-level tables
        count: usize
        used: usize logical end
        prepare(count, alphabet, memory)
        table(tree) Table
        tables() Tables
    }
    class Tables {
        roots: &[[Code; 256]]
        second: &[Code] whole storage
        table(tree) Table
    }
    class Table {
        root: &[Code; 256]
        second: &[Code]
        decode_fast(bits) usize
        decode(bits, input) Option~usize~
        decode_refilling(bits, input) Option~usize~
    }
    class Builder {
        next: [u16; 1128]
        head, tail, counts: [u16; 16]
        read(alphabet, bits, input, memory)
        build_slot(alphabet, group, tree, memory)
        fill(codes, start, alphabet, used, memory)
    }
    Group *-- Code
    Group --> Tables : borrows
    Tables --> Table : selects
    Builder --> Group : fills a slot
```

The stream keeps a power-of-two history ring. `run` first attempts a fast path
that decodes whole commands while a whole-word refill and output room allow: it
reads the command, literal run and distance from the reservoir and writes
literals and copies straight into the ring. `Stage::Literals` re-enters the
same loop mid-command whenever a whole-word refill is possible, so a run that
paused on output resumes in bulk and only its byte-exact remainder decodes one
symbol at a time. Every hot quantity lives in a local
for the duration of the loop and is written back only when the loop pauses:
the bit reservoir, the input cursor, position, remaining length, the four-slot
recent-distance cache and the command fields the resumable stages read. Ring
bytes are delivered to the caller when a write reaches the ring end, when the
loop pauses and when an error returns, and the output space still free is
tracked as a running count that includes pending ring bytes, so no per-command
delivery is needed. State that depends only on a block type (the literal
context lookup, the block's 64-entry context map slice, whether that slice is
trivial, its single tree when it is, and the command, distance and distance
context map tables) is refreshed at block switches rather than per command.
For a trivial literal context map, the loop decodes three symbols per reservoir
check when at least 45 bits are available. Each symbol consumes at most 15 bits.
The scalar loop handles short input and the final one or two symbols; batching
does not cross the existing block, output-space or ring boundary.

```mermaid
flowchart TD
    Resume[Stage::Literals: saved insert, copy, implicit] --> Lit
    Start[refill; command block switch?] --> Cmd[command symbol -> COMMANDS entry]
    Cmd --> Extra[insert extra, copy extra]
    Extra -->|insert > 0| Lit[literal run bounded by literals, block, space, ring end]
    Lit -->|trivial block| One[one table per symbol]
    Lit -->|context block| Ctx[context from p1, p2 -> map slice -> table]
    One --> More{run done?}
    Ctx --> More
    More -->|reservoir short| PauseL[pause: Stage::Literals]
    More -->|done| Dist
    Extra -->|insert = 0| Dist{implicit?}
    Dist -->|yes| Pop[cache.pop]
    Dist -->|no| Sym[distance block switch?; distance symbol]
    Sym -->|code 0| Pop
    Sym -->|codes 1..15| Short[cache offset]
    Sym -->|others| Tab[distance_table entry: base + extra << postfix]
    Pop --> Ref
    Short --> Ref
    Tab --> Ref{distance > available?}
    Ref -->|yes| Dict[prefix: pause Resolve / static word into ring]
    Ref -->|no| Push[cache.push; grow ring once per call]
    Push --> Copy{no wrap?}
    Copy -->|yes, distance >= length| Word[16/32-byte SIMD snapshot or copy_within]
    Copy -->|yes, overlapping| Overlap[fill, byte loop, or doubling]
    Copy -->|wraps| Ring[copy_ring pieces]
    Word --> Start
    Overlap --> Start
    Ring --> Start
    Dict --> Start
```

The fixed-width copy helpers load the entire source vector before storing it.
They accept unaligned, initialized ring slices and check both complete windows;
if either window is short they leave the ring unchanged and the caller uses
`copy_within` for the exact length. A 16-byte copy serves lengths 1–16; a
32-byte copy serves lengths 17–32, so at most fifteen bytes ahead of actual
output are overwritten. The ring's sixteen unreachable history slots make
those extra writes unobservable. The snapshot is also correct when ring slots
physically overlap: all actual source bytes precede any stores. Logical LZ77
expansion still uses fill, sequential bytes, or prefix doubling; it cannot use
a snapshot of bytes not yet generated. On AVX2 a 32-byte move uses one vector;
SSE2/SSE4.2 split it into two 128-bit moves. Fallback remains available for
internal differential tests. Neither SIMD kernel allocates or uses `unsafe`.

```mermaid
flowchart TD
    Stage[Scalar run_stages] --> Kind{stage}
    Kind -->|Command / resumable Literals| Dispatch[dispatch stored Backend]
    Dispatch --> Fast[Specialized command loop]
    Fast --> Copy[copy16 / copy32 with concrete Simd token]
    Kind -->|Copy with limited input or output| Bulk[dispatch once for copy_ring]
    Bulk --> Copy
    Kind -->|headers / raw / other stages| Scalar[Shared scalar implementation]
    Copy --> Bounds{both full vector windows fit?}
    Bounds -->|yes| Snapshot[load entire vector, then store]
    Bounds -->|no| Exact[caller copies exact byte count]
```

The command symbol indexes a 1024-slot `COMMANDS` table (the 704-symbol
alphabet padded to a power of two, so a mask replaces a bounds check) that
carries both length bases and extra-bit widths, the implicit-distance flag, the
copy code for resumption and the distance context. Distances above the short
codes resolve through a per-meta-block `distance_table` filled from the distance
parameters, and reused when the layout repeats, so one lookup yields the extra
width and the base distance. The recent-distance cache is a ring with a push
index, like the reference's `dist_rb`: an implicit distance or explicit code 0
pops the most recent slot and the copy pushes it back, so every command ends
with one slot write and no branch; a command that pauses before its copy, or a
prefix or static-dictionary reference, pushes the popped value back first. The
fast path returns to the byte-exact stages at every point where input or output
can run short, and those stages produce identical results and errors for any
chunking. Raw blocks, copies and prefix runs regenerate in bulk ring pieces
rather than one byte at a time; metadata is skipped in bulk. A copy near the end
of a small, highly expanding input still bulk-copies because `Stage::Copy`
copies a whole run.

Regenerated bytes update caller output and the ring together; the two most recent
output bytes for literal context are read back from the ring. Raw blocks update
the same ring as compressed blocks. The ring grows on actual output: the fast
path grows it once per call to cover what the call can still produce (bounded
by the meta-block's remaining length and the caller's output room), otherwise it
doubles up to the window size, so declaring a large window does not allocate it
eagerly, and a full ring wraps rather than growing. Distances and cumulative
positions use `u64`; a meta-block header validates `position + MLEN` once so the
hot path adds without checks, while the byte-exact stages use checked
operations and `u128` for large-window wire distances. The theoretical large
alphabet is `16 + NDIRECT + (124 << NPOSTFIX)` even for small large-window
headers. Every reachable distance symbol must have a maximum represented
distance no larger than `(1 << 63) - 4`, which also keeps every table entry a
decoded symbol can select within `u64`.

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
zero-bit command loops. Above that distance a transform that consumes its whole
word — `OmitLast4` over a four-byte word, for instance — is legal and emits
nothing, so the ring write tolerates an empty write: as a member's first command
it arrives with the ring still unallocated, and the ring mask is only derived
once there is something to store. Dictionary references do not enter the history distance
cache; normal/prefix references follow short-code cache update rules.

## Owned one-shot output

`core::stored` recognizes a complete empty member or one raw meta-block followed
by an empty final block, using the same window/meta-block parsers and padding
validation as streaming. `decompress` borrows that payload and copies it into
one fallibly reserved Vec when resource limits are unset, member mode is single,
and the decoder is not abandoned. Other shapes use the regular session, which
also preserves configured resource-error ordering.

A fresh workspace decoding into a zero-capacity Vec without a dictionary, in
single-member mode, can collect output in history. After the ordinary empty-output
probe accepts the window, a private session call uses the window size as output
capacity. The same state machine emits every byte into the ring and counts
progress without copying it to a second slice. On successful completion and
exact input consumption, ownership of the ring moves to the result, its length
is truncated to actual output, and its capacity leaves workspace accounting.
If the window fills before completion, copy that prefix into the caller Vec and
resume ordinary delivery while retaining history. Appends, preallocated output,
retained workspaces, dictionaries and concatenation use ordinary delivery.

```mermaid
flowchart TD
    Owned[decompress] --> Stored{complete stored member and eligible policy?}
    Stored -->|yes| Borrow[borrow payload; fallible reserve and one copy]
    Stored -->|no| Probe[session with empty output: accept headers]
    Probe --> Fresh{fresh workspace and zero-capacity Vec; single; no dictionary?}
    Fresh -->|yes| Collect[same state machine; ring is destination]
    Collect --> End{member completed before wrap?}
    End -->|yes| Transfer[validate tail; transfer ring allocation to result]
    End -->|no| Prefix[copy prefix; retain ring history]
    Prefix --> Stream[ordinary session output]
    Fresh -->|no| Stream
```

Before a ring wraps, raw growth reserves the new power-of-two allocation, writes
raw bytes into existing initialized slots and appends the remainder, then
initializes only padding. A unit-distance copy in the resumable copy stage can
fill new ring storage directly with its final byte; the existing unwritten
suffix gets the same byte. Both check the workspace reservation before changing
history or position. Other copies and wrapped history keep the established
SIMD/baseline kernels. No uninitialized slices or new unsafe code are used.
See [owned decoder output](decoder-owned-output.md) for evidence and limitations.

## Allocation and dictionary boundaries

`Memory::resize` checks arithmetic and workspace policy before `try_reserve_exact`,
growing a non-empty buffer to at least twice its capacity so a workspace that
fills in steps reallocates a logarithmic number of times; `Memory::reserve` is
the capacity-only half. Growth conservatively includes both old and replacement
buffers, since an allocator may implement realloc as allocate/copy/free.
Reported retained bytes count current owned capacities. The history ring,
context maps, Huffman groups, the distance symbol table and rare
prefix-crossing scratch reuse capacity across operations; Huffman group storage
and the distance table never shrink, so a smaller meta-block does not
reinitialize them. There is no allocation per output byte or transform. Header-only and metadata-only
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

## Executable API contracts

Rustdoc examples beside the public decoder APIs exercise complete Vec/slice
decoding, append rollback, unchanged slice tails, output backpressure with a
two-byte buffer, exact-size validation, concatenated members, session recovery,
retention and independent workspaces. The session example advances by per-call
counts and reoffers precisely the remaining suffix under `Finish`; it also
checks aggregate counters and the accepted window header.

Reader examples recover protocol suffixes by chaining `unread_input` before the
returned source. Writer examples explicitly finalize and distinguish codec
completion from delivery and sink flushing. Decode-only dictionary examples run
without compression; the encoder/decoder prefix round-trip example is gated by
`compression`. They document that dictionary identity is a caller contract.

```mermaid
flowchart LR
    Docs[Public rustdoc examples] --> Buffers[Vec rollback and slice prefix checks]
    Docs --> Session[Finish suffix and progress checks]
    Docs --> IO[Reader suffix and writer finalization checks]
    Docs --> Dictionary[Owned dictionary and borrowed session checks]
```

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

The decoder specializes the command loop and its fixed-size history copies,
while Huffman symbol decoding retains its serial dependency chain. Long logical
overlap uses fill, byte copies or doubling rather than independent SIMD lanes;
context-map transforms and table construction remain scalar. The new SIMD
kernels have x86_64 execution evidence only; Arm/NEON and AVX-512 still need
native validation. See [decoder SIMD measurements](decoder-simd.md) for the
profile, benchmark results and code-size tradeoff. The prefix-crossing
history path and short static-dictionary words still emit byte by byte, which
does not affect the primary corpora. A first decode on a fresh workspace pays
about a dozen separate allocations (ring, three Huffman groups, block and
context-map codes, context maps, the distance table) and the one-time zero fill
of their storage, which dominates decoding of payloads of a kilobyte and below
in the cold shape; the per-meta-block tables are not yet folded into one arena.
Appended and reused `Vec` destinations still zero-fill newly exposed output
slices; fresh owned results can instead take the history allocation. Completely
stored members need no history. Small compressed streams still pay entropy-table
initialization and session setup. Compatibility evidence and remaining acceptance gaps
are tracked separately in
[decompressor compatibility](decompressor-compatibility.md). There is no container
parser, authentication, seek, or async runtime in this raw decoder.
