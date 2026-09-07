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
from statistics import median

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


def medians(rows, quality):
    """Give each dataset equal weight after normalizing to its own C result."""
    reference = {r["corpus"]: r for r in rows
                 if r["quality"] == quality and r["implementation"] == "c-brotli"}
    result = {}
    for encoder in ENCODERS:
        group = [r for r in rows if r["quality"] == quality and r["implementation"] == encoder]
        if group:
            result[encoder] = {
                "speed": median(reference[r["corpus"]]["mean_ns"] / r["mean_ns"] for r in group),
                "size": median(r["compressed_bytes"] / reference[r["corpus"]]["compressed_bytes"] for r in group),
            }
    return result


def dataset_order(rows, quality):
    """Rank by mbrotli speed / fastest peer speed; retain corpus order on ties."""
    scores = {}
    for corpus in CORPORA:
        group = cases(rows, quality, corpus)
        own = next(r for r in group if r["implementation"] == "mbrotli")
        scores[corpus] = min(r["mean_ns"] for r in group if r["implementation"] != "mbrotli") / own["mean_ns"]
    return sorted(CORPORA, key=lambda corpus: -scores[corpus])


def quality_order(rows):
    """Put qualities with the highest median mbrotli speed relative to C first."""
    return sorted(range(12), key=lambda q: -medians(rows, q)["mbrotli"]["speed"])


def summary(rows, quality):
    lines = ["| Implementation | Median speed / C ↑ | Median output / C ↓ | Datasets |",
             "| --- | ---: | ---: | ---: |"]
    for encoder, values in medians(rows, quality).items():
        lines.append(f"| {ENCODERS[encoder][0]} | {values['speed']:.3f}× | {values['size']:.3f}× | 8 |")
    return lines


def bars(ax, names, values, colors, label, low=None, high=None, ratio=False):
    """Draw labeled vertical bars with a linear zero baseline and optional bounds."""
    errors = None if low is None else [[v - lo for v, lo in zip(values, low)],
                                      [hi - v for v, hi in zip(values, high)]]
    rectangles = ax.bar(range(len(values)), values, color=colors, width=.62,
                        yerr=errors, capsize=4, error_kw={"elinewidth": 1.2})
    labels = [f"{v:.3f}×" if ratio else f"{v:,.4g}" for v in values]
    ax.bar_label(rectangles, labels=labels, padding=7, fontsize=10,
                 bbox={"facecolor": "white", "edgecolor": "none", "alpha": .9, "pad": 1})
    ax.set_xticks(range(len(names)), names, fontsize=10)
    ax.set_ylim(0, max(high if high is not None else values) * 1.25)
    ax.set_ylabel(label)
    ax.ticklabel_format(axis="y", style="plain", useOffset=False)
    ax.grid(axis="y", alpha=.18)
    ax.set_axisbelow(True)
    if ratio:
        ax.axhline(1, color="#64748b", linestyle="--", linewidth=1)


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
    fig, (speed, size) = plt.subplots(1, 2, figsize=(13, 4.8))
    colors = [ENCODERS[r["implementation"]][1] for r in group]
    names = [ENCODERS[r["implementation"]][0] for r in group]
    bars(speed, names, values, colors, label, low, high)
    sizes = [r["compressed_bytes"] for r in group]
    bars(size, names, sizes, colors, "Compressed bytes · lower is better")
    size.yaxis.set_major_locator(MaxNLocator(nbins=5, integer=True))
    # Preserve exact byte counts even where the axis uses compact ticks.
    for annotation, value in zip(size.texts, sizes):
        annotation.set_text(f"{value:,}")
    fig.suptitle(f"Quality {quality} · {corpus} · {length:,} input bytes", fontsize=15)
    fig.text(.5, .015, "Linear axes from zero · cold APIs · window 22 · whiskers: 95% mean confidence bounds",
             ha="center", fontsize=10)
    fig.tight_layout(rect=(0, .045, 1, .95))
    fig.savefig(path, metadata={"Date": None})
    plt.close(fig)


