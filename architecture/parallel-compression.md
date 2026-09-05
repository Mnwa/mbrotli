# Parallel compression

The caller schedules tasks that encode fixed independent segments. Private
fragment encoders produce aligned non-final blocks, and the batch assembles them
in source order into one stream. The API supports qualities 0–11 with standard
windows; dictionary, Large Window, and framing extensions are unsupported.

## Public and ownership boundaries

`compressor::parallel` exposes `ParallelCompressor`, `ParallelConfig`,
`BatchConfig`, validated `SegmentSize` and `TaskCount`, memory/directory staging,
source wrappers (including `SeekSource<R>`), task/batch types, polling, statistics,
retention, and errors.
The `compressor` module is public; its `core` remains private. Serial APIs are
also re-exported at the crate root.

Validated integers use `TryFrom<usize>` and `get()`; standard values use
`SegmentSize::DEFAULT`, `TaskCount::ONE`, and `Default`. Configuration/staging
conversions use `From`. Source wrappers use `From<Arc<[u8]>>` and
`TryFrom<File>`. These trait-based spellings follow repository API conventions.
There is no mandatory executor trait, runtime dependency, or hidden thread pool.
Rayon is a development dependency for examples of scheduling in tests/benchmarks.
`tempfile` is a development dependency for test and benchmark fixtures only.
The private `parallel::core::spool` module uses the standard library for exclusive
spool creation and cleanup.

```mermaid
graph TD
    Public[compressor::parallel public wrappers] --> Planner[parallel::core::Compressor and Plan]
    Public --> Batch[parallel::core::batch]
    Public --> Task[parallel::core::task]
    Public --> Source[RandomAccessSource / SeekSource / FileSource / ArcBytesSource]
    Planner --> Reservoir[exclusive idle Workers]
    Task --> Worker[owned Worker: fragment codec, serial codec, input buffer]
    Worker --> Fragment[compressor::core::fragment]
    Fragment --> Driver[serial core::driver::Encoder]
    Driver --> Families[Fast / Greedy / HQ]
    Families --> Dispatch[core::dispatch Selected S, INDEPENDENT]
    Task --> Artifact[private memory artifact or Spool]
    Artifact --> Spool[parallel::core::spool: File + cleanup guard]
    Task --> Slot[one completion slot per task]
    Slot --> Batch
    Batch --> Destination[Vec / Write]
```

Each `ScopedParallelTask<'input>` owns its worker, artifact, immutable input
handle, cancellation observer and completion capability. It never borrows the
batch or parent compressor. `OwnedParallelTask` is the `'static` specialization;
`OwnedParallelBatch` similarly specializes the shared batch implementation. This
supports both scoped borrowing and detached execution without duplicated engines.
`ParallelCompressor` is `Send`, non-Clone, and owns no global state.

## Planning, source reads, and memory

Fixed segment sizes range from 64 KiB to 16 MiB, with a 4 MiB default. Boundaries
are `i * segment_size .. min((i + 1) * segment_size, source_len)` using `u64` file
coordinates. Effective task count is `min(requested, segments)`. Contiguous
balanced grouping gives the first remainder tasks one additional segment.
Neither grouping nor completion order changes bytes.

Empty input creates no tasks and uses canonical serial encoding. A nonempty input
below `minimum_parallel_size` (default 8 MiB) uses serial encoding only if it also
fits one segment. That fallback still creates one caller-run task. Restricting
fallback to one segment prevents user-selected thresholds from causing a
whole-file allocation. Setting the threshold to zero forces independent parts.

Borrowed slices go directly into codec calls. Detached sources fill one retained
segment buffer per task. `FileSource` retains an open regular file and uses Unix
`read_at` or Windows `seek_read`, retrying interruptions and short reads. Other
platforms return `Unsupported` for positional reads. File handles are never used
with a shared seek cursor. Metadata snapshots include length and modification
time, plus device, inode and change time on Unix. Metadata checks cannot prove
immutability against every in-place write; callers can provide stronger immutable
snapshot sources. Arbitrary blocking source calls cannot be forcibly interrupted.

