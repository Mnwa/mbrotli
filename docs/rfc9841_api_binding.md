# RFC 9841 API binding

Maps each role [RFC 9841] needs onto the symbol that plays it in this
repository, so that a reader can tell at a glance what already existed, what was
extended, and who owns the state. Symbols are the real ones; a role with no
symbol is not implemented yet and says so.

Pinned reference: **Google Brotli v1.2.0**, commit
`028fb5a23661f123017c060daa546b55cf4bde29`. Decisions this table depends on are
recorded in [`rfc9841_interop_decisions.md`](rfc9841_interop_decisions.md).

[RFC 9841]: https://www.rfc-editor.org/rfc/rfc9841.html

## Naming

The specification asks for one new public submodule named `shared`. This crate
already has a *private* `compressor::core::shared`, which means "code more than
one quality needs" and is unrelated. The public module is
`mbrotli::compressor::shared`; the two never appear in the same scope, and no
`core` type is reachable from the public one.

`SharedCompressOptions` does not exist and will not: every shared entry point
takes `CompressParams` and the context as separate arguments.

## Divergence: one window type, not two

Sections 2.1, 4.2, 5.1, 11.1, 54 and 55 of the specification require a separate
`LargeWindowBits` type stored as `Option<LargeWindowBits>` in `CompressParams`,
with `WindowBits` left at `10..=24`. **This repository does not do that**, at
the owner's explicit direction. The window and the header it selects are one
type:

```rust
pub struct WindowBits(WindowKind);          // WindowKind is private

enum WindowKind { Standard(u8), Large(u8) } // 10..=24 and 10..=62

WindowBits::standard(22)?                   // RFC 7932 header
WindowBits::large(30)?                      // RFC 9841 header
```

What is preserved from the specification's intent:

- **A large window is still never inferred.** It is selected by naming
  `WindowBits::large`, not by the size crossing 24. The two ranges deliberately
  overlap, and `WindowBits::large(22) != WindowBits::standard(22)`.
- **Neither range is widened.** `standard` still refuses anything above 24;
  `large` still refuses anything above 62.
- **Invalid states stay unrepresentable.** `WindowKind` is private, so the two
  validating constructors are the only way to build a value and nothing
  downstream re-checks a range.
- **There is still exactly one place a window decision is made**, and it is
  still `CompressParams`.

What is given up:

- `CompressParams::lgwin()` no longer always returns an RFC 7932 window; it
  returns whichever window was asked for, and `is_large()` says which.
- The non-destructive toggle of section 4.2 is gone. There is no
  `without_large_window`, because with one field there is no second value to
  restore; a caller that wants a different window builds one.
- `WindowBits::MAX` now means "largest *ordinary* window"; `LARGE_MAX` is the
  other ceiling.
- `TryFrom<usize> for WindowBits` is removed. With two target headers the
  conversion had no unambiguous meaning, which is why the constructors are
  named.

This is a source-breaking change to a pre-existing public type, so it is not a
"previously constructible params keep byte-identical behaviour" question but a
deliberate API break. Output bytes are unaffected: `tests/differential_c.rs`
still passes.

## Binding table

