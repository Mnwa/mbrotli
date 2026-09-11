import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
import xml.etree.ElementTree as ET

import matplotlib.pyplot as plt
from plot import before_after_chart, median_chart, render
from quality_docs import bars, cases, chart, load_rows, medians, quality_order


ROOT = Path(__file__).resolve().parents[2]


class PlotTests(unittest.TestCase):
    def test_bars_encode_values_vertically_from_zero_and_keep_bounds(self):
        fig, ax = plt.subplots()
        bars(ax, ["A", "B"], [1, 2], ["blue", "gray"], "Speed", [.8, 1.5], [1.3, 2.8], ratio=True)
        self.assertEqual([bar.get_height() for bar in ax.patches], [1, 2])
        self.assertEqual([bar.get_y() for bar in ax.patches], [0, 0])
        self.assertEqual(ax.get_ylim()[0], 0)
        self.assertEqual(ax.get_yscale(), "linear")
        self.assertGreater(ax.get_ylim()[1], 2.8)
        self.assertEqual([t.get_text() for t in ax.texts], ["1.000×", "2.000×"])
        segments = ax.containers[0].lines[2][0].get_segments()
        self.assertAlmostEqual(segments[0][0][1], .8)
        self.assertAlmostEqual(segments[1][1][1], 2.8)
        plt.close(fig)

    def test_dataset_figures_keep_latency_throughput_and_exact_output(self):
        rows = load_rows(ROOT / "docs/benchmarks/encoder-comparison.csv")
        for corpus in ["empty", "tiny-text", "alice29"]:
            group = cases(rows, 0, corpus)
            with patch("matplotlib.pyplot.close"):
                with tempfile.TemporaryDirectory() as directory:
                    chart(group, 0, corpus, Path(directory) / "chart.svg")
                fig = plt.gcf()
                speed, size = fig.axes
                length = group[0]["input_bytes"]
                expected = [r["mean_ns"] / (1 if length == 0 else 1000) if length <= 44
                            else length * 1e9 / r["mean_ns"] / 1048576 for r in group]
                self.assertEqual([bar.get_height() for bar in speed.patches], expected)
                self.assertEqual([bar.get_height() for bar in size.patches], [r["compressed_bytes"] for r in group])
                self.assertEqual([t.get_text() for t in size.texts], [f"{r['compressed_bytes']:,}" for r in group])
            plt.close(fig)

    def test_median_figures_use_calculated_ratios_and_render_complete_overviews(self):
        rows = load_rows(ROOT / "docs/benchmarks/encoder-comparison.csv")
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            with patch("matplotlib.pyplot.close"):
                median_chart(rows, 11, output / "median.svg")
                fig = plt.gcf()
                self.assertEqual([b.get_height() for b in fig.axes[0].patches],
                                 [v["speed"] for v in medians(rows, 11).values()])
            plt.close(fig)
            render(rows, output, "Test run")
            text = " ".join(ET.parse(output / "throughput.svg").getroot().itertext())
            positions = [text.index(f"Quality {q}") for q in quality_order(rows)]
            self.assertEqual(positions, sorted(positions))
            before_after_chart(ROOT / "docs/benchmarks/competitor-paths-before-after.csv", output / "before-after.svg")
            for filename in ["median.svg", "throughput.svg", "size.svg", "overview.svg", "before-after.svg"]:
                self.assertTrue(ET.parse(output / filename).getroot().tag.endswith("svg"))
            invalid = output / "invalid.csv"
            invalid.write_text("corpus,quality,before_ns,after_ns\nempty,0,1,1\n")
            with self.assertRaisesRegex(ValueError, "96 unique"):
                before_after_chart(invalid, output / "invalid.svg")


if __name__ == "__main__":
    unittest.main()
