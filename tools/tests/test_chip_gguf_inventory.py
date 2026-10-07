"""Malformed layouts must fail before an inference engine sees the file."""

import hashlib
from pathlib import Path
import struct
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "chip"))
import gguf_inventory as inventory


def string(value):
    raw = value.encode()
    return struct.pack("<Q", len(raw)) + raw


def fixture(tensors=None, metadata=None):
    if tensors is None:
        tensors = [("norm", [4], 0, 0), ("weight", [256, 2], 12, 32)]
    if metadata is None:
        metadata = [("general.architecture", 8, string("qwen35")),
                    ("tokenizer.chat_template", 8, string("{{ messages }}")),
                    ("tokenizer.ggml.tokens", 9, struct.pack("<IQ", 8, 2) + string("one") + string("two"))]
    raw = b"GGUF" + struct.pack("<IQQ", 3, len(tensors), len(metadata))
    for key, kind, value in metadata:
        raw += string(key) + struct.pack("<I", kind) + value
    for name, shape, kind, offset in tensors:
        raw += string(name) + struct.pack("<I", len(shape))
        raw += struct.pack("<" + "Q" * len(shape), *shape) + struct.pack("<IQ", kind, offset)
    raw += b"\0" * (-len(raw) % 32)
    return raw + b"\0" * 320


class GGUFInventoryTests(unittest.TestCase):
    def inspect(self, raw):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "model.gguf"
            path.write_bytes(raw)
            return inventory.inspect(path)

    def test_mixed_precision_and_template_tokenizer_identity(self):
        result = self.inspect(fixture())
        self.assertEqual(result["storage"]["Q4_K"], {"tensors": 1, "elements": 512, "payload_bytes": 288})
        self.assertEqual(result["storage"]["F32"]["payload_bytes"], 16)
        self.assertEqual(result["chat_template_sha256"], hashlib.sha256(b"{{ messages }}").hexdigest())
        tokens = result["metadata"]["tokenizer.ggml.tokens"]
        self.assertEqual(tokens["count"], 2)
        self.assertEqual(tokens["encoded_sha256"], hashlib.sha256(string("one") + string("two")).hexdigest())

    def test_duplicate_keys_tensors_and_unknown_storage_are_refused(self):
        cases = [fixture(metadata=[("same", 4, struct.pack("<I", 1))] * 2),
                 fixture(tensors=[("same", [4], 0, 0), ("same", [4], 0, 32)]),
                 fixture(tensors=[("weight", [256], 999, 0)])]
        for raw in cases:
            with self.subTest(raw=raw[:24]), self.assertRaises(ValueError):
                self.inspect(raw)

    def test_overlapping_out_of_bounds_and_nonblock_rows_are_refused(self):
        cases = [fixture(tensors=[("a", [16], 0, 0), ("b", [256], 12, 32)]),
                 fixture(tensors=[("a", [256, 3], 12, 0)]),
                 fixture(tensors=[("a", [128, 2], 12, 0)])]
        for raw in cases:
            with self.subTest(raw=raw[:24]), self.assertRaises(ValueError):
                self.inspect(raw)

    def test_truncation_extra_payload_and_nonzero_padding_are_refused(self):
        raw = fixture()
        gap = bytearray(raw)
        data = self.inspect(raw)["data_offset"]
        gap[data + 16] = 1
        for invalid in (raw[:20], raw[:-50], raw + b"\0" * 32, bytes(gap)):
            with self.subTest(length=len(invalid)), self.assertRaises(ValueError):
                self.inspect(invalid)

    def test_unbounded_array_and_nonfinite_metadata_are_refused(self):
        cases = [fixture(metadata=[("array", 9, struct.pack("<IQ", 8, inventory.MAX_ITEMS + 1))]),
                 fixture(metadata=[("nan", 6, struct.pack("<f", float("nan")))]),
                 fixture(metadata=[("bool", 7, b"\x02")]),
                 fixture(metadata=[("general.alignment", 4, struct.pack("<I", 3))])]
        for raw in cases:
            with self.subTest(raw=raw[:24]), self.assertRaises(ValueError):
                self.inspect(raw)

    def test_full_blob_identity_is_checked_against_actual_bytes(self):
        raw = fixture()
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "model.gguf"
            path.write_bytes(raw)
            digest = hashlib.sha256(raw).hexdigest()
            self.assertEqual(inventory.inspect(path, digest)["full_file_sha256"], digest)
            # Layout remains valid after a payload edit; only a whole-blob
            # identity check can distinguish the changed actual weights.
            changed = bytearray(raw)
            changed[-1] = 1
            path.write_bytes(changed)
            with self.assertRaisesRegex(ValueError, "payload digest mismatch"):
                inventory.inspect(path, digest)


if __name__ == "__main__":
    unittest.main()
