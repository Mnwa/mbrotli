#!/usr/bin/env python3
"""Generate decoder comparison pages and SVGs from one complete recorded run."""

import argparse
import json
import os
from pathlib import Path

from quality_docs import (CORPORA, ENCODERS, bars, cases, dataset_order, load_rows,
                          medians, quality_order, throughput)

DECODERS = {key: value for key, value in ENCODERS.items() if key != "simd-brotli"}


def overview_chart(rows, path, subtitle):
    import matplotlib.pyplot as plt

    order = quality_order(rows)
    fig, ax = plt.subplots(figsize=(14, 5))
    for index, (decoder, (name, color)) in enumerate(DECODERS.items()):
        positions = [i + (index - 1.5) * .2 for i in range(12)]
        values = [medians(rows, q)[decoder]["speed"] for q in order]
        ax.bar(positions, values, width=.19, color=color, label=name)
    ax.set_xticks(range(12), [f"q{q}" for q in order])
    ax.set_ylim(bottom=0)
    ax.set_ylabel("Median decode speed / C · higher is better")
    ax.set_xlabel("Source encoder quality (Google C, window 22)")
    ax.axhline(1, color="#64748b", linestyle="--", linewidth=1)
    ax.grid(axis="y", alpha=.18)
    ax.set_axisbelow(True)
    ax.legend(ncol=4, loc="upper center", bbox_to_anchor=(.5, 1.15), frameon=False)
    fig.suptitle("Decompression · median across all 8 datasets", fontsize=18)
    fig.text(.5, .02, subtitle + "\nIdentical compressed inputs · equal dataset weight, including empty and tiny input",
             ha="center", fontsize=10)
    fig.tight_layout(rect=(0, .09, 1, .91))
    fig.savefig(path, metadata={"Date": None})
    plt.close(fig)


def quality_chart(rows, quality, path):
    import matplotlib.pyplot as plt

    fig, axes = plt.subplots(4, 2, figsize=(13, 16))
    for ax, corpus in zip(axes.flat, dataset_order(rows, quality)):
        group = cases(rows, quality, corpus)
        length = group[0]["input_bytes"]
        if length <= 44:
            values = [r["mean_ns"] / 1000 for r in group]
            low = [r["mean_lower_ns"] / 1000 for r in group]
            high = [r["mean_upper_ns"] / 1000 for r in group]
            label = "Mean latency (µs) · lower is better"
        else:
            values = [throughput(r) for r in group]
            low = [length * 1e9 / r["mean_upper_ns"] / 1048576 for r in group]
            high = [length * 1e9 / r["mean_lower_ns"] / 1048576 for r in group]
            label = "Decoded MiB/s · higher is better"
        bars(ax, [DECODERS[r["implementation"]][0] for r in group], values,
             [DECODERS[r["implementation"]][1] for r in group], label, low, high)
        ax.set_title(f"{corpus} · {group[0]['compressed_bytes']:,} → {length:,} bytes")
    fig.suptitle(f"Decompression · source quality {quality} · identical C streams", fontsize=18)
    fig.text(.5, .015, "Cold APIs · window 22 · linear axes from zero · whiskers: 95% confidence bounds on mean timing",
             ha="center", fontsize=10)
    fig.tight_layout(rect=(0, .04, 1, .97))
    fig.savefig(path, metadata={"Date": None})
    plt.close(fig)


