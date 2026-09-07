#!/usr/bin/env python3
"""Generate quality-by-quality benchmark documentation from one recorded run.

Requires matplotlib==3.10.8 for SVG generation. Does not run benchmarks or modify
source CSVs, previous reports, or Criterion baselines.
"""

import argparse
import csv
import json
import math
import os
from pathlib import Path

ENCODERS = {
    "c-brotli": ("Google C", "#475569"),
    "mbrotli": ("mbrotli", "#0072B2"),
    "rust-brotli": ("Rust brotli", "#D55E00"),
    "simd-brotli": ("SIMD Brotli", "#009E73"),
    "burli": ("Burli", "#CC79A7"),
}
CORPORA = {
    "empty": "Empty input; measures API overhead.",
    "tiny-text": "44-byte quick-brown-fox sentence.",
    "alice29": "Vendored Alice in Wonderland text.",
    "text-1m": "Alice text repeated cyclically to 1 MiB.",
    "binary-64k": "64 KiB of structured binary bytes: ((i / 16) XOR i) modulo 256.",
    "random-64k": "64 KiB prefix of the deterministic pseudorandom corpus.",
    "random-1m": "1 MiB of deterministic xorshift64 pseudorandom bytes.",
    "repeated-1m": "1 MiB of repeated a bytes.",
}


def load_rows(path):
    """Reject missing, duplicate, unsupported, or inconsistent measurements."""
    with path.open() as source:
        rows = list(csv.DictReader(source))
    expected = {(q, corpus, encoder) for q in range(12) for corpus in CORPORA
                for encoder in ENCODERS if encoder != "burli" or q <= 5}
    found = set()
    lengths = {}
    for row in rows:
        for key in ["quality", "input_bytes", "compressed_bytes", "lgwin"]:
            row[key] = int(row[key])
        for key in ["mean_ns", "mean_lower_ns", "mean_upper_ns"]:
            row[key] = float(row[key])
            if not math.isfinite(row[key]) or row[key] <= 0:
                raise ValueError(f"invalid timing: {key}")
        key = row["quality"], row["corpus"], row["implementation"]
        if key not in expected or key in found:
            raise ValueError(f"duplicate or unsupported case: {key}")
        found.add(key)
        if not row["mean_lower_ns"] <= row["mean_ns"] <= row["mean_upper_ns"]:
            raise ValueError(f"invalid confidence bounds: {key}")
        length = row["input_bytes"]
        if length < 0 or row["compressed_bytes"] <= 0 or row["lgwin"] != 22:
            raise ValueError(f"invalid sizes or window: {key}")
        if (row["corpus"] == "empty") != (length == 0):
            raise ValueError(f"invalid empty corpus: {key}")
        if lengths.setdefault(row["corpus"], length) != length:
            raise ValueError(f"inconsistent input lengths: {key}")
    if found != expected:
        raise ValueError(f"incomplete matrix: {len(found)} cases; expected 432")
    return rows


def cases(rows, quality, corpus):
    return sorted((r for r in rows if r["quality"] == quality and r["corpus"] == corpus),
                  key=lambda r: list(ENCODERS).index(r["implementation"]))


def throughput(row):
    return row["input_bytes"] * 1e9 / row["mean_ns"] / 1048576


def summary(rows, quality):
    lines = ["| Dataset | Input bytes | Lowest mean latency | mbrotli / fastest speed | Smallest output, bytes | mbrotli output, bytes |",
             "| --- | ---: | --- | ---: | ---: | ---: |"]
    for corpus in CORPORA:
        group = cases(rows, quality, corpus)
        fastest = min(r["mean_ns"] for r in group)
        winners = ", ".join(ENCODERS[r["implementation"]][0] for r in group if r["mean_ns"] == fastest)
        own = next(r for r in group if r["implementation"] == "mbrotli")
        size = min(r["compressed_bytes"] for r in group)
        lines.append(f"| [{corpus}](#{corpus}) | {own['input_bytes']:,} | {winners} | "
                     f"{fastest / own['mean_ns']:.3f}× | {size:,} | {own['compressed_bytes']:,} |")
    return lines


