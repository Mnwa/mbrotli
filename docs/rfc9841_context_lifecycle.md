# RFC 9841 shared context lifecycle

> **Written before the pre-release API redesign.** The public names in this
> document — `Brotli`, `CompressParams`, `CompressWorkspace`, `QualityLevel`,
> `WindowBits`, `SharedContext` and the `compress_*` method families — no
> longer exist. The encoders below them did not change.
> [`pre_release_api_redesign.md`](pre_release_api_redesign.md) maps the old
> names onto the new ones.


What a `SharedContext` is, who owns it, what happens to it during a
compression call, and what a caller may rely on when reusing one. This
describes the code as it stands; the parts of Sections 8.3, 12.5 and 12.6 of
the implementation specification that are not written yet are named as such at
the end.

Pinned reference: **Google Brotli v1.2.0**, commit
`028fb5a23661f123017c060daa546b55cf4bde29`.

The mechanics, with diagrams, are in
[`architecture/shared-brotli.md`](../architecture/shared-brotli.md); the
symbol-by-symbol mapping is in
[`rfc9841_api_binding.md`](rfc9841_api_binding.md). This document is the
lifecycle contract on its own.

## 1. Ownership

A context is a plain owned value.

```rust
let compressor = Brotli::default().compressor();
let mut context = compressor
    .shared_context_builder(QualityLevel::Q5)
    .add_prefix_dictionary(dictionary_bytes)   // moved, not borrowed
    .prepare()?;
```

- The builder takes `B: Into<Box<[u8]>>`, so a `Vec<u8>` or a `Box<[u8]>` moves
  without copying and a `&[u8]` copies once. Nothing borrows the caller's
  buffer, and nothing keeps it alive by reference count.
- `SharedContext` is `Send` and `Sync`, because every field is. Neither is
  asserted with a marker; both fall out of ordinary owned storage.
- There is **no** `Arc`, `Rc`, `Mutex`, `RwLock`, atomic, global cache or
  interior mutability anywhere in the context or below it. The C reference's
  "lean" prepared dictionary — which retains a raw pointer to caller bytes and
  makes their lifetime the caller's problem — has no analogue here: a prepared
  index stores offsets, and the bytes it indexes are owned by the same value.
- `SharedContext` is deliberately not `Clone`. Cloning a context would mean
  copying the dictionaries and every index; a caller who needs two prepares
  two.

Concurrency is the caller's policy, and the crate takes no part in it. One
context may sit behind an `Arc<Mutex<_>>` the caller creates, which serialises
the compression as well as the access. For genuine parallelism, prepare one
context per worker.

## 2. Construction is a transaction

`SharedContextBuilder::prepare` either returns a whole context or returns an
error and leaves nothing behind.

Checks, in order, all before the first table is allocated:

| Check | Error |
| --- | --- |
| at most 15 attachments | `TooManyPrefixDictionaries { attached, limit }` |
| each attachment at most `2^31 - 1` bytes | `DictionaryTooLarge { bytes, limit }` |
| total prefix at most `max_prefix_bytes` | `DictionaryTooLarge { bytes, limit }` |
| total source at most `max_total_source_bytes` | `DictionaryTooLarge { bytes, limit }` |
| predicted *peak* allocation at most `max_allocated_bytes` | `SharedContextTooLarge { bytes, limit }` |

The allocation check compares an upper bound computed from the attachment
sizes, not the finished size, so the allocation a limit refuses is never made.
It bounds the build's high-water mark — the scratch tables and the finished
ones are alive at the same time — which is roughly eight bytes per source byte.
The bound over-counts the item table at one entry per hashable position, the
most the build can chain, and counts every other table exactly, so it also
bounds the finished context.

On failure: no partially usable context is returned, every temporary is
dropped, no global cache is created or poisoned, the builder's inputs follow
ordinary move semantics, and there is no synchronisation state to recover.

## 3. Attachment order is prefix order

Builder call order is the logical order of the prefix:

```text
first  add_prefix_dictionary  ->  farthest / oldest bytes
last   add_prefix_dictionary  ->  nearest / newest bytes, immediately
                                  before the stream's own output
```

The attachments are never concatenated in memory. `PrefixSources` gives them
one logical address space with a cumulative offset table, and every distance,
every match and every accessor works in that space.

A decoder must attach byte-identical dictionary data in byte-identical order.
Nothing about the context is written into the stream — not the bytes, not a
hash, not a length. The context is out-of-band by design.

## 4. What a call does to a context

```text
Prepared -> (borrowed &mut for one call) -> Prepared
```

That is the whole lifecycle today, and it is not a simplification of something
richer: **a context holds no stream state**. It holds the caller's bytes and
indexes derived from them by a pure function, and nothing else. There is no
LZ77 history in it, no distance cache, no pending literal or command, no
meta-block state, no framing continuation state and no input position.

Consequences a caller may rely on:

- Independent calls inherit nothing from each other, because there is nothing
  to inherit.
