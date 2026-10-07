#!/usr/bin/env python3
"""Read pinned safetensors headers with bounded HTTP ranges, never weight data."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import struct
import urllib.parse
import urllib.request

from model_profile import config_digest

MAX_HEADER = 4_000_000
MAX_U64 = (1 << 64) - 1
# Byte-addressable safetensors storage types. Packed logical weights stored in
# I32/U8 are counted as those storage elements, never as logical parameters.
# Unknown and sub-byte storage types require their own verified layout support.
DTYPE_BYTES = {"BOOL": 1, "I8": 1, "U8": 1, "I16": 2, "U16": 2,
               "I32": 4, "U32": 4, "I64": 8, "U64": 8,
               "F16": 2, "BF16": 2, "F32": 4, "F64": 8}


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def parse_json(raw):
    def invalid_constant(value):
        raise ValueError(f"invalid JSON constant: {value}")
    if isinstance(raw, bytes):
        raw = raw.decode("utf-8")
    try:
        return json.loads(raw, object_pairs_hook=unique_object, parse_constant=invalid_constant)
    except (TypeError, RecursionError) as error:
        raise ValueError("invalid or excessively nested JSON") from error


def read_url(url):
    with urllib.request.urlopen(url, timeout=30) as response:
        raw = response.read(MAX_HEADER + 1)
    if len(raw) > MAX_HEADER:
        raise ValueError("metadata exceeds 4 MB limit")
    return parse_json(raw)


def read_range(url, start, end):
    if (type(start) is not int or type(end) is not int
            or start < 0 or end < start or end - start + 1 > MAX_HEADER):
        raise ValueError("invalid or oversized header range")
    # Some intermediary caches do not vary their key by the Range header.
    request = urllib.request.Request(
        url + "?" + urllib.parse.urlencode({"header_range": f"{start}-{end}"}),
        headers={"Range": f"bytes={start}-{end}"},
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        match = re.fullmatch(r"bytes ([0-9]+)-([0-9]+)/([0-9]+)",
                             response.headers.get("Content-Range", ""))
        if (response.status != 206 or match is None
                or tuple(map(int, match.groups()[:2])) != (start, end)
                or not end < int(match.group(3)) <= MAX_U64):
            raise ValueError("server did not honor exact header range; refusing weight download")
        total = int(match.group(3))
        data = response.read(end - start + 2)
    if len(data) != end - start + 1:
        raise ValueError("truncated or oversized header range")
    return data, total


def read_header(url):
    prefix, total = read_range(url, 0, 7)
    length = struct.unpack("<Q", prefix)[0]
    if not 0 < length <= MAX_HEADER or 8 + length > total:
        raise ValueError("invalid or oversized safetensors header")
    raw, second_total = read_range(url, 8, 7 + length)
    if second_total != total:
        raise ValueError("shard size changed between header ranges")
    if not raw.startswith(b"{"):
        raise ValueError("safetensors header must start with an object")
    header = parse_json(raw)
    if not isinstance(header, dict):
        raise ValueError("safetensors header must be an object")
    return header, hashlib.sha256(raw).hexdigest(), total - 8 - length


def tensor_storage(tensor):
    if not isinstance(tensor, dict) or set(tensor) != {"shape", "data_offsets", "dtype"}:
        raise ValueError("invalid tensor descriptor")
    shape, offsets, dtype = tensor["shape"], tensor["data_offsets"], tensor["dtype"]
    if not isinstance(dtype, str) or dtype not in DTYPE_BYTES:
        raise ValueError("unsupported safetensors storage dtype")
    if not isinstance(shape, list) or any(type(n) is not int or not 0 <= n <= MAX_U64 for n in shape):
        raise ValueError("invalid tensor shape")
    elements = 0 if 0 in shape else 1
    if elements:
        for dimension in shape:
            elements *= dimension
            if elements > MAX_U64:
                raise ValueError("tensor element count exceeds supported range")
    if (not isinstance(offsets, list) or len(offsets) != 2
            or any(type(n) is not int or not 0 <= n <= MAX_U64 for n in offsets)
            or offsets[1] < offsets[0]):
        raise ValueError("invalid tensor offsets")
    size = offsets[1] - offsets[0]
    if size != elements * DTYPE_BYTES[dtype]:
        raise ValueError("tensor byte span disagrees with dtype and shape")
    return elements, size


def inventory(snapshot):
    if (not isinstance(snapshot, dict) or type(snapshot.get("schema_version")) is not int
            or snapshot["schema_version"] != 1):
        raise ValueError("unsupported snapshot schema")
    model, revision = snapshot.get("model"), snapshot.get("revision")
    if not isinstance(model, str) or not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", model):
        raise ValueError("invalid model identifier")
    if not isinstance(revision, str) or not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise ValueError("revision must be immutable")
    base = f"https://huggingface.co/{model}/resolve/{revision}/"
    if snapshot.get("config_url") != base + "config.json":
        raise ValueError("source identity mismatch")
    if not isinstance(snapshot.get("config"), dict) or config_digest(snapshot["config"]) != snapshot.get("config_sha256"):
        raise ValueError("config digest mismatch")
    index_url = base + "model.safetensors.index.json"
    index = read_url(index_url)
    mapping = index.get("weight_map") if isinstance(index, dict) else None
    if not isinstance(mapping, dict) or not mapping:
        raise ValueError("missing tensor map")
    if any(not isinstance(name, str) or not name or name == "__metadata__"
           or not isinstance(shard, str) for name, shard in mapping.items()):
        raise ValueError("invalid tensor map")
    tensors, shards = {}, {}
    for shard in sorted(set(mapping.values())):
        if not re.fullmatch(r"[A-Za-z0-9_.-]+\.safetensors", shard):
            raise ValueError("unsafe shard filename")
        header, digest, payload_size = read_header(base + shard)
        shards[shard] = {"header_sha256": digest, "source_url": base + shard,
                         "payload_bytes": payload_size}
        ranges = []
        for name, tensor in header.items():
            if name == "__metadata__":
                if not isinstance(tensor, dict) or any(not isinstance(v, str) for v in tensor.values()):
                    raise ValueError("invalid safetensors metadata")
                continue
            if name in tensors or mapping.get(name) != shard:
                raise ValueError("header and index disagree")
            elements, size = tensor_storage(tensor)
            begin, end = tensor["data_offsets"]
            ranges.append((begin, end))
            tensors[name] = {"shape": tensor["shape"], "dtype": tensor["dtype"],
                             "stored_elements": elements, "stored_bytes": size, "shard": shard}
        # Safetensors requires an entirely indexed payload, without holes:
        # https://github.com/safetensors/safetensors#format
        frontier = 0
        for begin, end in sorted(ranges):
            if begin != frontier or end > payload_size:
                raise ValueError("tensor ranges overlap, contain holes or exceed shard bounds")
            frontier = end
        if frontier != payload_size:
            raise ValueError("tensor ranges do not cover the complete shard payload")
    if set(tensors) != set(mapping):
        raise ValueError("index references tensors missing from headers")
    return {
        "schema_version": 1, "model": model, "revision": revision,
        "config_sha256": snapshot["config_sha256"],
        "index_source_url": index_url, "index_canonical_sha256": config_digest(index),
        "evidence": "checkpoint_header_inventory", "shards": shards, "tensors": tensors,
        "tensor_count": len(tensors),
        "stored_elements": sum(t["stored_elements"] for t in tensors.values()),
        "stored_bytes": sum(t["stored_bytes"] for t in tensors.values()),
        "limitations": [
            "Headers describe storage, not logical parameters of packed quantized weights.",
            "Only explicitly supported byte-addressable storage dtypes were validated.",
            "Weight payloads were not downloaded or verified.",
            "Source URLs and digests establish provenance and integrity, not publisher authentication.",
            "This is not a traced operator graph, physical design or quality measurement.",
        ],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("snapshot", type=Path)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    try:
        with args.snapshot.open("rb") as source:
            raw = source.read(MAX_HEADER + 1)
        if len(raw) > MAX_HEADER:
            raise ValueError("snapshot exceeds 4 MB limit")
        result = inventory(parse_json(raw))
        args.out.parent.mkdir(parents=True, exist_ok=True)
        with args.out.open("x") as output:
            json.dump(result, output, indent=2, sort_keys=True)
            output.write("\n")
        print(f"Inventoried {result['tensor_count']} tensors in {len(result['shards'])} shards; no weight payload downloaded")
    except (ValueError, KeyError, OSError) as error:
        parser.exit(1, f"tensor inventory failed: {error}\n")


if __name__ == "__main__":
    main()