def chart(group, quality, corpus, path):
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    from matplotlib.ticker import MaxNLocator

    plt.rcParams.update({"svg.fonttype": "none", "svg.hashsalt": "mbrotli-quality-docs",
                         "font.size": 11, "axes.spines.top": False, "axes.spines.right": False})
    length = group[0]["input_bytes"]
    latency = length <= 44
    scale = 1 if length == 0 else 1000
    if latency:
        values = [r["mean_ns"] / scale for r in group]
        low = [r["mean_lower_ns"] / scale for r in group]
        high = [r["mean_upper_ns"] / scale for r in group]
        unit = "ns" if length == 0 else "µs"
        label = f"Mean latency ({unit}) · lower is better"
    else:
        values = [throughput(r) for r in group]
        low = [length * 1e9 / r["mean_upper_ns"] / 1048576 for r in group]
        high = [length * 1e9 / r["mean_lower_ns"] / 1048576 for r in group]
        label = "Throughput (MiB/s) · higher is better"
    fig, (speed, size) = plt.subplots(1, 2, figsize=(12, 3.6), sharey=True)
    colors = [ENCODERS[r["implementation"]][1] for r in group]
    names = [ENCODERS[r["implementation"]][0] for r in group]
    for i, (value, lo, hi, color) in enumerate(zip(values, low, high, colors)):
        speed.errorbar(value, i, xerr=[[value - lo], [hi - value]], fmt="o", color=color,
                       capsize=4, markersize=7, elinewidth=1.5)
        speed.annotate(f"{value:,.4g}", (value, i), xytext=(8, 7),
                       textcoords="offset points", fontsize=9)
    if max(high) / min(low) > 20:
        speed.set_xscale("log")
        speed.set_xlim(min(low) / 1.8, max(high) * 2)
        label += " · log scale"
    else:
        speed.set_xlim(0, max(high) * 1.3)
    speed.set_xlabel(label)
    speed.set_yticks(range(len(group)), names)
    sizes = [r["compressed_bytes"] for r in group]
    size.barh(range(len(group)), sizes, color=colors, height=.55)
    for i, value in enumerate(sizes):
        size.annotate(f"{value:,}", (value, i), xytext=(5, 0), textcoords="offset points",
                      va="center", fontsize=10)
    size.set_xlim(0, max(sizes) * 1.35)
    size.xaxis.set_major_locator(MaxNLocator(nbins=4, integer=True))
    size.ticklabel_format(axis="x", style="plain", useOffset=False)
    size.set_xlabel("Compressed bytes · lower is better · zero baseline")
    speed.set_ylim(len(group) - .5, -.65)
    for ax in (speed, size):
        ax.grid(axis="x", alpha=.18)
        ax.set_axisbelow(True)
    fig.suptitle(f"Quality {quality} · {corpus} · {length:,} input bytes", fontsize=15)
    fig.text(.5, .015, "Cold native APIs · window 22 · timing whiskers: 95% mean confidence bounds",
             ha="center", fontsize=10)
    fig.tight_layout(rect=(0, .045, 1, .95))
    fig.savefig(path, metadata={"Date": None})
    plt.close(fig)


