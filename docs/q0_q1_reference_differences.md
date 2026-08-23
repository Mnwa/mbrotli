# Differences from the pinned reference

Reference: Google Brotli v1.2.0, commit `028fb5a`, vendored at
`brotli-ffi/vendor/brotli`.

## 1. Bitstream output

**There are none.** For every input, quality (0 and 1), and window size
(10 through 24) covered by the test suite, this encoder emits bytes that are
identical to `BrotliEncoderCompress` from the pinned reference.

That is asserted rather than claimed:

| Test | Coverage |
| --- | --- |
| `tests/differential_c.rs` | structural corpora and every boundary length, all fifteen window sizes |
| `tests/vendor_corpus.rs` | Google Brotli's own test data, plus a 12 MiB multi-fragment input |
| `tests/randomized.rs` | 3 400 seeded random inputs mixing literal runs, back-references and noise |
| `fuzz/afl/src/bin/differential_c.rs` | the same oracle under AFL, seeded from the vendored test data |

The benchmark harness re-asserts byte identity before every timed run, so a
reported speedup cannot come from a changed output.

## 2. Reference quirks reproduced on purpose

### 2.1. Asymmetric post-copy hash update at quality 1

In `CreateCommands`, with `min_match == 4`, the first post-match update path
hashes offsets `0, 1, 0`, while the chained path hashes `0, 1, 2`
(`c/enc/compress_fragment_two_pass.c`). The repeated `0` looks like a typo, and
it changes which candidate the next iteration finds, so it changes the command
stream and the compressed size.

This port reproduces it exactly, through the `FIRST_UPDATE` const parameter of
`update_hashes_after_copy`, and pins it with
`q1::tests::first_update_reproduces_the_reference_offset_quirk`. Changing it is
a separate experiment, not part of the parity port.

### 2.2. Stale depths in the command prefix code

`BrotliCreateHuffmanTree` writes a depth only for symbols with a non-zero
count, leaving every other entry of the caller's array untouched. Quality 0
therefore carries depths from the previous block for symbols the current block
never used. That is safe only because `kCmdHistoSeed` gives a non-zero seed to
every symbol the fast path can emit, and because the initial
`kDefaultCommandDepths` has zeros exactly where the seed does.

`create_huffman_tree` here does the same, and the arena keeps `cmd_depth` alive
across blocks the same way.

### 2.3. Single-precision logarithm table

`kBrotliLog2Table` is declared as `double[]` but initialised with `float`
literals, so every entry is the single-precision rounding of `log2(i)` widened
back to double (`c/enc/fast_log.c`). Both `ShouldMergeBlock` and
`ShouldCompress` are sensitive to those last bits, so `tables::LOG2_TABLE`
reproduces the widened float values rather than the exact doubles. A unit test
checks all 256 entries against `f64::from(log2(i) as f32)`.

### 2.4. Unsigned wraparound in Huffman node counts

`BrotliCreateHuffmanTree` sums `uint32_t` counts without overflow checks. The
port uses `wrapping_add` so a histogram that saturates a 32-bit counter behaves
identically instead of panicking in a debug build.

## 3. Behaviour that is equivalent but implemented differently

| Reference | Here | Why it is equivalent |
| --- | --- | --- |
| `IsMatch` compares four bytes, then the fifth (and sixth) | one 64-bit load per side, masked to `MIN_MATCH` bytes | both positions always have eight readable bytes inside a fragment; the mask compares exactly the same bytes |
| `FindMatchLengthWithLimit` is a pure 8-byte XOR loop | scalar prefix, then native vectors, then word and byte tails | the staging changes only how the length is found |
| `BrotliWriteBits` reads and writes through raw pointers | the same whole-word read-modify-write through a slice | identical bytes, with an overflow flag instead of undefined behaviour |
| the literal emit loop writes one code per call | two codes per call at quality 0, greedy packing at quality 1 | the bits and their order are unchanged |
| `for (i = 0; i < len; ++i) ++histogram[input[i]]` | four independent sub-histograms merged afterwards, above 4 KiB | addition is associative over the counts |
| `GetBrotliStorage` mallocs uninitialised scratch | a zeroed allocation, reused across fragments, or the caller's buffer directly | the encoder writes before it reads either way |
| `BrotliEncoderCompress` writes in place when the caller's buffer is large enough | the same, whenever the destination has room for the fragment reservation | identical bytes, one fewer copy |

## 4. API differences

The reference C API is not mirrored; this crate keeps its own public surface.
Notable consequences:

- **Mode and size hint are not exposed.** The fast path ignores both in the
  reference too, so output is unaffected.
- **Large window is not supported.** `WindowBits` stops at 24, and the
  fast path never sets the large-window bit — the reference disables it for
  quality ≤ 2 as well.
- **Quality 10 is not representable.** `QualityLevel` has no `Q10`
  variant; `TryFrom<usize>` reports it as
  `ParseQualityLevelError::Unrepresentable`.
- **Streaming semantics differ from the one-shot API.** The one-shot entry
  points reproduce `BrotliEncoderCompress`, including its single-byte output
  for empty input and its uncompressed fallback for streams that grew. The
  streaming adapters do neither, because both are properties of the one-shot
  wrapper rather than of the encoder; a streamed empty input produces the
  ordinary window header plus a terminating meta-block, which is a valid
  stream that decodes to nothing.

## 5. Performance differences

Compression ratio is identical, so every performance comparison is like for
like. Throughput is not yet at parity on all corpora; see
`docs/q0_q1_benchmarks.md` for the measured ratios, the buckets that are
behind, and the root cause.
