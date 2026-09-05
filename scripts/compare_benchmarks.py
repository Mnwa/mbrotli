#!/usr/bin/env python3
"""Check every measured Rust/C pair in a named Criterion baseline separately."""

import argparse
import csv
import json
from pathlib import Path
import sys


def compare(root, baseline, group):
    """Yield paired estimates; reject missing or incompatible C measurements."""
    measurements = {}
    for path in root.rglob(f"{baseline}/benchmark.json"):
        metadata = json.loads(path.read_text())
        if not metadata["group_id"].startswith(group):
            continue
        estimate = json.loads((path.parent / "estimates.json").read_text())["mean"]
        key = (metadata["group_id"], metadata.get("value_str", ""), metadata["function_id"])
        if key in measurements:
            raise ValueError(f"duplicate measurement: {key}")
        measurements[key] = (metadata, estimate)
    for (group_id, corpus, implementation), (metadata, rust) in sorted(measurements.items()):
        if not implementation.startswith("mbrotli"):
            continue
        # The dictionary-free control intentionally uses different settings.
        if implementation == "mbrotli-no-dictionary":
            continue
        key = (group_id, corpus, "c-brotli")
        if key not in measurements:
            raise ValueError(f"missing C measurement: {key}")
        reference, c = measurements[key]
        if reference.get("throughput") != metadata.get("throughput"):
            raise ValueError(f"throughput metadata differs: {key}")
        yield {
            "case": f"{group_id}/{implementation}/{corpus}",
            "c_ns": c["point_estimate"],
            "rust_ns": rust["point_estimate"],
            "percent_of_c": 100 * c["point_estimate"] / rust["point_estimate"],
            # Conservative interval from the two reported mean intervals.
            "percent_lower": 100 * c["confidence_interval"]["lower_bound"] / rust["confidence_interval"]["upper_bound"],
            "percent_upper": 100 * c["confidence_interval"]["upper_bound"] / rust["confidence_interval"]["lower_bound"],
        }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path("target/criterion"))
    parser.add_argument("--baseline", required=True, help="Named, complete run; avoid mixing stale new estimates")
    parser.add_argument("--group", default="", help="Optional group prefix, such as cold/")
    parser.add_argument("--threshold", type=float, default=95)
    parser.add_argument("--expected-cases", type=int, required=True, help="Expected Rust/C pair count; missing cases fail")
    parser.add_argument("--csv", type=Path, required=True)
    args = parser.parse_args()
    if not args.baseline or "/" in args.baseline or "\\" in args.baseline:
        parser.error("baseline must be a directory name")
    try:
        rows = list(compare(args.root, args.baseline, args.group))
    except (OSError, ValueError, KeyError) as error:
        print(error, file=sys.stderr)
        return 2
    if not rows:
        print("No matched measurements found", file=sys.stderr)
        return 2
    with args.csv.open("w", newline="") as output:
        writer = csv.DictWriter(output, fieldnames=[*rows[0], "passes"])
        writer.writeheader()
        for row in rows:
            writer.writerow({**row, "passes": row["percent_of_c"] >= args.threshold})
    failures = [row for row in rows if row["percent_of_c"] < args.threshold]
    for row in sorted(failures, key=lambda item: item["percent_of_c"]):
        print(f'{row["percent_of_c"]:7.2f}%  {row["case"]}')
    print(f"{len(rows) - len(failures)}/{len(rows)} measured cases reach {args.threshold:g}% of C throughput.")
    if len(rows) != args.expected_cases:
        print(f"INCOMPLETE: expected {args.expected_cases} cases, found {len(rows)}.")
    return int(bool(failures) or len(rows) != args.expected_cases)


if __name__ == "__main__":
    sys.exit(main())
