#!/usr/bin/env python3
"""Bounded, read-only GGUF v3 layout inventory; never execute model code."""

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import struct

# Pinned format reference: ggml-org/llama.cpp at
# 5e03bdd8700948b9c41c54dd1b00f28a2aebc03f, gguf-py/gguf/constants.py.
# Only explicitly supported storage types are accepted; unknown types fail.
REFERENCE = "https://github.com/ggml-org/llama.cpp/blob/5e03bdd8700948b9c41c54dd1b00f28a2aebc03f/gguf-py/gguf/constants.py"
STORAGE = {0: ("F32", 1, 4), 1: ("F16", 1, 2), 2: ("Q4_0", 32, 18),
           3: ("Q4_1", 32, 20), 6: ("Q5_0", 32, 22), 7: ("Q5_1", 32, 24),
           8: ("Q8_0", 32, 34), 12: ("Q4_K", 256, 144),
           14: ("Q6_K", 256, 210), 30: ("BF16", 1, 2)}
SCALARS = {0: "B", 1: "b", 2: "H", 3: "h", 4: "I", 5: "i", 6: "f",
           7: "B", 10: "Q", 11: "q", 12: "d"}
MAX_HEADER = 64 * 1024 * 1024
MAX_ITEMS = 1_000_000


class Reader:
    def __init__(self, stream, size):
        self.stream, self.size = stream, size
        self.digest = hashlib.sha256()

    def read(self, size):
        if size < 0 or self.stream.tell() + size > min(self.size, MAX_HEADER):
            raise ValueError("truncated or oversized GGUF header")
        raw = self.stream.read(size)
        if len(raw) != size:
            raise ValueError("truncated GGUF header")
        self.digest.update(raw)
        return raw

    def number(self, fmt):
        return struct.unpack("<" + fmt, self.read(struct.calcsize(fmt)))[0]

    def string(self):
        length = self.number("Q")
        if length > 1024 * 1024:
            raise ValueError("GGUF string exceeds inventory limit")
        return self.read(length).decode("utf-8")

    def value(self, kind):
        if kind == 8:
            return self.string()
        if kind not in SCALARS:
            raise ValueError("unsupported or nested GGUF metadata type")
        value = self.number(SCALARS[kind])
        if kind == 7:
            if value not in (0, 1):
                raise ValueError("invalid GGUF boolean")
            return bool(value)
        if isinstance(value, float) and not math.isfinite(value):
            raise ValueError("nonfinite GGUF metadata")
        return value

    def field(self, kind):
        if kind != 9:
            return self.value(kind)
        subtype, count = self.number("I"), self.number("Q")
        if count > MAX_ITEMS or subtype not in (*SCALARS, 8):
            raise ValueError("unsupported or oversized GGUF array")
        start = self.stream.tell()
        for _ in range(count):
            self.value(subtype)
        end = self.stream.tell()
        # Preserve exact tokenizer-array identity without duplicating it into
        # the report. Re-reading header bytes does not read tensor payloads.
        self.stream.seek(start)
        digest = hashlib.sha256(self.stream.read(end - start)).hexdigest()
        self.stream.seek(end)
        return {"element_type": subtype, "count": count, "encoded_sha256": digest}


