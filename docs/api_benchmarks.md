# The three newest APIs, measured

> **Written before the pre-release API redesign.** The public names in this
> document — `Brotli`, `CompressParams`, `CompressWorkspace`, `QualityLevel`,
> `WindowBits`, `SharedContext` and the `compress_*` method families — no
> longer exist. The encoders below them did not change.
> [`pre_release_api_redesign.md`](pre_release_api_redesign.md) maps the old
> names onto the new ones.


`CompressWorkspace`, `Write::flush` and attached RFC 9841 prefix dictionaries,
each against the reference doing the same work. The per-quality sweep is in
[`all_qualities_benchmarks.md`](all_qualities_benchmarks.md); this report
covers only what those three add.

**Ratio is C time divided by this crate's time, so above 1.00 is faster than
the reference.** Every case asserts byte identity with Google Brotli configured
the same way before it is timed, so no number here was bought with output size.

Same run, machine and command as the sweep: Apple M5 Pro, Neon, rustc 1.97.1,
`cargo bench --bench compress -- --sample-size 10 --warm-up-time 1
--measurement-time 3`, 2026-08-25. Raw output in
[`benchmarks/2026-08-25-apple-m5-pro-full.txt`](benchmarks/2026-08-25-apple-m5-pro-full.txt).

## 1. `CompressWorkspace`

Three arms per case. `mbrotli` and `c-brotli` are the ordinary one-shot calls —
both build a whole encoder per call, which is what the reference's own one-shot
entry point does and why it has no reuse arm to compare against.
`mbrotli-reused` is the same call through a retained workspace. The size hint
is pinned so every payload resolves to the same encoder shape and the workspace
stays on its reuse path.

| Quality | payload | fresh | reused | speed-up |
| --- | --: | --: | --: | --: |
| 1 | 256 B | 0.922 | **1.042** | 1.13× |
| 1 | 4 KiB | 0.979 | **1.094** | 1.12× |
| 1 | 64 KiB | 1.049 | **1.179** | 1.12× |
| 1 | 1 MiB | 1.116 | **1.176** | 1.05× |
| 2 | 256 B | 0.412 | 0.967 | 2.35× |
| 2 | 4 KiB | 0.643 | 0.970 | 1.51× |
| 2 | 64 KiB | 0.481 | 0.499 | 1.04× |
| 2 | 1 MiB | 0.853 | 0.869 | 1.02× |
| 5 | 256 B | 0.545 | 0.958 | 1.76× |
| 5 | 4 KiB | 0.627 | 0.950 | 1.51× |
| 5 | 64 KiB | 0.652 | 0.666 | 1.02× |
| 5 | 1 MiB | 0.975 | 0.992 | 1.02× |
| 9 | 256 B | 0.054 | **0.892** | **16.6×** |
| 9 | 4 KiB | 0.084 | **0.915** | **11.0×** |
| 9 | 64 KiB | 0.365 | 0.852 | 2.33× |
| 9 | 1 MiB | 0.874 | 0.923 | 1.06× |
| 11 | 256 B | 0.569 | 0.946 | 1.66× |
| 11 | 4 KiB | 0.875 | 0.914 | 1.04× |
| 11 | 64 KiB | 0.931 | 0.933 | 1.00× |
| 11 | 1 MiB | 0.934 | 0.935 | 1.00× |

The shape is the same everywhere and it is the shape a saved allocation has:
the win is largest where the payload is smallest, and it disappears once
compression dominates. What differs is *how* large, and that tracks how much
the quality allocates. Quality 1 gains 12% because its tables are small;
quality 9 gains 16.6× because they are not.

Read against the sweep, this is the whole of the quality 7 to 9 gap: a retained
workspace moves quality 9 from 0.054× to 0.892×, and the remaining 11% is the
search rather than the setup.

Reuse changes nothing observable. The benchmark asserts a reused workspace
emits exactly the stream a fresh one does before it times either, and
`tests/workspace.rs` asserts it over the structural corpora at every quality.

## 2. `Write::flush`

256 KiB of generated text through `CompressorWriter`, split into *N* chunks
with a flush between them; the reference is driven with
`BROTLI_OPERATION_FLUSH` at the same points. One chunk is the no-flush
baseline. Both sides produce identical bytes at every count.

