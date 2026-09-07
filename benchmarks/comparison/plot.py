#!/usr/bin/env python3
"""Render standalone SVG charts from report.py's complete comparison CSV.

Requires matplotlib==3.10.8. Plotting never reruns benchmarks or edits raw data.
"""

import argparse
import csv
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.ticker import MaxNLocator


ENCODERS = {
    "c-brotli": ("Google C", "#444444", "o"),
    "mbrotli": ("mbrotli", "#0072B2", "s"),
    "rust-brotli": ("Rust brotli", "#D55E00", "^"),
    "simd-brotli": ("simd-brotli", "#009E73", "D"),
    "burli": ("burli", "#CC79A7", "v"),
}
CORPORA = ["alice29", "text-1m", "binary-64k", "random-64k", "random-1m", "repeated-1m"]


def render(rows, output, subtitle):
    output.mkdir(parents=True, exist_ok=True)
    plt.rcParams.update({
        "font.size": 10, "axes.spines.top": False, "axes.spines.right": False,
        "axes.grid": True, "grid.alpha": 0.2, "svg.fonttype": "none",
        "svg.hashsalt": "mbrotli-comparison", "figure.facecolor": "white",
    })
    for kind in ["throughput", "size", "tradeoff"]:
        fig, axes = plt.subplots(2, 3, figsize=(15, 9))
        for ax, corpus in zip(axes.flat, CORPORA):
            for encoder, (label, color, marker) in ENCODERS.items():
                points = sorted((r for r in rows if r["corpus"] == corpus and r["implementation"] == encoder),
                                key=lambda r: int(r["quality"]))
                quality = [int(r["quality"]) for r in points]
                speed = [float(r["mib_per_second"]) for r in points]
                size = [int(r["compressed_bytes"]) for r in points]
                if kind == "throughput":
                    ax.plot(quality, speed, label=label, color=color, marker=marker, markersize=4)
                    lo = [int(r["input_bytes"]) * 1e9 / float(r["mean_upper_ns"]) / 1048576 for r in points]
                    hi = [int(r["input_bytes"]) * 1e9 / float(r["mean_lower_ns"]) / 1048576 for r in points]
                    ax.fill_between(quality, lo, hi, color=color, alpha=0.12)
                    ax.set_ylabel("Input MiB/s (log scale) · higher is faster")
                    ax.set_yscale("log")
                elif kind == "size":
                    ax.plot(quality, size, label=label, color=color, marker=marker, markersize=4)
                    ax.set_ylabel("Compressed bytes (log scale) · lower is smaller")
                    ax.set_yscale("log")
                else:
                    fraction = [100 * float(r["compressed_fraction"]) for r in points]
                    ax.plot(fraction, speed, label=label, color=color, marker=marker, markersize=4, linewidth=1)
                    for q, x, y in zip(quality, fraction, speed):
                        if encoder == "mbrotli" and q in [0, 5, 11]:
                            ax.annotate(f"q{q}", (x, y), xytext=(3, 4), textcoords="offset points", fontsize=7, color=color)
                    ax.set_xlabel("Compressed / input (%) · smaller to the left")
                    ax.set_ylabel("Input MiB/s (log scale) · faster upward")
                    ax.set_yscale("log")
                    ax.ticklabel_format(axis="x", style="plain", useOffset=False)
                    ax.xaxis.set_major_locator(MaxNLocator(nbins=5))
                if kind != "tradeoff":
                    ax.set_xlabel("Quality (each encoder's own effort policy)")
                    ax.set_xticks(range(12))
                ax.set_title(corpus)
            if kind == "size":
                lengths = [int(r["compressed_bytes"]) for r in rows if r["corpus"] == corpus]
                # Keep tiny overhead differences on random data from looking
                # like large compression-ratio improvements.
                if max(lengths) < 2 * min(lengths):
                    ax.set_ylim(min(lengths) / 1.5, max(lengths) * 1.5)
        titles = {
            "throughput": "Cold compression throughput by quality",
            "size": "Output size by quality",
            "tradeoff": "Compression speed versus output size",
        }
        fig.suptitle(titles[kind], fontsize=19, fontweight="bold", y=0.99)
        handles, labels = axes.flat[0].get_legend_handles_labels()
        fig.legend(handles, labels, loc="upper center", bbox_to_anchor=(0.5, 0.955), ncol=5, frameon=False)
        fig.text(0.5, 0.01,
                 subtitle + " · cold serial APIs · lgwin=22 · burli q0–q5 only"
                 + (" · shading: 95% mean intervals" if kind == "throughput" else "")
                 + (" · q labels: mbrotli" if kind == "tradeoff" else ""),
                 ha="center", fontsize=10)
        fig.tight_layout(rect=(0, 0.035, 1, 0.90))
        fig.savefig(output / f"{kind}.svg", metadata={"Date": None})
        plt.close(fig)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--csv", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--subtitle", default="Implementation comparison")
    args = parser.parse_args()
    with args.csv.open() as source:
        rows = list(csv.DictReader(source))
    if len(rows) != 432:
        parser.error("expected the complete 432-row export")
    render(rows, args.output, args.subtitle)


if __name__ == "__main__":
    main()
