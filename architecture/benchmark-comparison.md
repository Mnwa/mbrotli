# Implementation comparison

## Boundaries

`benchmarks/comparison/` is an unpublished standalone Cargo workspace. Its lockfile
pins competitors without changing the library's dependency resolution. The
comparison library exposes `run` and `run_decoders`; private `core` owns corpus
generation, codec adapters, validation, sampling, and output. `core::decoder`
owns the decoder comparison described below. The root package excludes
this directory from publication. Library APIs and dispatch remain unchanged.

```mermaid
graph TD
    Bench[implementations main] --> Run[comparison run]
    Run --> Core[private core]
    Decoders[decoders main] --> DecodeRun[run_decoders]
    DecodeRun --> DecodeCore[private core::decoder]
    DecodeCore --> Core
    DecodeCore --> DecodeResults[separate criterion-decoders output]
    Core --> API[local mbrotli public API]
    Core --> C[google-brotli-ffi]
    Core --> Competitors[brotli, simd-brotli, burli]
    Core --> Criterion[isolated target/criterion]
    Criterion --> Export[report.py: complete matrix CSV]
    Export --> Plot[plot.py: median speed, size, paired overview bars]
    Export --> QualityDocs[quality_docs.py: validate and group recorded rows]
    QualityDocs --> Medians[per-dataset ratios to C, then median across all 8 datasets]
    Medians --> Order[qualities sorted by mbrotli median speed / C]
    Order --> Overview[generated median overview and 12 median SVGs]
    QualityDocs --> Rank[datasets sorted by mbrotli speed / fastest peer]
    Rank --> Pages[12 quality pages and 96 vertical dataset SVGs]
    Plot --> Medians
    Matched[separate 96-case before-after CSV] --> BeforeAfter[plot.py: ranked vertical speedup bars]
    Index[docs/benchmarks/README.md] --> Pages
    Index --> Overview
    Index --> Reports[dated run reports and raw data]
    Old[existing benches] --> OldResults[root target/criterion]
```

## Control and data flow

The driver creates eight deterministic corpora and validates 432 compressed
streams before timing. Four adapters cover q0–q11; Burli covers q0–q5. All use
generic mode, window 22, and a complete input with size hints where configurable.
No custom dictionaries or flushes are introduced. Each encoder selects its own
default SIMD path; the harness adds no CPU feature detection.

```mermaid
sequenceDiagram
    participant Main
    participant Core
    participant Encoder
    participant C as C decoder
    participant Timer as Criterion
    Main->>Core: run
    Core->>Core: construct fixed corpora
    loop every supported case
        Core->>Encoder: cold compress
        Encoder-->>Core: owned stream or error
        Core->>C: decode into bounded buffer
        C-->>Core: bytes and status
        Core->>Core: require original bytes; record length
    end
    Core->>Core: write sizes.csv
    loop selected cases and iterations
        Timer->>Encoder: construct, allocate, encode, dispose
        Encoder-->>Timer: black-box output or captured error
    end
    Timer-->>Main: estimates and summary
```

The C adapter owns an initialized destination sized by C's one-shot bound.
Unsafe calls receive disjoint slices and explicit writable capacities. The
decoder allocates at least one byte for empty-input validation. Unit tests cover
empty/tiny inputs, buffer boundaries, corruption, wrong expected content,
unknown adapters, and unsupported Burli quality. Criterion test mode exercises
the full matrix and driver.

Private typed errors and dependency errors propagate through `Result` to the
executable. Timed encoding errors are captured and returned after that Criterion
case. Preflight failure aborts before timing or replacement of `sizes.csv`.

`report.py` joins a unique named baseline with the size manifest by full case ID.
It checks throughput input lengths and rejects duplicate or incomplete records.
Its 432-row export includes latency confidence bounds, throughput, speed relative
to C, and output sizes. Existing 95%-of-C gates use only the original benchmarks.

`plot.py` uses the same complete-matrix validator and median helpers as
`quality_docs.py`. It writes three standalone SVG figures: speed medians, output
size medians, and a paired overview. For each quality and encoder, speed ratios
are C mean latency / encoder mean latency, and size ratios are encoder bytes /
C bytes for the same dataset. Each median uses all eight per-dataset ratios with
equal weight, including empty input. For eight values the median averages the
fourth and fifth sorted values. These are not medians of Criterion samples, nor
ratios of pooled times or bytes. The summary does not infer confidence bounds.
Qualities are ordered by descending mbrotli median speed / C, with numeric
quality order breaking ties. Burli is absent above q5, never represented as zero.
The filenames `throughput.svg`, `size.svg`, and `tradeoff.svg` are retained for
existing links, but all three now show normalized medians on vertical bars.

The optional matched before/after renderer validates 96 unique cases and shows
before mean / after mean per quality. It ranks corpus panels by median speedup
across qualities, retaining the declared corpus order on ties. This separate
run is never pooled with the five-encoder summary.

`quality_docs.py` reads one complete CSV and its environment record, validates
unique supported quality/dataset/encoder keys, finite positive timing bounds,
consistent input lengths, and window settings, then writes twelve Markdown
pages and a median overview under `docs/benchmarks/qualities/`. Each page starts
with median speed/size bars and a table, then shows all eight dataset charts.
The current pages and index use `docs/benchmarks/library-comparison.csv` and its
environment and run report; earlier comparison and optimization records retain
their original data. Each page links to its own run's measurement limitations.
Datasets are ordered by fastest peer mean latency / mbrotli mean latency,
descending, with declared corpus order breaking ties. The peer excludes mbrotli
so leads above 1× remain distinguishable. Exact tables and measurement setup
use Markdown details blocks. All 432 measurements remain accessible.

