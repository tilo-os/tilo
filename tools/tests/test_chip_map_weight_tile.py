"""Failure receipts and immutable input gates for research library mapping."""

import json
from copy import deepcopy
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "chip"))
import map_weight_tile as mapping
import tools.tests.test_chip_compile_weight_tile as compiler_fixtures


class MappingInputTests(unittest.TestCase):
    def prepare(self, root):
        sample, library = root / "sample", root / "library"
        library.mkdir()
        compiler_fixtures.WeightCompilerTests().compile(sample)
        sources = {}
        for name in mapping.SOURCES:
            raw = ("test library:" + name).encode()
            (library / name).write_bytes(raw)
            sources[name] = (name, mapping.sha(raw))
        return sample, library, sources

    def test_mutated_library_refused_before_tool_execution(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            sample, library, sources = self.prepare(root)
            (library / mapping.LIBRARY).write_bytes(b"changed")
            with patch.object(mapping, "SOURCES", sources), patch.object(mapping.shutil, "which") as tool:
                with self.assertRaisesRegex(ValueError, "library identity"):
                    mapping.observe(sample, library, root / "run")
                tool.assert_not_called()
            receipt = json.loads((root / "run/mapping.json").read_text())
            self.assertEqual(receipt["status"], "failed")
            self.assertEqual(receipt["failure_stage"], "input_validation")
            self.assertEqual(receipt["checks"], {})

    def test_changed_sample_and_oversized_input_refused(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            sample, library, sources = self.prepare(root)
            (sample / "sample.bf16").write_bytes(b"different")
            with patch.object(mapping, "SOURCES", sources):
                with self.assertRaisesRegex(ValueError, "artifact identity"):
                    mapping.observe(sample, library, root / "run")
            with (root / "large").open("wb") as stream:
                stream.truncate(mapping.BOUND + 1)
            with self.assertRaisesRegex(ValueError, "byte bound"):
                mapping.bounded(root / "large")

    def test_rehashed_forged_bench_and_wrong_quantization_fail_before_tools(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            sample, library, sources = self.prepare(root)
            original = (sample / "sample.json").read_bytes()
            original_bench = (sample / "tb.sv").read_bytes()
            forged = b'module tb; initial begin $display("RTL_PASS fake"); $finish; end endmodule\n'
            (sample / "tb.sv").write_bytes(forged)
            manifest = json.loads(original)
            manifest["artifact_sha256"]["tb.sv"] = mapping.sha(forged)
            (sample / "sample.json").write_text(json.dumps(manifest))
            with patch.object(mapping, "SOURCES", sources), patch.object(mapping.shutil, "which") as tool:
                with self.assertRaisesRegex(ValueError, "independent regeneration"):
                    mapping.observe(sample, library, root / "forged-bench")
                tool.assert_not_called()
            receipt = json.loads((root / "forged-bench/mapping.json").read_text())
            self.assertEqual(receipt["status"], "failed")
            self.assertEqual(receipt["checks"], {})
            # Metadata is checked against independently decoded source bytes.
            (sample / "tb.sv").write_bytes(original_bench)
            wrong_scale = json.loads(original)
            wrong_scale["quantization"]["group_scales_exact"][0]["denominator"] = 99
            (sample / "sample.json").write_text(json.dumps(wrong_scale))
            with patch.object(mapping, "SOURCES", sources), patch.object(mapping.shutil, "which") as tool:
                with self.assertRaisesRegex(ValueError, "quantization metadata"):
                    mapping.observe(sample, library, root / "wrong-scale")
                tool.assert_not_called()
            # A self-consistent hash list cannot excuse fabricated provenance.
            manifest = json.loads(original)
            manifest["revision"] = "main"
            (sample / "sample.json").write_text(json.dumps(manifest))
            with patch.object(mapping, "SOURCES", sources):
                with self.assertRaisesRegex(ValueError, "checkpoint identity"):
                    mapping.observe(sample, library, root / "moving-source")

    def test_statistics_require_actual_instances_functional_models_and_complete_area(self):
        stats = {"num_cells_by_type": {"DFF_X1": 2, "AND2_X1": 1, "$scopeinfo": 1},
                 "num_cells": 4, "num_memories": 0, "num_memory_bits": 0,
                 "num_processes": 0, "num_submodules": 0, "area": 10.0}
        mapped = {"modules": {"fixed_linear_fixture": {"cells": {
            "a": {"type": "DFF_X1"}, "b": {"type": "DFF_X1"},
            "c": {"type": "AND2_X1"}, "scope": {"type": "$scopeinfo"}}}}}
        models = {"modules": {"DFF_X1": {"attributes": {"area": "4.0"}, "ports": {"Q": {"direction": "output"}}},
                              "AND2_X1": {"attributes": {"area": "2.0"}, "ports": {"Z": {"direction": "output"}}}}}
        self.assertEqual(mapping.validate_mapping(stats, mapped, models), (3, 10.0))
        cases = []
        bad = deepcopy(models)
        bad["modules"].pop("DFF_X1")
        cases.append((stats, mapped, bad, "functional library model"))
        bad = deepcopy(models)
        bad["modules"]["DFF_X1"].pop("ports")
        cases.append((stats, mapped, bad, "functional outputs"))
        bad = deepcopy(models)
        bad["modules"]["DFF_X1"]["attributes"]["blackbox"] = "1"
        cases.append((stats, mapped, bad, "black box"))
        bad = deepcopy(models)
        bad["modules"]["DFF_X1"]["attributes"].pop("area")
        cases.append((stats, mapped, bad, "invalid library area"))
        for field, value, message in [("num_submodules", 1, "flat"), ("num_cells", 3, "count mismatch"),
                                      ("area", 1.0, "area"), ("area", float("nan"), "area")]:
            bad = deepcopy(stats)
            bad[field] = value
            cases.append((bad, mapped, models, message))
        bad = deepcopy(mapped)
        bad["modules"]["fixed_linear_fixture"]["cells"]["a"]["type"] = "UNKNOWN_CELL"
        cases.append((stats, bad, models, "actual mapped instances"))
        for stats_case, mapped_case, model_case, message in cases:
            with self.subTest(message=message), self.assertRaisesRegex(ValueError, message):
                mapping.validate_mapping(stats_case, mapped_case, model_case)

    def test_missing_tools_records_failure_and_preserves_prior_receipt(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            sample, library, sources = self.prepare(root)
            out = root / "run"
            with patch.object(mapping, "SOURCES", sources), patch.object(mapping.shutil, "which", return_value=None):
                with self.assertRaisesRegex(ValueError, "required"):
                    mapping.observe(sample, library, out)
            original = (out / "mapping.json").read_bytes()
            receipt = json.loads(original)
            self.assertEqual(receipt["status"], "failed")
            self.assertEqual(receipt["checks"], {"input_validation": "passed"})
            self.assertEqual(receipt["failure_stage"], "mapping")
            self.assertFalse(receipt["manufacturable"])
            with self.assertRaises(FileExistsError):
                mapping.observe(sample, library, out)
            self.assertEqual((out / "mapping.json").read_bytes(), original)


if __name__ == "__main__":
    unittest.main()
