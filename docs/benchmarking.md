# Benchmarks and profiling

Criterion benchmarks compare Rust compression with the pinned C implementation
in the `google-brotli-ffi` workspace crate. Each harness validates output before
timing and records compressed sizes alongside throughput.

Initialize the corpus submodule from the repository root:

```sh
git submodule update --init --recursive
```

## Serial APIs

```sh
cargo bench --bench compress --locked
```

| Group | Measurement |
| --- | --- |
| `cold` | Compressor construction and encoding |
| `reused` | Successive operations with retained workspace |
| `presized` | Encoding into a preallocated destination |
| `tiny` | Short inputs and setup costs |
| `streaming` | Reader, writer, and session APIs |
| `flush` | Explicit intermediate flushes |
| `dictionary` | Attached prefix compression |
| `universal` | Empty and incompressible inputs under the serial byte-identity contract |

The serial C oracle uses matching streaming settings. The streaming adapter
normalizes fast-quality block scheduling; its staging costs are charged to C.
Native C one-shot output can differ and is not an interchangeable baseline.

## Parallel and experimental APIs

```sh
cargo bench --bench parallel --locked
cargo bench --bench track_b --features experimental --locked
```

The parallel harness compares task counts, source adapters, and serial C/Rust
compression. Parallel and serial streams have different history policies;
their sizes must be reported separately. The experimental harness covers custom
dictionaries, framing, and metadata. C framing comparisons wrap C-compressed
payloads in an independently constructed container envelope; C has no container
writer API.

Use Criterion's test mode to run validation without collecting timing samples:

```sh
cargo bench --bench compress --locked -- --test
cargo bench --bench parallel --locked -- --test
cargo bench --bench track_b --features experimental --locked -- --test
```

## Profiling

The optional `hotpath` features instrument release builds:

```sh
cargo run --release --features hotpath-cpu --example profile_compressor -- brotli-ffi/vendor/brotli/tests/testdata/alice29.txt
cargo run --release --features hotpath-alloc --example profile_compressor -- brotli-ffi/vendor/brotli/tests/testdata/alice29.txt
```

Keep CPU and allocation profiling separate from throughput measurements.

## Reading results

Compare identical input bytes, quality, window, dictionary, declared size, and
flush schedule. Record the command, revision, compiler, target, CPU, corpus,
API mode, compressed sizes, and timing intervals. Include allocation and setup
costs only when the named benchmark measures them.

Criterion reports live under `target/criterion/`. Generated reports and profiles
are local artifacts. Short runs and shared CI hosts provide limited timing
evidence; they do not establish a general speed claim across qualities or
machines.
