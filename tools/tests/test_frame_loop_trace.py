from __future__ import annotations

import csv
import importlib.util
import io
from pathlib import Path
import re
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "frame_loop_bench", ROOT / "scripts/perf/frame-loop-bench.py"
)
assert SPEC and SPEC.loader
bench = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bench)

HEADER = (
    "seq,start_us,period_us,input_us,pump_us,layout_us,scene_us,submit_us,"
    "testctl_us,clients_us,sleep_us,painted,present_async,input_latency_us,"
    "input_retirement_latency_us,backpressured,deadline_late_us,wake,cpu_us,"
    "input_step,input_step_us"
).split(",")


def sample() -> dict:
    return dict(zip(HEADER, (
        0, 1_000_000, 16_667, 100, 200, 300, 400, 500, 10, 20, 15_000,
        1, 0, None, None, 0, 0, "io", 1_500, "portal", 200,
    )))


class FrameLoopTraceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "synthetic.csv"

    def write(self, header: list[str], rows: list[dict]) -> None:
        stream = io.StringIO(newline="")
        writer = csv.writer(stream, lineterminator="\r\n")
        writer.writerow(header)
        for row in rows:
            writer.writerow([row[key] for key in header])
        self.path.write_text(stream.getvalue(), encoding="utf-8", newline="")

    def test_native_schema_strings_and_optional_numbers_roundtrip(self) -> None:
        source = (ROOT / "crates/session/shell/src/compositor/metrics.rs").read_text()
        match = re.search(r'pub const CSV_HEADER: &str = "(.*?)";', source, re.S)
        self.assertIsNotNone(match)
        self.assertEqual(re.sub(r"\\\n\s*", "", match.group(1)).split(","), HEADER)
        first = sample()
        second = first | {"seq": 1, "start_us": 1_016_667, "wake": "17", "cpu_us": None}
        self.write(HEADER, [first, second])
        header, rows = bench.read_trace(self.path)
        self.assertEqual(header, HEADER)
        self.assertEqual(rows, [first, second])
        summary = bench.summarize(rows, 0, 16_667)
        self.assertEqual(summary["ticks"], 2)
        self.assertEqual(summary["submit"]["mean"], 500)
        self.assertEqual(summary["input_latency"]["count"], 0)

    def test_quoted_commas_quotes_and_newlines_preserve_labels(self) -> None:
        row = sample() | {"wake": 'io, "ready"', "input_step": "scene\nsubmission"}
        self.write(HEADER, [row])
        self.assertEqual(bench.read_trace(self.path)[1], [row])

    def test_empty_text_labels_stay_text(self) -> None:
        row = sample() | {"wake": "", "input_step": ""}
        self.write(HEADER, [row])
        self.assertEqual(bench.read_trace(self.path)[1], [row])

    def test_legacy_numeric_columns_and_missing_optional_columns(self) -> None:
        optional = {
            "wake", "cpu_us", "input_step", "input_step_us", "input_retirement_latency_us",
            "backpressured", "deadline_late_us",
        }
        header = [key for key in reversed(HEADER) if key not in optional] + ["future_counter"]
        row = {key: value for key, value in sample().items() if key not in optional}
        row["future_counter"] = 123
        self.write(header, [row])
        actual_header, rows = bench.read_trace(self.path)
        self.assertEqual(actual_header, header)
        self.assertEqual(rows, [row])
        self.assertEqual(bench.summarize(rows, 0, 16_667)["ticks"], 1)

    def test_missing_required_columns_and_values_fail(self) -> None:
        with self.subTest(case="column absent"):
            self.write([key for key in HEADER if key != "layout_us"], [sample()])
            with self.assertRaisesRegex(ValueError, "Missing required frame trace columns"):
                bench.read_trace(self.path)
        with self.subTest(case="required value absent"):
            self.write(HEADER, [sample() | {"layout_us": None}])
            with self.assertRaisesRegex(ValueError, "Missing required frame trace value"):
                bench.read_trace(self.path)

    def test_empty_duplicate_and_incomplete_records_fail(self) -> None:
        records = {
            "empty": "",
            "header only": ",".join(HEADER) + "\n",
            "duplicate header": "seq,seq\n0,1\n",
            "missing header name": "seq,\n0,1\n",
            "malformed header quoting": '"unterminated\n',
            "short row": ",".join(HEADER) + "\n0,1\n",
            "long row": ",".join(HEADER) + "\n" + ",".join(["1"] * (len(HEADER) + 1)) + "\n",
            "unterminated quote": ",".join(HEADER) + '\n"unterminated\n',
        }
        for name, text in records.items():
            with self.subTest(case=name):
                self.path.write_text(text, encoding="utf-8")
                with self.assertRaises(ValueError):
                    bench.read_trace(self.path)

    def test_invalid_numeric_field_fails_without_exposing_cell(self) -> None:
        self.write(HEADER, [sample() | {"layout_us": "synthetic-invalid-cell"}])
        with self.assertRaises(ValueError) as error:
            bench.read_trace(self.path)
        self.assertNotIn("synthetic-invalid-cell", str(error.exception))


if __name__ == "__main__":
    unittest.main()
