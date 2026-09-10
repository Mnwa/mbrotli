# Decoder regression seeds

Decoder target directories contain bytes only; this provenance file is kept
outside those directories so the regression runner does not interpret it as input.

- `empty-member.bin` is the RFC 7932 empty member `3b` (WBITS=22).
- `invalid-padding.bin` is `bb`, the same header/end with nonzero terminal padding.
- `quickfox.compressed`, `quickfox_repeated.compressed`, `alice29.txt.compressed`,
  `x.compressed.00`, `empty.compressed.00`, and `cp852-utf8.compressed` are copied
  unchanged from Google Brotli's `tests/testdata`, submodule SHA
  `028fb5a23661f123017c060daa546b55cf4bde29`. Original encoder flags are not
  recorded upstream; these are historical raw-format seeds, not evidence for a
  specific requested C parameter. License: Brotli Authors MIT, repository NOTICE.
- `continuation-standard.br`, `continuation-large.br`, and `official-cli.br`
  match `tests/fixtures/decompress/manifest.json`; all require base features only.
- `raw-empty-member.bin` encodes the raw-attachment target split byte `1`, followed
  by the empty member. The first byte is also its one-byte RAW prefix.
- `all-operations.bin` is bytes 0 through 19, exercising every lifecycle operation.
- `valid-empty.bin` is serialized-target split length 5, the complete empty
  serialized dictionary `91 00 00 00 00`, and empty member `3b`. This target alone
  requires experimental Rust/C builds.

No AFL output/findings files are committed. Campaigns use explicit output and
workspace budgets and retain findings locally for triage.

`decode_dictionary/timeout-replay.bin` is a 62-byte saved timeout from the
initial experimental campaign. Separate-process replay completed in 2–6 ms;
it is retained for deterministic and repeated-campaign investigation. The
original finding and AFL statistics remain in local target artifacts.

`timeout-replay-2.bin` is the 52-byte timeout saved by a second persistent
experimental dictionary campaign. Both timeout seeds pass 30,000 alternating
same-process calls; the six experimental and five base 60-second fork-mode
campaigns completed with 100% stability and no saved crashes/hangs. The cause
of the original persistent events remains unresolved. See
`architecture/decompressor-compatibility.md` for exact executions and limitations.

`arbitrary-8.bin`, `arbitrary-32.bin`, and `arbitrary-256.bin` in `decompress`
and `decode_streaming` are synthetic arbitrary bytes, with no encoder header or
pre-compression step. They were generated sequentially with xorshift64 shifts
13/7/17, initial state `0x9e3779b97f4a7c15`, taking bits 32–39 after each update.
These seeds exercise error returns without requiring successful decoding; AFL
mutates them directly. The focused arbitrary-byte test replays sizes 0 through
4096 on every available host backend using the same deterministic PRNG.

`crash-zero-length-dictionary-word.bin` in `decompress`, `decode_dictionary`
and `decode_io_limits` are three saved crashes from the 2026-09-10 decoder
campaign, one per target that reached the defect. Each drives a transformed
built-in dictionary word that decodes to no bytes at all — `OmitLast4` over a
four-byte word, which RFC 7932 permits above distance code 120 — as the first
command of a member, where the ring buffer is still unallocated and the ring
write underflowed its `len() - 1` mask. The `decode_io_limits` input is
`cargo afl tmin` minimised; the other two are the saved inputs, which byte
deletion cannot shrink because the stream is bit-packed. The same path is
covered deterministically, without AFL, by
`tests/decompress_wire.rs::zero_length_transformed_dictionary_word_at_position_zero_matches_c`.
