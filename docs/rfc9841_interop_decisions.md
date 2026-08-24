# RFC 9841 interoperability decisions

Every place where [RFC 9841] is ambiguous, where the pinned reference
implementation stops short of the format, or where this crate deliberately
narrows what it emits, is recorded here with the reason and the date it was
checked. Nothing in this file changes the wire format; it records which of
several defensible readings this encoder implements, so a later change can tell
a decision from an accident.

Pinned reference: **Google Brotli v1.2.0**, commit
`028fb5a23661f123017c060daa546b55cf4bde29`.

[RFC 9841]: https://www.rfc-editor.org/rfc/rfc9841.html

## D1 — The container flag contradiction (checked 2026-08-24)

RFC 9841 contradicts itself about the meaning of bit 2 of the container flags
byte:

| Location | Text |
| --- | --- |
| Section 8.1 | "If 1, the file may contain one or more resources, metadata, and a central directory, and it must contain a final footer." |
| Section 8.4.11 | "The final footer chunk closes the file and is only present if bit 2 of the initial container flags was set." |
| Section 8.4.12, rule 10 | "If bit 2 of the container flags is set, there may be only a single resource, no metadata chunks of any type, no central directory, and no final footer." |
| Section 8.4.12, rule 11 | "If bit 2 of the container flags is not set, there must be exactly 1 final footer chunk, and it must be the last chunk in the file." |

Sections 8.1 and 8.4.11 agree with each other; the last two rules of 8.4.12
state the reverse.

**Errata status.** The RFC Editor errata database was queried on 2026-08-24 at
<https://www.rfc-editor.org/errata/rfc9841> and reports *no matching errata*.
The contradiction is therefore unresolved upstream.

**Decision.** Follow Sections 8.1 and 8.4.11, which are mutually consistent and
yield a coherent format:

```text
bit 2 = 0:  single resource, no metadata, no central directory, no final footer
bit 2 = 1:  extended profile, one or more resources, metadata and central
            directory allowed, exactly one final footer required
```

Reading 8.4.12 literally would put the final footer — which exists to point
back at the central directory — in the profile that is forbidden to have a
central directory, and would leave the extended profile with no way to find one.
That is not a format anyone can implement.

**Revisit when** an accepted erratum or a document that updates RFC 9841 says
otherwise. The framing writer is not implemented yet; when it is, the named
tests `interop_9841_container_flag_simple`,
`interop_9841_container_flag_indexed` and `interop_9841_footer_flag_consistency`
pin this decision.

## D1a — One window type instead of two (repository owner's decision)

The specification requires a separate `LargeWindowBits` stored as
`Option<LargeWindowBits>` in `CompressParams`, with `WindowBits` left at
`10..=24` (sections 2.1, 4.2, 5.1, 11.1, 54, 55). This repository merges them at
the owner's explicit direction:

```rust
pub struct WindowBits(WindowKind);           // WindowKind is private

WindowBits::standard(22)?                    // RFC 7932 header, 10..=24
WindowBits::large(30)?                       // RFC 9841 header, 10..=62
```

**Why it is still sound.** The specification's real constraint is that a large
window must never be *inferred* — not that the two must be separate types. It
is not inferred here: the caller names a constructor, and because the ranges
overlap, `WindowBits::large(22)` and `WindowBits::standard(22)` are different
values producing different streams. Neither range is widened. Keeping
`WindowKind` private means the two validating constructors are the only way to
build a value, so no window can exist that no header can express and nothing
downstream re-checks a range.

**What it costs.** The non-destructive toggle of section 4.2 is gone: with one
field there is no stored ordinary window to restore, so `without_large_window`
has no meaning and does not exist. `CompressParams::lgwin()` no longer always
returns an RFC 7932 window. `TryFrom<usize> for WindowBits` is removed, because
with two target headers the conversion is ambiguous — which is exactly why the
constructors are named rather than inferred.

