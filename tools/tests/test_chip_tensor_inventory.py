"""Check bounded downloads and checkpoint/header consistency without a network."""

import importlib.util
import json
from pathlib import Path
import struct
import subprocess
import tempfile
import sys
import unittest
from unittest.mock import patch


TOOLS = Path(__file__).resolve().parents[1] / "chip"
sys.path.insert(0, str(TOOLS))
SPEC = importlib.util.spec_from_file_location("tensor_inventory", TOOLS / "tensor_inventory.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class Response:
    def __init__(self, content_range, data=b"12345678", status=206):
        self.status = status
        self.headers = {"Content-Range": content_range}
        self.data = data
        self.read_sizes = []

    def __enter__(self):
        return self

    def __exit__(self, *_):
        pass

    def read(self, size):
        self.read_sizes.append(size)
        return self.data[:size]


class TensorInventoryTests(unittest.TestCase):
    def record(self):
        config = {"model_type": "example"}
        return {"schema_version": 1, "model": "Org/Model", "revision": "a" * 40,
                "config": config, "config_sha256": MODULE.config_digest(config),
                "config_url": "https://huggingface.co/Org/Model/resolve/" + "a" * 40 + "/config.json"}

    def inventory(self, header, payload, mapping=None):
        index = {"weight_map": mapping or {name: "model.safetensors" for name in header
                                          if name != "__metadata__"}}
        with patch.object(MODULE, "read_url", return_value=index), \
                patch.object(MODULE, "read_header", return_value=(header, "b" * 64, payload)):
            return MODULE.inventory(self.record())

    def test_only_bounded_header_ranges_are_read(self):
        raw = json.dumps({"weight": {"shape": [2, 3], "dtype": "BF16", "data_offsets": [0, 12]}}).encode()
        total = 8 + len(raw) + 12
        with patch.object(MODULE, "read_range", side_effect=[(struct.pack("<Q", len(raw)), total),
                                                           (raw, total)]) as read:
            header, digest, payload = MODULE.read_header("https://example.org/model.safetensors")
        self.assertEqual(header["weight"]["shape"], [2, 3])
        self.assertEqual(len(digest), 64)
        self.assertEqual(payload, 12)
        self.assertEqual(read.call_args_list[1].args[1:], (8, 7 + len(raw)))
        with patch.object(MODULE, "read_range", return_value=(struct.pack("<Q", MODULE.MAX_HEADER + 1), total)) as read:
            with self.assertRaisesRegex(ValueError, "oversized"):
                MODULE.read_header("https://example.org/model.safetensors")
            self.assertEqual(read.call_count, 1)

    def test_server_ignoring_or_misreporting_range_is_rejected_before_reading(self):
        for content_range, status in [("", 200), ("bytes 0-7/*", 206),
                                      ("bytes 0-7/7", 206), ("bytes 8-15/100", 206),
                                      ("bytes 0-7/100 trailing", 206)]:
            with self.subTest(content_range=content_range):
                response = Response(content_range, status=status)
                with patch.object(MODULE.urllib.request, "urlopen", return_value=response):
                    with self.assertRaisesRegex(ValueError, "refusing weight download"):
                        MODULE.read_range("https://example.org/model.safetensors", 0, 7)
                self.assertEqual(response.read_sizes, [])

    def test_exact_range_cache_key_and_body_length(self):
        response = Response("bytes 0-7/100")
        with patch.object(MODULE.urllib.request, "urlopen", return_value=response) as request:
            data, total = MODULE.read_range("https://example.org/model.safetensors", 0, 7)
        self.assertEqual((data, total), (b"12345678", 100))
        self.assertEqual(response.read_sizes, [9])
        self.assertIn("header_range=0-7", request.call_args.args[0].full_url)
        self.assertEqual(request.call_args.args[0].get_header("Range"), "bytes=0-7")
        for body in [b"short", b"123456789"]:
            with patch.object(MODULE.urllib.request, "urlopen", return_value=Response("bytes 0-7/100", body)):
                with self.assertRaisesRegex(ValueError, "truncated or oversized"):
                    MODULE.read_range("https://example.org/model.safetensors", 0, 7)
        with patch.object(MODULE.urllib.request, "urlopen") as request:
            for start, end in [(-1, 7), (8, 7), (0, MODULE.MAX_HEADER), (True, 7)]:
                with self.assertRaises(ValueError):
                    MODULE.read_range("https://example.org/model.safetensors", start, end)
            request.assert_not_called()

    def test_header_duplicates_and_changing_file_size_fail(self):
        raw = b'{"x":{},"x":{}}'
        total = 8 + len(raw)
        with patch.object(MODULE, "read_range", side_effect=[(struct.pack("<Q", len(raw)), total), (raw, total)]):
            with self.assertRaisesRegex(ValueError, "duplicate JSON key"):
                MODULE.read_header("https://example.org/model.safetensors")
        with patch.object(MODULE, "read_range", side_effect=[(struct.pack("<Q", len(raw)), total), (raw, total + 1)]):
            with self.assertRaisesRegex(ValueError, "size changed"):
                MODULE.read_header("https://example.org/model.safetensors")
        with self.assertRaises(ValueError):
            MODULE.parse_json('{"x":NaN}')

    def test_valid_storage_counts_provenance_scalars_empty_and_packed_storage(self):
        header = {"weight": {"shape": [2, 3], "dtype": "BF16", "data_offsets": [0, 12]},
                  "scalar": {"shape": [], "dtype": "F32", "data_offsets": [12, 16]},
                  "empty": {"shape": [0, 100], "dtype": "I32", "data_offsets": [16, 16]},
                  "packed": {"shape": [2], "dtype": "I32", "data_offsets": [16, 24]}}
        result = self.inventory(header, 24)
        self.assertEqual(result["stored_bytes"], 24)
        self.assertEqual(result["stored_elements"], 9)
        self.assertEqual(result["tensors"]["packed"]["stored_elements"], 2)
        self.assertEqual(result["shards"]["model.safetensors"]["payload_bytes"], 24)
        self.assertIn("/resolve/" + "a" * 40 + "/", result["index_source_url"])
        self.assertEqual(result["index_canonical_sha256"], MODULE.config_digest({"weight_map": {
            name: "model.safetensors" for name in header}}))

    def test_index_mismatch_and_unsafe_filenames_fail(self):
        header = {"weight": {"shape": [2, 3], "dtype": "BF16", "data_offsets": [0, 12]}}
        with self.assertRaisesRegex(ValueError, "disagree"):
            self.inventory(header, 12, {"absent": "model.safetensors"})
        with self.assertRaisesRegex(ValueError, "unsafe"):
            self.inventory(header, 12, {"weight": "../escape.safetensors"})

    def test_shape_dtype_spans_and_layout_are_verified(self):
        for tensor in [{"shape": [2, 3], "dtype": "BF16", "data_offsets": [0, 999]},
                       {"shape": [2], "dtype": "F4", "data_offsets": [0, 1]},
                       {"shape": [True], "dtype": "U8", "data_offsets": [0, 1]},
                       {"shape": [-1], "dtype": "U8", "data_offsets": [0, 1]},
                       {"shape": [1 << 63, 2], "dtype": "U8", "data_offsets": [0, 1]},
                       {"shape": [1], "dtype": "U8", "data_offsets": [True, 1]},
                       None]:
            with self.subTest(tensor=tensor), self.assertRaises(ValueError):
                self.inventory({"weight": tensor}, 999)
        for offsets, payload in [([0, 4], 4), ([8, 12], 12), ([4, 8], 7), ([4, 8], 9)]:
            header = {"a": {"shape": [2], "dtype": "BF16", "data_offsets": [0, 4]},
                      "b": {"shape": [2], "dtype": "BF16", "data_offsets": offsets}}
            with self.subTest(offsets=offsets, payload=payload), self.assertRaises(ValueError):
                self.inventory(header, payload)

    def test_snapshot_and_index_structures_fail_cleanly(self):
        for record in [[], {}, dict(self.record(), schema_version=2), dict(self.record(), schema_version=True),
                       dict(self.record(), model=[]),
                       dict(self.record(), config=[]), dict(self.record(), revision=None)]:
            with self.subTest(record=record), self.assertRaises(ValueError):
                MODULE.inventory(record)
        for index in [[], {"weight_map": []}, {"weight_map": {"weight": []}},
                      {"weight_map": {"__metadata__": "model.safetensors"}}]:
            with patch.object(MODULE, "read_url", return_value=index), self.assertRaises(ValueError):
                MODULE.inventory(self.record())
        with self.assertRaisesRegex(ValueError, "metadata"):
            self.inventory({"weight": {"shape": [1], "dtype": "U8", "data_offsets": [0, 1]},
                            "__metadata__": {"unexpected": 42}}, 1)

    def test_cli_malformed_snapshot_fails_without_creating_report(self):
        with tempfile.TemporaryDirectory() as directory:
            snapshot = Path(directory) / "snapshot.json"
            output = Path(directory) / "report.json"
            snapshot.write_text("[]")
            result = subprocess.run([sys.executable, str(TOOLS / "tensor_inventory.py"),
                                     str(snapshot), "--out", str(output)],
                                    capture_output=True, text=True, timeout=10, check=False)
            self.assertEqual(result.returncode, 1)
            self.assertIn("unsupported snapshot schema", result.stderr)
            self.assertNotIn("Traceback", result.stderr)
            self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
