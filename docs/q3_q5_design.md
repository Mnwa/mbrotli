# Quality 3 / 4 / 5 design record

What the greedy port is, why it is shaped the way it is, and what was measured
along the way. The mechanics live in
[`architecture/greedy-encoder.md`](../architecture/greedy-encoder.md); this
document is the reasoning.

Reference: `google/brotli` v1.2.0, commit `028fb5a`, built without
`BROTLI_MAX_SIMD_QUALITY`.

## 1. Why a second encoder rather than an extension of the first

The fast qualities compress one fragment at a time with no memory of the
previous one: a fragment is hashed, encoded and forgotten, and the only state
that crosses a boundary is the partial byte. Qualities three and up cannot work
that way — a match may reach back a whole window, the distance cache persists,
and a meta-block is a decision, not a fixed slice of input.

So `core::greedy` is a separate tree with its own state machine, and
`core::driver` owns the routing plus the two things both families share: the
empty-input shortcut and the final fallback to an uncompressed stream. What is
genuinely common — the bit writer, the Huffman builders, the match-length scan,
the reference logarithms, the entropy tables — moved to `core::shared` rather
than being duplicated or borrowed across trees.

## 2. The parameters the port had to add

The reference exposes eight encoder parameters. This crate modelled two of them
(quality, window size). Five more change the emitted bytes at these qualities
and had to become part of the public API:

| Parameter | Reachable effect |
| --- | --- |
| Mode | Font mode selects one postfix bit and twelve direct distance codes at quality four and above. |
| Size hint | Qualities four and five choose a different match finder at one mebibyte. |
| Block size | Changes where meta-blocks end. |
| Distance layout | Changes the distance alphabet. |
| Literal context modelling | Quality five models contexts unless told not to. |

Each is a validated type rather than a raw integer, so a configuration the
format cannot express cannot be built. That is a deliberate departure from the
reference, which silently rewrites an invalid distance layout to `(0, 0)`; the
sanitiser still implements the rewrite, because font mode reaches the same code
path, but the public API cannot reach it.

The eighth parameter, the stream offset, is not exposed: it exists to splice a
stream onto an earlier one, which needs a decoder-side counterpart this crate
does not have.

### 2.1. The one place streaming and one-shot disagree

`BrotliEncoderCompress` sets the size hint to the input length;
`BrotliEncoderCompressStream` leaves it at zero. Both behaviours are
reproduced, which means a quality four or five stream compressed through the
adapters can differ from the same bytes compressed in one shot. Hiding that —
by making the adapters guess, or by making the one-shot path stop
substituting — would have broken byte parity with one reference entry point or
the other. It is documented on `with_size_hint` and in the architecture
specification instead, and the tests pin both halves.

## 3. Determinism: the hasher plan

The specification's central constraint is that the machine must not be able to
change the output. The reference makes its matcher choice at compile time
through `BROTLI_MAX_SIMD_QUALITY`; a runtime-dispatched Rust port could easily
have turned that into a runtime choice, which would have made the output depend
on the host.

`params::choose_hasher` is therefore a `const fn` of quality, window size and
size hint alone, and it runs once when the encoder is built. Each plan is a
distinct type, so the bucket geometry is a compile-time constant inside the
probe loop, and the enum that picks between them is matched once per input
block. The SIMD token never reaches a decision — it reaches exactly one leaf,
the match-length scan.

`tests/simd_backends.rs` and the `simd_equivalence` fuzz target assert the
consequence: every backend the host can run emits the same bytes.

## 4. What the port deliberately did not simplify

Three places looked like they could be tidied and were not, because tidying
them would have changed the output:

- **The delayed-search shortcut.** Below quality five the delayed candidate
  starts from the length already found; at quality five it starts from zero.
  This is compression semantics, not a tuning flag, and it is expressed as
  `GreedyQuality::extensive_reference_search` rather than being folded away.
- **The sparse-search strides.** "Skip more when the data looks
  incompressible" is easy to write generically and impossible to get
  byte-identical. The thresholds, the stride sizes and which of the skipped
  positions get stored are copied exactly.
- **The three-context literal model.** The reference computes an honest
  estimate and then multiplies it out of contention below quality seven. The
  branch is kept, priced the same way, rather than deleted.

## 5. The static dictionary

Only the encoder half is needed. A dictionary match becomes an ordinary
distance past the end of the window, and the decoder applies the transform, so
the 121-entry transform table never has to be carried — the encoder only
computes which transform id a prefix cut maps to, which is a shift of a single
64-bit constant.

What is carried is the 122,784-byte word table and the 32,768-bucket hash over
their four-byte prefixes, extracted verbatim from the reference and embedded
with `include_bytes!`. Three binary blobs beside the module, rather than
generated Rust source, because a 32,768-element array literal costs compile
time and reads no better.

The reference builds the same hash at static-initialisation time in one of its
build configurations; the default configuration embeds the table, and the
embedded table is what the differential tests compare against.

## 6. Safety without `unsafe`

The reference leaves three allocations uninitialised and relies on a counter or
a sentinel to keep them unread: the bucket array of `HashLongestMatch`, the
slot banks of `HashForgetfulChain`, and the Huffman node pool. This crate zeroes
them. Each is provably never read before it is written, so the zeroing costs
time and changes nothing — see
[`q3_q5_reference_differences.md`](q3_q5_reference_differences.md).

Everywhere else the pattern is the one the quality 0 and 1 port established:
`get`, `first_chunk`, `as_chunks` and masked indices, so the bounds checks that
survive are the ones that were going to be a branch anyway.

## 7. What was measured, and what it cost

Byte parity came first and was reached before any tuning. Then:

| Change | Effect on quality 3 / 4 / 5 throughput |
| --- | --- |
| `#[inline(always)]` on `Matcher::find_longest_match` and `Matcher::store` | +8.5% / +8.4% / +7.0% |
| Hoisting the bucket bounds check in the quick matcher | +0.6% / +0.5% / +1.3% |
| `#[inline(always)]` on the command length and extra-bit helpers | no measurable change |
| Fat LTO and one codegen unit | −3% / −2% / −5% (kept off) |
| Replacing the hybrid match-length scan with a pure scalar one | −11% / −1% / −2% (kept hybrid) |

The first is the one that mattered: without it the match finder was a real call
per position, which meant the query and the result travelled through memory.
The reference marks the same function `BROTLI_INLINE`.

Measured on an Apple M5 Pro over `alice29.txt`, 300 rounds each; the full
report is in [`q3_q5_benchmarks.md`](q3_q5_benchmarks.md), including the gates
that are **not** met.
