# Pre-release compressor API redesign

`mbrotli` is unreleased, so the compressor API was redesigned rather than
extended. This document is the before/after map for contributors. It is
informational: nothing here argues for keeping an old name, and no compatibility
shim was added for any of them.

The current contract prioritizes universal Rust API byte identity over native
C one-shot identity. Empty inputs now keep the same header as sessions, and
one-shot calls no longer rewrite expanded output or use a different stream to
fit a short slice. Differential tests use Google Brotli v1.2.0 (`028fb5a`)
streaming with equivalent settings; decoder compatibility remains required.

## Why

The old API separated a `Copy` `Compressor` holding a SIMD level, a
`CompressParams` value passed to every call, and an *optional*
`CompressWorkspace` the caller could supply to get allocation reuse. That made
the allocation-heavy path the simplest one to write and the fast path an
advanced variant. It also put per-call values — the size hint above all — into a
type named for per-encoder settings, so one field meant different things
depending on which entry point read it.

The redesign inverts that. A `Compressor` is stateful and owns its workspace, so
reuse is what ordinary code gets; `EncoderConfig` holds only what is stable
across streams; and what one stream knows about itself moved to `StreamConfig`.

## Type map

| Removed | Replacement |
| --- | --- |
| `Brotli` | gone; backend detection happens in `Compressor::new`, and `CompressorBuilder::with_backend` pins an opaque, host-validated `Backend` |
| `Compressor` (`Copy`, stateless) | `Compressor` (stateful, owns the workspace, `&mut self` methods) |
| `CompressParams` | `EncoderConfig` for settings, `StreamConfig` for what one stream knows |
| `CompressWorkspace` | gone; the compressor *is* the workspace |
| `QualityLevel` (enum) | `Quality` (validated newtype over `0..=11`) |
| `WindowBits` | `Window` plus `WindowEncoding` |
| `BlockBits` (`TryFrom<usize>`) | `BlockBits` (`TryFrom<u8>`) inside `BlockSize` |
| `CompressMode` | `CompressionMode` |
| `DistanceCodes` | `DistanceParams` (`Auto` or a validated `Explicit`) |
| `CompressParams::with_literal_context_modeling(bool)` | `LiteralContextMode` |
| `BrotliCompressError` | `ConfigError`, `DictionaryError`, `EncodeError`, `SizeOverflow` |
| `ParseQualityLevelError`, `ParseWindowBitsError`, `ParseBlockBitsError`, `ParseDistanceCodesError` | `ConfigError` |
| `SharedContext`, `SharedContextBuilder` | `PreparedDictionary`, `DictionaryBuilder` |
| `SharedContextLimits` | `DictionaryLimits` |
| `SharedBrotliError` | `ConfigError::LargeWindowUnsupportedForQuality`, `DictionaryError`, `EncodeError::DictionaryUnsupportedForQuality` |
| `PrefixMatch` | `PrefixMatch`, behind the `diagnostics` feature |
| `CompressorReader`, `CompressorWriter` | `EncoderReader`, `EncoderWriter` |

## Method map

| Removed | Replacement |
| --- | --- |
| `Brotli::default().compressor()` | `Compressor::new(config)?` |
| `compressor.compress(params, src)` | `encoder.compress(src)?` |
| `compressor.compress_with(&mut ws, params, src)` | `encoder.compress(src)?` — reuse is the default |
| — | `encoder.compress_into(src, &mut dst)?`, the primary repeated entry point |
| `compressor.compress_to_slice(params, src, dst)` | `encoder.compress_to_slice(src, dst)?` |
| `compressor.compress_to_slice_with(&mut ws, ...)` | `encoder.compress_to_slice(src, dst)?` |
| `compressor.calculate_bound(&params, n)` | `Compressor::max_compressed_size(n)?` |
| `compressor.calculate_shared_bound(...)` | `Compressor::max_compressed_size(n)?` |
| `compressor.compress_writer(params, w)` | `encoder.writer(w, stream)?` |
| `compressor.compress_reader(params, r)` | `encoder.reader(r, stream)?` |
| `compressor.shared_context_builder(quality)` | `DictionaryBuilder::new()` — no quality |
| `compressor.compress_shared(params, &mut ctx, src)` | `encoder.compress_with_dictionary(&dictionary, src)?` |
| `compressor.compress_shared_to_slice(...)` | `encoder.compress_with_dictionary_to_slice(...)?` |
| `compressor.longest_prefix_match(&ctx, input)` | `dictionary.longest_match(input)`, behind `diagnostics` |
| `CompressorWriter::finish() -> io::Result<W>` | `EncoderWriter::try_finish()` and `finish() -> Result<W, FinishError<Self>>` |
| — | `EncoderReader::into_parts()` |
| — | `Compressor::start`, `EncoderSession::process` |
| — | `Compressor::reconfigure`, `fork_empty`, `retained_bytes`, `trim`, `recover` |

