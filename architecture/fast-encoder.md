# Fast Encoder Core (quality 0 and quality 1)

Scope: `src/compressor/core/fast/`. This is the implementation of the two fast
Brotli qualities: a one-pass encoder (quality 0) and a two-pass encoder
(quality 1), both ported from Google Brotli v1.2.0, commit `028fb5a`, and both
byte-identical to it.

Everything here is private. No type, error or SIMD detail from this tree
appears in the public API; [compressor.md](compressor.md) describes the surface
that wraps it.

## 1. Module map

```mermaid
graph TD
    modmod["fast/mod.rs<br/>FastEncoder, dispatch, one-shot driver"]
    q0["fast/q0.rs<br/>one-pass scan and meta-blocks"]
    q1["fast/q1.rs<br/>two-pass scan, replay, TwoPassState"]
    cmd["fast/commands.rs<br/>insert/copy/distance mapping"]
    huff["fast/huffman.rs<br/>tree build, canonical codes, serialisation"]
    hist["fast/histogram.rs<br/>chunked byte counting"]
    ml["fast/match_len.rs<br/>hybrid scalar/SIMD match length"]
    bits["fast/bits.rs<br/>LSB-first bit writer"]
    tabs["fast/tables.rs<br/>reference constant tables"]
    consts["fast/constants.rs<br/>normative constants"]
    ws["fast/workspace.rs<br/>OnePassArena, TwoPassArena"]

    modmod --> q0
    modmod --> q1
    modmod --> ws
    q0 --> cmd
    q0 --> huff
    q0 --> hist
    q0 --> ml
    q0 --> bits
    q1 --> cmd
    q1 --> huff
    q1 --> hist
    q1 --> ml
    q1 --> bits
    cmd --> bits
    cmd --> tabs
    huff --> bits
    huff --> tabs
    q0 --> consts
    q1 --> consts
    tabs --> consts
```

## 2. Ownership and reuse

`FastEncoder` owns every buffer the encoder needs and reuses them across
fragments, so after construction no allocation happens inside a match scan or a
command replay.

```mermaid
classDiagram
    class FastEncoder {
        -Level level
        -FastCore core
        -usize block_size_limit
        -u16 last_bytes
        -u32 last_bytes_bits
        -Vec~i32~ table
        -Vec~u8~ storage
        -bool finished
        +encode_block(input, is_last) BrotliResult~&[u8]~
    }
    class FastCore {
        <<enum>>
        OnePass
        TwoPass
    }
    class OnePassArena {
        +lit_depth [u8;256]
        +lit_bits [u16;256]
        +cmd_depth [u8;128]
        +cmd_bits [u16;128]
        +cmd_histo [u32;128]
        +cmd_code [u8;512]
        +cmd_code_numbits usize
        +tree Vec~HuffmanNode~
        +histogram [u32;256]
        +tmp_depth [u8;704]
        +tmp_bits [u16;64]
    }
    class TwoPassState {
        +arena Box~TwoPassArena~
        +commands Vec~u32~
        +literals Vec~u8~
    }
    class TwoPassArena {
        +lit_histo [u32;256]
        +lit_depth [u8;256]
        +lit_bits [u16;256]
        +cmd_histo [u32;128]
        +cmd_depth [u8;128]
        +cmd_bits [u16;128]
        +tmp_tree Vec~HuffmanNode~
        +tmp_depth [u8;704]
        +tmp_bits [u16;64]
    }

    FastEncoder *-- FastCore
    FastCore *-- OnePassArena : Q0
    FastCore *-- TwoPassState : Q1
    TwoPassState *-- TwoPassArena
```

The hash table and the scratch output buffer grow but never shrink. Only the
active table range is cleared between fragments; unused capacity is left
untouched. A buffer that has to grow is replaced by a freshly zeroed
allocation rather than resized in place, because the allocator hands out zero
pages while a resize would memset a region the encoder immediately overwrites.

## 3. Fragment lifecycle

Every call to `encode_block` handles one fragment of at most `1 << lgwin`
bytes, mirroring `BrotliEncoderCompressStreamFast`.

```mermaid
stateDiagram-v2
    [*] --> Sized: reserve 2 * len + 503 + 8 bytes
    Sized --> Seeded: storage[0..2] = last_bytes
    Seeded --> Tabled: clear the active hash table range
    Tabled --> Dispatched: dispatch!(level, simd => ...)
    Dispatched --> Encoded: q0 or q1 writes meta-blocks
    Encoded --> Checked: writer overflow?
    Checked --> Failed: yes
    Checked --> Carried: no
    Carried --> [*]: emit position >> 3 bytes,\ncarry the partial byte
    Failed --> [*]: BufferOverflow
```