| Quality | chunks | ratio | output | vs. no flush |
| --- | --: | --: | --: | --: |
| 1 | 1 | 1.059 | 2 674 B | 1.00× |
| 1 | 4 | 1.133 | 3 224 B | 1.21× |
| 1 | 32 | 1.071 | 8 560 B | 3.20× |
| 1 | 256 | 1.022 | 46 375 B | **17.34×** |
| 5 | 1 | 0.831 | 2 353 B | 1.00× |
| 5 | 4 | 0.843 | 2 292 B | 0.97× |
| 5 | 32 | 0.861 | 2 390 B | 1.02× |
| 5 | 256 | 0.924 | 5 535 B | 2.35× |
| 9 | 1 | 0.601 | 2 187 B | 1.00× |
| 9 | 4 | 0.619 | 2 049 B | 0.94× |
| 9 | 32 | 0.659 | 2 348 B | 1.07× |
| 9 | 256 | 0.782 | 5 510 B | 2.52× |
| 11 | 1 | 0.934 | 2 330 B | 1.00× |
| 11 | 4 | 0.937 | 2 326 B | 1.00× |
| 11 | 32 | 0.933 | 2 712 B | 1.16× |
| 11 | 256 | 0.921 | 5 540 B | 2.38× |

Two things worth saying plainly, because a caller reaching for `flush` needs
both:

**The cost is ratio, not time.** Flushing never slowed this crate relative to
the reference — the ratio column is flat or improves, because a flush is work
both implementations do identically. What it costs is output. At quality 1,
flushing every kibibyte made the stream **seventeen times larger**: the entropy
codes are rebuilt from a kibibyte of statistics instead of 256, and each flush
adds its padding block. Quality 1 is worst hit because it rebuilds a full code
description per meta-block with nothing to amortise it over.

**A few flushes are free.** At four and thirty-two chunks the output at
qualities 5, 9 and 11 is within a few per cent of the unflushed stream, and at
four chunks it is occasionally *smaller* — a flush ends a meta-block early,
which can happen to split the input where the encoder would have wanted to.

Flush on the boundaries the protocol actually has, not on every write.

## 3. Attached prefix dictionaries

`alice29.txt` split in half: the first half is the dictionary, the second the
payload. That is the shape a shared dictionary is deployed in. Preparation
happens once outside the timed region on both sides — it is a per-connection
cost, not a per-request one.

| Quality | ratio vs. C | with dictionary | without | saved | time cost |
| --- | --: | --: | --: | --: | --: |
| 5 | 0.759 | 24 334 B | 26 850 B | 9.4% | 1.84× |
| 9 | 0.689 | 24 024 B | 26 263 B | 8.5% | 1.26× |
| 11 | 0.899 | 22 230 B | 23 713 B | 6.3% | 1.30× |

The dictionary is doing real work: 6% to 9% off a 76 KiB payload, and the
streams are byte-identical to the reference's compound-dictionary output, so
the saving is the reference's saving and not a different set of choices.

It is not free. Consulting the prefix costs 1.26× to 1.84× the time of
compressing the same payload without it, because every position now runs a
second bucket walk. The relative position against C tracks the ordinary
qualities — 0.76× at quality 5 against 0.756× in the sweep — so the prefix path
is no further behind the reference than the encoder around it is.

This is the first measurement of that path. Nothing in it has been optimised:
the chain walk and the byte comparison are scalar, and the high-quality merge
allocates a vector per position that contributes a match.

## Known gaps in this measurement

- **One machine, one target, ten samples.** As the sweep; see its own gap list.
- **The workspace group uses generated text only.** The reuse win is an
  allocation property rather than a data property, so the corpus should not
  matter, but that is an argument rather than a measurement.
- **The flush group uses one payload size.** 256 KiB at four chunk counts; the
  ratio cost of a flush depends on how much statistics each meta-block gets,
  so a different payload would move the output column.
- **The prefix group uses one dictionary and one payload.** A dictionary that
  matches less, or one much larger than the payload, would report a different
  saving and a different cost.
- **No allocation counts anywhere.** `hotpath-alloc` exists and has not been
  run against any of these three.