def quality_page(rows, quality, environment, links):
    lines = [f"# Decompression of quality {quality} streams", "",
             "[Decoder overview](README.md) · [Benchmark index](../README.md)", "",
             f"Recorded run: **{environment['baseline']}** ({environment['date']}).",
             f"[CSV]({links[0]}) · [Environment]({links[1]}) · [Methodology and limits]({links[2]}).", "",
             "Quality belongs to the C encoder that prepared the input; decoders have no quality setting.",
             "All four decode the same bytes. Burli is included at every source quality; SIMD Brotli",
             "re-exports Rust brotli's decoder and is omitted.", "",
             "## Median speed across eight datasets", "",
             "Median of eight per-dataset C mean time / decoder mean time ratios, with equal weight",
             "including empty and tiny input. Higher is faster; 1× matches C. No confidence interval",
             "is inferred for this across-dataset median.", "",
             "| Decoder | Median speed / C |", "| --- | ---: |"]
    for decoder, value in medians(rows, quality).items():
        lines.append(f"| {DECODERS[decoder][0]} | {value['speed']:.3f}× |")
    lines += ["", f"![Decompression by dataset at source quality {quality}](charts/q{quality}.svg)", "",
              "Datasets are ranked by mbrotli speed relative to the fastest peer, highest first.",
              "Throughput counts restored bytes; empty input has latency only. Whiskers and intervals",
              "describe sampling uncertainty, not all host variation. Cold APIs include allocation",
              "and disposal. C knows the output capacity; Rust brotli includes its 4 KiB I/O adapter.", ""]
    for corpus in dataset_order(rows, quality):
        group = cases(rows, quality, corpus)
        length, compressed = group[0]["input_bytes"], group[0]["compressed_bytes"]
        fraction = f"{compressed / length:.3%}" if length else "undefined"
        reference = next(r["mean_ns"] for r in group if r["implementation"] == "c-brotli")
        lines += [f"## {corpus}", "", CORPORA[corpus], "",
                  f"**{compressed:,} compressed → {length:,} restored bytes; compressed / original: {fraction}.**", "",
                  "| Decoder | Mean µs | 95% mean interval µs | Decoded MiB/s | Speed / C |",
                  "| --- | ---: | ---: | ---: | ---: |"]
        for row in group:
            speed = f"{throughput(row):,.2f}" if length else "—"
            lines.append(f"| {DECODERS[row['implementation']][0]} | {row['mean_ns'] / 1000:,.4g} | "
                         f"{row['mean_lower_ns'] / 1000:,.4g}–{row['mean_upper_ns'] / 1000:,.4g} | "
                         f"{speed} | {reference / row['mean_ns']:.3f}× |")
        lines.append("")
    return "\n".join(lines)


def generate(rows, environment, output, sources):
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    plt.rcParams.update({"svg.fonttype": "none", "svg.hashsalt": "mbrotli-decoder-comparison",
                         "font.size": 11, "axes.spines.top": False, "axes.spines.right": False})
    output.mkdir(parents=True, exist_ok=True)
    charts = output / "charts"
    charts.mkdir(exist_ok=True)
    links = [Path(os.path.relpath(path.resolve(), output.resolve())).as_posix() for path in sources]
    overview_chart(rows, charts / "overview.svg", f"{environment['cpu']} · {environment['date']} · {environment['sampling']['samples']} samples")
    lines = ["# Decoder comparison", "", "[Benchmark index](../README.md)", "",
             f"Recorded run: **{environment['baseline']}** ({environment['date']}).",
             f"[CSV]({links[0]}) · [Environment]({links[1]}) · [Methodology and limits]({links[2]}).", "",
             "![Median decompression speed relative to C](charts/overview.svg)", "",
             "Each value is the median of C mean time / decoder mean time across all eight datasets,",
             "equally weighted, including empty and tiny input. Higher is faster; 1× matches C.",
             "Qualities are ordered by mbrotli median speed / C. Medians do not imply statistical significance.", "",
             "All four decoders restore identical C-generated streams. Source quality is an encoder setting,",
             "not a decoder setting. Burli decodes q0–q11; SIMD Brotli shares Rust brotli's decoder and is omitted.",
             "Compressed sizes and ratios are properties of those shared inputs, so there is no decoder size ranking.", "",
             "| Source quality | Google C | mbrotli | Rust brotli | Burli |", "| --- | ---: | ---: | ---: | ---: |"]
    for quality in quality_order(rows):
        values = medians(rows, quality)
        lines.append(f"| [q{quality}](q{quality}.md) | " + " | ".join(f"{values[d]['speed']:.3f}×" for d in DECODERS) + " |")
    lines += ["", "Open a source quality for all eight datasets, compressed/restored byte counts,",
              "mean latency, confidence bounds and throughput. Measurements include cold construction,",
              "allocation, decoding and disposal. C receives the known output capacity; Rust brotli",
              "includes its native 4 KiB I/O adapter. See the linked methodology for reproducible commands",
              "and the limits of this single-host comparison.", ""]
    (output / "README.md").write_text("\n".join(lines))
    for quality in range(12):
        quality_chart(rows, quality, charts / f"q{quality}.svg")
        (output / f"q{quality}.md").write_text(quality_page(rows, quality, environment, links))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--csv", type=Path, required=True)
    parser.add_argument("--environment", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    rows = load_rows(args.csv, decoding=True)
    environment = json.loads(args.environment.read_text())
    generate(rows, environment, args.output, [args.csv, args.environment, args.report])
    print("Generated decoder overview, 12 quality pages and 13 SVGs from 384 measurements.")


if __name__ == "__main__":
    main()
