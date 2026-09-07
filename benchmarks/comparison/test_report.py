import csv
import json
from pathlib import Path
import tempfile
import unittest

from report import collect


class ReportTests(unittest.TestCase):
    def test_rejects_default_or_path_baselines(self):
        for baseline in ["", "new", "base", "..", "x/y", "x\\y"]:
            with self.assertRaises(ValueError):
                collect(Path("unused"), baseline)

    def test_complete_matrix_and_missing_measurement(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sizes = []
            for corpus in range(8):
                for quality in range(12):
                    for encoder in ["c-brotli", "mbrotli", "rust-brotli", "simd-brotli", "burli"]:
                        if encoder == "burli" and quality > 5:
                            continue
                        row = dict(corpus=str(corpus), input_bytes=1024, quality=quality,
                                   lgwin=22, implementation=encoder, compressed_bytes=128)
                        sizes.append(row)
                        path = root / str(len(sizes)) / "run1"
                        path.mkdir(parents=True)
                        key = f"implementations/cold/{corpus}/q{quality}/{encoder}"
                        (path / "benchmark.json").write_text(json.dumps({"full_id": key, "throughput": {"Bytes": 1024}}))
                        (path / "estimates.json").write_text(json.dumps({"mean": {
                            "point_estimate": 200 if encoder == "c-brotli" else 100,
                            "confidence_interval": {"lower_bound": 90, "upper_bound": 210}}}))
            with (root / "sizes.csv").open("w") as output:
                writer = csv.DictWriter(output, fieldnames=sizes[0])
                writer.writeheader()
                writer.writerows(sizes)
            rows = collect(root, "run1")
            self.assertEqual(len(rows), 432)
            self.assertEqual(rows[1]["speed_relative_to_c"], 2)
            self.assertEqual(rows[1]["compressed_fraction"], 0.125)
            (root / "1/run1/benchmark.json").unlink()
            with self.assertRaisesRegex(ValueError, "incomplete"):
                collect(root, "run1")


if __name__ == "__main__":
    unittest.main()
