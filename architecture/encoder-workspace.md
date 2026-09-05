# Encoder workspace

Retained encoder storage, shared stream scheduling, SIMD dispatch, and writer
backpressure live behind the public compressor API.

## Ownership and public boundary

`Compressor` owns one retained encoder, input staging and pending output. Public
configuration remains small and copyable. `Backend` is an opaque, host-validated
value: `Default` detects the host, `SCALAR` selects the independent baseline, and
`available()` enumerates distinct runnable backends. `with_backend` selects one
through the builder; no SIMD or implementation type is exposed.

```mermaid
graph TD
    C[Compressor] --> Cache[EncoderCache: one resolved encoder]
    C --> Staging[undecided input tail]
    C --> Pending[durable encoded suffix and cursor]
    Cache --> Family[Fast / Greedy / HQ]
    Family --> Kernel[Box dyn Kernels: Selected S]
    Family --> Search[retained matcher and ring buffer]
    Family --> Entropy[retained splits, histograms, trees and codes]
    Session[public EncoderSession] --> Owner[private SessionCore: borrows Compressor]
    Owner --> State[private StreamState]
    OneShot[private one-shot driver] --> State
    Dictionary[immutable PreparedDictionary] -. borrowed .-> Family
```

`retained_bytes` sums every owned heap allocation, including boxed state and
capacity rather than length for vectors. It excludes stack fields, caller output,
shared dictionaries and allocator bookkeeping. The allocator-instrumented
`compressor_memory` tests compare this sum with live requested heap bytes at all
qualities. This is allocation accounting, not a process-RSS estimate.

The configured retention policy applies after one-shot completion and when a
session drops, including through readers/writers. A finished session can retain
its resettable encoder; abandonment invalidates it first. `Bounded` releases the
workspace when its full accounting exceeds the ceiling. Session staging and
pending buffers obey the same policy. A forgotten session still requires
explicit `recover`; the exclusive borrow alone cannot detect `mem::forget`.

## One scheduler, borrowed complete blocks

`core::stream::StreamState` owns the phase, resolved block limit and continuation
restart flag. `core::session::SessionCore` owns the compressor/dictionary borrows,
checks logical positions, and translates private encoder errors into `EncodeError`.
One-shot vector and slice entry points call the same scheduler with `Finish`.

```mermaid
flowchart TD
    Call[Process / Flush / Finish] --> Drain[drain durable pending output]
    Drain --> Pending{suffix remains?}
    Pending -->|yes| NeedOut[NeedsOutput, accept no new input]
    Pending -->|no| Finished{final block already emitted?}
    Finished -->|yes| Done[Finished]
    Finished -->|no| Decide{block end or explicit operation known?}
    Decide -->|no| Stage[stage undecided tail, NeedsInput]
    Decide -->|yes| Source{staging empty?}
    Source -->|yes| Borrow[borrow input block directly]
    Source -->|no| Fill[complete staged block]
    Borrow --> Encode[encode / flush / finish selected family]
    Fill --> Encode
    Encode --> Deliver[direct destination, retain only overflow]
    Deliver --> Drain
```

One-shot calls need no staging or pending allocation: all input and its finality
are known, and the output is an append destination or a non-resumable slice. Fast
encoders write directly to slices with enough fragment reservation; otherwise
completed bytes come from retained encoder scratch. Slice overflow returns a
private output-capacity error; sessions retain the suffix and report `NeedsOutput`.

```mermaid
stateDiagram-v2
    [*] --> Open
    Open --> Open: emit non-final block or stage tail
    Open --> Flushed: explicit Flush
    Flushed --> Flushed: redundant Flush
    Flushed --> Open: accept new input
    Open --> FinalPending: Finish emits final block
    Flushed --> FinalPending: Finish
    FinalPending --> FinalPending: drain part of output, NeedsOutput
    FinalPending --> Finished: no output remains
    Finished --> Finished: ignore later input
    Open --> Failed: encoder failure
    Flushed --> Failed: encoder failure
    Failed --> Failed: session rejects further processing
```

`FinalPending` and `Finished` share the internal final phase; pending-buffer
emptiness distinguishes them. Public `is_finished()` is true only in the latter.
For experimental continuations, the logical position is checked before input
acceptance and advanced by consumed bytes. The two-byte restart uses the same
scheduler's flush action. Finished sessions ignore even input that would overflow
the logical-position limit.

