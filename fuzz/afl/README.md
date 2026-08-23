# AFL fuzz targets

Coverage-guided fuzzing for the quality 0 and quality 1 encoders, following the
[Rust Fuzz Book AFL setup](https://rust-fuzz.github.io/book/afl/setup.html).

This package is deliberately excluded from the workspace so that AFL's
instrumentation never affects an ordinary `cargo test` or `cargo clippy` run.

## Setup

AFL needs a C compiler and `make`:

```sh
cargo install cargo-afl
```

On macOS and Linux the shared-memory and crash-reporting settings usually need
one privileged tweak before the first run:

```sh
cargo afl system-config
```

## Seed corpora

The seeds are Google Brotli's own test data, taken from the vendored submodule
at `brotli-ffi/vendor/brotli/tests/testdata`: Canterbury text, binary blobs,
already-compressed payloads, long zero runs and back-reference edge cases.
Materialise them before the first run — they are not committed, because the
submodule already carries them:

```sh
fuzz/afl/prepare-seeds.sh
```

That produces two corpora:

- `seeds/generic` — the raw test data, for targets that fuzz the payload only.
- `seeds/params` — the same files behind a three byte header, for targets that
  decode the quality, window size and streaming chunk size from the input.

## Building

```sh
cd fuzz/afl
cargo afl build --release
```

## Running

```sh
cd fuzz/afl
cargo afl fuzz -i seeds/generic -o findings/q0 target/release/q0_roundtrip
```

Targets that decode their own parameters from the first three input bytes use
the `seeds/params` corpus instead:

```sh
cargo afl fuzz -i seeds/params -o findings/params target/release/params_roundtrip
```

## Targets

| Target | Input | Oracle |
|---|---|---|
| `q0_roundtrip` | `seeds/generic` | no panic, size bound, C decoder round-trip |
| `q1_roundtrip` | `seeds/generic` | no panic, size bound, C decoder round-trip |
| `params_roundtrip` | `seeds/params` | randomised legal settings, determinism, round-trip |
| `simd_equivalence` | `seeds/params` | every host SIMD backend emits identical bytes |
| `differential_c` | `seeds/params` | byte identity with Google Brotli v1.2.0 |
| `streaming_equivalence` | `seeds/params` | writer and reader agree, round-trip |
| `output_capacity` | `seeds/params` | exact buffer accepted, short buffer reported |

## Parallel campaigns

```sh
cargo afl fuzz -M main -i seeds/params -o findings/params target/release/differential_c
cargo afl fuzz -S worker1 -i seeds/params -o findings/params target/release/differential_c
```

## Triage

Minimise a crash and turn it into a deterministic regression test before fixing
anything:

```sh
cargo afl tmin -i findings/params/default/crashes/id:000000,... -o minimised.bin \
    target/release/differential_c
```

Findings directories are local artifacts and are not committed.