## What moved, and what to write instead

### Reuse is the default

```rust
// Before
let compressor = Brotli::default().compressor();
let params = CompressParams::new(QualityLevel::Q5, WindowBits::DEFAULT);
let mut workspace = CompressWorkspace::default();
for input in inputs {
    let out = compressor.compress_with(&mut workspace, params, input)?;
}

// After
let mut encoder = Compressor::new(EncoderConfig::default().with_quality(Quality::Q5))?;
let mut output = Vec::new();
for input in inputs {
    output.clear();
    encoder.compress_into(input, &mut output)?;
}
```

### The size hint became the stream's business

`CompressParams::with_size_hint` did two different jobs. The one-shot entry
points substituted the true input length when it was absent; the streaming
adapters treated absence as zero. It is now explicit and lives on the operation:

- `Compressor::compress`, `compress_into` and `compress_to_slice` always declare
  the true source length, which is what `BrotliEncoderCompress` does.
- A session declares `StreamConfig::from(InputSize::Exact(n))` or leaves it
  `InputSize::Unknown`, which is what `BrotliEncoderCompressStream` leaves
  `BROTLI_PARAM_SIZE_HINT` at.

Declaring a size other than the true one — which the old API could do through the
one-shot path — is now expressible only through a session, because that is the
only place it means anything.

### A Large Window at a quality that cannot carry one is a configuration error

`SharedBrotliError::UnsupportedLargeWindow` was raised when a call ran. It is now
`ConfigError::LargeWindowUnsupportedForQuality`, raised by `Compressor::new` and
`reconfigure`, before any input is touched.

### A dictionary is immutable and quality-free

`SharedContext` took a maximum quality at construction and was handed to a call
by `&mut`. A `PreparedDictionary` takes no quality, is borrowed by `&`, and can
back any number of compressors at once — including across threads — with no lock,
because there is nothing mutable in it.

An empty dictionary is refused by `DictionaryBuilder::build` rather than behaving
like no dictionary at all.

### The writer is transactional

The old `CompressorWriter` advanced encoder state before `write_all` had
delivered the resulting block, so a partial sink write followed by an error left
no cursor for the unwritten suffix. `EncoderWriter` keeps compressed bytes in a
cursor-addressed buffer until the sink has actually taken them, drains before
accepting new input, and retries from exactly where a failing sink stopped.
`try_finish` is retryable, and `finish` hands the adapter back inside
`FinishError` rather than destroying it.

### One state machine underneath

`EncoderSession` is the incremental encoder. `EncoderReader` and `EncoderWriter`
are adapters over it, and the one-shot entry points share its parameter
resolution and its encoders. Where a session and the one-shot path differ, they
differ because the reference does: `BrotliEncoderCompress` answers an empty input
with a single byte and rewrites a stream that grew as uncompressed meta-blocks,
and `BrotliEncoderCompressStream` does neither.

## What did not change

- The underlying encoder kernels and format compatibility; outer one-shot
  rewrites were subsequently removed for cross-API identity.
- Every encoder under `src/compressor/core/`, apart from crate-internal
  signatures: a flush now carries the attached dictionary, and the retained
  workspace is reachable from the public layer.
- Safe Rust in `src/`, enforced by `#![cfg_attr(not(test), forbid(unsafe_code))]`.
- Determinism across SIMD backends.
