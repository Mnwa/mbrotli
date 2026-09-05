# mbrotli

Brotli compression in safe Rust, with identical bytes across compression APIs.

`mbrotli` implements every Brotli quality as a port of
[google/brotli] v1.2.0, commit `028fb5a`. One-shot, vector, slice, session,
reader and writer APIs emit the same bytes for equivalent stream settings,
including empty and incompressible input. The differential suite compares against
C streaming with matching parameters and block scheduling, and verifies output
with the C decoder. Native C one-shot empty-input and whole-stream fallback
rewrites are intentionally omitted.

For one-shot/session identity, supply `InputSize::Exact(input.len() as u64)` and
use the same configuration and dictionary without extra flushes. Unknown size,
different flush boundaries and nonzero continuation offsets are different stream
settings, not alternate ways to encode the same stream.

[google/brotli]: https://github.com/google/brotli/tree/028fb5a

- **No `unsafe`.** Not in the bit writer, not in the match scan, nowhere in
  `src/`. Hot loops shed their bounds checks through `as_chunks`, `first_chunk`
  and const-generic widths instead.
- **Reuse is the default.** A `Compressor` owns its workspace — hash tables,
  sliding window, histograms — and hands it to the next call. There is no
  separate workspace type to discover.
- **SIMD resolved once.** `fearless_simd` picks the instruction set when the
  compressor is built, never inside a loop, and every backend produces identical
  bytes. The match finder is chosen from the configuration alone, so the machine
  cannot change the output.
- **RFC 7932 output.** Verified by round-tripping through Google's C decoder.
- **RFC 9841 Large Window.** A window of up to 62 bits, asked for by name —
  `Window::large(30)` rather than a number that happens to exceed 24, so the
  wider header is never inferred. Declaring a wide window allocates nothing: the
  encoder keeps at most 30 bits of history whatever the header says.
- **RFC 9841 prefix dictionaries, without shared ownership.** A
  `PreparedDictionary` is immutable and holds no per-stream state, so any number
  of compressors can borrow one at once — across threads — with no `Arc`, no
  lock and no atomic of this crate's making. Its prepared index is byte-identical
  to the reference's, entry for entry, and so are the streams the match finders
  produce from it.

## Why encoding takes `&mut self`

Every encoding method takes `&mut self`, because every one of them advances state
the compressor owns. That is the whole design. The alternative — `&self` plus
hidden interior mutability — would communicate the wrong ownership model and make
the synchronisation choice implicit.

One compressor belongs to one worker. To encode separate streams concurrently,
use one `Compressor` per worker; they can share an immutable `PreparedDictionary`.
To divide one input among workers and produce one stream, use
`compressor::parallel::ParallelCompressor`.

## Status

| Feature | State |
| --- | --- |
| Quality 0–11 | implemented, compared with equivalent C streaming settings |
| Decoder | not implemented, and out of scope |
| One-shot, appending, fixed-slice APIs | implemented |
| Low-level `EncoderSession` | implemented — one state machine under every path |
| `Read` / `Write` adapters | implemented, transactional under short writes and sink errors |
| `Write::flush` | implemented — makes everything written so far decodable without ending the stream |
| Workspace reuse, retention policy, `trim` | implemented |
| Mode, block size, distance layout, literal context modelling | implemented |
| RFC 9841 Large Window (qualities 3–11) | implemented — qualities 0, 1 and 2 refuse it when the compressor is built |
| RFC 9841 prefix dictionaries (qualities 5–11) | implemented — byte-identical to the reference's compound dictionary |
| RFC 9841 prefix dictionaries at qualities 0–4 | refused with `DictionaryUnsupportedForQuality`, never ignored — the reference has no search there either |
| Stream offset | experimental headerless continuations at qualities 2–11; checked 63-bit logical positions |
| RFC 9841 serialized dictionary format | implemented behind the `experimental` feature — parsed, validated, built and written, and differential-tested against the reference parser; see below |
| RFC 9841 framing container | experimental streaming writer, chunk types 0–10, explicit references, metadata, directory and footer |
| Custom static dictionaries | experimental preparation and compression at qualities 5–11, including transforms and context combinations |

### The `experimental` feature

