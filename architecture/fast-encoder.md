# Fast Encoder Core (quality 0 and quality 1)

Scope: `src/compressor/core/fast/`, and the parts of
`src/compressor/core/shared/` it uses. This is the implementation of the two fast
Brotli qualities: a one-pass encoder (quality 0) and a two-pass encoder
(quality 1), both ported from Google Brotli v1.2.0, commit `028fb5a`, and both
byte-identical to it.

Everything here is private. No type, error or SIMD detail from this tree
appears in the public API; [compressor.md](compressor.md) describes the surface
that wraps it.

## 1. Module map

```mermaid
graph TD
    modmod["fast/mod.rs<br/>FastEncoder, dispatch"]
    q0["fast/q0.rs<br/>one-pass scan and meta-blocks"]
    q1["fast/q1.rs<br/>two-pass scan, replay, TwoPassState"]
    cmd["fast/commands.rs<br/>insert/copy/distance mapping"]
    hist["fast/histogram.rs<br/>chunked byte counting"]
    tabs["fast/tables.rs<br/>quality 0 and 1 command tables"]
    consts["fast/constants.rs<br/>quality 0 and 1 constants"]
    ws["fast/workspace.rs<br/>OnePassArena, TwoPassArena"]

    subgraph shared["compressor::core::shared"]
        huff["huffman.rs<br/>tree build, canonical codes, serialisation"]
        ml["match_len.rs<br/>hybrid scalar/SIMD match length"]
        bits["bits.rs<br/>LSB-first bit writer"]
        log["fast_log.rs<br/>reference logarithms"]
        stabs["tables.rs<br/>entropy-coding tables"]
        sconsts["constants.rs<br/>format constants"]
    end

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
    cmd --> log
    huff --> bits
    huff --> stabs
    q0 --> consts
    q1 --> consts
    consts --> sconsts
    log --> stabs
    stabs --> sconsts
```

The bit writer, the Huffman builders, the match-length scan, the reference
logarithms and the entropy-coding tables live in `compressor::core::shared`
rather than in this tree, because the greedy encoder needs exactly the same
implementations; see [greedy-encoder.md](greedy-encoder.md).

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
    F -->|16| G["u8x16 loop, stride 16"]
    F -->|32| H["u8x32 loop, stride 32"]
    F -->|64| I["u8x64 loop, stride 64"]
    F -->|other| J["byte scan, stride 1"]

    classDef scalar fill:#fce5cd,stroke:#b45f06;
    class C,D,J scalar;
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

### 7.1. The stride invariant

Each stage decides "did every step match?" by comparing what the scan reported
against the window rounded down to a whole number of steps, and returns early
when the scan stopped short. That test is only exact when the size it rounds by
is the step the scan really takes, so `native_vector_stride` reports the stride
rather than the lane count: every width without a vector loop degrades to the
byte scan, whose stride is one. Rounding by a lane count the scan does not use
would round the window up past what the scan can report and truncate a match
that ran to the limit. No backend `fearless_simd` ships reaches that arm —
NEON, SSE2 and the fallback all have `u8s::N == 16` — so it is an invariant kept
honest rather than a live path.

### 7.2. Measured reach of the vector stage

The prefix is 16 bytes and the widest shipping backend on the reference host is
also 16 lanes, so the vector loop only ever sees matches of 16 bytes or more.
Counting how often it runs, per whole file at quality 0:

| Corpus | `find_match_length` calls | Exit in the scalar prefix | Vector iterations |
| --- | ---: | ---: | ---: |
| `alice29.txt` | 19,291 | 99.4% | 29 |
| `lcet10.txt` | 52,743 | 98.8% | 519 |
| `plrabn12.txt` | 68,960 | 99.9% | 47 |
| `mapsdatazrh` | 10,319 | 91.0% | 1,633 |
| `random_org_10k.bin` | 0 | — | 0 |

Text finds short matches, so the scalar word prefix answers almost every call
and the vector stage earns its keep only on structured binary. This is a
property of the fast qualities, not a tuning gap: the stages exist so that a
long match does not pay a byte-at-a-time scan, and short matches never enter
them.

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
| `emit_literals` (q0) | `as_chunks::<4>` above an eight literal run, then `as_chunks::<2>`, then singles |
| `pack_literals` (q1) | greedy accumulation up to the writer's 56 bit limit |

### 9.1. Literal packing width

The tree builder caps a literal depth at fourteen bits, so four codes are at
most 56 bits and always fit one `BitWriter::write`; two fit with room to spare.
Quality 0 emits literals between matches, and run length varies by an order of
magnitude across corpora — roughly three literals per run on text against
twenty on structured binary — so the quadruple loop is gated on the run being
at least eight literals long and short runs go straight to the pair loop. An
ungated quadruple loop measures 4% slower on `alice29.txt`, where a run rarely
fills one quadruple, while keeping the full gain on `mapsdatazrh`.

Quality 1 replays a whole meta-block of literals at once, so its packer stays
greedy: it accumulates codes until the next would overflow the 56 bit limit,
which reaches far more codes per store than a fixed quadruple. Batching that
loop four at a time to amortise its per-literal branch measures within noise on
every corpus, so it keeps the simpler shape.

## Flushing

`FastEncoder::flush_block` is the fast path's `BROTLI_OPERATION_FLUSH`. These
qualities already close a meta-block on every call, so the flush is only the
empty metadata block that pushes the stream back onto a byte boundary — and,
when there is buffered input, a short non-final fragment ahead of it. An empty
input skips the fragment entirely, exactly as the reference does when a flush
arrives with nothing buffered, and nothing at all is emitted when the stream
was already aligned. See [compressor.md](compressor.md) §3.1.

Flushing costs these qualities more ratio than it costs any other: quality 1
rebuilds a whole code description per meta-block with nothing to amortise it
over, so flushing every kibibyte of a 256 KiB text made the stream seventeen
times larger. The time cost is nil — the ratio against the reference is flat
across flush counts, because a flush is work both implementations do the same
way. See [`docs/api_benchmarks.md`](../docs/api_benchmarks.md) §2.

## Known gaps

- **No attached prefix.** These qualities carry no compound-dictionary search
  in the reference either, so a `PreparedDictionary` is refused rather
  than ignored.
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
