import copy
import csv
import json
from pathlib import Path
import tempfile
import unittest

from quality_docs import CORPORA, dataset_order, load_rows, medians, quality_order, quality_page, summary


ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "docs/benchmarks/encoder-comparison.csv"
ENVIRONMENT = ROOT / "docs/benchmarks/encoder-comparison-environment.json"


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
            self.assertEqual(page.count("\n| Burli |"), 9 if quality <= 5 else 0)
            self.assertEqual(page.count("\n| mbrotli |"), 9)
            self.assertIn("Median across all datasets", page)
            self.assertIn(f"charts/q{quality}-summary.svg", page)
            headings = [line[4:] for line in page.splitlines() if line.startswith("### ")]
            self.assertEqual(headings, dataset_order(rows, quality))
            for corpus in CORPORA:
                self.assertIn(f"charts/q{quality}-{corpus}.svg", page)
            if quality > 5:
                self.assertIn("has no results at this quality", page)

    def test_median_uses_all_eight_normalized_datasets_with_equal_weight(self):
        rows = load_rows(SOURCE)
        # The two central ratios are 4 and 6. An outlier and widely varying
        # reference times distinguish the median of ratios from pooled timing.
        ratios = dict(zip(CORPORA, [1, 2, 3, 4, 6, 7, 8, 1000]))
        for row in rows:
            ratio = ratios[row["corpus"]]
            row["mean_ns"] = ratio * ratio if row["implementation"] == "c-brotli" else ratio
            row["compressed_bytes"] = 10 if row["implementation"] == "c-brotli" else 10 * ratio
        values = medians(rows, 11)
        self.assertEqual(values["mbrotli"], {"speed": 5, "size": 5})
        self.assertEqual(values["c-brotli"], {"speed": 1, "size": 1})
        self.assertNotIn("burli", values)
        self.assertIn("burli", medians(rows, 5))
        table = "\n".join(summary(rows, 11))
        self.assertIn("| mbrotli | 5.000× | 5.000× | 8 |", table)

    def test_dataset_order_uses_fastest_peer_and_keeps_all_tied_cases(self):
        rows = load_rows(SOURCE)
        for row in rows:
            row["mean_ns"] = 10 if row["implementation"] == "mbrotli" else 20
            if row["corpus"] == "empty" and row["implementation"] == "rust-brotli":
                row["mean_ns"] = 1
        self.assertEqual(dataset_order(rows, 0), list(CORPORA)[1:] + ["empty"])
        for row in rows:
            row["mean_ns"] = 10
        self.assertEqual(dataset_order(rows, 0), list(CORPORA))
        self.assertEqual(quality_order(rows), list(range(12)))
        for row in rows:
            if row["quality"] == 11 and row["implementation"] == "mbrotli":
                row["mean_ns"] = 1
        self.assertEqual(quality_order(rows), [11] + list(range(11)))


if __name__ == "__main__":
    unittest.main()