Planning checks staged-byte bounds, descriptor sizes, active-worker estimates,
completion metadata, assembly scratch and currently retained workspaces before
returning tasks. Staged payload allowance is `2 * input + 1024 * max(segments,1)`.
Memory staging also charges descriptor storage to its explicit limit. Workspace
estimates are deliberately conservative: per task, 4 MiB + 16 times segment size
for q0/q1, 64 MiB + 128 times segment size for q2–q4, and 256 MiB + 256 times
segment size above q4. These ceilings are not measured RSS. File staging keeps
payload RAM proportional to active workers and segment size; descriptor memory
is proportional to segment count and included in the aggregate ceiling.

## Generic source entry point and seekable readers

`prepare_source<S, T>` accepts `S: RandomAccessSource + ?Sized` and
`T: Into<Arc<S>>`. Owned sources, shared concrete sources, custom conversions, and
`Arc<dyn RandomAccessSource>` use this single entry point. `FileSource` is a
source adapter. `Arc` and custom
conversion arguments may need explicit source types due to `Into` ambiguity:
`prepare_source::<FileSource, _>(shared_file, config)` or
`prepare_source::<dyn RandomAccessSource, _>(erased_source, config)`.

The conversion occurs once before planning. A private sized `SharedSource<S>`
bridge lets both sized and unsized sources enter the existing erased core path.
It forwards length, identity and reads. Tasks clone the outer shared handle;
source bytes are never copied by this bridge. The bridge adds one small shared
allocation per prepared batch, not per task or segment.

`SeekSource<R>` takes ownership through `From<R>`. For `R: Read + Seek + Send +
'static`, it implements `RandomAccessSource` without requiring `R: Sync`.
The public wrapper delegates all locking and I/O to `parallel::core::source`.
Each length query holds the mutex while seeking to the end. Each range read
holds that same mutex for its absolute seek and the entire `read_exact` call.
The reader's original cursor is ignored; all future reads seek explicitly.
A short read or interruption is handled by `Read::read_exact`; seek/read errors
propagate as I/O errors into existing batch metadata/read errors. A reader panic
poisons the mutex and subsequent access returns an I/O error. Worker panic
handling continues to discard the affected codec workspace.

```mermaid
sequenceDiagram
    participant Caller
    participant Public as prepare_source / SeekSource
    participant Core as core::source
    participant Reader as R: Read + Seek + Send
    participant Task
    Caller->>Public: source.into() to Arc<S>
    Public->>Core: wrap shared source for type erasure
    Core->>Reader: lock, seek End(0), unlock
    Note over Core,Reader: planning snapshots current length
    Task->>Core: read_exact_at(offset, segment buffer)
    Core->>Reader: lock, seek Start(offset)
    Core->>Reader: read_exact (retry interrupted/short reads)
    Core-->>Task: unlock, bytes or I/O error
    Task->>Task: compress without the reader lock
```

Seekable input reads are serialized; task compression still overlaps. No
metadata identity can be inferred from the generic traits, so this adapter
provides live length checks only and callers must keep bytes immutable.
`FileSource` retains positional reads and file identity verification; it remains
the preferred file adapter when concurrent input reads are desired.

## Fragment format and encoder state

Format plan version 1 uses uniform two-byte raw prefixes and explicit distances
throughout every segment. It follows RFC 7932 sections 11.2 and 11.3. Every part
starts at a byte boundary (except the first part also owns the window header),
contains only non-final data blocks, and ends aligned. Alignment metadata uses
the existing flush writer's when-needed empty metadata blocks. The assembler
only copies validated bytes and appends the final-empty byte `0x03` at the proven
byte boundary. It never parses or edits complete streams.

