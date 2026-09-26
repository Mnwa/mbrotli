# Continuous integration

Workflow definitions live in [`.github/workflows/`](../.github/workflows/).
All checkouts include vendored submodules. Local commands are in
[development](../docs/development.md).

## Triggers and checks

| Workflow | Trigger | Scope |
| --- | --- | --- |
| `ci.yml` | Push to master, pull request | Formatting, Clippy, docs, packaging, API compatibility, feature checks, tests and AFL replay |
| `ci-coverage.yml` | Manual | Std and no_std function coverage; 100% gate and HTML report |
| `ci-benchmarks.yml` | Manual | Criterion validation and timing on Linux x86-64 and ARM64 |
| `ci-fuzz.yml` | Every tag, manual | Bounded AFL campaigns in stable and experimental profiles |
| `ci-miri.yml` | Every tag, manual | Retained-storage checks and pure Rust decoder goldens |
| `ci-sanitizer.yml` | Every tag, manual | AddressSanitizer integration tests |
| `ci-decompressor-heavy.yml` | `v*` tag, manual | Actual 25–30-bit history and counters beyond 4 GiB in four decoder profiles |
| `release.yml` | Manual, from a `v*` tag | Publishes `mbrotli` to crates.io through trusted publishing |

```mermaid
flowchart TD
    Branch[master push or pull request] --> Routine[ci.yml]
    Routine --> Build[lint, docs, package and semver]
    Routine --> Features[std, no_std and isolated codec consumers]
    Routine --> Tests[platform tests and AFL regression replay]
    Tag[any pushed tag] --> HeavyTools[fuzz, Miri and ASan]
    Release[v* tag] --> Decoder[ignored heavy decoder tests]
    Manual[workflow_dispatch] --> HeavyTools
    Manual --> Decoder
    Manual --> Bench[benchmarks]
    Manual --> Coverage[std tests plus no_std overrides]
    Manual --> Publish[release.yml: publish mbrotli]
    Coverage --> Gate[100% function coverage and HTML artifact]
```

Routine tests cover Linux x86-64, Linux ARM64 and macOS, with an MSRV 1.89 job.
All-features builds activate `no_std`; separate std checks retain I/O, parallel
and framing coverage. Alloc-only jobs also compile `thumbv7em-none-eabi`.
Consumer scripts check available imports and imports that must fail.

The semver job compares the default `mbrotli` API with the latest published
release. Experimental APIs and the development-only C FFI crate are outside
that gate. A violation requires a sufficient package version bump.

## Release publishing

`release.yml` runs only on manual dispatch and refuses any ref that is not a
`v*` tag. It checks that the tag equals `v` plus the `mbrotli` package version,
then exchanges the job's GitHub OIDC token for a short-lived crates.io token
with `rust-lang/crates-io-auth-action` and runs `cargo publish --package
mbrotli`, which builds the packaged crate before uploading it. No registry
token is stored in the repository. The job runs in the `release` environment,
which must match the trusted publisher configured on crates.io. The
development-only `mbrotli-ffi` and `google-brotli-ffi` crates are not published.

```mermaid
sequenceDiagram
    actor Maintainer
    participant GH as release.yml
    participant CIO as crates.io
    Maintainer->>GH: workflow_dispatch on a v* tag
    GH->>GH: fail unless ref is a v* tag
    GH->>GH: fail unless tag == v + mbrotli version
    GH->>CIO: OIDC token (id-token: write)
    CIO-->>GH: short-lived publish token
    GH->>CIO: cargo publish --package mbrotli
```

## AFL execution and artifacts

AFL jobs cache Cargo downloads/tools but disable target caching: instrumented
builds can use CPU-specific instructions. Each job force-installs the pinned
`cargo-afl` and runs `cargo afl config --build --force` to restore its sources
and runtime for the current compiler.

Campaign runners configure direct core dumps before starting AFL. Target-specific
seed selection uses generated corpora or committed lifecycle, framing and parallel
regressions. Base decoder targets run in both feature profiles; custom targets
require `experimental`.

The experimental matrix includes `framed_decode`, `framed_roundtrip` and `framed_encode`, each
using its committed `regressions/TARGET` seeds. The arbitrary-byte framed decoder
also uses `dictionaries/framed.dict`. Campaigns run for ten minutes, with a
five-second per-input timeout for decoder targets and a one-second default for
other targets.

```mermaid
flowchart LR
    Matrix[experimental framed targets] --> Seeds[committed regression seeds]
    Seeds --> Parser[framed_decode with framing dictionary]
    Seeds --> Roundtrip[framed_roundtrip]
    Seeds --> Encode[framed_encode: native and I/O schedule agreement]
    Parser --> Campaign[ten-minute AFL campaign]
    Roundtrip --> Campaign
    Encode --> Campaign
    Campaign --> Check[fail on saved crashes or hangs]
    Campaign --> Archive[upload archived findings even on failure]
```

Saved crashes or hangs fail the job. Findings are archived as tar.gz and uploaded
when present, including after failures; this preserves AFL filenames with colons.
Regression replay does not start a campaign or require host core-dump configuration.

## Coverage and limitations

Coverage cleans workspace profiles first, then accumulates a std workspace run
with explicit experimental, diagnostics and profiling features plus a no_std
unit/API run. Both use test optimization level 1 and `CARGO_INCREMENTAL=1` to keep
coverage code generation consistent. The report enforces 100% function coverage;
HTML rendering and upload run even if tests or the gate fail.

There is no scheduled workflow, and publishing never starts on its own: a
pushed tag runs checks only. Each manual dispatch starts only its selected
workflow. Ordinary branches do not run heavy tools; coverage and benchmarks
always require explicit dispatch. Bounded fuzz campaigns and shared-runner timings
provide limited evidence, not exhaustive verification or stable speed guarantees.