This is a source-breaking change to a pre-existing public type. Emitted bytes
are unaffected; `tests/differential_c.rs` still passes unchanged. The full
trade-off is recorded in
[`rfc9841_api_binding.md`](rfc9841_api_binding.md#divergence-one-window-type-not-two).

## D2 — Declared window versus retained history (checked 2026-08-24)

RFC 9841 allows a declared window of `10..=62` bits. This encoder writes every
one of those 53 values into the header exactly, and keeps at most **30 bits** of
history regardless.

**Reason.** A shorter history only produces shorter backward distances, and
every distance an encoder emits is legal for any decoder whose window is at
least the declared one. Sizing anything to a declared 62-bit window would mean a
4-exbibyte allocation for a stream that may hold four bytes, which Section 43.1
of the implementation specification forbids outright. The reference C encoder
draws the line in the same place (`BROTLI_LARGE_MAX_WBITS`, 30), which keeps the
two byte-comparable wherever both implement the feature.

**Consequence.** For any declared window at or above 30 bits, the emitted stream
is byte-identical to the 30-bit stream apart from the six window bits in the
header. The test
`large_window::a_window_wider_than_the_c_decoder_only_changes_the_header` asserts
exactly that.

**Consequence for 64-bit arithmetic.** Because retained history never exceeds 30
bits, no distance this encoder emits can exceed `2^30 - 16`, and the RFC 9841
distance-alphabet limit is `(1 << 31) - 4` in any case. Distance and window
arithmetic is therefore proven to fit a `usize` on every supported target rather
than being carried in `u64` and narrowed. Widening the retained history past 30
bits would make `u64` positions load-bearing and is a separate change.

## D3 — Verification above 30 declared window bits (checked 2026-08-24)

The pinned C **decoder** rejects a declared window above 30
(`BROTLI_LARGE_MAX_WBITS`, `c/dec/decode.c`, state
`BROTLI_STATE_LARGE_WINDOW_BITS`), so it cannot serve as the independent oracle
for `31..=62`, which Section 11.8 of the implementation specification
anticipates.

**Decision.** Split the evidence:

- `10..=30`: every stream is decoded by the pinned C decoder with
  `BROTLI_DECODER_PARAM_LARGE_WINDOW` set.
- `31..=62`: the header bits are asserted against the RFC directly (marker
  `0b00010001` then six window bits), and the payload is asserted to be
  byte-identical to a 30-bit stream that the C decoder *has* accepted.

**Known gap.** No 64-bit RFC 9841 decoder is available in this repository, so
the `31..=62` range is not exercised by an independent decoder end to end. The
argument above reduces it to a header check over a payload that was
independently validated, which is strong but not the same thing. Closing the gap
needs either a 64-bit decoder or this crate's own decoder, neither of which
exists yet.

## D4 — Large window at qualities 0, 1 and 2 (checked 2026-08-24)

Qualities 0 and 1 report
`SharedBrotliError::UnsupportedLargeWindow` rather than emitting a stream.
Quality 2 reports `UnsupportedQuality(2)`, which is the more fundamental
problem: it has no encoder at all.

**Reason.** The pinned C encoder clears `large_window` for every quality at or
below `MAX_QUALITY_FOR_STATIC_ENTROPY_CODES` (`SanitizeParams`,
`c/enc/quality.h`), so the request is *silently dropped* there. Silently
dropping an explicit format request is what Section 19.6 of the implementation
specification forbids, and the specification supplies the
`UnsupportedLargeWindow` variant for exactly this case.

**What it would take to lift.** The two fast encoders store their distance tree
with a hard-coded 64-symbol alphabet (`store_huffman_tree(&depth[64..], 64, …)`
in `core::fast::q0` and `core::fast::q1`). A large-window stream needs the
140-symbol alphabet, which changes the width of a simple prefix code's symbols
from six bits to eight. Making that alphabet a parameter is a small change to
the hottest loop in the crate with no C oracle to compare against, because the
reference never produces such a stream. It is deliberately not part of this
change.

**Consistency.** The refusal is raised before the empty-input shortcut, so
`compress(q0 + large window, b"")` fails exactly like
`compress(q0 + large window, b"payload")` does. Nothing that was constructible
before this change reaches the new check.

## D5 — Empty input ignores the declared window (checked 2026-08-24)

For an empty input every quality emits the single byte `0x06` — an ordinary
RFC 7932 stream header with `ISLAST` and `ISLASTEMPTY` set — even when a large
window was requested.

**Reason.** This is the reference's own one-shot shortcut
(`BrotliEncoderCompress`, `c/enc/encode.c`) and predates this change; altering
it would break the byte-stability contract for every previously constructible
parameter set. An empty stream carries no distances, so the declared window has
no observable meaning in it.

**Scope.** The shortcut applies to `compress` and `compress_to_slice`. A
streaming adapter that is finished without any input goes through the ordinary
encoder and does emit the requested header.
