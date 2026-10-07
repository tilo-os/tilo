"""Validate compiler artifacts by regenerating their arithmetic and testbench.

Hashes preserve bytes; this extra gate checks meaning. Source URLs/range hashes
remain recorded provenance, not offline authentication of checkpoint payloads.
"""

from fractions import Fraction
import hashlib
import json
import math
from pathlib import Path
import re
import struct

from tensor_inventory import parse_json

CONFIG_SHA256 = "137754ebd46991bec0c0d9e660dc98256be0b20f0e825b28c95c31de6142d041"
MODEL = "Qwen/Qwen3.8-27B"
REVISION = "1d4bf0f2ff6012fd82039f2fa52739d0dd7c60c0"
TENSOR = "model.language_model.layers.0.mlp.gate_proj.weight"
SHAPE = [17408, 5120]


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def identity(value):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) is not None


def ratio(value):
    return {"numerator": value.numerator, "denominator": value.denominator}


def validate_artifacts(raw_manifest, artifacts):
    """Validate bounded snapshot bytes and return the independently checked manifest."""
    manifest = parse_json(raw_manifest)
    if (not isinstance(manifest, dict) or type(manifest.get("schema_version")) is not int
            or manifest["schema_version"] != 1 or manifest.get("evidence") != "checkpoint_weight_tile"):
        raise ValueError("not a compiled checkpoint tile")
    names = ["sample.bf16", "fixed_linear_fixture.sv", "tb.sv", "vectors.json"]
    hashes = manifest.get("artifact_sha256")
    if not isinstance(hashes, dict) or set(hashes) != set(names):
        raise ValueError("invalid compiler artifact identities")
    for name in names:
        if not identity(hashes[name]) or digest(artifacts[name]) != hashes[name]:
            raise ValueError("compiled artifact identity changed")
    if (manifest.get("model") != MODEL or manifest.get("revision") != REVISION
            or manifest.get("config_sha256") != CONFIG_SHA256 or manifest.get("tensor") != TENSOR
            or manifest.get("tensor_shape") != SHAPE or manifest.get("source_dtype") != "BF16"):
        raise ValueError("pinned checkpoint identity or tensor layout mismatch")
    base = f"https://huggingface.co/{MODEL}/resolve/{REVISION}/"
    url = manifest.get("source_url")
    if not isinstance(url, str) or not url.startswith(base) or not re.fullmatch(r"[A-Za-z0-9_.-]+\.safetensors", url[len(base):]):
        raise ValueError("checkpoint source URL mismatch")
    for field in ["header_sha256", "index_canonical_sha256"]:
        if not identity(manifest.get(field)):
            raise ValueError("missing checkpoint metadata identity")
    rows, columns = manifest.get("rows"), manifest.get("columns")
    first_row, first_column = manifest.get("first_row"), manifest.get("first_column")
    if (any(type(value) is not int for value in [rows, columns, first_row, first_column])
            or not 1 <= rows <= 8 or not 1 <= columns <= 128 or first_row < 0 or first_column < 0
            or first_row + rows > SHAPE[0] or first_column + columns > SHAPE[1]):
        raise ValueError("invalid sampled dimensions")
    raw = artifacts["sample.bf16"]
    if type(manifest.get("sample_bytes")) is not int or manifest["sample_bytes"] != len(raw) or len(raw) != rows * columns * 2:
        raise ValueError("sample size disagrees with declared coordinates")
    ranges, total = manifest.get("ranges"), manifest.get("shard_total_bytes")
    if not isinstance(ranges, list) or len(ranges) != rows or type(total) is not int or not 0 < total < 2**64:
        raise ValueError("invalid sampled ranges")
    start0 = None
    for index, entry in enumerate(ranges):
        if not isinstance(entry, dict) or set(entry) != {"row", "start", "end", "sha256"}:
            raise ValueError("invalid sampled range descriptor")
        row, start, end = entry["row"], entry["start"], entry["end"]
        if (any(type(value) is not int for value in [row, start, end]) or row != first_row + index
                or not 0 <= start <= end < total or end - start + 1 != columns * 2):
            raise ValueError("sample range disagrees with row layout")
        if start0 is None:
            start0 = start
        if start != start0 + index * SHAPE[1] * 2 or digest(raw[index * columns * 2:(index + 1) * columns * 2]) != entry["sha256"]:
            raise ValueError("sample range stride or byte identity mismatch")
    source_hashes = manifest.get("source_sha256")
    required = ["tools/chip/compile_weight_tile.py", "tools/chip/tensor_inventory.py",
                "hardware/llm-asic/rtl/integer_group_tile.sv", "hardware/llm-asic/verify_tile.py",
                "hardware/llm-asic/integer_reference.py"]
    if not isinstance(source_hashes, dict) or any(not identity(source_hashes.get(name)) for name in required):
        raise ValueError("compiler source identities missing")
    hardware = Path(__file__).resolve().parents[2] / "hardware/llm-asic"
    for name in required[2:]:
        if digest((hardware / name.removeprefix("hardware/llm-asic/")).read_bytes()) != source_hashes[name]:
            raise ValueError("arithmetic reference or RTL source identity changed")
    # BF16 values are exactly representable in binary64, including subnormals.
    # This decoding route differs from the compiler's exponent/significand math.
    decoded = []
    for index in range(rows):
        row = []
        for (word,) in struct.iter_unpack("<H", raw[index * columns * 2:(index + 1) * columns * 2]):
            value = struct.unpack("<f", struct.pack("<I", word << 16))[0]
            if not math.isfinite(value):
                raise ValueError("non-finite BF16 sample")
            row.append(Fraction.from_float(value))
        decoded.append(row)
    scales, weights = [], []
    for row in decoded:
        maximum = max(map(abs, row))
        scale = maximum / 7 if maximum else Fraction(1)
        scales.append(ratio(scale))
        weights.append([max(-7, min(7, round(value / scale))) for value in row])
    expected_quantization = {"experimental": True, "symmetric_range": [-7, 7], "rounding": "nearest_ties_even",
                             "scale": "per sampled row group: absmax/7; all-zero scale=1", "group_scales_exact": scales}
    if manifest.get("quantization") != expected_quantization or manifest.get("decoded_exact") != [[ratio(value) for value in row] for row in decoded]:
        raise ValueError("sample decoding or quantization metadata disagrees with raw weights")
    from compile_weight_tile import hardware_module
    runner = hardware_module()
    _, inputs, _ = runner.fixtures(rows, columns)
    expected = [runner.group_dot(vector, weights) for vector in inputs]
    wrapper = runner.wrapper(weights).replace(
        f"Generated SYNTHETIC coefficient fixture, seed {runner.SEED}.",
        "Generated EXPERIMENTAL W4 coefficients from a bounded real BF16 sample.")
    vectors = {"synthetic_activations": True, "weights": weights, "inputs": inputs, "expected": expected}
    regenerated = {"fixed_linear_fixture.sv": wrapper.encode(), "tb.sv": runner.bench(inputs, expected).encode(),
                   "vectors.json": json.dumps(vectors, indent=2).encode()}
    if any(artifacts[name] != value for name, value in regenerated.items()):
        raise ValueError("compiler wrapper or oracle fixtures disagree with independent regeneration")
    return manifest
