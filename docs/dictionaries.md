# Dictionaries and extended formats

## Prefix dictionaries

A `PreparedDictionary` owns immutable dictionary bytes and search indexes.
Prepare it once and borrow it from multiple compressors, including across
threads. Prefix compression supports qualities 5–11.

```rust
use mbrotli::dictionary::DictionaryBuilder;
use mbrotli::{Compressor, EncoderConfig, Quality};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dictionary = DictionaryBuilder::new()
        .add_prefix(&b"HTTP/1.1 200 OK\r\nContent-Type: "[..])
        .build()?;
    let mut encoder = Compressor::new(
        EncoderConfig::default().with_quality(Quality::Q5),
    )?;
    let input = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n";
    let compressed = encoder.compress_with_dictionary(&dictionary, input)?;
    assert!(!compressed.is_empty());
    Ok(())
}
```

Attachment order is significant. The decoder must receive matching dictionary
bytes in the same order; a raw Brotli stream does not embed those external bytes
or resolve dictionary identifiers.

Dictionary overloads are available for vector, slice, session, reader, and writer
output. Passing a dictionary below quality 5 returns
`EncodeError::DictionaryUnsupportedForQuality`.

`DictionaryBuilder::with_limits` accepts `DictionaryLimits` to bound source
bytes, prefix bytes, attachment count, and prepared storage. Preparation returns
either a complete dictionary or an error. Inspect `source_bytes()`,
`attachment_count()`, and `retained_bytes()` for its stored sizes.

## Large Window

Use `Window::large(bits)` in `EncoderConfig` to select the extended header.
Values range from 10 to 62, and compressor construction requires quality 3–11.
`Window::standard(bits)` selects ordinary Brotli with 10–24 bits.

The declared window controls the header. The encoder retains at most 30 bits
of history, even with a wider declaration. Consumers must support the extended
format. The repository's C decoder checks original streams through 30 bits;
wider declarations have header and payload checks but no independent end-to-end
decoder validation here.

## Feature flags

| Feature | Surface |
| --- | --- |
| `experimental` | Serialized shared dictionaries, custom static encoding, nonzero stream offsets, and framing |
| `diagnostics` | `PreparedDictionary::longest_match` prefix coverage diagnostic |
| `hotpath`, `hotpath-cpu`, `hotpath-alloc` | Optional profiling instrumentation |

The experimental API may change in a patch release.

## Serialized and custom static dictionaries

With `experimental`, `SerializedDictionary` parses and writes shared dictionary
descriptions containing an LZ77 prefix, word lists, transform lists, combinations,
and a 64-entry context map. `DictionaryBuilder::add_serialized` prepares that
description for compression at qualities 5–11.

Parsing validates structure and rejects trailing bytes. Serialization emits a
canonical representation. Preparation applies limits to the owned description,
prefix indexes, transformed words, static entries, and temporary storage.
Use the additional serialized, word, transform, combination, and expansion
ceilings in `DictionaryLimits` when processing external descriptions.

Custom search supports transformed words and context combinations. Tests require
Rust backend/API identity and independent C decoding. Extended search does not
have a blanket byte-identity guarantee with C's experimental encoder.

See [serialized format mechanics](../architecture/serialized-dictionary.md) and
[custom dictionary encoding](../architecture/rfc9841-encoding.md).

## Headerless continuations

With `experimental`, `StreamConfig::with_stream_offset` accepts nonzero
offsets at qualities 2–11. Logical positions, including new input, must fit in
63 bits. A continuation has no stream header and is not independently decodable.

The caller joins it after a compatible stream's byte-aligned flush boundary.
The encoder uses a restart prefix and does not invent prior history from the
offset. Use [parallel compression](parallel.md) for the API that plans and
assembles independent parts of one input.

## Framing containers

`Compressor::framed_writer` creates an experimental Shared Brotli container
writer over a non-seekable `Write` sink. It supports compressed and uncompressed
resources, dictionary references, metadata, padding, repeated metadata, a central
directory, and a footer.

Start a resource with `resource`, `resource_with_dictionary`, or
`uncompressed_resource`. Write its input, call the resource's `try_finish`,
then drop that borrow before starting another resource. Finish the container
explicitly. Resource and container finalization retain pending bytes for I/O
retries; dropping either performs no I/O. Dropping an unfinished resource
abandons the container.

The caller supplies dictionary IDs, matching dictionary references, and optional
checksums. The writer does not calculate hashes or fetch dictionaries.
`FramingConfig` bounds chunk input, metadata, framing storage, resources, and
chunk count. The single-resource profile has no metadata or directory.

The repository has no container decoder. Tests check the container structure
independently and decode compressed payloads with C.
See [framing mechanics and supported forms](../architecture/framing.md).
