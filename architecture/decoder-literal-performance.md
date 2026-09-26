# Decoder literals and stored headers

Private `decompressor::core::stream` owns literal decoding inside the
backend-specialized command loop. `core::stored` recognizes complete stored
members for eligible owned-output calls. No additional public API is exposed.

## Literal batches

The context-free literal loop decodes three symbols per reservoir check.
A Huffman symbol consumes at most 15 bits, so 45 buffered bits suffice. Whole-word refill leaves
at least 56 bits; it reads only a checked eight-byte slice inside the input
budget. The batch is bounded by the pending literal count, literal block count,
output space and ring end. It cannot cross a block switch or wrap. Contextual
literals continue using a context-dependent loop.

```mermaid
flowchart TD
    Run[bounded literal run] --> Context{trivial context map?}
    Context -->|no| Existing[context-dependent loop]
    Context -->|yes| Batch{at least 3 output slots?}
    Batch -->|yes| Bits{45 bits buffered or refill succeeds?}
    Bits -->|yes| Three[decode 3 symbols; consume at most 45 bits]
    Three --> Batch
    Bits -->|no| Scalar[scalar remainder: check 15 bits per symbol]
    Batch -->|no| Scalar
    Scalar --> State[update position, remaining counts and resumable stage]
    Existing --> State
```

The scalar remainder also runs when input cannot refill a batch but still has
one or two decodable symbols.

## Byte-exact literal runs

Within eight bytes of the input's acceptable end the command loop cannot refill,
so the resumable `Stage::Literals` decodes the rest of the run. It settles the
pending block switch, the context mode, the context-map row and the run bound
(pending literals, block remainder and output room up to the call's fast end)
once, then loops over symbols with byte-exact loading, writing the ring and the
destination directly. Counters are committed before any pause or error leaves,
so progress matches the per-symbol path. When the command loop could still
refill, the stage decodes one symbol and re-enters it instead, since it stopped
at a boundary it does not cross. Small streams, whose whole input lies inside
that tail, decode all their literals here.

```mermaid
flowchart TD
    Stage[Stage::Literals] --> Ready{output ready?}
    Ready -->|no| PauseOut[Stop::Output]
    Ready -->|yes| Room{eight bytes before fast end?}
    Room -->|yes| Fast[command loop]
    Fast --> Again{literals pending and eight bytes still left?}
    Again -->|yes| One[one symbol] --> Stage
    Again -->|no| Settle
    Room -->|no| Settle[block switch, context row, run bound]
    Settle --> Loop[decode byte-exactly up to the bound]
    Loop -->|input exhausted| Commit[commit counters; Stop::Input]
    Loop -->|bound reached| Stage
``` Input/output pauses, consumed/produced counts,
resource failures and public error propagation follow the shared decoder contract.
SIMD dispatch remains outside the command loop; batching adds no feature
selection and uses safe ordinary Rust because the entropy dependency is serial.

## Stored headers

Stored recognition checks a common fixed header directly: four bits of
standard window declaration (18–24), non-final flag, four length nibbles and raw
flag occupy exactly 24 bits. The shape check is `(header & 0x800071) == 0x800001`, with a
nonzero window code in bits 1–3. The payload length is bits 7–22 plus one. The
window must satisfy the configured limit, total length must equal payload plus
four bytes, and the last byte must be exactly `0x03` (final empty block with zero
padding). Checked slicing borrows the payload. Other header shapes retain the
general parser. Rejected recognition returns to the full driver,
which produces the public typed error. Numeric limits and abandoned-session
ordering are guarded by the public owned-decode entry point.

```mermaid
flowchart TD
    Input[complete input, eligible owned decode] --> Header{three-byte raw header shape?}
    Header -->|yes| Exact{window allowed, exact length, final byte 03?}
    Exact -->|yes| Borrow[borrow payload then fallible reserve and one copy]
    Exact -->|no| Driver[full decoder: typed error or supported alternative]
    Header -->|no| General[general stored parser]
    General -->|recognized| Borrow
    General -->|not recognized| Driver
```

## Invariants and known gaps

Batching adds no allocation or feature detection. Literal decoding is serial
entropy work; the selected backend is passed into the surrounding command loop.
Tests compare output, progress and failures against the baseline and available
host backends, including truncated headers and short output buffers.

Contextual literals use their own loop, and stored shapes outside the recognizer
use the full decoder. See [owned output](decoder-owned-output.md) for eligibility
and retention, and [decoder mechanics](decompressor.md) for errors and lifecycle.
