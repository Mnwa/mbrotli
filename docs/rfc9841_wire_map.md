# RFC 9841 wire map

Every RFC 9841 field this encoder writes or reads, with its width, its byte or
bit order, the rule that validates it, and the function that implements it.
Fields belonging to parts of the format that are not implemented are listed at
the end, marked as such, rather than described as if they worked.

Pinned reference: **Google Brotli v1.2.0**, commit
`028fb5a23661f123017c060daa546b55cf4bde29`. Related decisions are in
[`rfc9841_interop_decisions.md`](rfc9841_interop_decisions.md).

## Implemented: the Large Window stream header

RFC 9841 replaces the RFC 7932 window header with a fourteen-bit form. The
underlying bit order is RFC 7932's: bits are written least significant first
within each byte, and a multi-bit field's least significant bit goes first.

| Field | Width | Value | Validation | Implemented by |
| --- | --- | --- | --- | --- |
| Escape marker | 8 bits | `0b00010001` (`0x11`) | constant | `ResolvedWindow::header` |
| `WBITS` | 6 bits | `10..=62` | `WindowBits::large` rejects anything outside, and its `WindowKind` is private so no other value can exist; `& 0x3F` is a defensive mask on an already-validated value | `ResolvedWindow::header` |

The marker is exactly the seven-bit RFC 7932 pattern that would decode as
`WBITS = 9` — a value RFC 7932 forbids — followed by one zero bit. That is what
makes it unambiguous: an RFC 7932 decoder must reject it, and an RFC 9841
decoder recognises it and reads six more bits.

Written as a `(u16, u32)` pair in the reference's `last_bytes` /
`last_bytes_bits` form:

```text
value = ((WBITS & 0x3F) << 8) | 0x11
width = 14
```

so the first stream byte is always `0x11` and the low six bits of the second are
`WBITS`. A test asserts exactly that for all 53 legal values.

For comparison, the RFC 7932 header this replaces, also in
`ResolvedWindow::header`:

| `lgwin` | Width | Value |
| --- | --- | --- |
| 16 | 1 bit | `0` |
| 17 | 7 bits | `1` |
| 18..=24 | 4 bits | `((lgwin - 17) << 1) \| 1` |
| 10..=15 | 7 bits | `((lgwin - 8) << 4) \| 1` |

## Implemented: the Large Window distance alphabet

Not a header field, but a wire-visible consequence: the size of the distance
alphabet changes what a meta-block header means, because a simple prefix code
spends `Log2Floor(alphabet_size - 1) + 1` bits per symbol.

| Quantity | Ordinary | Large window | Implemented by |
| --- | --- | --- | --- |
| `alphabet_size_max` (written) | `16 + NDIRECT + (24 << (NPOSTFIX + 1))` | `16 + NDIRECT + (62 << (NPOSTFIX + 1))` | `DistanceParams::new` / `new_large` |
| `alphabet_size_limit` (usable) | equal to `alphabet_size_max` | `distance_code_limit(0x7FFFFFFC, …).max_alphabet_size` | `DistanceParams::new_large` |
| `max_distance` | `NDIRECT + (1 << (24 + NPOSTFIX + 2)) - (1 << (NPOSTFIX + 2))` | `distance_code_limit(0x7FFFFFFC, …).max_distance` | `DistanceParams::new_large` |

`0x7FFFFFFC` is `BROTLI_MAX_ALLOWED_DISTANCE`: the largest distance that keeps
every decoder-side calculation inside a signed 32-bit range. `NPOSTFIX` stays
`0..=3` and `NDIRECT` stays `0..=120` in a whole number of postfix groups, as
RFC 7932 requires; RFC 9841 does not widen either.

Both quantities reach the meta-block header through
`core::shared::bitstream`, which writes `alphabet_size_max` as the alphabet a
decoder must assume and uses `alphabet_size_limit` to size the block encoder.

## Implemented: validation rules

| Rule | Where | On failure |
| --- | --- | --- |
| `10 <= WBITS <= 62` | `WindowBits::large` | `ParseWindowBitsError::{LowerBound, LargeUpperBound}` |
| Large window requires quality 3 or above | `core::driver::check_large_window`, `FastEncoder::new` | `SharedBrotliError::UnsupportedLargeWindow` |
| `10 <= WBITS <= 24` for the RFC 7932 header | `WindowBits::standard` | `ParseWindowBitsError::{LowerBound, UpperBound}` |
| Large window at quality 2 | `core::driver::check_large_window` | `BrotliCompressError::UnsupportedQuality(2)` |
| Emitted distance never exceeds the declared window | by construction: retained history is `min(WBITS, 30)` bits, so the largest distance is `2^min(WBITS,30) - 16` | — |
| The usable distance alphabet fits a 544-symbol histogram | `distance_code_limit`, asserted over every legal `(NPOSTFIX, NDIRECT)` pair | — |

## Not implemented

These parts of RFC 9841 have no code and therefore no wire map entry yet. They
are listed so the gap is visible, not to describe behaviour.

| Area | Fields the RFC defines |
| --- | --- |
| Varints | base-128 canonical varint (≤ 9 bytes, ≤ 63 bits); reversed varint for the final footer |
| Serialized shared dictionary | signature `0x91 0x00`; `LZ77_DICTIONARY_LENGTH` varint; `NUM_CUSTOM_WORD_LISTS` and `NUM_CUSTOM_TRANSFORM_LISTS` (`0..=64`); `NUM_DICTIONARIES` (`1..=64`); word/transform index pairs; `CONTEXT_ENABLED`; a 64-byte context map |
| Custom word lists | 28 `SIZE_BITS_BY_LENGTH` bytes for lengths `4..=31`; `1 << b` words of each length, `b <= 15` |
| Custom transforms | little-endian 16-bit prefix/suffix table length; zero-terminated stringlets; `NTRANSFORMS` byte; per-transform prefix, suffix and operation indices (`0..=22`); little-endian 16-bit shift parameters when a shift operation is present |
| Framing container | signature `0x91 0x0A 0x42 0x52`; container flags byte; chunk length varint; type byte; codec byte; uncompressed-size varint; dictionary reference headers; metadata fields; central directory; final footer with reversed varints |