def quality_page(rows, quality, environment, source_link, environment_link, report_link):
    versions = environment["versions"]
    sampling = environment["sampling"]
    navigation = ["[Benchmark index](../README.md)", "[Median overview](README.md)"]
    if quality > 0:
        navigation.append(f"[← Quality {quality - 1}](q{quality - 1}.md)")
    if quality < 11:
        navigation.append(f"[Quality {quality + 1} →](q{quality + 1}.md)")
    lines = [f"# Compression quality {quality}", "", " · ".join(navigation), "",
             f"Recorded run: **{environment['baseline']}** ({environment['date']}).",
             f"[Raw results]({source_link}) · [Environment]({environment_link}) · [Run analysis]({report_link}).", "",
             "## Median across all datasets", "",
             "Each of the eight datasets has equal weight, including empty and tiny input.",
             "Speed / C = C mean latency / implementation mean latency; output / C = implementation bytes / C bytes.",
             "We take the median of these eight per-dataset ratios separately for speed and size.",
             "1× matches C; higher speed and lower output are better. These are medians across datasets,",
             "not median sample latencies, and do not imply equal compressed size or statistical significance.", "",
             f"![Median speed and output relative to C at quality {quality}](charts/q{quality}-summary.svg)", "",
             *summary(rows, quality), "", "<details>", "<summary>Measurement setup and interpretation</summary>", "",
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
              "Chart whiskers and table intervals describe 95% confidence bounds on mean timing.",
              "They do not capture all host or allocator variation. Throughput bounds are transformed latency bounds.",
              "Output / input is compressed bytes divided by input bytes; smaller is better, and values above 100% mean expansion.",
              "For empty input, throughput and output / input are undefined (—).",
              f"See the [run analysis]({report_link}#final-measurements) for measurement limitations.",
              "", "</details>", "", "## Dataset details", "",
              "Ordered by mbrotli speed / fastest competing implementation, highest first; all eight datasets are shown.",
              "This order uses recorded mean latency, not statistical significance. Bars start at zero.", "",
              " · ".join(f"[{corpus}](#{corpus})" for corpus in dataset_order(rows, quality)), ""]
    for corpus in dataset_order(rows, quality):
        description = CORPORA[corpus]
        group = cases(rows, quality, corpus)
        c_time = next(r["mean_ns"] for r in group if r["implementation"] == "c-brotli")
        length = group[0]["input_bytes"]
        scale, unit = (1, "ns") if length == 0 else (1000, "µs")
        own = next(r for r in group if r["implementation"] == "mbrotli")
        peer = min((r for r in group if r["implementation"] != "mbrotli"), key=lambda r: r["mean_ns"])
        lines += [f"### {corpus}", "", f"{description} **Input: {length:,} bytes.**", "",
                  f"mbrotli speed / fastest peer ({ENCODERS[peer['implementation']][0]}): "
                  f"**{peer['mean_ns'] / own['mean_ns']:.3f}×**; "
                  f"output: {own['compressed_bytes']:,} / {peer['compressed_bytes']:,} bytes.", "",
                  f"![Quality {quality}: {corpus} speed and compressed size](charts/q{quality}-{corpus}.svg)", "",
                  "<details>", "<summary>Exact measurements and confidence bounds</summary>", "",
                  f"| Implementation | Mean, {unit} | 95% mean interval, {unit} | MiB/s | Output bytes | Output / input | Speed / C |",
                  "| --- | ---: | ---: | ---: | ---: | ---: | ---: |"]
        for r in group:
            speed = f"{throughput(r):,.2f}" if length else "—"
            fraction = f"{100 * r['compressed_bytes'] / length:.3f}%" if length else "—"
            lines.append(f"| {ENCODERS[r['implementation']][0]} | {r['mean_ns'] / scale:,.4g} | "
                         f"{r['mean_lower_ns'] / scale:,.4g}–{r['mean_upper_ns'] / scale:,.4g} | "
                         f"{speed} | {r['compressed_bytes']:,} | {fraction} | {c_time / r['mean_ns']:.3f}× |")
        lines.extend(["", "</details>", ""])
    return "\n".join(lines)


def main():
    from plot import median_chart

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
        median_chart(rows, quality, charts / f"q{quality}-summary.svg")
        for corpus in CORPORA:
            chart(cases(rows, quality, corpus), quality, corpus, charts / f"q{quality}-{corpus}.svg")
        (args.output / f"q{quality}.md").write_text(quality_page(rows, quality, environment, *links))
    overview = ["# Median results by quality", "", "[Benchmark index](../README.md)", "",
                f"Recorded run: **{environment['baseline']}** ({environment['date']}).",
                f"[Raw results]({links[0]}) · [Environment]({links[1]}) · [Run analysis]({links[2]}).", "",
                "All eight datasets contribute equally to each median, including empty and tiny input.",
                "For each dataset, speed / C = C mean latency / implementation mean latency;",
                "output / C = implementation bytes / C bytes. Medians are taken over those ratios.",
                "Qualities are ordered by mbrotli median speed / C, highest first. Burli supports q0–q5 only.", "",
                "| Quality | mbrotli speed / C ↑ | mbrotli output / C ↓ | Datasets |",
                "| --- | ---: | ---: | ---: | ---: |"]
    for quality in quality_order(rows):
        values = medians(rows, quality)["mbrotli"]
        overview.append(f"| [Quality {quality}](q{quality}.md) | {values['speed']:.3f}× | {values['size']:.3f}× | 8 |")
    for quality in quality_order(rows):
        overview.extend(["", f"## [Quality {quality}](q{quality}.md)", "",
                         f"![Quality {quality}: median across all datasets](charts/q{quality}-summary.svg)"])
    (args.output / "README.md").write_text("\n".join(overview) + "\n")
    print("Generated quality overview, 12 quality pages, 12 median charts and 96 dataset charts from 432 cases.")


if __name__ == "__main__":
    main()
