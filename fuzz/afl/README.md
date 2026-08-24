# AFL fuzz targets

Coverage-guided fuzzing for the quality 0, 1, 3, 4 and 5 encoders, following the
[Rust Fuzz Book AFL setup](https://rust-fuzz.github.io/book/afl/setup.html).

This package is deliberately excluded from the workspace so that AFL's
instrumentation never affects an ordinary `cargo test` or `cargo clippy` run at
the repository root. It is not exempt from the checks, though — see
[Required checks](#required-checks).

## Layout

| Path | Role |
| --- | --- |
| `src/lib.rs` | Input decoding, payload cap, backend enumeration, C encoder/decoder oracles. |
| `src/targets.rs` | The target bodies, one function each, with no AFL dependency. |
| `src/bin/` | Three line `afl::fuzz!` adapters around the bodies in `targets.rs`. |
| `tests/regressions.rs` | Replays the committed corpus through the same bodies. |
| `regressions/` | Committed boundary cases and minimised crash reproducers. |
| `seeds/` | Generated from the vendored Brotli test data; not committed. |
| `findings/` | AFL output; not committed. |

Target bodies live in `targets.rs` rather than in each `main`, so a minimised
crash reproduces identically under AFL, under `cargo afl test` and under a
debugger, without an instrumented binary.

## Setup

AFL needs a C compiler and `make`:

```sh
cargo install cargo-afl --locked
```

On macOS and Linux the shared-memory and crash-reporting settings need one
privileged tweak before the first run. Without it `cargo afl fuzz` and
`cargo afl cmin` fail with `shmget() failed`:

```sh
cargo afl system-config    # runs sudo; changes kernel shm and crash reporter settings
```

## Seed corpora

The seeds are Google Brotli's own test data, taken from the vendored submodule
at `brotli-ffi/vendor/brotli/tests/testdata`: Canterbury text, binary blobs,
already-compressed payloads, long zero runs and back-reference edge cases.
Materialise them before the first run — they are not committed, because the
submodule already carries them:

```sh
./prepare-seeds.sh
```

That produces four corpora:

- `seeds/generic` — the raw test data, for targets that fuzz the payload only.
- `seeds/params` — the same files behind a six byte parameter header, for
  targets that decode their settings from the start of the input.
- `seeds/large_window` — the parameter seeds behind one more byte, the declared
  RFC 9841 window the `large_window` target reads first.
- `seeds/shared_context` — the parameter seeds behind two more bytes, the
  attachment count and the limit squeeze the `shared_context` target reads
  first.

Then reduce them to a coverage-equivalent subset:

```sh
cargo afl build --release
./minimise-seeds.sh          # keeps the originals in seeds/*.raw
```

Minimisation is worth running: `prepare-seeds.sh` emits the same payload under
up to eight parameter headers and only a few of those reach distinct code, so
`cmin` removes most of the `params` corpus by file count. It is a reduction in
file count, not in bytes, because the large fixtures do carry coverage the
small ones miss and `cmin` keeps them. Per-iteration cost is bounded by
`MAX_PAYLOAD`, not by minimisation.
The script exports `AFL_NO_FORKSRV=1` for `cmin`, and must keep doing so:
`afl-cmin` measures coverage with `afl-showmap -I`, whose folder mode stalls
against a persistent-mode binary with a deferred forkserver — every input
blocks until the `-t` timeout expires, giving roughly six seconds per seed and
an empty output directory. Without the forkserver, showmap execs the target
once per input the way its single-file mode already does; the captured edge
sets are identical and the whole corpus takes under two seconds.

## Input model

Targets taking `seeds/params` read six header bytes:

| Byte | Meaning |
| --- | --- |
| 0 | quality, one of the five this crate implements |
| 1 | window size, `10 + value % 15`, spanning `WindowBits::MIN` to `MAX` |
| 2 | streaming chunk size, `1 << (value % 18)` |
| 3 | mode in the low two bits, literal context modelling in bit 2 |
| 4 | block size: zero leaves it to the encoder, otherwise `16 + value % 9` |
| 5 | distance layout: postfix bits in the low two bits, direct groups above |

The rest is the payload, and the size hint is pinned to its length so the
streaming and one-shot paths stay comparable. `parameter_parsing` reads two
header bytes instead and treats them as *numeric* quality and window values, so
it reaches the rejection paths the other targets can never construct.

Payloads are truncated to `MAX_PAYLOAD` (128 KiB). Input length stops adding
coverage quickly, and the multi-backend targets pay for every extra byte
several times per iteration; the cap roughly doubles to sextuples throughput
depending on the target. Large multi-fragment inputs are covered instead by
`tests/vendor_corpus.rs` at the repository root, which is not throughput bound.

## Targets

| Target | Input | Oracle |
| --- | --- | --- |
| `q0_roundtrip` | `seeds/generic` | no panic, size bound, C decoder round-trip |
| `q1_roundtrip` | `seeds/generic` | no panic, size bound, C decoder round-trip |
| `q3_roundtrip` | `seeds/generic` | no panic, size bound, C decoder round-trip |
| `q4_roundtrip` | `seeds/generic` | no panic, size bound, C decoder round-trip |
| `q5_roundtrip` | `seeds/generic` | no panic, size bound, C decoder round-trip |
| `params_roundtrip` | `seeds/params` | randomised legal settings, determinism, round-trip |
| `simd_equivalence` | `seeds/params` | every distinct host SIMD backend emits identical bytes |
| `differential_c` | `seeds/params` | byte identity with Google Brotli v1.2.0, configured identically |
| `streaming_equivalence` | `seeds/params` | writer and reader agree, round-trip |
| `output_capacity` | `seeds/params` | exact buffer accepted, short buffer reported |
| `parameter_parsing` | `seeds/params` | illegal settings rejected, unimplemented qualities reported not panicked |
| `large_window` | `seeds/large_window` | RFC 9841 window validation, refusal at qualities 0 and 1, determinism, backend identity, round-trip through the large-window C decoder |
| `shared_context` | `seeds/shared_context` | context preparation is a transaction, accessors agree with what was attached, a reported prefix match really matches, the distance mapping round-trips, the match does not depend on the compressor's backend, an empty context emits what `compress` emits, a non-empty one is refused not ignored |

## Building and running

```sh
cargo afl build --release
cargo afl fuzz -i seeds/generic -o findings/q0 -- target/release/q0_roundtrip
cargo afl fuzz -i seeds/params  -o findings/params -- target/release/differential_c
```

Resume an existing output directory with `-i -`.

No dictionary is used. The targets consume arbitrary payload bytes rather than
a token grammar, so a dictionary has nothing stable to contribute; the three
byte parameter header is small enough for the mutator to cover unaided.

## Parallel campaigns

One process per core, unique names, one shared output directory:

```sh
cargo afl fuzz -M main    -i seeds/params -o findings/params -- target/release/differential_c
cargo afl fuzz -S worker1 -i seeds/params -o findings/params -c - -- target/release/differential_c
cargo afl fuzz -S worker2 -i seeds/params -o findings/params -c - -- target/release/differential_c
```

`cargo afl fuzz` enables CmpLog by default. Keep it on at most one or two
workers and pass `-c -` to the rest, or drop its instrumentation entirely when
no worker will use it:

```sh
AFLRS_NO_CMPLOG=1 cargo afl build --release
```

Do not mix binaries built with different instrumentation in one output
directory without noting the topology in the campaign log.

Monitor every worker with:

```sh
cargo afl whatsup -s findings/params
```

## Triage

Minimise a crash and turn it into a deterministic regression test *before*
fixing anything:

```sh
cargo afl tmin -i findings/params/default/crashes/id:000000,... \
    -o regressions/differential_c/crash-short-description.bin \
    -- target/release/differential_c
cargo afl test          # must fail on the new input
```

Fix the bug, then run `cargo afl test` again; it must pass. See
`regressions/README.md`. Replay outside AFL at any time with:

```sh
RUST_BACKTRACE=1 ./target/release/differential_c < regressions/differential_c/crash-....bin
```

`findings/` is a local artifact and is not committed.

## Required checks

The repository checklist in `AGENTS.md` runs at the workspace root, which does
not reach this package. After changing anything here, also run, from
`fuzz/afl/`:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo afl test
```

`cargo afl test` rather than `cargo test`: the binaries link AFL's runtime, so
plain `cargo test` cannot link them even though the library and the regression
test themselves have no AFL dependency.

## Recorded environment and last campaign

Validated on cargo-afl 0.18.2 (AFL++ 4.40c), Rust edition 2024,
`aarch64-apple-darwin` (18 cores), `google-brotli-ffi` pinned to Brotli v1.2.0,
`fearless_simd` pinned to 0.7.0 with `force_support_fallback`. `Cargo.lock` is
committed so a campaign can be reproduced against the same dependency set.
`cargo afl system-config` had been run; without it `fuzz` and `cmin` fail at
`shmget()`.

### Before the greedy qualities

Sixty seconds per target, all eight targets of the time in parallel, minimised
corpora, no dictionary, CmpLog on (single instance each):

| Target | Execs | Execs/s | Stability | Bitmap | Crashes | Hangs |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `q0_roundtrip` | 93461 | 1557 | 99.91% | 10.06% | 0 | 0 |
| `q1_roundtrip` | 86473 | 1441 | 99.90% | 9.00% | 0 | 0 |
| `params_roundtrip` | 45434 | 757 | 99.95% | 17.59% | 0 | 0 |
| `simd_equivalence` | 47874 | 798 | 99.97% | 30.50% | 0 | 0 |
| `differential_c` | 59641 | 994 | 99.95% | 17.36% | 0 | 0 |
| `streaming_equivalence` | 44342 | 739 | 99.95% | 17.75% | 0 | 0 |
| `output_capacity` | 42062 | 701 | 99.95% | 18.36% | 0 | 0 |
| `parameter_parsing` | 14667 | 244 | 99.94% | 14.44% | 0 | 0 |

434k executions, nothing saved. Stability above 99.9% everywhere, which is
what the absence of mutable global state predicts; the residue is allocator
and detection noise, not harness state.

### After the greedy qualities

Thirty to forty-five seconds per target, run one at a time over the
*unminimised* `seeds/params` corpus (127 seeds), so the numbers below are new
corpus items and coverage rather than execution counts:

| Target | Seconds | New corpus items | Coverage | Crashes | Hangs |
| --- | ---: | ---: | ---: | ---: | ---: |
| `differential_c` | 45 | 650 | 21.65% | 0 | 0 |
| `simd_equivalence` | 30 | 736 | 33.54% | 0 | 0 |
| `streaming_equivalence` | 30 | 607 | 20.63% | 0 | 0 |
| `params_roundtrip` | 30 | 482 | 20.76% | 0 | 0 |
| `q5_roundtrip` | 30 | 35 | 7.01% | 0 | 0 |

Both are smoke runs, not real campaigns — under a minute finds only what is
very shallow.