RFC 9841's serialized shared dictionary format is behind the `experimental`
Cargo feature, because it has no stable reference encoder: the C library
compiles its parser out unless `BROTLI_EXPERIMENTAL` is defined, and has never
exposed it as a supported API. The default encoder's equivalent-C-streaming byte
oracle therefore does not cover full custom static search, and the API may change
in a patch release. Rust API/backend identity and decoder compatibility remain
required for equivalent stream settings.

```toml
[dependencies]
mbrotli = { version = "0.1", features = ["experimental"] }
```

What the feature adds:

- `SerializedDictionary` and its builder, which parse and write the RFC 9841
  section 5 dictionary stream: LZ77 prefix, custom word lists, custom transform
  lists, combinations and the sixty-four entry context map;
- `WordList`, `TransformList` and `TransformOperation`, covering all
  twenty-three transform operations including the two scalar shifts;
- `DictionaryBuilder::add_serialized`, which prepares the embedded prefix and
  custom static search indexes in the same immutable `PreparedDictionary`;
- additional `DictionaryLimits` ceilings for parsing and transformed index
  expansion, checked before allocation;
- headerless continuation streams through `StreamConfig::with_stream_offset`;
- `framing::FramedWriter` and borrowing resource writers, with bounded chunks,
  explicit caller-supplied dictionary IDs, transactional sink recovery and no
  implicit finalization on drop.

See [custom dictionary and continuation mechanics](architecture/rfc9841-encoding.md)
and [container mechanics and supported forms](architecture/framing.md). Extended
custom-transform behavior is verified by independent C decoding; it does not
claim byte identity with C's more restricted experimental search implementation.
Local test results, benchmark observations and open release gates are recorded in
[the Track B validation report](docs/track_b_validation.md).

### Quality guide

| Quality | What it does |
| --- | --- |
| 0 | One pass, static entropy codes — fastest, largest output |
| 1 | Two passes, per-block entropy codes |
| 2 | Greedy matching, with the format's fixed command and distance codes while the meta-block stays small |
| 3 | Greedy matching, one prefix code per stream |
| 4 | Adds block splitting, histogram optimisation, distance parameters |
| 5 | Adds an extensive delayed search and literal context modelling |
| 6–9 | Deepens the search: 32 to 256 bucket candidates, 4 to 16 cached distances, and from 7 the three-context literal model |
| 10 | Replaces greedy matching with a Zopfli search over every match a binary tree can find, and clusters histograms into real context maps |
| 11 | The same, searching harder and re-pricing everything from the commands its first pass produced — slowest, smallest output |

`EncoderConfig::default()` is quality 11, which mirrors the reference encoder's
default and is far slower than most callers want. For online compression, say
so.

## Usage

```rust
use mbrotli::{Compressor, EncoderConfig, Quality};

let mut encoder = Compressor::new(EncoderConfig::default().with_quality(Quality::Q5))?;
let payload = "brotli ".repeat(1000);

let compressed = encoder.compress(payload.as_bytes())?;
```

Compressing more than one thing? Append into a buffer you own. Both the
encoder's workspace and the destination's capacity are reused, so a warm
compressor writing into a destination that is already big enough allocates
nothing at all:

```rust
let mut output = Vec::new();
for payload in payloads {
    output.clear();
    encoder.compress_into(payload, &mut output)?;
}
```

Into a caller-owned slice, sized so it always fits:

```rust
let bound = Compressor::max_compressed_size(payload.len())?;
let mut buffer = vec![0u8; bound];
let written = encoder.compress_to_slice(payload.as_bytes(), &mut buffer)?;
```

Streaming through a `Write`. The stream is only terminated by `finish`, because
`Write` has no closing hook and a meta-block boundary need not land on a byte
boundary:

```rust
use mbrotli::io::FinishError;
use mbrotli::{InputSize, StreamConfig};
use std::io::Write;

let stream = StreamConfig::from(InputSize::Exact(payload.len() as u64));
let mut sink = encoder.writer(Vec::new(), stream)?;
for chunk in payload.as_bytes().chunks(512) {
    sink.write_all(chunk)?;
}
let streamed = sink.finish().map_err(FinishError::into_error)?;

assert_eq!(streamed, compressed);
```

That last assertion holds because the stream *declared its size*. Qualities four
and five choose their match finder from how much input is coming, so a stream
that does not say produces different — equally valid — bytes. `reader` is the
pull-shaped counterpart, and `start` is the state machine both are built on.

The whole thing runs as [`examples/compress.rs`](examples/compress.rs):

```sh
cargo run --example compress
```

## Dictionaries

