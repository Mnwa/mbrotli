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

## Divergence: limits are a value with accessors, not public fields

Section 12.3 sketches `SharedContextLimits` as a struct with six public fields.
This crate keeps the fields private behind `Default`, `with_*` setters and
getters, matching `CompressParams` and every other parameter type here. Public
fields would make adding the remaining three limits a breaking change, which is
the wrong trade for a type whose whole purpose is to grow as more of RFC 9841
lands.

Three of the six exist today, because three is what something checks:
`max_total_source_bytes`, `max_prefix_bytes` and `max_allocated_bytes`.
`max_transformed_word_bytes` and `max_trie_nodes` land with the serialized
dictionary; `max_reusable_workspace_bytes` lands with the reusable encoder
workspace. A limit that nothing enforces would be a promise the code does not
keep.

## Extension: the prefix search is reachable before the encoders use it

`Compressor::longest_prefix_match`, `PrefixMatch`,
`SharedContext::backward_distance` and `SharedContext::dictionary_offset` are
not in the specification. They are here because Milestone 2 requires a
"scalar prefix match oracle" and "virtual concatenation addressing", and this
repository's completion checks run `clippy -D warnings` and forbid `#[allow]`:
code with no consumer cannot land. The encoders become that consumer in
Milestone 3; until then these four read-only entry points are, and they are
useful in their own right — `longest_prefix_match` answers how well a candidate
dictionary actually covers a corpus, which is the question worth asking before
shipping one, and the two mapping functions are the RFC's own distance
arithmetic, which a decoder-side fixture needs.

They are additive, take `&SharedContext`, mutate nothing, and expose no `core`
type: `PrefixMatch` is a public `Copy` value built from the private one by
`From`.

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
| Shared error | `mbrotli::compressor::shared::SharedBrotliError` | public, `#[non_exhaustive]`; gained `TooManyPrefixDictionaries`, `DictionaryTooLarge`, `SharedContextTooLarge`, `SharedContextQualityMismatch` and `UnsupportedSharedContextForQuality`; variants are still added only as they become reachable | value |
| Fast matchers | `compressor::core::fast::q0`, `compressor::core::fast::q1` | not extended | — |
| Quick / chain / bucket matchers | `compressor::core::greedy::hashers::{QuickMatcher, ChainMatcher, BucketMatcher, MatchFinder}` | not extended | context workspace, once the encoders consult a context |
| H10 matcher | `compressor::core::hq::h10::BinaryTreeMatcher` | not extended | context workspace, once the encoders consult a context |
| Static dictionary | `compressor::core::shared::dictionary` | not extended | built-in RFC 7932 data only |
| Shared context | `mbrotli::compressor::shared::SharedContext` (new) | owns `SharedDictionaryData` and `PreparedDictionaryIndexes` plus the prepared `QualityLevel`; `Send + Sync` by its fields alone, no `Arc`, no lock, no atomic, no interior mutability | caller-owned; borrowed `&mut` per call |
| Context builder | `mbrotli::compressor::shared::SharedContextBuilder` (new) | consuming builder; `add_prefix_dictionary<B: Into<Box<[u8]>>>`, `with_limits`, `prepare`; call order is prefix order | owns `Vec<Box<[u8]>>` until `prepare` moves it |
| Context limits | `mbrotli::compressor::shared::SharedContextLimits` (new) | `Copy` value with `Default`, three `with_*` setters and three getters; see the divergence above | `Copy`, inside the builder |
| Attachment list | `compressor::core::rfc9841::prefix::PrefixSources` (new) | `Box<[Box<[u8]>]>` plus a `Box<[u64]>` cumulative offset table; `locate`, `run_from`, `address_of`, `distance_of`, `match_length` | owned by the context, immutable |
| Prefix limit | `prefix::MAX_PREFIX_DICTIONARIES` (15), `MAX_PREFIX_SEGMENT_BYTES` (`2^31 - 1`) | ports `SHARED_BROTLI_MAX_COMPOUND_DICTS`; the segment cap is this port's, replacing the reference's unchecked `u32` truncation of `source_size` | constants |
| Prepared index | `compressor::core::rfc9841::prepared::PreparedPrefix` (new) | ports `CreatePreparedDictionary`; three boxed tables instead of one flat allocation carved by pointer arithmetic, and offsets instead of a retained source pointer | one per attachment, immutable |
| Candidate walk | `prepared::Candidates` | ports the chain half of `FindCompoundDictionaryMatch`; yields source offsets newest first | borrows the index |
| Prefix match scan | `compressor::core::rfc9841::prefix::common_prefix_len` (new) | its own scalar whole-word scan, deliberately *not* the encoders' vector kernel: reusing that meant refactoring it, which cost about 6% of quality 1 (see `rfc9841_benchmarks.md`), and Section 42.3 lists this kernel as one to profile before vectorising | pure |
| Context state | `compressor::core::rfc9841::context::SharedContextInner` (new) | dictionaries plus indexes; **no mutable third part yet** — see the context lifecycle document | owned by `SharedContext` |
| Context limits (internal) | `context::Budget` | flat `Copy` mirror of the public limits, so no accessor is called inside a check loop | value |
| Shared bound | `Compressor::calculate_shared_bound` (new) | validates the prepared quality, then delegates to `calculate_bound`; takes `&SharedContext`, activates nothing | pure |
| Shared one-shot | `Compressor::compress_shared`, `compress_shared_to_slice` (new) | validate, then route an empty context to the ordinary driver and refuse a non-empty one | call-scoped |
| Shared validation | `compressor::core::driver::check_shared`, `check_quality_implemented`, `check_shared_context` (new) | Section 21.1's order, minus the context-quality step the public layer owns | call-scoped |
| Prefix search | `Compressor::longest_prefix_match`, `shared::PrefixMatch` (new) | see the extension note above | read-only |
| Prepared-index oracle | `mbrotli_shim_prepare_dictionary` in `brotli-ffi/shim/` (new) | exposes `CreatePreparedDictionary` and copies its three tables out for the differential test | test-only |
| Streaming shared adapters | — | **not implemented** | `SharedCompressorWriter`, `SharedCompressorReader` land with the encoder integration |
| Prefix matching in the encoders | — | **not implemented**; refused with `UnsupportedSharedContextForQuality`, never ignored | — |
| Serialized dictionary | — | **not implemented** | — |
| Canonical / reversed varints | — | **not implemented** | lands with its first consumer, see below |
| Framing container | — | **not implemented** | — |

