# Owned decoder output

The owned Vec path can recognize stored members directly or transfer its history
allocation to the caller. The same decoder state machine enforces decoded bytes,
trailing-data policy, window limits, dictionaries and resource budgets.

## Ownership, control and data flow

Private `core::stored` first recognizes the exact three-byte header consisting
of a four-bit standard window, a non-final raw block and a 16-bit length. It
checks the declared window, exact payload length and final byte `0x03`. Other
shapes use the existing demand-driven window/meta-block parsers to recognize
an empty member or one raw block followed by a final empty block.
It verifies window policy, all padding, exact payload bounds and the absence of
a tail, then returns a borrowed slice. Unsupported or malformed shapes return
`None` and enter the normal driver, which owns public error reporting.
`decompress` uses this path only for single-member operations without numeric
limits and without an abandoned session. It reserves fallibly, copies once,
and applies retention policy. No full state machine or history is necessary.

Otherwise the Vec driver probes with empty output. A zero-capacity destination,
fresh workspace, single-member mode and absent dictionary permit collection.
The private session passes `Output::collect = Some(window_size)` to the same
state machine. Ready/fast-end still enforce output policies. Emit and flush
advance progress but do not copy ring bytes to an external slice. Collection
stops before history wraps. Successful complete input transfers the ring's
allocation, truncates its logical length to produced bytes and subtracts its
capacity from live workspace accounting. If the member is larger than its
window, the collected prefix is copied once into the destination, history is
retained and ordinary streaming delivery resumes. Appends, preallocated Vecs,
retained workspaces, dictionaries and concatenation use ordinary delivery.

```mermaid
flowchart TD
    Owned[Owned decode] --> Stored{eligible complete stored member?}
    Stored -->|yes| Borrow[borrow validated payload]
    Borrow --> Copy[fallible allocation and one copy]
    Stored -->|no| Probe[probe existing session with empty output]
    Probe --> Eligible{fresh single member and empty allocation?}
    Eligible -->|yes| Collect[decode into history; retain progress and limits]
    Eligible -->|no| Stream[ordinary slice delivery]
    Collect --> Complete{finished before wrap?}
    Complete -->|yes| Tail[validate exact input end]
    Tail --> Transfer[move allocation to caller; subtract workspace bytes]
    Complete -->|no| Prefix[copy prefix; preserve history]
    Prefix --> Stream
```

Before the first wrap, raw growth reserves a power-of-two allocation, copies
into existing initialized slots and appends the remaining raw bytes, then
zeroes only unused padding. A growing unit-distance copy in the resumable copy
stage fills newly allocated slots with their final byte. Both reserve under
workspace accounting before modifying history/position. Wrapped and other
copies retain the existing baseline/SIMD kernels. All slices remain initialized, all output bytes are actually materialized and
SIMD dispatch stays outside the command and copy loops.

```mermaid
flowchart LR
    Grow[history must grow] --> Reserve[check peak workspace; fallible reserve]
    Reserve --> Raw[raw: append source bytes then initialize padding]
    Reserve --> Repeat[unit distance: resize with repeated byte]
    Reserve --> Other[other paths: ordinary zeroed growth]
    Raw --> History[initialized power-of-two history]
    Repeat --> History
    Other --> History
```

## Read-ahead accounting

At output pauses and member boundaries, `Bits::unread` returns speculative whole
bytes accepted by the current call so the caller can reoffer the exact suffix.
At input pauses, incomplete fields retain accepted bits. Bytes accepted by an
earlier call are never subtracted from the current call's consumed count.

```mermaid
stateDiagram-v2
    Decode --> InputPause: field incomplete
    InputPause --> Decode: retain bits; accept next chunk
    Decode --> OutputPause: destination or collection window full
    OutputPause --> Decode: unread current-call whole bytes; reoffer suffix
    Decode --> MemberEnd: validate padding and unread tail
    MemberEnd --> Decode: reset for next member
```

Public error conversion and lifecycle rules remain in
[the decoder API and state machine](decompressor.md). Boundary tests exercise
stored recognition, transfer, history wrap, concatenation, limits and read-ahead.

## Known gaps

- Recognition covers empty members and one raw block plus terminator; metadata
  and mixed/multiple data blocks use the full driver.
- Collection requires a fresh workspace and destination; transfer gives the
  caller a power-of-two allocation and leaves no history window for reuse.
- Appended and reused Vec output initializes newly exposed destination slices.
- Small compressed streams still build entropy tables and session state.
