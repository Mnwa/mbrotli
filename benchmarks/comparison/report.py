#!/usr/bin/env python3
"""Export a complete, named implementation benchmark with sizes and intervals."""

import argparse
import csv
import json
from pathlib import Path


def collect(root, baseline):
    if not baseline or baseline in {".", "..", "new", "base"} or any(c in baseline for c in "/\\"):
        raise ValueError("use a unique named baseline")
    with (root / "sizes.csv").open() as source:
        sizes = list(csv.DictReader(source))
    expected = {}
    for row in sizes:
        key = f'implementations/cold/{row["corpus"]}/q{row["quality"]}/{row["implementation"]}'
        if key in expected:
            raise ValueError(f"duplicate size row: {key}")
        expected[key] = row
    measured = {}
    for path in root.rglob(f"{baseline}/benchmark.json"):
        metadata = json.loads(path.read_text())
        key = metadata["full_id"]
        if key not in expected or key in measured:
            raise ValueError(f"unexpected or duplicate measurement: {key}")
        if metadata["throughput"] != {"Bytes": int(expected[key]["input_bytes"])}:
            raise ValueError(f"input size mismatch: {key}")
        estimate = json.loads((path.parent / "estimates.json").read_text())["mean"]
        measured[key] = estimate
    if measured.keys() != expected.keys() or len(expected) != 432:
        raise ValueError(f"incomplete matrix: {len(measured)} measurements, {len(expected)} sizes; expected 432")
    rows = []
    for key, size in expected.items():
        estimate = measured[key]
        ns = estimate["point_estimate"]
        reference = measured[key.rsplit("/", 1)[0] + "/c-brotli"]["point_estimate"]
        length = int(size["input_bytes"])
        rows.append({
            **size,
            "mean_ns": ns,
            "mean_lower_ns": estimate["confidence_interval"]["lower_bound"],
            "mean_upper_ns": estimate["confidence_interval"]["upper_bound"],
            "mib_per_second": length * 1e9 / ns / 1048576,
            "speed_relative_to_c": reference / ns,
            "compressed_fraction": int(size["compressed_bytes"]) / length if length else "",
        })
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).parent / "target/criterion")
    parser.add_argument("--baseline", required=True)
    parser.add_argument("--csv", type=Path, required=True)
    args = parser.parse_args()
    try:
        rows = collect(args.root, args.baseline)
    except (OSError, ValueError, KeyError) as error:
        parser.error(str(error))
    with args.csv.open("w", newline="") as output:
        writer = csv.DictWriter(output, fieldnames=rows[0])
        writer.writeheader()
        writer.writerows(rows)
    print(f"Exported {len(rows)} validated size/timing rows to {args.csv}")


if __name__ == "__main__":
    main()