```rust
use mbrotli::dictionary::DictionaryBuilder;

let dictionary = DictionaryBuilder::new()
    .add_prefix(&b"HTTP/1.1 200 OK\r\nContent-Type: "[..])
    .build()?;

let compressed = encoder.compress_with_dictionary(&dictionary, payload)?;
```

Preparing is the expensive half and compressing is the cheap half, which is why
the two are separate types. Below quality five no match finder can consult a
dictionary, and one handed to such a compressor is refused rather than ignored:
a stream compressed without the dictionary it was given decodes perfectly well,
which is what would make the mistake invisible.

## API

| Item | Role |
| --- | --- |
| `EncoderConfig` | Everything stable across streams: quality, window, block size, mode, distance layout, literal contexts |
| `Compressor` | A reusable encoder and the workspace it owns |
| `CompressorBuilder` | Chooses the retention policy and, for testing, the backend |
| `StreamConfig` | What one stream knows about itself: `InputSize`, stream offset |
| `EncoderSession` | One incremental stream: `process`, `Operation`, `Progress`, `EncoderStatus` |
| `EncoderReader` / `EncoderWriter` | Adapters over a session |
| `PreparedDictionary` / `DictionaryBuilder` | Immutable RFC 9841 prefix dictionaries |
| `Quality`, `Window`, `BlockSize`, `CompressionMode`, `DistanceParams`, `LiteralContextMode` | Configuration values that cannot hold what the format cannot express |
| `ConfigError`, `DictionaryError`, `EncodeError`, `SizeOverflow` | One error type per domain |
| `RetentionPolicy` | What a compressor keeps between operations |

Everything below that surface is private. No encoder internal, SIMD type or FFI
detail escapes the public API.

The API was redesigned before the first release;
[`docs/pre_release_api_redesign.md`](docs/pre_release_api_redesign.md) maps the
old names onto the new ones.

## Performance

Compressed size is **exactly identical** to the reference encoder, so speed
comparisons are like-for-like by construction.

The benchmark harness in [`benches/compress.rs`](benches/compress.rs) compares
this crate with the same pinned C encoder the output is checked against, across
the shapes a stateful encoder makes genuinely different: `cold`, `reused`,
`presized`, `tiny`, `streaming`, `flush` and `dictionary`.

```sh
cargo bench --bench compress
```

The published figures in
[`docs/all_qualities_benchmarks.md`](docs/all_qualities_benchmarks.md) were
measured against the **previous** API and its one-shot/pre-sized shapes, on an
Apple M5 Pro. They are still a fair picture of where the encoders stand — the
encoders themselves did not change — but they predate both the new shapes and
the workspace-by-default model, and have not been re-run. Quality 1 was ahead of
the reference and quality 0 at parity; the greedy qualities ran at roughly 0.75×
to 0.80×, and short inputs worse still, because this crate pays initialisation
costs the reference skips. Making reuse the default addresses part of that
directly; the rest is match-finder work that has not been done yet. Where the
time goes and what would close the gap are in
[`docs/q3_q5_benchmarks.md`](docs/q3_q5_benchmarks.md).

### Caller-scheduled threaded compression

`mbrotli::compressor::parallel` compresses fixed independent segments into one
Brotli stream. The caller schedules its `Send` tasks with scoped threads, Rayon,
or another executor. Memory staging has an explicit worst-case limit; directory
staging supports file-scale inputs without holding the full payload in RAM.
Parallel bytes are deterministic across task counts, but differ from serial bytes.

```rust
use mbrotli::{EncoderConfig, Quality};
use mbrotli::compressor::parallel::{BatchConfig, ParallelCompressor, ParallelConfig, TaskCount};
let mut encoder = ParallelCompressor::new(
    EncoderConfig::default().with_quality(Quality::Q5), ParallelConfig::default())?;
let mut batch = encoder.prepare_slice(input, BatchConfig::memory(TaskCount::try_from(4)?, 256 << 20))?;
let tasks = batch.take_tasks()?;
std::thread::scope(|scope| {
    for task in tasks { scope.spawn(move || task.run()); }
});
let mut output = Vec::new();
batch.finish_into(&mut output)?;
```

Files use `batch.finish_to_writer(file)`; the caller controls creation, flushing,
and publication. For a complete example, run
`cargo run --release --example parallel -- INPUT OUTPUT`.
See [the architecture and current release gates](architecture/parallel-compression.md).
