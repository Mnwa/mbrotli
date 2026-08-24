# Fuzzing subsystem

AFL++ coverage-guided fuzzing for the quality 0 and quality 1 encoders. This
document describes the `fuzz/afl` package as it exists today: its module
boundaries, the input model, where each oracle comes from, how a finding
travels back into the test suite, and which boundaries are still unfuzzed.

## Ownership boundaries

`fuzz/afl` is a separate, unpublished package, deliberately excluded from the
root workspace (`Cargo.toml`, `exclude = ["fuzz/afl"]`) so that AFL's
instrumentation and its runtime never reach an ordinary root `cargo test` or
`cargo clippy`. It depends on `mbrotli` and on `google-brotli-ffi` by path, and
pins `fearless_simd` with `force_support_fallback` so the scalar backend is
reachable for the equivalence target.

The package is split so that the AFL dependency stops at the binary layer:

```mermaid
graph TD
    subgraph engine["Engine layer (depends on afl)"]
        bins["src/bin/ — seventeen afl::fuzz! adapters"]
    end

    subgraph neutral["Engine-neutral layer (no afl dependency)"]
        targets["src/targets.rs<br/>TARGETS registry, one fn per target"]
        lib["src/lib.rs<br/>Context, decode_case, cap,<br/>host_levels, C oracles"]
    end

    subgraph replay["Replay layer"]
        regtest["tests/regressions.rs"]
        corpus["regressions/ — one directory per target"]
    end

    subgraph under["Under test / oracles"]
        mbrotli["mbrotli public API"]
        ffi["google-brotli-ffi<br/>(C encoder and decoder)"]
        simd["fearless_simd::Level"]
    end

    bins --> targets
    regtest --> targets
    regtest --> corpus
    targets --> lib
    targets --> mbrotli
    lib --> ffi
    lib --> simd

    classDef engineNode fill:#f9d6d5,stroke:#a94442;
    class bins engineNode;
```

Only `src/bin/` names `afl`. Every target body is a plain
`fn(&Context, &[u8])`, so the same code runs under the fuzzer, under
`cargo afl test`, and under a debugger. That is what makes a minimised crash
reproducible without an instrumented binary.

`Context` is built once per process and carries the prepared state: a
`Compressor` pinned to the detected level, and the deduplicated list of host
backends. Nothing in `mbrotli` holds mutable global state — there is no
`static mut`, `thread_local`, `OnceLock`, `RefCell`, `Mutex` or atomic in
`src/`, and `Compressor` is `Copy` over a resolved `Level` — so AFL's
persistent mode needs no reset hook and `fuzz_with_reset!` is not used.

## Input model

Five input shapes exist. All cap the payload at `MAX_PAYLOAD` (128 KiB) by
truncation rather than rejection, so an oversized input still contributes the
structure its prefix carries.

```mermaid
flowchart TD
    input["AFL input bytes"] --> shape{"target shape"}

    shape -->|payload only| raw["whole input is the payload<br/>q0, q1, q3, q4, q5 roundtrip"]
    raw --> capA["cap to MAX_PAYLOAD"]
    capA --> fixed["params = (fixed quality, WindowBits::DEFAULT)"]

    shape -->|settings header| hdr["decode_case: 6 header bytes"]
    hdr --> q["byte 0 — IMPLEMENTED_QUALITIES indexed by b mod 5"]
    hdr --> w["byte 1 — WindowBits 10 + b mod 15<br/>spans MIN to MAX, always legal"]
    hdr --> c["byte 2 — chunk = 1 shl (b mod 18), always at least 1"]
    hdr --> f["byte 3 — mode in the low two bits,<br/>literal context modelling in bit 2"]
    hdr --> bl["byte 4 — zero leaves lgblock to the encoder,<br/>otherwise BlockBits 16 + b mod 9"]
    hdr --> dc["byte 5 — postfix bits and direct groups,<br/>falling back to the default pair when unrepresentable"]
    hdr --> capB["remainder capped to MAX_PAYLOAD,<br/>size hint pinned to its length"]

    shape -->|numeric settings| pp["parameter_parsing: 2 header bytes"]
    pp --> qn["byte 0 — quality value b mod 20<br/>reaches 10 and 12 and above, which are illegal"]
    pp --> wn["byte 1 — window value b, 0 to 255<br/>reaches below 10 and above 24"]

    shape -->|"large window"| lw["large_window: 1 byte, then decode_case"]
    lw --> lwn["byte 0 — declared window b mod 70<br/>reaches below 10 and above 62, both illegal"]
    lw --> lwr["remainder — a whole decode_case input,<br/>so quality and distance layout still vary"]

    shape -->|"shared context"| sc["shared_context: 2 bytes, then decode_case"]
    sc --> scn["byte 0 — attachments b mod 18<br/>reaches 16 and 17, both past the format's limit"]
    sc --> scs["byte 1 — every fourth value squeezes<br/>SharedContextLimits to an impossible budget"]
    sc --> scr["remainder — a whole decode_case input;<br/>its payload is cut into the attachments<br/>and then matched against them"]
```

