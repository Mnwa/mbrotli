# User guide

`mbrotli` compresses bytes into Brotli streams. Start with the
[README example](../README.md#getting-started), then choose the output API
that fits your application.

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

A writer borrows the compressor and owns its sink. Use `write_all` for input,
`flush` for an intermediate decoding boundary, and `finish` to terminate the
stream. Frequent flushes add block boundaries and can increase output size.

`try_finish(&mut self)` supports retrying finalization after an I/O error.
Consuming `finish` returns the sink on success; `FinishError` retains the writer
on failure. Use `into_parts()` to recover both the error and writer for retry.
`into_error()`, as used by the README's in-memory example, discards the writer.

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
- [Benchmarks and profiling](benchmarking.md)
- [Development checks](development.md)
- [Compressor mechanics](../architecture/compressor.md)
