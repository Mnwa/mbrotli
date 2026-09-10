import copy
import csv
import json
from pathlib import Path
import tempfile
import unittest
import xml.etree.ElementTree as ET

from decoder_docs import DECODERS, burli_speed, generate, quality_page
from quality_docs import CORPORA, load_rows, medians, validate_rows
from report import collect


def matrix():
    return [dict(corpus=corpus, input_bytes=0 if corpus == "empty" else 1024,
                 quality=q, lgwin=22, implementation=decoder, compressed_bytes=16,
                 mean_ns=200 if decoder == "c-brotli" else 100,
                 mean_lower_ns=90, mean_upper_ns=210)
            for corpus in CORPORA for q in range(12) for decoder in DECODERS]


class DecoderTests(unittest.TestCase):
    def test_direct_burli_median_is_not_ratio_of_c_medians(self):
        rows = matrix()
        for row in rows:
            i = list(CORPORA).index(row["corpus"])
            row["mean_ns"] = {"c-brotli": 1, "mbrotli": [1, 2, 3, 4, 5, 6, 7, 8][i],
                              "burli": [8, 7, 6, 5, 4, 3, 2, 1][i], "rust-brotli": 1}[row["implementation"]]
        expected = (4 / 5 + 5 / 4) / 2
        self.assertAlmostEqual(burli_speed(rows, 5), expected)
        relative = medians(rows, 5)
        self.assertNotAlmostEqual(burli_speed(rows, 5), relative["mbrotli"]["speed"] / relative["burli"]["speed"])

    def test_complete_matrix_includes_burli_high_qualities_and_equal_dataset_medians(self):
        rows = validate_rows(matrix(), decoding=True)
        self.assertEqual(len(rows), 384)
        self.assertEqual(medians(rows, 11)["burli"]["speed"], 2)
        page = quality_page(rows, 11, {"baseline": "test", "date": "2026-09-10"}, ["a", "b", "c"])
        self.assertIn("Burli | 2.000×", page)
        self.assertIn("—", page)
        self.assertNotIn("SIMD Brotli |", page)
        self.assertIn("compressed →", page)

    def test_rejects_missing_duplicate_wrong_decoder_mismatched_stream_and_invalid_timing(self):
        good = matrix()
        bad_rows = [good[:-1], good + good[:1]]
        for field, value in [("corpus", "unsupported-corpus"), ("implementation", "simd-brotli"), ("compressed_bytes", 17),
                             ("input_bytes", 1), ("lgwin", 21), ("quality", 12),
                             ("mean_ns", float("nan")), ("mean_lower_ns", 300)]:
            rows = copy.deepcopy(good)
            rows[1][field] = value
            bad_rows.append(rows)
        for rows in bad_rows:
            with self.subTest(row=rows[1]):
                with self.assertRaises(ValueError):
                    validate_rows(rows, decoding=True)

    def test_export_pairs_decoders_and_rejects_bad_throughput_and_missing_case(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            rows = matrix()
            sizes = [{k: v for k, v in r.items() if not k.startswith("mean_")} for r in rows]
            with (root / "sizes.csv").open("w") as output:
                writer = csv.DictWriter(output, fieldnames=sizes[0])
                writer.writeheader()
                writer.writerows(sizes)
            for index, row in enumerate(rows):
                path = root / str(index) / "run1"
                path.mkdir(parents=True)
                key = f"decoders/cold/{row['corpus']}/q{row['quality']}/{row['implementation']}"
                (path / "benchmark.json").write_text(json.dumps({"full_id": key, "throughput": {"Bytes": row['input_bytes']}}))
                (path / "estimates.json").write_text(json.dumps({"mean": {
                    "point_estimate": row['mean_ns'], "confidence_interval": {
                        "lower_bound": row['mean_lower_ns'], "upper_bound": row['mean_upper_ns']}}}))
            exported = collect(root, "run1", decoding=True)
            self.assertEqual(len(exported), 384)
            self.assertEqual(exported[1]["speed_relative_to_c"], 2)
            self.assertEqual(exported[1]["compressed_fraction"], "")
            self.assertEqual(exported[1]["mib_per_second"], 0)
            estimates = root / "0/run1/estimates.json"
            original = estimates.read_text()
            for value in [0, -1, float("nan"), float("inf")]:
                data = json.loads(original)
                data["mean"]["point_estimate"] = value
                estimates.write_text(json.dumps(data))
                with self.assertRaisesRegex(ValueError, "invalid timing"):
                    collect(root, "run1", decoding=True)
            estimates.write_text(original)
            path = root / "0/run1/benchmark.json"
            data = json.loads(path.read_text())
            data["throughput"] = {"Bytes": 16}
            path.write_text(json.dumps(data))
            with self.assertRaisesRegex(ValueError, "input size mismatch"):
                collect(root, "run1", decoding=True)
            path.unlink()
            with self.assertRaisesRegex(ValueError, "incomplete matrix"):
                collect(root, "run1", decoding=True)

    def test_generation_produces_all_pages_and_valid_svg_without_changing_csv(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source.csv"
            with source.open("w") as output:
                rows = matrix()
                writer = csv.DictWriter(output, fieldnames=rows[0])
                writer.writeheader()
                writer.writerows(rows)
            before = source.read_bytes()
            rows = load_rows(source, decoding=True)
            generate(rows, {"baseline": "test", "date": "2026-09-10", "cpu": "test CPU", "sampling": {"samples": 30}},
                     root / "docs", [source, root / "environment.json", root / "report.md"])
            self.assertEqual(len(list((root / "docs").glob("*.md"))), 13)
            charts = list((root / "docs/charts").glob("*.svg"))
            self.assertEqual(len(charts), 13)
            for chart in charts:
                ET.parse(chart)
            self.assertEqual(source.read_bytes(), before)


if __name__ == "__main__":
    unittest.main()