Every bar is vertical on a linear axis starting at zero, with consistent encoder
colors and labeled values. Dataset charts show mean latency for empty and tiny
input, throughput for larger inputs, and exact compressed bytes. Timing whiskers
retain recorded confidence bounds; throughput bounds invert latency bounds.
Empty input has no defined throughput or compressed fraction, but its relative
latency and output / C are defined and included in the medians. Rankings do not
claim significance or equal compressed size. Generation does not run encoders
or alter CSVs and environment records. Older report charts use their own CSV.

## Known gaps

- This suite measures cold serial compression. The existing harnesses retain
  streaming, workspace reuse, parallel, and experimental measurements.
- Equal numeric quality is not equal output size across encoders.
- `BrotliCompress` includes 4 KiB I/O adaptation; this compares complete APIs.
- The size manifest describes the latest preflight. Archive it with the named
  baseline; the exporter cannot detect mixed revisions with identical input sizes.
- Fixed encoder order and a shared host can bias timing. Sampling confidence
  intervals do not capture all environmental bias.

## Decoder comparison

The separate `decoders` Criterion entry point calls `run_decoders`; private
`core::decoder` owns the adapters, validation and timing. It shares only the
existing deterministic corpus generator and C encoder helper with `core`.
Neither codec's production API, state machine nor SIMD dispatch is changed.

```mermaid
flowchart TD
    Main[decoders benchmark main] --> Public[run_decoders]
    Public --> Core[private core::decoder]
    Corpora[core::corpora: 8 original datasets] --> Prepare
    Core --> Prepare[C encode once: q0-q11, generic, window 22]
    Prepare --> Streams[96 retained compressed streams]
    Streams --> Validate[4 decoders restore exact original bytes]
    Validate -->|any failure| Abort[return error before manifest or timing]
    Validate --> Manifest[target/criterion-decoders/sizes.csv: 384 rows]
    Manifest --> Timing[Criterion: cold decode of retained streams]
    Timing --> Results[target/criterion-decoders: named baseline]
    Results --> Export[report.py --decoding: full matrix and shared-size validation]
    Manifest --> Export
    Export --> CSV[384 rows: latency, bounds, restored throughput, input sizes]
    CSV --> Docs[decoder_docs.py: overview, 12 quality pages, 13 SVGs]
```

Every decoder receives exactly the same C-generated bytes for a corpus/quality.
Quality is a property of the source encoder, not a decoder parameter. Google C,
mbrotli, Rust `brotli` and Burli all cover q0–q11. `simd-brotli` is omitted because
its decompression API re-exports `brotli-decompressor`, as Rust `brotli` does.
The lockfile resolves 6.0.0 for Rust `brotli` and 5.0.3 for `simd-brotli`; only
the former is measured, and equal performance between those versions is not assumed.
Burli's encoder q5 ceiling does not restrict its decoder.

All 384 validations finish before the manifest is replaced or timing begins,
including filtered runs. The 96 compressed buffers remain alive and immutable
through the timed sweep. Preflight compares each restored payload with the
original; dependency failures propagate through `Result`, while private typed
errors identify C failures, unknown adapters, and mismatched corpus/quality.
Timed errors are captured and returned after the affected Criterion case.

The cold contract includes decoder construction, output/workspace allocation,
decoding and disposal. C uses `BrotliDecoderDecompress` into an initialized
allocation of the known output length (at least one byte for empty output).
The other adapters use `Decompressor::decompress`, `BrotliDecompress` into a new
Vec, and `burli::decompress`. C therefore knows the output capacity in advance;
Rust Brotli includes its native 4 KiB I/O adaptation. These are native API
comparisons, not identically allocated decoder-kernel measurements. Default
backend selection remains inside each library; the harness adds no SIMD dispatch.

Throughput uses restored byte counts. CSV `input_bytes` retains the encoder
suite's meaning of original corpus length, while `compressed_bytes` describes
the shared compressed input. Empty-input throughput is recorded as zero and
shown as undefined in tables; latency remains meaningful. Compression ratio
is recorded once per shared stream in each quality page; there is no decoder
size ranking. The exporter rejects incomplete matrices, unsupported keys,
inconsistent lengths/window/shared compressed sizes, and invalid timing bounds.
Reports rank source qualities by mbrotli median C-time/decoder-time ratio over
all eight equally weighted datasets; dataset panels rank mbrotli relative to
the fastest peer. They reuse the encoder suite's ratio and plotting helpers,
but retain separate source data, environment records and generated pages.

Boundary tests cover every decoder at every source quality on empty, tiny,
SIMD-width-adjacent and 4 KiB-adjacent lengths, malformed input, wrong restored
content and insufficient C output capacity. Criterion test mode executes the
complete matrix and the public driver. Python tests check export pairing,
invalid/missing records, q11 Burli inclusion, shared-input size consistency,
medians, generated pages and SVG validity.

Decoder-specific limits: only cold serial standard-window streams from the C
encoder are compared here. Reused state, slice APIs, streaming, custom
dictionaries and other stream producers remain outside this comparison; the
root `benches/decompress.rs` retains its separate API/backend measurements.
The latest manifest must be archived alongside its matching named baseline;
matching sizes alone do not prove source identity across different revisions.