The trailing partial byte is never emitted: it is kept in `last_bytes` and
re-seeded into the next fragment's scratch buffer, exactly as the reference
carries `s->last_bytes_`.

## 4. Quality 0: one pass

```mermaid
stateDiagram-v2
    [*] --> OpenBlock
    OpenBlock --> Scan: header, 13 zero bits,\nliteral code, command code
    state Scan {
        [*] --> Probe
        Probe --> Probe: no match, skip += 1 >> 5
        Probe --> Emit: five byte match
        Emit --> Chain: literals, distance, copy
        Chain --> Chain: immediate match, no literals
        Chain --> Probe: no immediate match
        Emit --> Uncompressed: insert >= 6210 and\nShouldUseUncompressedMode
    }
    Scan --> Remainder: ip reaches ip_limit
    Remainder --> Merge: ShouldMergeBlock and\ntotal + next <= 1 MiB
    Merge --> Scan: patch MLEN, keep the meta-block open
    Remainder --> OpenBlock: input left, new meta-block
    Remainder --> NextCode: input exhausted
    Uncompressed --> OpenBlock
    NextCode --> [*]: build the next fragment's command code\nwhen is_last is false
```

Observable decisions preserved verbatim from the reference:

- the repeat candidate (`ip - last_distance`) is tested **before** the hash
  table candidate;
- the hash table is written at the same points and in the same order;
- post-copy hash updates cover `ip - 3`, `ip - 2`, `ip - 1` and `ip`, taken
  from a single word load;
- `ShouldMergeBlock` uses stride 43 and the current literal depths;
- the final size guard rewrites the whole fragment verbatim when the compressed
  form exceeds `31 + 8 * len` bits.

## 5. Quality 1: two passes

```mermaid
sequenceDiagram
    participant Blk as block loop
    participant P1 as CreateCommands
    participant SC as ShouldCompress
    participant P2 as StoreCommands
    participant W as BitWriter

    loop each 128 KiB block
        Blk->>P1: scan, fill command and literal buffers
        P1-->>Blk: num_commands, num_literals
        Blk->>SC: literals < 98% of block, else sampled entropy
        alt compressible
            Blk->>W: meta-block header, 13 zero bits
            Blk->>P2: exact histograms, prefix codes, replay
            P2->>W: command bits, extra bits, literals
        else incompressible
            Blk->>W: uncompressed meta-block
        end
    end
```

The first pass stores each command as a packed `u32`: the low eight bits are
the fast command code (`0..128`) and the high bits carry the extra value. The
second pass builds exact literal and command histograms from those buffers,
seeds command codes 1, 2, 64 and 84, and replays the buffer.

### 5.1. Post-copy hash updates

The pinned reference contains an asymmetry that changes the command stream:
with `min_match == 4`, the first post-match update path hashes offsets
`0, 1, 0` where the chained path hashes `0, 1, 2`.

```mermaid
flowchart TD
    A["copy emitted"] --> B{min_match}
    B -->|4| C{first update after literals?}
    C -->|yes| D["offsets 0, 1, 0 from the word at ip - 3"]
    C -->|no| E["offsets 0, 1, 2 from the word at ip - 3"]
    B -->|6| F["offsets 0, 1, 2 from ip - 5,<br/>then 0, 1 from ip - 2"]
    D --> G["candidate = table[hash at offset 3]"]
    E --> G
    F --> H["candidate = table[hash at offset 2 of ip - 2]"]
```

`update_hashes_after_copy` takes a `FIRST_UPDATE` const parameter that
reproduces it. A targeted unit test pins the difference so the quirk cannot be
"fixed" by accident.

## 6. Bitstream layer

```mermaid
graph LR
    pos["position: bit index"] --> w["BitWriter"]
    w -->|write| store["storage[pos >> 3 .. +8] as one word"]
    w -->|update| patch["patch an already emitted MLEN field"]
    w -->|rewind| back["drop everything after a saved position"]
    w -->|align| byte["advance to the next byte boundary"]
    w -->|write_bytes| raw["copy an uncompressed meta-block verbatim"]
```

