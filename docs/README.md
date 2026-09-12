# User guide

[Quick start](../README.md#quick-start) · [API reference](https://docs.rs/mbrotli)

| Task | Read |
| --- | --- |
| Decode and set resource limits | [Decompression](#decompression) |
| Choose compression settings | [Configuration](#configuration) |
| Control allocations | [Output buffers](#output-buffers) and [reuse](#reusing-memory) |
| Connect synchronous I/O or drive a session | [Streaming](#streaming-and-completion) |
| Share a dictionary or use extended formats | [Dictionaries](dictionaries.md) |
| Write or decode framing containers | [Framing containers](dictionaries.md#framing-containers) |
| Split a job across workers | [Parallel compression](parallel.md) |
| Compare libraries | [Benchmarks](benchmarks/README.md) |

## Decompression

`Decompressor` owns reusable state. `decompress` returns a Vec,
`decompress_into` appends atomically, and `decompress_to_slice` writes into
caller storage. Sessions report consumed/produced counts; `reader` and `writer`
adapt synchronous I/O.

The default accepts one complete raw member and extended window declarations
up to 62 bits. Numeric budgets are unlimited. Configure them for your application:

```rust
use mbrotli::{DecodeLimits, DecoderConfig, Decompressor, WindowLimit};

fn decode_payload(input: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let limits = DecodeLimits::default()
        .with_max_input_bytes(Some(1 << 20))
        .with_max_output_bytes(Some(8 << 20))
        .with_max_workspace_bytes(Some(32 << 20));
    let config = DecoderConfig::default()
        .with_window_limit(WindowLimit::standard(24)?)
        .with_limits(limits);
    let mut decoder = Decompressor::new(config)?;
    Ok(decoder.decompress(input)?)
}
```

The workspace budget excludes caller output and borrowed dictionaries; it is
not a process-memory limit. `MemberMode::Concatenated` accepts successive raw
members. `DecodeDictionary` prepares decoding data without encoder indexes;
`PreparedDictionary` can also be borrowed. RAW dictionaries work in all profiles;
serialized/custom dictionaries require `experimental`.

See [decoder mechanics](../architecture/decompressor.md) and
[compatibility limits](../architecture/decompressor-compatibility.md).

## Configuration

`EncoderConfig` holds settings shared by successive streams:

| Setting | Type | Behavior |
| --- | --- | --- |
| Quality | `Quality` | 0–11; default 11 |
| Window | `Window` | `Window::standard(bits)` for 10–24 bits; `Window::large(bits)` for 10–62 bits |
| Input block size | `BlockSize` / `BlockBits` | Automatic or explicit 16–24 bits |
| Data mode | `CompressionMode` | Generic, text, or font |
| Distance coding | `DistanceParams` | Validated postfix bits and direct distance groups |
| Literal contexts | `LiteralContextMode` | Controls literal context modelling |

Validated values reject out-of-range inputs. `Compressor::new` and
`reconfigure` check combinations, including Large Window's quality minimum.
See [extended formats](dictionaries.md) for support limits.

`StreamConfig` carries the declared input size and logical stream offset.
Its default is unknown size and offset zero. For a complete known input,
use `StreamConfig::from(InputSize::Exact(input.len() as u64))`.

## Output buffers

`compress` returns a new `Vec<u8>`. `compress_into` appends to the supplied
vector and returns the range containing the new stream; existing bytes remain
intact. On failure, the vector's length rolls back to its original value.

`compress_to_slice` returns the number of bytes written. Allocate a conservative
buffer using `Compressor::max_compressed_size`:

```rust
use mbrotli::{Compressor, EncoderConfig, Quality};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = b"data to compress";
    let mut encoder = Compressor::new(
        EncoderConfig::default().with_quality(Quality::Q5),
    )?;
    let bound = Compressor::max_compressed_size(input.len())?;
    let mut output = vec![0; bound];
    let written = encoder.compress_to_slice(input, &mut output)?;
    output.truncate(written);
    assert_eq!(output, encoder.compress(input)?);
    Ok(())
}
```

A slice exactly as long as the stream also suffices. An undersized slice returns
an error and may contain a partial prefix; retry the complete operation with a
larger slice. Capacity does not select a different encoding.

## Reusing memory

A compressor retains one encoder workspace. Compatible calls reset its logical
state and reuse its allocations. Incompatible resolved settings can rebuild that
workspace. Destination capacity is managed separately by the caller.

| Retention policy | Effect |
| --- | --- |
| `Aggressive` (default) | Keeps allocated buffers between operations |
| `CurrentConfig` | Releases incompatible storage when configuration changes |
| `Bounded { max_bytes }` | Releases retained storage when its accounting exceeds the ceiling |
| `ReleaseAll` | Releases storage after each operation |

Set the policy with `Compressor::builder(config).with_retention(policy).build()`.
Use `trim(policy)` for immediate cleanup. `retained_bytes()` reports owned
heap allocation sizes; it excludes caller buffers, shared dictionaries, and
allocator overhead. A retention ceiling is not a peak process-memory limit.

For simultaneous independent streams, use one compressor per worker.
`fork_empty()` copies configuration, backend, and retention settings without
copying the workspace. For one stream split across workers, see
[parallel compression](parallel.md).

## Streaming and completion

`reader(source, stream)` consumes source bytes and yields transformed bytes
through `Read`. `writer(sink, stream)` accepts source bytes through `Write`
and delivers transformed bytes to the sink. Compression takes plain input;
decompression takes compressed input. Both adapter families require std APIs.

```rust
use mbrotli::{Compressor, EncoderConfig, InputSize, Quality};
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let payload = b"streamed payload";
    let mut encoder = Compressor::new(
        EncoderConfig::default().with_quality(Quality::Q5),
    )?;
    let stream = InputSize::Exact(payload.len() as u64).into();
    let mut writer = encoder.writer(Vec::new(), stream)?;
    writer.write_all(payload)?;
    let compressed = writer.finish().map_err(mbrotli::io::FinishError::into_error)?;
    assert!(!compressed.is_empty());
    Ok(())
}
```

For a decoder reader, pass compressed input and `DecodeStreamConfig::default()`,
then read the restored bytes to EOF. `into_parts()` recovers any reader read-ahead.
A decoder writer must also be finished: finalization reports incomplete or invalid
input. In a direct decoder session, declare final input with `DecodeOperation::Finish`.


A writer borrows the compressor and owns its sink. Use `write_all` for input,
`flush` for an intermediate decoding boundary, and `finish` to terminate the
stream. Frequent flushes add block boundaries and can increase output size.

`try_finish(&mut self)` supports retrying finalization after an I/O error.
Consuming `finish` returns the sink on success; `FinishError` retains the writer
on failure. Use `into_parts()` to recover both the error and writer for retry.
`into_error()` discards the writer.

The writer retains pending output across short writes and sink errors. An input
write can succeed after accepting bytes even if delivery then fails; that
delivery error is reported on a subsequent drain. Continue according to the
returned byte counts rather than replaying accepted input. Dropping a writer
performs no finalization.

A reader owns its input source and borrows the compressor. Read until EOF to
receive the complete compressed stream. Dropping it early abandons that stream.

For direct buffer control, `start` creates an `EncoderSession`.
Call `process` with input, output, and an `Operation`:

| Operation/status | Meaning |
| --- | --- |
| `Operation::Process` | Accept input without ending the stream |
| `Operation::Flush` | Emit accepted input and a decoding boundary |
| `Operation::Finish` | End the stream after the supplied input |
| `EncoderStatus::NeedsInput` | Supply more input or an explicit flush/finish |
| `EncoderStatus::NeedsOutput` | Supply output space and continue with unconsumed input |
| `EncoderStatus::Finished` | All final output has been delivered |

Use `Progress` to advance by the reported consumed and written counts.
Finish can require several calls when output buffers are small. Sessions retain
their own pending data and do not retain the caller's slices.

A dropped unfinished session invalidates partial encoder state, allowing a fresh
stream on the next call. A session discarded with `mem::forget` requires
`Compressor::recover()` before reuse.

## Output compatibility

Serial API identity requires the same encoder configuration, dictionary,
declared size, flush boundaries, and offset. For one-shot equivalence, declare
the exact input size, keep offset zero, and avoid explicit flushes. Chunk size,
destination shape, workspace reuse, and SIMD selection do not change bytes.
Unknown size can change match-finder selection.

The ordinary encoder differential uses the pinned C encoder's streaming API
with matching settings and block scheduling. C's native one-shot API can rewrite
empty or expanded output, and its fast streaming APIs can emit fragments at
caller chunk boundaries. Those schedules are not interchangeable byte oracles.

## Errors and further reading

`ConfigError` covers configuration, `DictionaryError` covers dictionary
preparation, `EncodeError` covers compression, and `SizeOverflow` covers
size-bound arithmetic. I/O adapters return `std::io::Error`; writer finalization
preserves the writer in `FinishError`.

- [Dictionaries and extended formats](dictionaries.md)
- [Parallel compression](parallel.md)
- [Benchmark results by compression quality](benchmarks/README.md)
- [Running benchmarks and profiling](benchmarking.md)
- [Compatibility and validation](correctness.md)
- [Development checks](development.md)
- [Compressor mechanics](../architecture/compressor.md)
