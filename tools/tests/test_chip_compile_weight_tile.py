"""Exercise real coefficient compilation with bounded fake HTTP payloads."""

from fractions import Fraction
import importlib.util
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

TOOLS = Path(__file__).resolve().parents[1] / "chip"
sys.path.insert(0, str(TOOLS))
SPEC = importlib.util.spec_from_file_location("compile_weight_tile", TOOLS / "compile_weight_tile.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class WeightCompilerTests(unittest.TestCase):
    def test_exact_bf16_decoder_and_ties_even(self):
        for bits, expected in [(0x3f80, 1), (0xc000, -2), (0, 0), (0x8000, 0),
                               (1, Fraction(1, 2**133)), (0x3f81, Fraction(129, 128))]:
            self.assertEqual(MODULE.bf16(bits), expected)
        for bits in [0x7f80, 0xff80, 0x7fc0]:
            with self.assertRaisesRegex(ValueError, "non-finite"):
                MODULE.bf16(bits)
        self.assertEqual([MODULE.round_even(Fraction(n, 2)) for n in [-7, -5, -3, -1, 1, 3, 5, 7]],
                         [-4, -2, -2, 0, 0, 2, 2, 4])
        self.assertEqual(MODULE.quantize([Fraction(0), Fraction(0)]), ([0, 0], Fraction(1)))
        self.assertEqual(MODULE.quantize([Fraction(-7), Fraction(7), Fraction(1, 2), Fraction(3, 2)]),
                         ([-7, 7, 0, 2], Fraction(1)))

    def compile(self, out, bad_total=False, dtype="BF16", header_digest=None):
        descriptor = {"shape": MODULE.SHAPE, "dtype": dtype,
                      "data_offsets": [0, MODULE.SHAPE[0] * MODULE.SHAPE[1] * 2]}
        header = json.dumps({MODULE.TENSOR: descriptor}).encode()
        total = 8 + len(header) + descriptor["data_offsets"][1]
        url = "https://huggingface.co/" + MODULE.MODEL + "/resolve/" + MODULE.REVISION + "/model.safetensors"
        metadata = {"tensors": {MODULE.TENSOR: {"shape": MODULE.SHAPE, "dtype": dtype, "shard": "model.safetensors"}},
                    "shards": {"model.safetensors": {"source_url": url, "header_sha256": header_digest or MODULE.sha(header),
                                                    "payload_bytes": descriptor["data_offsets"][1]}},
                    "config_sha256": "c" * 64, "index_canonical_sha256": "d" * 64}
        calls = []
        def read_range(_url, start, end):
            calls.append((start, end))
            if start == 0:
                return struct.pack("<Q", len(header)), total
            if start == 8:
                return header, total
            return struct.pack("<HHHH", 0xbf80, 0x3f80, 0, 0x3f00), total + int(bad_total)
        snapshot = {"model": MODULE.MODEL, "revision": MODULE.REVISION}
        with patch.object(MODULE.inventory, "inventory", return_value=metadata), \
                patch.object(MODULE.inventory, "read_range", side_effect=read_range):
            result = MODULE.compile_tile(snapshot, out, rows=2, columns=4)
        return result, calls, header

    def test_compilation_preserves_real_bytes_offsets_scales_and_integer_constants(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory) / "compiled"
            result, calls, header = self.compile(out)
            base = 8 + len(header)
            self.assertEqual(calls[2:], [(base, base + 7), (base + 5120 * 2, base + 5120 * 2 + 7)])
            self.assertEqual(result["sample_bytes"], 16)
            self.assertEqual((out / "sample.bf16").read_bytes(), struct.pack("<HHHH", 0xbf80, 0x3f80, 0, 0x3f00) * 2)
            self.assertEqual(result["quantization"]["group_scales_exact"],
                             [{"numerator": 1, "denominator": 7}] * 2)
            vectors = json.loads((out / "vectors.json").read_text())
            self.assertEqual(vectors["weights"], [[-7, 7, 0, 4]] * 2)
            self.assertEqual(vectors["expected"][0], [-512, -512])
            self.assertIn(".WEIGHTS(32'h40794079)", (out / "fixed_linear_fixture.sv").read_text())
            self.assertFalse("SYNTHETIC coefficient" in (out / "fixed_linear_fixture.sv").read_text())
            for name, digest in result["artifact_sha256"].items():
                self.assertEqual(MODULE.sha((out / name).read_bytes()), digest)
            (out / "sample.bf16").write_bytes(b"changed")
            with self.assertRaisesRegex(ValueError, "artifact identity"):
                MODULE.verify_existing(out, run_out=Path(directory) / "failed-run")

    def test_failed_new_invocation_cannot_reuse_a_previous_success_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            out = base / "compiled"
            self.compile(out)
            previous = base / "previous-run"
            previous.mkdir()
            prior_receipt = previous / "verification.json"
            prior_bytes = b'{"status":"passed","sample_manifest_sha256":"prior-observation"}\n'
            prior_receipt.write_bytes(prior_bytes)
            # Reusing any completed run is refused, without rewriting evidence.
            with self.assertRaises(FileExistsError):
                MODULE.verify_existing(out, run_out=previous)
            self.assertEqual(prior_receipt.read_bytes(), prior_bytes)
            refused = subprocess.run(
                [sys.executable, str(TOOLS / "compile_weight_tile.py"), "--verify-existing",
                 "--out", str(out), "--run-out", str(previous)],
                capture_output=True, text=True, timeout=10,
            )
            self.assertNotEqual(refused.returncode, 0)
            self.assertNotIn("receipt:", refused.stderr, "a refused invocation cannot identify an old receipt as its result")
            (out / "sample.bf16").write_bytes(b"corrupted after prior observation")
            current = base / "current-run"
            with patch.object(MODULE, "hardware_module") as hardware:
                with self.assertRaisesRegex(ValueError, "artifact identity"):
                    MODULE.verify_existing(out, run_out=current)
                hardware.assert_not_called()
            result = json.loads((current / "verification.json").read_text())
            self.assertEqual(result["status"], "failed")
            self.assertEqual(result["input_validation"], "failed")
            self.assertEqual(result["failure_stage"], "input_validation")
            self.assertEqual(result["checks"], {})
            self.assertEqual(result["run_directory"], str(current))
            self.assertEqual(prior_receipt.read_bytes(), prior_bytes)
            # Malformed or missing source manifests also get their own failure.
            (out / "sample.json").write_bytes(b"[]")
            malformed = base / "malformed-run"
            with self.assertRaisesRegex(ValueError, "compiled weight tile"):
                MODULE.verify_existing(out, run_out=malformed)
            self.assertEqual(json.loads((malformed / "verification.json").read_text())["status"], "failed")
            (out / "sample.json").unlink()
            missing = base / "missing-run"
            with self.assertRaises(FileNotFoundError):
                MODULE.verify_existing(out, run_out=missing)
            self.assertEqual(json.loads((missing / "verification.json").read_text())["status"], "failed")

    def test_changed_size_header_unknown_dtype_and_unpinned_revision_fail(self):
        with tempfile.TemporaryDirectory() as directory:
            for kwargs in [{"bad_total": True}, {"dtype": "F16"}, {"header_digest": "a" * 64}]:
                out = Path(directory) / "not-created"
                with self.subTest(kwargs=kwargs), self.assertRaises(ValueError):
                    self.compile(out, **kwargs)
                self.assertFalse(out.exists())
            with patch.object(MODULE.inventory, "inventory") as fetch:
                for snapshot in [{"model": MODULE.MODEL, "revision": "main"},
                                 {"model": "Other/Model", "revision": MODULE.REVISION}]:
                    with self.assertRaisesRegex(ValueError, "pinned"):
                        MODULE.compile_tile(snapshot, Path(directory) / "out")
                fetch.assert_not_called()
                with self.assertRaisesRegex(ValueError, "coordinates"):
                    MODULE.compile_tile({}, Path(directory) / "out", rows=9)
                fetch.assert_not_called()


if __name__ == "__main__":
    unittest.main()