```mermaid
sequenceDiagram
    participant Task
    participant Fragment as FragmentEncoder
    participant Codec as retained Encoder
    participant Artifact
    Task->>Fragment: encode(segment bytes, first role)
    Fragment->>Codec: reset semantic state, preserve allocations
    Fragment->>Codec: begin_fragment(prefix)
    Note over Codec: greedy/HQ seed local ring and prior bytes via two-byte flush
    Fragment->>Fragment: optional header + raw min(length,2) prefix
    loop complete body input blocks
        Fragment->>Codec: non-final encode; flush final body block
        Codec-->>Fragment: completed bytes
    end
    Fragment->>Codec: verify no pending partial bits
    Fragment-->>Task: sealed AlignedFragment
    Task->>Artifact: append bytes + source/segment descriptor
```

| State | Reset/ownership rule |
| --- | --- |
| Header and pending bits | Common prefix writer owns the sole window header; body codecs start headerless. Every successful fragment proves zero pending bits. |
| Ring and input coordinates | `reset_for` resets local positions and history; no prior segment is present. Greedy/HQ seed only the two current prefix bytes. |
| Greedy matcher | Existing dirty/partial-prepare protocol clears logical candidates while retaining allocations. Dictionary statistics use the permanently disabled sentinel. |
| HQ matcher/DP | Existing tree preparation resets roots; all-match and Zopfli arenas are rebuilt from this segment. Static-dictionary match distance is outside the representable range. |
| Recent distances | All wire distances are explicit. HQ excludes cached-distance DP edges. Greedy still uses local cache values as search candidates but writes full distances. Inherited decoder cache values are never consulted. |
| q0/q1 commands | Insert commands copy two bytes; the remaining copy and its distance are explicit. q0's independent starter/adaptive histograms include symbols 17/18 for three/four-byte copy remainders. q1 preserves compact symbol 40 at wire symbol 128 instead of overwriting it with the unused insert-zero alias, preserving the canonical Huffman order. |
| Literal contexts | Greedy/HQ prefix seeding advances prior-byte state exactly once. Fast codecs use no literal contexts. Prefix data is emitted exactly once by the common writer. |
| Entropy and splits | Rebuilt locally; q0 adaptive trees never cross a segment reset. |
| SIMD | Backend detected by the planner once; `Selected<S, true>` installed when a fragment encoder is allocated, retained across reset. Serial `Selected<S, false>` remains separately monomorphized. |

Every segment uses explicit distances throughout; cache-relative coding does
not resume after a prefix. Dictionary overloads are absent. Parallel Large
Window configuration returns a typed configuration error.

## Completion, cancellation, and reuse

```mermaid
stateDiagram-v2
    [*] --> Prepared
    Prepared --> Running: caller runs consumed task
    Prepared --> Abandoned: dropped task guard
    Running --> Success: commit artifact
    Running --> Error: source / codec / staging failure
    Running --> Panicked: catch_unwind, discard worker
    Running --> Cancelled: cancellation checkpoint
    Success --> Published
    Error --> Published
    Panicked --> Published
    Cancelled --> Published
    Abandoned --> Published
    Published --> [*]: one slot and one completion ID
```

A bounded channel holds exactly one small ID per task. Payloads and workers move
through single-assignment slots, protected only during ownership transfer. Codec
execution holds no mutex. Channel capacity equals task count, so tasks do not
need a coordinator to drain their payload or another task to progress.

Each task catches unwinding panics and discards the affected worker. Ordinary
errors return resettable workers. The drop guard reports abandonment once.
`panic=abort` remains unrecoverable. Cancellation uses a batch-local relaxed
atomic flag, carrying no other published state; checks run around reads and
segment encoding and before artifact commit. Successful bytes do not depend on
cancellation timing.