## No shared ownership anywhere

Nothing here introduces `Arc`, `Rc`, `Mutex`, `RwLock`, an atomic, a global
cache, or interior mutability. `WindowBits` is a `Copy` newtype over a two-byte
private enum inside a `Copy` `CompressParams`; `ResolvedWindow` is a `Copy`
value resolved once per session and read from there. There is no
`SharedCompressOptions` and no context handle to clone.

`SharedContext` owns its dictionary bytes as `Box<[u8]>` and its indexes as
`Box<[u32]>`, `Box<[u16]>` and `Box<[PreparedPrefix]>` — every collection that
has stopped growing is boxed rather than a `Vec`, so the capacity word is gone
and `push` is not reachable on something documented as immutable. The only
`Vec` is the builder's attachment list, which is the only collection that
grows. `SharedContext` is `Send` and `Sync` because its fields are, not because
anything asserted it; a caller who wants one context on several threads writes
the `Arc<Mutex<_>>` themselves, and the crate neither creates it nor knows
about it.

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

Section 53 of the implementation specification lists five documents. All five
now exist: this one, `rfc9841_interop_decisions.md`, `rfc9841_wire_map.md`,
`rfc9841_security.md` and `rfc9841_context_lifecycle.md`.

## Deferred: varints

Section 45 of the implementation specification lists canonical and reversed
varints in Milestone 1, before anything reads them. This repository's completion
checks run `clippy -D warnings` and forbid `#[allow]`, so an unreferenced module
cannot land. The varint module therefore lands with its first consumer — the
serialized shared dictionary parser — rather than ahead of it. Its contract is
unchanged: base 128, seven payload bits per byte, least significant group first,
at most nine bytes and 63 bits, shortest encoding only, and a reversed form for
the final footer that reverses bytes and not bits.