def quality_page(rows, quality, environment, source_link, environment_link, report_link):
    versions = environment["versions"]
    sampling = environment["sampling"]
    navigation = ["[Benchmark index](../README.md)"]
    if quality > 0:
        navigation.append(f"[← Quality {quality - 1}](q{quality - 1}.md)")
    if quality < 11:
        navigation.append(f"[Quality {quality + 1} →](q{quality + 1}.md)")
    lines = [f"# Compression quality {quality}", "", " · ".join(navigation), "",
             f"Recorded run: **{environment['baseline']}** ({environment['date']}).",
             f"[Raw results]({source_link}) · [Environment]({environment_link}) · [Run analysis]({report_link}).", "",
             f"Google C Brotli {versions['c-brotli']}, mbrotli at the recorded optimized checkout, Rust brotli {versions['rust-brotli']},",
             f"and SIMD Brotli {versions['simd-brotli']} are compared at this quality.",
             f"Burli {versions['burli']} is also included." if quality <= 5 else
             "**Burli supports only qualities 0–5; it has no results at this quality.**", "",
             "These are cold native API measurements: construction, allocation, compression, and disposal.",
             f"{environment['mode'].capitalize()} mode, window {environment['lgwin']}, known input size; "
             f"{sampling['samples']} samples, {sampling['warmup_seconds']:g} s warmup, "
             f"at least {sampling['requested_measurement_seconds']:g} s measurement.",
             f"Host: {environment['cpu']}, {environment['affinity']}; {environment['rustflags']}.",
             "Rust brotli and SIMD Brotli include their native 4 KiB I/O adapters.", "",
             "## Dataset summary", "",
             "Lowest latency means the lowest recorded mean, not a statistically established winner.",
             "`mbrotli / fastest speed` is fastest mean latency divided by mbrotli mean latency; 1× is fastest.",
             "Smallest output includes stream overhead. Equal quality numbers do not imply equal output size.", ""]
    lines.extend(summary(rows, quality))
    lines += ["", "## Dataset details", "",
              "Chart whiskers and table intervals describe 95% confidence bounds on mean timing.",
              "They do not capture all host or allocator variation. Throughput bounds are transformed latency bounds.",
              "Output / input is compressed bytes divided by input bytes; smaller is better, and values above 100% mean expansion.",
              "For empty input, throughput and output / input are undefined (—).",
              f"See the [optimization tradeoffs and rechecks]({report_link}#final-measurements) for before/after limitations.", ""]
    for corpus, description in CORPORA.items():
        group = cases(rows, quality, corpus)
        c_time = next(r["mean_ns"] for r in group if r["implementation"] == "c-brotli")
        length = group[0]["input_bytes"]
        scale, unit = (1, "ns") if length == 0 else (1000, "µs")
        lines += [f"### {corpus}", "", f"{description} **Input: {length:,} bytes.**", "",
                  f"![Quality {quality}: {corpus} speed and compressed size](charts/q{quality}-{corpus}.svg)", "",
                  f"| Implementation | Mean, {unit} | 95% mean interval, {unit} | MiB/s | Output bytes | Output / input | Speed / C |",
                  "| --- | ---: | ---: | ---: | ---: | ---: | ---: |"]
        for r in group:
            speed = f"{throughput(r):,.2f}" if length else "—"
            fraction = f"{100 * r['compressed_bytes'] / length:.3f}%" if length else "—"
            lines.append(f"| {ENCODERS[r['implementation']][0]} | {r['mean_ns'] / scale:,.4g} | "
                         f"{r['mean_lower_ns'] / scale:,.4g}–{r['mean_upper_ns'] / scale:,.4g} | "
                         f"{speed} | {r['compressed_bytes']:,} | {fraction} | {c_time / r['mean_ns']:.3f}× |")
        lines.append("")
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--csv", type=Path, required=True)
    parser.add_argument("--environment", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    rows = load_rows(args.csv)
    environment = json.loads(args.environment.read_text())
    args.output.mkdir(parents=True, exist_ok=True)
    charts = args.output / "charts"
    charts.mkdir(exist_ok=True)
    links = [Path(os.path.relpath(path.resolve(), args.output.resolve())).as_posix()
             for path in (args.csv, args.environment, args.report)]
    for quality in range(12):
        for corpus in CORPORA:
            chart(cases(rows, quality, corpus), quality, corpus, charts / f"q{quality}-{corpus}.svg")
        (args.output / f"q{quality}.md").write_text(quality_page(rows, quality, environment, *links))
    print("Generated 12 quality pages and 96 dataset charts from 432 recorded cases.")


if __name__ == "__main__":
    main()
