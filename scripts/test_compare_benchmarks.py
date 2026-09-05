"""Regression checks for the per-case performance gate.

Run with: python3 -m unittest discover -s scripts -p 'test_*.py'
"""

import csv
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("compare_benchmarks.py")


class BenchmarkGateTests(unittest.TestCase):
    def test_each_case_must_pass_and_incomplete_evidence_is_rejected(self):
        scenarios = [
            ("exact threshold", [95], 1, False, 0),
            ("fast case cannot cover slow case", [200, 90], 2, False, 1),
            ("missing pair", [100], 2, False, 1),
            ("missing C control", [None], 1, False, 2),
            ("different input lengths", [100], 1, True, 2),
        ]
        for label, percentages, expected, different_input, status in scenarios:
            with self.subTest(label=label), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                for index, percent in enumerate(percentages):
                    for implementation, time in [("mbrotli", 100), ("c-brotli", percent)]:
                        if time is None:
                            continue
                        target = root / str(index) / implementation / "candidate"
                        target.mkdir(parents=True)
                        metadata = {
                            "group_id": "cold/q5",
                            "value_str": str(index),
                            "function_id": implementation,
                            "throughput": {"Bytes": 2 if different_input and implementation == "c-brotli" else 1},
                        }
                        (target / "benchmark.json").write_text(json.dumps(metadata))
                        (target / "estimates.json").write_text(json.dumps({"mean": {
                            "point_estimate": time,
                            "confidence_interval": {"lower_bound": time, "upper_bound": time},
                        }}))
                output = root / "comparison.csv"
                result = subprocess.run([
                    sys.executable, str(SCRIPT), "--root", str(root),
                    "--baseline", "candidate", "--expected-cases", str(expected),
                    "--csv", str(output),
                ], capture_output=True, text=True, check=False)
                self.assertEqual(result.returncode, status, result.stdout + result.stderr)
                if status != 2:
                    with output.open() as source:
                        rows = list(csv.DictReader(source))
                    self.assertEqual(len(rows), len(percentages))
                    self.assertEqual([row["passes"] == "True" for row in rows],
                                     [percent >= 95 for percent in percentages])


if __name__ == "__main__":
    unittest.main()