`write` materialises a whole 64-bit word, which is what clears the bits above
the new position — the same trick the reference uses — so the buffer keeps
eight bytes of headroom past the largest position the encoder reaches. Writes
past the end of the buffer set an overflow flag instead of panicking, and the
encoder turns that into `BufferOverflow`.

Meta-block layout for a compressed fast-path block:

| Field | Width | Source |
| --- | --- | --- |
| `ISLAST` | 1 | always 0 for a data block |
| `MNIBBLES - 4` | 2 | 4, 5 or 6 nibbles by length |
| `MLEN - 1` | 16, 20 or 24 | block length |
| `ISUNCOMPRESSED` | 1 | 0 for a compressed block |
| block splits / contexts | 13 | always zero on the fast path |
| literal prefix code | variable | `build_and_store_huffman_tree_fast` |
| command prefix code | variable | full 704 symbol alphabet |
| distance prefix code | variable | 64 symbols |
| commands, extras, literals | variable | the scan |

## 7. SIMD dispatch points

```mermaid
graph TD
    A["FastEncoder::encode_block"] -->|"dispatch! once"| B["encode_fragment&lt;S: Simd&gt;"]
    B --> C["q0::compress_fragment&lt;S&gt;"]
    B --> D["q1::compress_fragment&lt;S&gt;"]
    C --> E["find_match_length&lt;S&gt;"]
    D --> E
    E --> F{"S::u8s::N"}
    F -->|16| G["u8x16 loop"]
    F -->|32| H["u8x32 loop"]
    F -->|64| I["u8x64 loop"]

    classDef scalar fill:#fce5cd,stroke:#b45f06;
    class C,D scalar;
```

Only the exact match-length scan is vectorised. Hash lookups, candidate
selection, skip logic, command encoding, the bit writer and the Huffman builder
all stay scalar, in reference order. The lane count is resolved into a const
parameter at monomorphisation time so the vector loop can split its windows
with `as_chunks` and load them with `load_array_ref`, which leaves neither a
bounds check nor a length assertion in the loop.

`find_match_length` is a four-stage pipeline: a 16-byte scalar word prefix,
then native vectors, then whole words, then single bytes. The staging only
changes how a length is discovered, never which length it is, so every backend
emits identical bytes.

## 8. Table-bit specialisation

```mermaid
flowchart LR
    A["fragment length"] --> B["HashTableSize:<br/>256, doubling"]
    B --> C{quality}
    C -->|0| D["force an odd shift:<br/>9, 11, 13, 15"]
    C -->|1| E["8 .. 17"]
    D --> F["q0::TableBits"]
    E --> G["q1::TableBits"]
    F --> H["compress_fragment_impl&lt;S, TABLE_BITS&gt;"]
    G --> I["compress_fragment_impl&lt;S, TABLE_BITS, MIN_MATCH&gt;"]
```

The table width and, for quality 1, the minimum match length are const
parameters, so the shift and the match predicate are compile-time constants
inside the hot loop. Re-slicing the table to `1 << TABLE_BITS` at the top of
the implementation lets the bounds check on every hash lookup fold away,
because the hash is a `64 - TABLE_BITS` shift.

## 9. Data-processing loops

Bulk loops are written as chunked iterators rather than indexed loops, which
removes their bounds checks without any unsafe code:

| Loop | Shape |
| --- | --- |
| `match_len_words` | `as_chunks::<8>` over both windows, zipped |
| `match_len_vectors_*` | `as_chunks::<LANES>` plus `load_array_ref` |
| `match_len_bytes` | `zip` + `take_while` over the tails |
| `histogram::accumulate` | `as_chunks::<4>` into four independent counters, scalar tail |
| `emit_literals` (q0) | `as_chunks::<2>`, two codes per bit-writer call |
| `pack_literals` (q1) | greedy accumulation up to the writer's 56 bit limit |

## Known gaps

- **No static dictionary matching**, no block splitting and no context
  modelling: out of scope for both fast qualities, as in the reference.
- **`ShouldMergeBlock` and `ShouldCompress` use floating point.** They
  reproduce the reference's `double` arithmetic, including its
  single-precision `log2` table, because the decisions are observable in the
  output.
- **The command prefix code carries stale depths for symbols with a zero
  count.** `create_huffman_tree` writes depths only for symbols that appear,
  exactly as the reference does; the seed histogram guarantees every symbol
  that can be emitted has a non-zero count.
- **Throughput has not reached the reference on every corpus.** See
  `docs/q0_q1_benchmarks.md` for measured ratios and the buckets that are still
  behind.
