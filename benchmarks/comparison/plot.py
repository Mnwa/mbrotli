#!/usr/bin/env python3
"""Render vertical SVG bars from a complete comparison CSV; requires matplotlib==3.10.8."""

import argparse
import csv
from pathlib import Path
from statistics import median

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

from quality_docs import CORPORA, ENCODERS, bars, load_rows, medians, quality_order


def style():
    plt.rcParams.update({
        "font.size": 11, "axes.spines.top": False, "axes.spines.right": False,
        "svg.fonttype": "none", "svg.hashsalt": "mbrotli-comparison",
        "figure.facecolor": "white",
    })


def median_chart(rows, quality, path):
    """Compare per-dataset relative speed and size without mixing units or weights."""
    style()
    values = medians(rows, quality)
    names = [ENCODERS[e][0] for e in values]
    colors = [ENCODERS[e][1] for e in values]
    fig, axes = plt.subplots(1, 2, figsize=(13, 4.8))
    for ax, metric, label in zip(axes, ["speed", "size"],
                               ["Median speed / C · higher is better", "Median output / C · lower is better"]):
        bars(ax, names, [v[metric] for v in values.values()], colors, label, ratio=True)
    fig.suptitle(f"Quality {quality} · median across all 8 datasets", fontsize=17)
    fig.text(.5, .02, "Equal dataset weight, including empty input · per-dataset ratios to Google C · dashed line: C = 1×",
             ha="center", fontsize=10)
    fig.tight_layout(rect=(0, .06, 1, .95))
    fig.savefig(path, metadata={"Date": None})
    plt.close(fig)


def render(rows, output, subtitle):
    """Write median speed, size and paired overview bars; strongest mbrotli qualities first."""
    output.mkdir(parents=True, exist_ok=True)
    style()
    order = quality_order(rows)
    summaries = {q: medians(rows, q) for q in order}
    for kind, metric, label in [("throughput", "speed", "Median speed / C · higher is better"),
                                ("size", "size", "Median output / C · lower is better")]:
        fig, axes = plt.subplots(4, 3, figsize=(15, 15))
        for ax, quality in zip(axes.flat, order):
            values = summaries[quality]
            bars(ax, [ENCODERS[e][0].replace(" ", "\n") for e in values],
                 [v[metric] for v in values.values()], [ENCODERS[e][1] for e in values], "", ratio=True)
            ax.set_title(f"Quality {quality}")
        fig.suptitle(label + "\nMedian across all 8 datasets", fontsize=19)
        fig.text(.5, .018, subtitle + "\nEqual dataset weight · strongest mbrotli median speed / C first · Burli q0–q5 only",
                 ha="center", fontsize=10)
        fig.tight_layout(rect=(0, .05, 1, .94))
        fig.savefig(output / f"{kind}.svg", metadata={"Date": None})
        plt.close(fig)
    fig, axes = plt.subplots(2, 1, figsize=(14, 8))
    for ax, metric, label in zip(axes, ["speed", "size"],
                               ["Median speed / C · higher is better", "Median output / C · lower is better"]):
        for index, (encoder, (name, color)) in enumerate(ENCODERS.items()):
            positions = [i + (index - 2) * .16 for i, q in enumerate(order) if encoder in summaries[q]]
            values = [summaries[q][encoder][metric] for q in order if encoder in summaries[q]]
            ax.bar(positions, values, width=.15, color=color, label=name)
        ax.set_xticks(range(12), [f"q{q}" for q in order])
        ax.set_ylim(bottom=0)
        ax.set_ylabel(label)
        ax.axhline(1, color="#64748b", linestyle="--", linewidth=1)
        ax.grid(axis="y", alpha=.18)
        ax.set_axisbelow(True)
    fig.suptitle("Speed and size · median across all 8 datasets", fontsize=19)
    handles, labels = axes[0].get_legend_handles_labels()
    fig.legend(handles, labels, loc="upper center", bbox_to_anchor=(.5, .94), ncol=5, frameon=False)
    fig.text(.5, .02, subtitle + "\nPer-dataset ratios to C · equal weight · qualities ordered by mbrotli median speed / C",
             ha="center", fontsize=10)
    fig.tight_layout(rect=(0, .07, 1, .89))
    fig.savefig(output / "tradeoff.svg", metadata={"Date": None})
    plt.close(fig)


def before_after_chart(source, path):
    """Keep the separate matched run visible, with descending median speedup panels."""
    with source.open() as stream:
        rows = list(csv.DictReader(stream))
    expected = {(corpus, q) for corpus in CORPORA for q in range(12)}
    keys = [(r["corpus"], int(r["quality"])) for r in rows]
    if len(keys) != len(expected) or set(keys) != expected:
        raise ValueError("expected all 96 unique matched cases")
    values = {corpus: [float(r["before_ns"]) / float(r["after_ns"])
                       for r in sorted(rows, key=lambda r: int(r["quality"])) if r["corpus"] == corpus]
              for corpus in CORPORA}
    order = sorted(CORPORA, key=lambda corpus: -median(values[corpus]))
    style()
    fig, axes = plt.subplots(4, 2, figsize=(14, 13))
    for ax, corpus in zip(axes.flat, order):
        bars(ax, [str(q) for q in range(12)], values[corpus], [ENCODERS["mbrotli"][1]] * 12,
             "Before time / after time", ratio=True)
        for annotation, value in zip(ax.texts, values[corpus]):
            annotation.set_fontsize(8)
            annotation.set_text(f"{value:.2f}")
        ax.set_title(corpus)
        ax.set_xlabel("Quality")
    fig.suptitle("Matched mbrotli before / after · higher is faster", fontsize=19)
    fig.text(.5, .015, "Separate 96-case run · ordered by median speedup across qualities · dashed line: unchanged = 1×",
             ha="center", fontsize=10)
    fig.tight_layout(rect=(0, .04, 1, .96))
    fig.savefig(path, metadata={"Date": None})
    plt.close(fig)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--csv", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--subtitle", default="Implementation comparison")
    parser.add_argument("--before-after", type=Path)
    parser.add_argument("--before-after-output", type=Path)
    args = parser.parse_args()
    if bool(args.before_after) != bool(args.before_after_output):
        parser.error("--before-after and --before-after-output must be supplied together")
    render(load_rows(args.csv), args.output, args.subtitle)
    if args.before_after:
        before_after_chart(args.before_after, args.before_after_output)


if __name__ == "__main__":
    main()