`decode_case` is closed over the legal domain by construction: its window index
covers exactly `WindowBits::MIN..=MAX`, so the `unwrap_or(DEFAULT)` fallback is
unreachable, `chunk` is never zero, and an unrepresentable distance layout
falls back to the default pair. The size hint is pinned to the payload length,
which is what the one-shot entry points would substitute anyway, so the
streaming and one-shot targets stay comparable with each other and with the C
reference. That keeps the equivalence and differential targets focused on
encoder behaviour. `parameter_parsing` exists
because of that closure — it is the only target that can reach the validating
conversions and the unimplemented-quality path.

## Targets and oracles

| Target | Input | Oracle |
| --- | --- | --- |
| `q0_roundtrip` | payload | no panic, `compressed.len() <= calculate_bound`, C decoder round-trip |
| `q1_roundtrip` | payload | same, at quality 1 |
| `q3_roundtrip` | payload | same, at quality 3 |
| `q4_roundtrip` | payload | same, at quality 4 |
| `q5_roundtrip` | payload | same, at quality 5 |
| `q6_roundtrip` | payload | same, at quality 6 |
| `q7_roundtrip` | payload | same, at quality 7 |
| `q8_roundtrip` | payload | same, at quality 8 |
| `q9_roundtrip` | payload | same, at quality 9 |
| `q10_roundtrip` | payload | same, at quality 10 |
| `q11_roundtrip` | payload | same, at quality 11 |
| `params_roundtrip` | header | bound, determinism across two runs, round-trip, over every legal setting |
| `simd_equivalence` | header | every distinct host backend emits identical bytes |
| `differential_c` | header | byte identity with Google Brotli v1.2.0 configured with the same quality, window, mode, block size, size hint, distance layout and context setting |
| `streaming_equivalence` | header | writer output equals reader output at an arbitrary chunk size, and round-trips |
| `output_capacity` | header | exactly sized `dst` accepted, one byte short reported as `OutputTooSmall` |
| `parameter_parsing` | numeric | `TryFrom` contracts hold; unimplemented qualities reported by all four entry points |
| `large_window` | large window | `WindowBits::large` contract holds; qualities 0 and 1 refuse rather than dropping the request; bound, determinism, backend identity; C decoder round-trip up to 30 declared bits, and above it the stream differs from the 30-bit stream only in the six header bits |
| `shared_context` | shared context | preparation is a transaction — a count or limit refusal yields no context; the accessors agree with what was attached; a reported prefix match really matches those bytes and fits inside both sides; the offset-to-distance mapping round-trips and saturates at both ends; the match does not depend on which backend the compressor resolved; an empty context emits exactly what `compress` emits and round-trips; a non-empty one is refused with `UnsupportedSharedContextForQuality` rather than ignored |

The oracles are layered rather than independent: `differential_c` is the
strongest (byte identity with the reference), `params_roundtrip` and the
streaming target hold when the reference is unavailable, and the bound and
capacity checks pin the API contract regardless of the bytes produced.

## Per-iteration flow

```mermaid
sequenceDiagram
    participant AFL as AFL++ forkserver
    participant Bin as src/bin adapter
    participant Body as targets body
    participant Lib as mbrotli
    participant C as google-brotli-ffi

    Note over Bin: Context::default() once, before the loop<br/>(SIMD detection, backend enumeration)
    AFL->>Bin: persistent iteration, input bytes
    Bin->>Body: body(&ctx, data)
    Body->>Body: decode_case / cap
    Body->>Lib: compress, compress_to_slice,<br/>compress_writer, compress_reader
    Lib-->>Body: compressed bytes, or BrotliCompressError
    alt oracle needs the reference
        Body->>C: BrotliEncoderCompress
        C-->>Body: reference bytes
    end
    Body->>C: BrotliDecoderDecompress
    C-->>Body: decoded bytes
    Body->>Body: assert oracle
    Body-->>Bin: return, or panic on violation
    Bin-->>AFL: iteration result
```

A panic is the signal; nothing catches it. Errors that are part of the API
contract — `UnsupportedQuality`, `OutputTooSmall`, the `TryFrom` rejections —
are asserted on rather than treated as crashes.

## SIMD dispatch point

`host_levels` enumerates the backends the equivalence target compares. It
gathers `Level::new()`, `Level::baseline()`, `Level::fallback()` and every
architecture token the host exposes, then deduplicates by enum variant: those
routinely resolve to the same backend — on aarch64 the first two and the Neon
token are all Neon — and comparing a backend against itself costs an iteration
without buying coverage. Detection happens once, in `Context::default()`, never
inside the loop.

## Finding lifecycle