- A failed call leaves the context exactly as a successful one does, because
  neither writes to it.
- Repeated calls with the same parameters, context and input emit identical
  bytes, whatever ran in between and whether it succeeded.
  `shared_context::reusing_one_context_is_deterministic_across_failures` runs
  `A, B, fail, fail, A` and requires the two `A` outputs to be equal.
- The exclusive borrow is still real and still enforced: `compress_shared` and
  `compress_shared_to_slice` take `&mut SharedContext`, so the borrow checker
  prevents a second operation on the same context for the duration of a call.
  That is the API contract Milestone 3 needs, and it costs nothing to honour
  now.

## 5. Validation order

Section 21.1 of the implementation specification fixes the order. Split across
two layers, because only the public one knows what the context was prepared
for:

1. `Compressor::compress_shared*` — the context's `max_quality`
   (`SharedContextQualityMismatch`).
2. `core::driver::check_shared`:
   1. quality support — every quality the format defines has an encoder, so
      this step is now vacuous and kept only as the fixed place to consult;
   2. large-window support at this quality (`UnsupportedLargeWindow`);
   3. the shared path itself — a non-empty context below quality five reports
      `UnsupportedSharedContextForQuality`.

Everything runs before any input is consumed and before the output bound is
allocated.

`calculate_shared_bound` performs step 1 only and takes `&SharedContext`: a
bound activates nothing, mutates nothing and needs no exclusive borrow.

## 6. An empty context, and a non-empty one

An **empty** context — nothing attached, or every attachment empty — routes to
the ordinary driver with no wrapper and no extra allocation. It emits exactly
the bytes `Compressor::compress` emits for the same parameters, for ordinary
and large-window streams alike.
`shared_context::an_empty_context_compresses_exactly_as_the_ordinary_call_does`
checks this at every implemented quality over the structural corpora, with a C
round trip on each result.

A **non-empty** context is refused with
`SharedBrotliError::UnsupportedSharedContextForQuality { quality }`, at every
quality, because no match finder consults a context yet. This is a deliberate
refusal rather than a silent drop: a stream compressed without the dictionary
it was handed decodes perfectly well on its own, so ignoring the dictionary
would surface only as corruption at a decoder that *did* attach it — the exact
failure Section 19.6 exists to prevent.

## 7. Reading a context

Four accessors report what a context holds — `attachment_count`,
`prefix_dictionary_count`, `has_custom_static_dictionary`, `source_size` — and
`allocated_size` reports what it costs. `allocated_size` counts the two
categories the context is responsible for, the dictionary bytes and the
prepared indexes, and needs no synchronisation because there is none to take.
The encoder workspace a call uses is not part of the context and is not
counted.

Three read-only entry points expose what the indexes are for:
`Compressor::longest_prefix_match`, `SharedContext::backward_distance` and
`SharedContext::dictionary_offset`. All three take `&SharedContext`. Why they
exist before the encoders do is recorded in
[`rfc9841_api_binding.md`](rfc9841_api_binding.md#extension-the-prefix-search-is-reachable-before-the-encoders-use-it).

## 8. Backend neutrality

Preparation and the prefix search are both scalar and both pure functions of
their inputs, so a context prepared on one machine is bit-for-bit the context
prepared on any other, and a search over it returns the same answer on any
machine. Nothing here dispatches SIMD, and nothing here is compiled into a file
any encoder uses.

A context therefore has no backend affinity: one prepared while a compressor
resolved AVX2 is used unchanged by a compressor that resolves the scalar
fallback, and the answers do not move. `Compressor::longest_prefix_match` still
takes the compressor, so that a vectorised scan — if profiling ever justifies
one — can be dispatched from the level it already holds without the method
moving.

## 9. What is not written yet

- **No reusable workspace, and therefore no generation counter and no RAII
  idle guard.** Sections 8.3, 12.5 and 12.6 describe both around reusable
  match-finder and command buffers that a context lends to a session. No such
  buffer exists yet, so neither does the machinery that would reset it.
  Documenting an `Idle -> Active -> Idle` state machine over a context with no
  state would describe intent as behaviour. It lands with the match finders.
- **No streaming shared adapters.** `SharedCompressorWriter` and
  `SharedCompressorReader` do not exist. Section 22's contract — the exclusive
  borrow held for the whole session, and the borrowed context becoming reusable
  when the adapter is dropped — lands with them, because an adapter that
  refused every non-empty context would hold a borrow for a session that cannot
  happen.
- **No serialized dictionary attachments.**
  `SharedContextBuilder::add_serialized_dictionary` does not exist, so
  `attachment_count` and `prefix_dictionary_count` are equal today and
  `has_custom_static_dictionary` is always `false`.
- **Three of the six specified limits are absent.** See the divergence note in
  [`rfc9841_api_binding.md`](rfc9841_api_binding.md#divergence-limits-are-a-value-with-accessors-not-public-fields).
