"""Production input/refusal boundaries for the shared-product experiment."""

import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "chip"))
import compare_product_tiles as comparison
import tools.tests.test_chip_map_weight_tile as mapping_fixtures


class ProductComparisonTests(unittest.TestCase):
    def test_corrupt_real_sample_fails_before_any_tools(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            sample, library, sources = mapping_fixtures.MappingInputTests().prepare(root)
            (sample / "sample.bf16").write_bytes(b"corrupted")
            with patch.object(comparison.mapping, "SOURCES", sources), patch.object(comparison.shutil, "which") as tools:
                with self.assertRaisesRegex(ValueError, "artifact identity"):
                    comparison.observe(sample, library, root / "run")
                tools.assert_not_called()
            receipt = json.loads((root / "run/comparison.json").read_text())
            self.assertEqual(receipt["status"], "failed")
            self.assertEqual(receipt["checks"], {})
            self.assertEqual(receipt["variants"], {})

    def test_unavailable_eda_fails_and_prior_receipt_is_immutable(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            sample, library, sources = mapping_fixtures.MappingInputTests().prepare(root)
            out = root / "run"
            with patch.object(comparison.mapping, "SOURCES", sources), patch.object(comparison.shutil, "which", return_value=None):
                with self.assertRaisesRegex(ValueError, "execution cannot be skipped"):
                    comparison.observe(sample, library, out)
            prior = (out / "comparison.json").read_bytes()
            receipt = json.loads(prior)
            self.assertEqual(receipt["status"], "failed")
            self.assertEqual(receipt["checks"], {"canonical_input_validation": "passed"})
            self.assertEqual(receipt["variants"], {})
            self.assertIn("integer_group_tile_shared.sv", receipt["input_sha256"])
            self.assertIn("shared_fixture.sv", receipt["input_sha256"])
            with self.assertRaises(FileExistsError):
                comparison.observe(sample, library, out)
            self.assertEqual((out / "comparison.json").read_bytes(), prior)

    def test_wrapper_transformation_cannot_change_interface_or_constants(self):
        raw = b"integer_group_tile #(.LANES(4), .GROUP_SIZE(128), .WEIGHTS(2048'h80)) tile (.*);"
        transformed = comparison.alternative_wrapper(raw)
        self.assertEqual(transformed.replace(b"integer_group_tile_shared", b"integer_group_tile"), raw)
        for invalid in [b"module forged; endmodule", raw + raw]:
            with self.assertRaisesRegex(ValueError, "exactly one"):
                comparison.alternative_wrapper(invalid)


if __name__ == "__main__":
    unittest.main()