```mermaid
stateDiagram-v2
    [*] --> Campaign: cargo afl fuzz
    Campaign --> Crash: oracle violated
    Crash --> Minimised: cargo afl tmin
    Minimised --> Committed: committed as crash-*.bin
    Committed --> Failing: cargo afl test must fail
    Failing --> Fixed: fix the encoder, not the harness
    Fixed --> Passing: cargo afl test must pass
    Passing --> Campaign: resume with -i -
    Passing --> [*]
```

`tests/regressions.rs` walks the `TARGETS` registry, and for each entry replays
every `.bin` file under `regressions/<name>/` through that target's body. It
also asserts that no target has an empty corpus, so adding a target without
seeding it fails the suite. The corpus holds hand-written `boundary-*.bin`
cases — empty input, truncated and extreme headers, minimum and maximum window
sizes, smallest and largest chunk sizes, incompressible payloads — plus
`crash-*.bin` reproducers as findings arrive.

## Seed corpora

Seeds are generated, not committed: `prepare-seeds.sh` derives them from the
vendored submodule at `brotli-ffi/vendor/brotli/tests/testdata`, and
`minimise-seeds.sh` reduces each corpus with `cargo afl cmin`, keeping the
unminimised original alongside as `seeds/*.raw`. `seeds/generic` is the raw
test data (24 files, minimised to 21); `seeds/params` is the same files behind
a parameter header (114, minimised to 47 — most headers reach the same code);
`seeds/shared_context` is each parameter seed behind two more bytes, at four
attachment counts (0, 1, 15, 16 — the empty-context path, one dictionary, the
format's limit and one past it) crossed with a generous and an impossible
budget.

`seeds/large_window` is each parameter seed behind one more byte, at four
declared windows — the floor, the default, the widest the pinned C decoder
reads, and the widest the format allows.
Minimisation cuts the file count, not the byte count: the large fixtures carry
coverage the small ones miss and survive `cmin`. Per-iteration cost is bounded
by `MAX_PAYLOAD`, not by the corpus. No dictionary is used — the targets
consume arbitrary payload bytes rather than a token grammar.

`minimise-seeds.sh` must export `AFL_NO_FORKSRV=1`. `afl-cmin` measures
coverage with `afl-showmap -I`, and that folder mode stalls against these
binaries — persistent mode with a deferred forkserver — so every input blocks
until the `-t` timeout expires: roughly six seconds per seed, and an empty
output directory at the end. Without the forkserver, showmap execs the target
once per input the way its single-file mode already does. The captured edge
sets are identical; the corpus takes under two seconds instead of minutes.

## Known gaps

- **No decompression target.** There is no decoder in `mbrotli`; round-trip
  oracles use Google's C decoder. A decoder target has to wait for one.
- **Qualities 2 and 6 through 11 are only fuzzed for their refusal.**
  `parameter_parsing` asserts they report `UnsupportedQuality` from all four
  entry points; there is no implementation behind them to fuzz.
- **Payloads are capped at 128 KiB.** Inputs longer than that are truncated, so
  windows of 2^17 and above never span multiple encoder blocks under the
  fuzzer. `tests/vendor_corpus.rs` covers multi-fragment inputs instead,
  including a 12 MiB case.
- **No CI fuzzing.** The repository has no CI configuration at all, so neither
  a bounded smoke campaign nor the regression replay runs automatically.
- **The regression corpora for every quality target above one are seeded, not
  found.** `q3_roundtrip` through `q11_roundtrip` start from the same boundary
  cases as `q0_roundtrip`; nothing has crashed yet to replace them.
- **The `large_window` regression corpus is seeded, not found.** Its twenty
  inputs are the boundary cases written when the target was added — every edge
  of the `10..=62` range, both refusing qualities, an empty payload, a single
  byte and incompressible bytes — and a 150-second campaign over
  `seeds/large_window` on `aarch64-apple-darwin` found 1024 new corpus items,
  24.45% coverage, no crashes and no timeouts.
- **Only smoke campaigns have been run,** and none since qualities six to
  eleven were added, so the figures below predate more than half the targets.
  The most recent was thirty to forty-five seconds per target on
  `aarch64-apple-darwin` over the unminimised parameter corpus:
  `differential_c` 650 new corpus items and 21.7% coverage,
  `simd_equivalence` 736 and 33.5%, `streaming_equivalence` 607 and 20.6%,
  `params_roundtrip` 482 and 20.8%, `q5_roundtrip` 35 and 7.0%; 0 crashes and
  0 hangs throughout. That depth finds shallow faults only; no long campaign
  has been run, and no crash has ever been triaged, so the tmin-to-regression
  path in `regressions/` is exercised by boundary cases rather than by a real
  finding.
- **`cargo afl fuzz` and `cargo afl cmin` need `cargo afl system-config`** on
  macOS, which runs `sudo`. Without it both fail at `shmget()`.
