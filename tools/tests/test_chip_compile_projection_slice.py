"""Production bounded fetch, independent regeneration and numerical receipts."""

import json
from pathlib import Path
import struct
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "chip"))
import compile_projection_slice as projection


class ProjectionSliceTests(unittest.TestCase):
    def fixture(self, sample):
        sample.mkdir()
        payload = projection.single.SHAPE[0] * projection.COLUMNS * 2
        header = json.dumps({projection.single.TENSOR: {"dtype": "BF16", "shape": projection.single.SHAPE, "data_offsets": [0, payload]}}).encode()
        raw = struct.pack("<H", 0x3f80) * (projection.ROWS * projection.COLUMNS)
        total = 8 + len(header) + payload
        url = f"https://huggingface.co/{projection.single.MODEL}/resolve/{projection.single.REVISION}/weights.safetensors"
        metadata = {"tensors": {projection.single.TENSOR: {"shape": projection.single.SHAPE, "dtype": "BF16", "shard": "weights.safetensors"}},
                    "shards": {"weights.safetensors": {"source_url": url, "payload_bytes": payload, "header_sha256": projection.single.sha(header)}},
                    "index_canonical_sha256": "b" * 64}
        calls = []
        def ranges(source, start, end):
            calls.append((source, start, end))
            if (start, end) == (0, 7):
                return struct.pack("<Q", len(header)), total
            if (start, end) == (8, len(header) + 7):
                return header, total
            self.assertEqual((start, end), (len(header) + 8, len(header) + 7 + projection.PAYLOAD_BYTES))
            return raw, total
        snapshot = {"model": projection.single.MODEL, "revision": projection.single.REVISION, "config_sha256": projection.CONFIG_SHA256}
        with patch.object(projection.inventory, "inventory", return_value=metadata), patch.object(projection.inventory, "read_range", side_effect=ranges):
            projection.fetch(snapshot, sample)
        return calls

    def test_fetch_reads_only_bounded_full_input_rows_and_regenerates_40_groups(self):
        with tempfile.TemporaryDirectory() as temp:
            sample = Path(temp) / "sample"
            calls = self.fixture(sample)
            self.assertEqual(len(calls), 3)
            self.assertEqual(calls[-1][2] - calls[-1][1] + 1, 81920)
            m, decoded, weights, scales = projection.validate_slice(sample)
            self.assertEqual(m["rows"], 8)
            self.assertEqual(len(decoded[0]), 5120)
            self.assertEqual(weights[0], [7] * 5120)
            self.assertEqual(len(scales[0]), 40)
            self.assertEqual(len((sample / "coefficients.w4").read_bytes()), 20480)

    def test_rehashed_wrong_coefficients_scales_ranges_and_source_fail_before_evaluation(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            sample = root / "sample"
            self.fixture(sample)
            original = (sample / "sample.json").read_bytes()
            coefficients = (sample / "coefficients.w4").read_bytes()
            forged = bytes([0]) + coefficients[1:]
            (sample / "coefficients.w4").write_bytes(forged)
            manifest = json.loads(original)
            manifest["artifact_sha256"]["coefficients.w4"] = projection.single.sha(forged)
            (sample / "sample.json").write_text(json.dumps(manifest))
            with patch.object(projection, "evaluate") as evaluation:
                with self.assertRaisesRegex(ValueError, "independent projection quantization"):
                    projection.observe(None, root / "failure", sample)
                evaluation.assert_not_called()
            failed = json.loads((root / "failure/projection.json").read_text())
            self.assertEqual(failed["status"], "failed")
            self.assertEqual(failed["checks"], {})
            (sample / "coefficients.w4").write_bytes(coefficients)
            for key, value in [("revision", "main"), ("columns", 128), ("sample_bytes", 81921)]:
                manifest = json.loads(original)
                manifest[key] = value
                (sample / "sample.json").write_text(json.dumps(manifest))
                with self.assertRaises(ValueError):
                    projection.validate_slice(sample)
            manifest = json.loads(original)
            manifest["range"]["start"] += 1
            (sample / "sample.json").write_text(json.dumps(manifest))
            with self.assertRaisesRegex(ValueError, "storage range"):
                projection.validate_slice(sample)
            (sample / "sample.json").write_bytes(original)
            raw_scales = (sample / "weight-scales.json").read_bytes()
            scales = json.loads(raw_scales)
            scales[0][0]["numerator"] += 1
            encoded = json.dumps(scales).encode()
            (sample / "weight-scales.json").write_bytes(encoded)
            manifest = json.loads(original)
            manifest["artifact_sha256"]["weight-scales.json"] = projection.single.sha(encoded)
            (sample / "sample.json").write_text(json.dumps(manifest))
            with self.assertRaisesRegex(ValueError, "independent projection quantization"):
                projection.validate_slice(sample)

    def test_production_reference_reports_exact_dot_errors_and_immutable_receipt(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            sample = root / "sample"
            self.fixture(sample)
            result = projection.observe(None, root / "run", sample)
            self.assertEqual(result["status"], "passed")
            self.assertEqual(result["profile_summaries"]["zero"]["max_bf16_absolute_error"], 0)
            profile = json.loads((root / "run/profiles.json").read_text())["sparse"]
            self.assertEqual(profile["differences"][0]["unquantized_sample_dot_exact"], {"numerator": -83, "denominator": 1})
            self.assertEqual(profile["projection"]["outputs"][0]["bf16_bits"], 0xc2a7)  # -83.5; source exact dot -83.
            self.assertEqual(profile["differences"][0]["bf16_error_exact"], {"numerator": -1, "denominator": 2})
            prior = (root / "run/projection.json").read_bytes()
            with self.assertRaises(FileExistsError):
                projection.observe(None, root / "run", sample)
            self.assertEqual((root / "run/projection.json").read_bytes(), prior)
            self.assertIn("rtl_execution", result["excludes"])


if __name__ == "__main__":
    unittest.main()
