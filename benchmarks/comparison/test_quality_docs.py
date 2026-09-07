import copy
import csv
import json
from pathlib import Path
import tempfile
import unittest

from quality_docs import CORPORA, load_rows, quality_page, summary


ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "docs/benchmarks/competitor-paths-comparison.csv"
ENVIRONMENT = ROOT / "docs/benchmarks/competitor-paths-environment.json"


class QualityDocsTests(unittest.TestCase):
    def test_complete_matrix_and_rejects_corrupt_measurements(self):
        with SOURCE.open() as source:
            original = list(csv.DictReader(source))
        self.assertEqual(len(load_rows(SOURCE)), 432)
        variants = []
        variants.append((original[:-1], "incomplete"))
        variants.append((original[:-1] + [original[0]], "duplicate"))
        for key, value in [("mean_ns", "nan"), ("mean_lower_ns", "-1"),
                           ("mean_upper_ns", "0.0001"), ("input_bytes", "1")]:
            rows = copy.deepcopy(original)
            rows[0][key] = value
            variants.append((rows, "invalid"))
        rows = copy.deepcopy(original)
        next(r for r in rows if r["implementation"] == "burli")["quality"] = "6"
        variants.append((rows, "unsupported"))
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "data.csv"
            for rows, message in variants:
                with path.open("w", newline="") as output:
                    writer = csv.DictWriter(output, fieldnames=original[0])
                    writer.writeheader()
                    writer.writerows(rows)
                with self.assertRaisesRegex(ValueError, message):
                    load_rows(path)

    def test_pages_cover_empty_input_and_unsupported_quality_without_inventing_rows(self):
        rows = load_rows(SOURCE)
        environment = json.loads(ENVIRONMENT.read_text())
        environment["versions"]["rust-brotli"] = "test-version"
        for quality in range(12):
            page = quality_page(rows, quality, environment, "data.csv", "env.json", "report.md")
            self.assertEqual(page.count("![Quality"), 8)
            self.assertEqual(page.count("### "), 8)
            self.assertIn("test-version", page)
            self.assertIn("| — |", page)
            self.assertEqual(page.count("\n| Burli |"), 8 if quality <= 5 else 0)
            self.assertEqual(page.count("\n| mbrotli |"), 8)
            for corpus in CORPORA:
                self.assertIn(f"charts/q{quality}-{corpus}.svg", page)
            if quality > 5:
                self.assertIn("has no results at this quality", page)

    def test_summary_preserves_exact_ties_instead_of_choosing_an_arbitrary_winner(self):
        rows = load_rows(SOURCE)
        for row in rows:
            if row["quality"] == 11 and row["corpus"] == "alice29":
                row["mean_ns"] = 100
        table = "\n".join(summary(rows, 11))
        self.assertIn("Google C, mbrotli, Rust brotli, SIMD Brotli | 1.000×", table)


if __name__ == "__main__":
    unittest.main()