| RFC 9841 role | Repository symbol | Extension made | Ownership / lifetime |
| --- | --- | --- | --- |
| Standard params | `mbrotli::compressor::CompressParams` | unchanged shape: the window it already carried now selects the header too, so no field and no method was added; still `Copy + Clone + Debug + Eq + PartialEq` | copied per call |
| Window and its header | `mbrotli::compressor::WindowBits` | reshaped from `struct WindowBits(usize)` into `struct WindowBits(WindowKind)` over a **private** `enum WindowKind { Standard(u8), Large(u8) }`; built only by `WindowBits::standard` (`10..=24`) or `WindowBits::large` (`10..=62`), read by `bits` and `is_large`; `ParseWindowBitsError` gains `LargeUpperBound` | `Copy`, inside `CompressParams` |
| Window resolution | `compressor::core::rfc9841::window::ResolvedWindow` (new) | single place that turns params into header bits, retained history and syntax choice | value, resolved once per session |
| Stream header | `ResolvedWindow::header` | emits the fourteen-bit RFC 9841 form (`0b00010001` marker, six window bits) as well as the RFC 7932 one; replaced three copies of `EncodeWindowBits` in `core::fast`, `core::greedy::encoder`, `core::hq::encoder` | session-scoped `last_bytes` / `last_bytes_bits` |
| SIMD level | `fearless_simd::Level` held by `mbrotli::Brotli` and `mbrotli::compressor::Compressor` | reused unchanged | resolved once, pinned per session |
| One-shot driver | `compressor::core::driver::compress_to_vec`, `compress_to_slice` | added `check_large_window`, which runs before the empty-input shortcut | call-scoped |
| Quality routing | `compressor::core::driver::Encoder` | unchanged | owns one encoder per session |
| Encoder params (q3-q9) | `compressor::core::greedy::params::GreedyParams` | carries `window: ResolvedWindow`; `lgwin` is now the *retained* window | value, per session |
| Encoder params (q10-q11) | `compressor::core::hq::params::HqParams` | same | value, per session |
| Encoder params (q0-q1) | `compressor::core::fast::FastEncoder` | refuses a large window (see decision D4) | value, per session |
| Ring buffer | `compressor::core::shared::ringbuffer::RingBuffer` | sized from `ResolvedWindow::encoder_bits()`, which never exceeds 30, so a declared 62-bit window allocates nothing extra | session-scoped |
| Distance params | `compressor::core::shared::distance::DistanceParams` | added `new_large`, `for_window` and the `distance_code_limit` port of `BrotliCalculateDistanceCodeLimit`; `alphabet_size_max` and `alphabet_size_limit` now genuinely differ | value, per meta-block |
| Per-block distance retune | `compressor::core::hq::metablock::choose_distance_params` | candidate alphabets are built with `DistanceParams::for_window`, so a retune cannot silently drop back to the RFC 7932 alphabet | per meta-block |
| Meta-block writer | `compressor::core::shared::bitstream` | unchanged: it already separated `alphabet_size_max` from `alphabet_size_limit` | per meta-block |
| Bit writer | `compressor::core::shared::bits::BitWriter` | unchanged | borrows the output buffer |
| Bound calculator | `compressor::core::bound::bound`, `Compressor::calculate_bound` | reads the new field; stays `const fn` and keeps counting fragments from `CompressParams::lgwin`, which is what makes it conservative (see below) | pure |
| Writer adapter | `mbrotli::compressor::writer::CompressorWriter` | unchanged, still infallible to construct | owns the sink |
| Reader adapter | `mbrotli::compressor::reader::CompressorReader` | unchanged, still infallible to construct | owns the source |
| Error type | `mbrotli::compressor::BrotliCompressError` | added `Shared(#[from] SharedBrotliError)`, `#[error(transparent)]` | public, `#[non_exhaustive]` |
| Shared error | `mbrotli::compressor::shared::SharedBrotliError` (new) | public, `#[non_exhaustive]`; variants are added as they become reachable | value |
| Fast matchers | `compressor::core::fast::q0`, `compressor::core::fast::q1` | not extended | — |
| Quick / chain / bucket matchers | `compressor::core::greedy::hashers::{QuickMatcher, ChainMatcher, BucketMatcher, MatchFinder}` | not extended | context workspace, once shared contexts exist |
| H10 matcher | `compressor::core::hq::h10::BinaryTreeMatcher` | not extended | context workspace, once shared contexts exist |
| Static dictionary | `compressor::core::shared::dictionary` | not extended | built-in RFC 7932 data only |
| Shared context | — | **not implemented** | caller-owned `&mut SharedContext` when it lands |
| Serialized dictionary | — | **not implemented** | — |
| Canonical / reversed varints | — | **not implemented** | lands with its first consumer, see below |
| Framing container | — | **not implemented** | — |

## No shared ownership anywhere

Nothing added by this change introduces `Arc`, `Rc`, `Mutex`, `RwLock`, an
atomic, a global cache, or interior mutability. `WindowBits` is a `Copy`
newtype over a two-byte private enum inside a `Copy` `CompressParams`;
`ResolvedWindow` is a `Copy` value resolved once per session and read from
there. There is no `SharedCompressOptions` and no context handle to clone.

## Why the bound did not change

`calculate_bound` counts meta-block overhead by dividing the input by a fragment
size derived from `CompressParams::lgwin`. A large window never makes that count
too small:

- a large window only ever *widens* the effective window, and a wider window
  gives a block size at least as large, so the real number of meta-blocks can
  only fall;
- the declared window is deliberately not used, because feeding a 62-bit window
  into the same formula would claim one fragment for the whole input and make
  the bound less conservative rather than more;
- the header itself grows from at most seven bits to fourteen, which is still
  inside the two bytes the bound already reserves for it.

`large_window::the_bound_covers_every_large_window_stream` checks this over the
structural and boundary corpora at three qualities and four declared windows.

## Deliverables that are deferred

Section 53 of the implementation specification lists five documents. Four exist:
this one, `rfc9841_interop_decisions.md`, `rfc9841_wire_map.md` and
`rfc9841_security.md`. `rfc9841_context_lifecycle.md` does not, because the
mutable `SharedContext` whose lifecycle it would describe is not implemented;
writing it now would describe intended behaviour as if it were real, which the
repository's architecture rules forbid. It lands with the context.

## Deferred: varints

Section 45 of the implementation specification lists canonical and reversed
varints in Milestone 1, before anything reads them. This repository's completion
checks run `clippy -D warnings` and forbid `#[allow]`, so an unreferenced module
cannot land. The varint module therefore lands with its first consumer — the
serialized shared dictionary parser — rather than ahead of it. Its contract is
unchanged: base 128, seven payload bits per byte, least significant group first,
at most nine bytes and 63 bits, shortest encoding only, and a reversed form for
the final footer that reverses bytes and not bits.
