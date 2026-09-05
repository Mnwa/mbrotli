# Parallel compression

`mbrotli::compressor::parallel` divides a known input into fixed independent
segments and assembles them into one Brotli stream. It supports qualities 0–11
with standard windows. Dictionaries, Large Window, and framing are unsupported
by this API.

The caller runs the tasks using scoped threads, Rayon, or another executor.
The library does not create a thread pool.

## Compressing a slice

```rust
use mbrotli::compressor::parallel::{
    BatchConfig, ParallelCompressor, ParallelConfig, TaskCount,
};
use mbrotli::{EncoderConfig, Quality};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = vec![b'a'; 8 << 20];
    let mut encoder = ParallelCompressor::new(
        EncoderConfig::default().with_quality(Quality::Q5),
        ParallelConfig::default(),
    )?;
    let mut batch = encoder.prepare_slice(
        &input,
        BatchConfig::memory(TaskCount::try_from(4)?, 256 << 20),
    )?;
    let tasks = batch.take_tasks()?;
    std::thread::scope(|scope| {
        for task in tasks {
            scope.spawn(move || task.run());
        }
    });
    let mut output = Vec::new();
    batch.finish_into(&mut output)?;
    assert!(!output.is_empty());
    Ok(())
}
```

`take_tasks` transfers each task once. Every taken task must run or be dropped
so its completion can be reported. Finalization collects task results, validates
artifacts, and assembles them in input order. A blocking wait requires the
caller's executor to remain runnable.

## Segments and compressed size

Segment size defaults to 4 MiB and accepts 64 KiB–16 MiB. Effective task count
is at most the number of segments. For fixed input and segment settings, task
grouping and completion order do not change output bytes.

Independent segments do not share match history and use explicit distances.
Their compression ratio can be worse than serial compression, especially for
repetition across segment boundaries. Compare both output size and elapsed time.

Empty input uses serial encoding without tasks. A nonempty input below the
default 8 MiB parallel threshold uses a single serial task only when it also
fits in one segment.

## Sources and staging

| Input | Entry point |
| --- | --- |
| Borrowed slice | `prepare_slice` |
| Regular file | `prepare_source(FileSource::open(path)?, config)` |
| Owned seekable reader | `prepare_source(SeekSource::from(reader), config)` |
| Custom random-access source | `prepare_source(source, config)` with `RandomAccessSource` |

`FileSource` uses positional reads on Unix and Windows. `SeekSource` accepts
`Read + Seek + Send`, serializes seek/read operations, and lets compression run
concurrently. Sources must remain immutable for the batch lifetime; metadata
checks cannot detect every in-place mutation.

Owned sources and shared handles use the same generic entry point. For an
existing `Arc<FileSource>`, type inference can require
`prepare_source::<FileSource, _>(shared_file, config)`.

`BatchConfig::memory(tasks, max_bytes)` sets a staged-output budget. Planning
also accounts for worker and assembly storage. These estimates are not a process
RSS measurement. `BatchConfig::directory(tasks, directory)` stages output in
exclusive temporary files in an existing directory, keeping payload RAM
proportional to active workers and segment size. File cleanup is best effort
on drop.

## Output and cancellation

`finish_into` appends to a vector and restores its length on failure.
`finish_to_writer` consumes the batch and returns the destination with
statistics on success. On failure it preserves destination ownership and the
number of accepted bytes; the consumed batch cannot retry assembly. The caller
controls flushing, file durability, and publication.

Cancellation is cooperative around source reads and segment encoding. A running
codec call or blocking source read can delay it. Dropping a batch cancels work
without waiting for detached tasks. Worker retention is configurable and defaults
to retaining no workers.

The file example uses directory staging and creates a new output file:

```sh
cargo run --release --example parallel -- INPUT OUTPUT
```

See [parallel mechanics](../architecture/parallel-compression.md) for lifecycle,
resource accounting, source checks, and error propagation.