def inspect(path, expected_sha256=None):
    path = Path(path)
    with path.open("rb") as stream:
        before = os.fstat(stream.fileno())
        size = before.st_size
        reader = Reader(stream, size)
        if reader.read(4) != b"GGUF" or reader.number("I") != 3:
            raise ValueError("requires little-endian GGUF version 3")
        count, fields = reader.number("Q"), reader.number("Q")
        if not 0 < count <= 100_000 or not 0 < fields <= 100_000:
            raise ValueError("invalid GGUF tensor or metadata count")
        metadata, types = {}, {}
        for _ in range(fields):
            key, kind = reader.string(), reader.number("I")
            if not key or key in metadata:
                raise ValueError("empty or duplicate GGUF metadata key")
            metadata[key], types[key] = reader.field(kind), kind
        alignment = metadata.get("general.alignment", 32)
        if (types.get("general.alignment", 4) != 4 or type(alignment) is not int
                or not 1 <= alignment <= 4096 or alignment & (alignment - 1)):
            raise ValueError("invalid GGUF alignment")
        tensors, names, storage = [], set(), {}
        for _ in range(count):
            name, dims = reader.string(), reader.number("I")
            if not name or name in names or not 1 <= dims <= 4:
                raise ValueError("invalid or duplicate GGUF tensor")
            names.add(name)
            shape = [reader.number("Q") for _ in range(dims)]
            kind, offset = reader.number("I"), reader.number("Q")
            if kind not in STORAGE:
                raise ValueError("unsupported GGUF tensor storage type")
            label, block, byte_count = STORAGE[kind]
            elements = math.prod(shape)
            if (any(not 0 < d <= (1 << 32) for d in shape)
                    or elements > (1 << 63) or shape[0] % block or offset % alignment):
                raise ValueError("invalid GGUF block shape or tensor offset")
            payload = elements // block * byte_count
            tensors.append({"name": name, "shape_ggml_order": shape, "storage": label,
                            "elements": elements, "payload_bytes": payload, "relative_offset": offset})
            group = storage.setdefault(label, {"tensors": 0, "elements": 0, "payload_bytes": 0})
            group["tensors"] += 1
            group["elements"] += elements
            group["payload_bytes"] += payload
        header_end = stream.tell()
        data_start = (header_end + alignment - 1) // alignment * alignment
        if data_start > size or any(stream.read(data_start - header_end)):
            raise ValueError("truncated or nonzero GGUF header padding")
        end = data_start
        for tensor in sorted(tensors, key=lambda item: item["relative_offset"]):
            start = data_start + tensor["relative_offset"]
            if start < end or start - end >= alignment or start + tensor["payload_bytes"] > size:
                raise ValueError("overlapping, missing or out-of-bounds GGUF payload")
            stream.seek(end)
            if any(stream.read(start - end)):
                raise ValueError("nonzero GGUF alignment padding")
            end = start + tensor["payload_bytes"]
        if not 0 <= size - end < alignment:
            raise ValueError("unaccounted GGUF trailing data")
        stream.seek(end)
        if any(stream.read(size - end)):
            raise ValueError("nonzero GGUF trailing padding")
        actual = None
        if expected_sha256 is not None:
            stream.seek(0)
            actual = hashlib.file_digest(stream, "sha256").hexdigest()
            if actual != expected_sha256:
                raise ValueError("GGUF full payload digest mismatch")
        after = os.fstat(stream.fileno())
        if (before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (after.st_size, after.st_mtime_ns, after.st_ctime_ns):
            raise ValueError("GGUF file changed during inventory")
    template = metadata.get("tokenizer.chat_template")
    result = {"schema_version": 1, "evidence": "gguf_layout_inventory", "format_reference": REFERENCE,
            "file_bytes": size, "header_bytes": header_end, "header_sha256": reader.digest.hexdigest(),
            "data_offset": data_start, "alignment": alignment, "metadata": metadata,
            "chat_template_sha256": hashlib.sha256(template.encode()).hexdigest() if isinstance(template, str) else None,
            "storage": storage, "tensors": tensors,
            "excludes": ["checkpoint_conversion_lineage", "tensor_numerical_equivalence", "model_quality",
                         "full_payload_digest_without_explicit_hash_option", "ASIC_quantization_compatibility"]}
    if actual is not None:
        result["full_file_sha256"] = actual
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--expected-sha256")
    args = parser.parse_args()
    result = inspect(args.model, args.expected_sha256)
    with args.out.open("x") as stream:
        json.dump(result, stream, indent=2, sort_keys=True, allow_nan=False)
        stream.write("\n")


if __name__ == "__main__":
    main()