The coordinator collects by task ID and chooses the lowest failing ID after all
completions, preferring originating errors over cancellation fallout. Repeated
poll/wait errors use `BatchFailed(Arc<ParallelEncodeError>)` to preserve the typed
cause and its source chain. Blocking wait requires the caller's executor to remain
runnable. A timeout does not cancel. Dropping a batch cancels, drops untaken tasks,
collects only already-published results, and never waits for detached work. Late
tasks release their own resources. The default retention count is zero; callers
can retain compatible workspaces and trim them by aggregate size.

## Assembly and output failure

Directory staging resolves the existing directory to an absolute path. Each
task uses a fresh `RandomState` to hash candidate names, then atomically opens a
read/write file with `OpenOptions::create_new(true)`. Existing files and symlinks
are never opened or overwritten. Name collisions retry up to 128 times; other
filesystem errors propagate immediately through the existing staging I/O error
path. Unix files are created with mode `0600` (subject to the process umask);
other platforms use their default file permissions.

The spool owns its `File` before a separate path cleanup guard in declaration
order, so dropping an artifact closes the handle before attempting removal,
including on Windows. Success, errors, cancellation and unwinding all use this
same ownership cleanup. Removal is best effort because destructors cannot return
I/O errors. Callers must keep the staging directory stable and trusted; path
replacement, process termination or filesystem errors can prevent cleanup.
Spooling adds no SIMD dispatch or changes to encoded bytes.

```mermaid
stateDiagram-v2
    [*] --> Creating: canonicalize directory
    Creating --> Creating: candidate already exists, retry within limit
    Creating --> Failed: filesystem error or collision limit
    Creating --> Open: exclusive read/write create
    Open --> Open: append / validate length / seek and copy
    Open --> Closed: artifact dropped, File drops first
    Closed --> Removed: path guard removes file
    Closed --> LeftBehind: removal fails, error ignored
    Failed --> [*]
    Removed --> [*]
    LeftBehind --> [*]
```

```mermaid
flowchart TD
    Wait[all task completions] --> Verify[source snapshot + ordered descriptors + spool lengths]
    Verify --> Valid{all valid?}
    Valid -->|no| Error[return typed error, destination untouched]
    Valid -->|yes| Kind{destination}
    Kind --> Vec[reserve exact append size, rollback length on error]
    Kind --> Write[copy in order, count every accepted short write]
```

Before the first destination write, assembly validates segment order and exact
source ranges, artifact offsets/nonzero lengths, total segment count, and actual
spool length. Directory artifacts use sequential reads and a 128 KiB copy buffer.
`Interrupted`, short reads/writes and `WriteZero` are handled. A generic writer
failure returns its ownership and exact accepted byte count. It does not flush
or sync implicitly and is not retryable through the consumed batch.

Files are ordinary `Write` destinations. Callers own file creation, buffering,
flushing, durability, and any temporary-file publication policy. No path-specific
finish API or file commit policy is exposed.

## Verification and known gaps

`tests/parallel.rs` covers all qualities, standard windows, host backends, modes,
segment edges, fallback, source/staging identity, std/Rayon scoped and detached
execution, reverse task order, worker reuse, source panics/errors/mutation,
abandonment, timeout, cancellation, file output, and every output failure byte
for a representative fast stream. Private fragment tests independently decode
joined parts, and regressions cover q0 short explicit copies omitted by the serial starter
tree and q1 copy-two depth/ordering aliases found by AFL. The full existing serial differential/streaming suite is
also required. `fuzz/afl` adds the `parallel` target and committed seeds.
`benches/parallel.rs` validates before timing and prints sizes alongside scaling
and serial C/Rust measurements; those policies are labeled separately.

## Known gaps

- Cancellation occurs around segment encoding, not inside q11 optimization.
  A running codec call or blocking source read may delay cancellation.
- Independent segments lose cross-segment history and dictionary coding, so
  their ratio and speed are distinct from serial encoding.
- Source metadata checks cannot detect every in-place mutation.
- Spool cleanup is best effort; process termination and filesystem errors can
  leave files behind.
- Positional file reads are implemented for Unix and Windows. Other platforms
  return `Unsupported`.