The one-shot driver shares the streaming finish path described in
[universal-encoding.md](universal-encoding.md). All output destinations receive
the same stream, or an output-capacity error. Vector appends roll back on failure.
Slice contents may be partially written on failure, as documented.

## Reusable entropy and search storage

Fast arena resets clear fixed tables in place while retaining command, literal
and tree vectors. Ring-buffer initialization resizes its existing allocation.
The shared meta-block writer retains tree/context-map scratch and all depth/bit
tables; block encoders borrow those tables. Move-to-front uses bounded stack
scratch. Greedy splitters accept their previous split and histogram storage.
HQ retains split/cluster/literal-cost storage and the full meta-block shape.

HQ prefix candidates occupy retained workspace. They merge backwards into the
existing match arena, without `split_off` or a temporary merge vector. Earlier
arena entries remain unchanged; the ordering remains ascending match length,
then smaller distance, with tree matches first on exact ties. Boundary tests pin
the tie rule using distinguishable dictionary length codes.

```mermaid
flowchart LR
    Reset[reset logical lengths and validity] --> Search[fill retained matcher / candidate arena]
    Search --> Split[fill retained splits and histograms]
    Split --> Codes[borrow retained entropy tables]
    Codes --> Output[emit completed bytes]
    Output --> Policy{retention policy}
    Policy -->|keep| Reset
    Policy -->|release or exceed ceiling| Drop[drop all owned storage]
```

## Cold matcher allocation and SIMD

Bucket matchers retain counters and encoded offsets. q7–q9 allocate four starter
positions on first touch and promote a bucket once to its full reference depth
when a fifth slot is needed. Promotion copies the four valid positions in place
and keeps the old starter region allocated. The high offset bit marks sparse
storage; low bits encode base plus one. Counters determine validity, not stale
payload bytes. Reset preserves allocations and clears validity; it never demotes
a promoted bucket. Worst-case abandoned starters add four positions per bucket.
Forgetful-chain matchers materialize banks on first touch rather than allocating
every bank's slots up front. Their heads/counters likewise govern validity.

q5/q6 use parallel byte tags. A 16-byte `fearless_simd` comparison produces a
candidate mask; inactive circular slots are masked out and surviving slots are
visited newest first. The scalar backend deliberately scans without filtering as
an independent oracle. q7–q9 use untagged bucket scans.

Selection dispatch runs when an encoder is created. Its `Box<dyn Kernels>` stores
the selected proof token; current tokens are zero-sized. Each outer kernel call
enters the selected feature-enabled body and passes the token to generic inner
loops. There is no per-candidate feature detection or virtual call. Cache reuse
checks the backend discriminant and resolved parameter shape before reset.

```mermaid
sequenceDiagram
    participant Builder
    participant Cache
    participant Selected as Selected S / dyn Kernels
    participant Inner as generic inner loops
    Builder->>Builder: validate opaque Backend
    Cache->>Selected: dispatch once when constructing encoder
    loop blocks and reused streams
        Cache->>Selected: outer kernel call
        Selected->>Inner: vectorize body, pass S
        Inner-->>Cache: reference-ordered results
    end
```

## Bounded writer backpressure

The writer retains an initialized 128 KiB outbox; only `head..end` is live.
It does not clear/reinitialize the whole allocation for every sink write.
`write` drains previously owned bytes before accepting new input, pumps a bounded
session output, and returns the amount accepted. A sink error after acceptance
is deferred until the next drain, so callers do not replay input already owned.
`Flush` and `Finish` loop over bounded pulls. Finishing forbids new writes even
while final bytes await delivery. Fault-injection tests cover every output byte
position, short writes, `Interrupted`, `WouldBlock`, zero writes and retryable
flush/finalization failures.

## Verification

Allocator-instrumented tests check retained requested-byte accounting and warmed
allocation behavior. Lifecycle and streaming tests cover workspace reuse,
retention, abandonment, recovery and writer backpressure. Private tests cover
sparse promotion, prefix merge ordering and baseline/host-SIMD equivalence.

See [development checks](../docs/development.md) and
[benchmarking](../docs/benchmarking.md) for commands.

## Known gaps

- The cache has one slot; alternating incompatible resolved settings rebuilds it.
- Retained-byte accounting excludes allocator overhead and is not process RSS.
- Host-backend tests only exercise instruction sets available on the test host.
