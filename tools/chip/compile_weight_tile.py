#!/usr/bin/env python3
"""Compile a bounded pinned BF16 checkpoint slice to an experimental W4 tile.

No model code executes. Payload hashes cover sampled bytes, not whole shards.
Synthetic activation simulation establishes integer lowering, not model quality.
"""

import argparse
from fractions import Fraction
import hashlib
import importlib.util
import json
from pathlib import Path
import struct
import subprocess
import sys

import tensor_inventory as inventory

REPO = Path(__file__).resolve().parents[2]
HARDWARE = REPO / "hardware/llm-asic"
MODEL = "Qwen/Qwen3.8-27B"
REVISION = "1d4bf0f2ff6012fd82039f2fa52739d0dd7c60c0"
TENSOR = "model.language_model.layers.0.mlp.gate_proj.weight"
SHAPE = [17408, 5120]
MAX_SAMPLE_BYTES = 4096


def sha(raw):
    return hashlib.sha256(raw).hexdigest()


def bf16(word):
    """Decode little-endian BF16 storage to an exact finite rational."""
    exponent = (word >> 7) & 255
    mantissa = word & 127
    if exponent == 255:
        raise ValueError("non-finite BF16 sample")
    value = Fraction(mantissa if exponent == 0 else 128 + mantissa)
    power = -133 if exponent == 0 else exponent - 134
    value *= Fraction(2) ** power
    return -value if word & 32768 else value


def round_even(value):
    """Exact nearest-integer rounding, with ties to an even integer."""
    lower, remainder = divmod(value.numerator, value.denominator)
    twice = remainder * 2
    return lower + int(twice > value.denominator or
                       (twice == value.denominator and lower % 2 != 0))


def quantize(row):
    maximum = max(map(abs, row))
    scale = maximum / 7 if maximum else Fraction(1)
    weights = [max(-7, min(7, round_even(value / scale))) for value in row]
    return weights, scale


def ratio(value):
    return {"numerator": value.numerator, "denominator": value.denominator}


def hardware_module():
    sys.path.insert(0, str(HARDWARE))
    spec = importlib.util.spec_from_file_location("tile_contract_verifier", HARDWARE / "verify_tile.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def compile_tile(snapshot, out, rows=4, columns=128, first_row=0, first_column=0):
    if (any(type(value) is not int for value in [rows, columns, first_row, first_column])
            or not 1 <= rows <= 8 or not 1 <= columns <= 128
            or first_row < 0 or first_column < 0
            or first_row + rows > SHAPE[0] or first_column + columns > SHAPE[1]
            or rows * columns * 2 > MAX_SAMPLE_BYTES):
        raise ValueError("invalid or oversized sample coordinates")
    if (not isinstance(snapshot, dict) or snapshot.get("model") != MODEL
            or snapshot.get("revision") != REVISION):
        raise ValueError("compiler requires the pinned target checkpoint")
    metadata = inventory.inventory(snapshot)
    tensor = metadata["tensors"].get(TENSOR)
    if tensor is None or tensor["shape"] != SHAPE or tensor["dtype"] != "BF16":
        raise ValueError("target tensor must have the expected BF16 shape")
    shard = metadata["shards"][tensor["shard"]]
    url = shard["source_url"]
    prefix, total = inventory.read_range(url, 0, 7)
    length = struct.unpack("<Q", prefix)[0]
    if not 0 < length <= inventory.MAX_HEADER or 8 + length > total:
        raise ValueError("invalid safetensors header length")
    raw_header, second_total = inventory.read_range(url, 8, 7 + length)
    if (second_total != total or total - 8 - length != shard["payload_bytes"]
            or sha(raw_header) != shard["header_sha256"]):
        raise ValueError("shard header identity or size changed")
    descriptor = inventory.parse_json(raw_header).get(TENSOR)
    inventory.tensor_storage(descriptor)
    if descriptor["shape"] != SHAPE or descriptor["dtype"] != "BF16":
        raise ValueError("refetched tensor layout changed")
    payload_base = 8 + length
    begin, end = descriptor["data_offsets"]
    if not 0 <= begin <= end <= shard["payload_bytes"]:
        raise ValueError("tensor offsets exceed validated shard")
    samples, ranges = [], []
    for row in range(first_row, first_row + rows):
        start = payload_base + begin + (row * SHAPE[1] + first_column) * 2
        finish = start + columns * 2 - 1
        if finish >= payload_base + end:
            raise ValueError("sample exceeds tensor span")
        raw, observed_total = inventory.read_range(url, start, finish)
        if observed_total != total:
            raise ValueError("shard size changed during payload sampling")
        samples.append(raw)
        ranges.append({"row": row, "start": start, "end": finish, "sha256": sha(raw)})
    raw_sample = b"".join(samples)
    decoded = [[bf16(word[0]) for word in struct.iter_unpack("<H", raw)] for raw in samples]
    quantized = [quantize(row) for row in decoded]
    weights = [item[0] for item in quantized]
    module = hardware_module()
    _, inputs, _ = module.fixtures(rows, columns)
    expected = [module.group_dot(vector, weights) for vector in inputs]
    out.mkdir(parents=True, exist_ok=False)
    artifacts = {
        "sample.bf16": raw_sample,
        "fixed_linear_fixture.sv": module.wrapper(weights).replace(
            f"Generated SYNTHETIC coefficient fixture, seed {module.SEED}.",
            "Generated EXPERIMENTAL W4 coefficients from a bounded real BF16 sample."),
        "tb.sv": module.bench(inputs, expected),
        "vectors.json": json.dumps({"synthetic_activations": True, "weights": weights,
                                     "inputs": inputs, "expected": expected}, indent=2),
    }
    for name, data in artifacts.items():
        (out / name).write_bytes(data if isinstance(data, bytes) else data.encode())
    record = {
        "schema_version": 1, "evidence": "checkpoint_weight_tile", "model": MODEL,
        "revision": REVISION, "config_sha256": metadata["config_sha256"],
        "index_canonical_sha256": metadata["index_canonical_sha256"],
        "header_sha256": shard["header_sha256"], "source_url": url,
        "shard_total_bytes": total, "tensor": TENSOR, "tensor_shape": SHAPE,
        "source_dtype": "BF16", "first_row": first_row, "first_column": first_column,
        "rows": rows, "columns": columns, "sample_bytes": len(raw_sample), "ranges": ranges,
        "quantization": {"experimental": True, "symmetric_range": [-7, 7],
                         "rounding": "nearest_ties_even", "scale": "per sampled row group: absmax/7; all-zero scale=1",
                         "group_scales_exact": [ratio(item[1]) for item in quantized]},
        "decoded_exact": [[ratio(value) for value in row] for row in decoded],
        "source_sha256": {"tools/chip/compile_weight_tile.py": sha(Path(__file__).read_bytes()),
                          "tools/chip/tensor_inventory.py": sha(Path(inventory.__file__).read_bytes()),
                          "hardware/llm-asic/rtl/integer_group_tile.sv": sha((HARDWARE / "rtl/integer_group_tile.sv").read_bytes()),
                          "hardware/llm-asic/verify_tile.py": sha((HARDWARE / "verify_tile.py").read_bytes()),
                          "hardware/llm-asic/integer_reference.py": sha((HARDWARE / "integer_reference.py").read_bytes())},
        "artifact_sha256": {name: sha((out / name).read_bytes()) for name in artifacts},
        "limitations": ["Payload identity covers sampled ranges only; complete checkpoint payload is unverified.",
                        "Group scaling and activation quantization are not implemented by this integer tile.",
                        "Scales describe these sampled groups, not a calibrated full-model quantization.",
                        "Synthetic activations do not establish numerical layer error or coding quality.",
                        "No physical PPA, throughput or manufacturability measurement."],
    }
    (out / "sample.json").write_text(json.dumps(record, sort_keys=True, indent=2) + "\n")
    return record


def verify_existing(out, toolchain_image=None, run_out=None):
    """Observe a sample in a new immutable run, including failed validation."""
    if run_out is None:
        raise ValueError("a new verification run directory is required")
    run_out.mkdir(parents=True, exist_ok=False)
    receipt = run_out / "verification.json"
    result = {"schema_version": 1, "evidence": "checkpoint_tile_integer_contract", "status": "running",
              "input_directory": str(out), "run_directory": str(run_out), "checks": {},
              "input_validation": "pending", "failure_stage": "input_validation",
              "declared_toolchain_image": toolchain_image,
              "verifier_sha256": sha(Path(__file__).read_bytes()),
              "excludes": ["full_checkpoint_integrity", "group_rescaling", "activation_quantization",
                           "full_model_execution", "coding_quality", "physical_ppa"]}
    receipt.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    try:
        with (out / "sample.json").open("rb") as source:
            raw_manifest = source.read(inventory.MAX_HEADER + 1)
        if len(raw_manifest) > inventory.MAX_HEADER:
            raise ValueError("sample manifest exceeds metadata bound")
        result["sample_manifest_sha256"] = sha(raw_manifest)
        record = inventory.parse_json(raw_manifest)
        if not isinstance(record, dict) or record.get("evidence") != "checkpoint_weight_tile":
            raise ValueError("not a compiled weight tile")
        # Tools consume the exact validated snapshot, never mutable input paths.
        inputs = run_out / "inputs"
        inputs.mkdir()
        (inputs / "sample.json").write_bytes(raw_manifest)
        for name in ["sample.bf16", "fixed_linear_fixture.sv", "tb.sv", "vectors.json"]:
            with (out / name).open("rb") as source:
                raw = source.read(inventory.MAX_HEADER + 1)
            if len(raw) > inventory.MAX_HEADER or sha(raw) != record["artifact_sha256"][name]:
                raise ValueError("compiled artifact identity changed")
            (inputs / name).write_bytes(raw)
        rtl = HARDWARE / "rtl/integer_group_tile.sv"
        raw_rtl = rtl.read_bytes()
        if sha(raw_rtl) != record["source_sha256"]["hardware/llm-asic/rtl/integer_group_tile.sv"]:
            raise ValueError("production RTL identity changed")
        rtl = inputs / "integer_group_tile.sv"
        rtl.write_bytes(raw_rtl)
        result["input_validation"] = "passed"
        result["synthetic_activations"] = True
        result["failure_stage"] = "tool_execution"
        module = hardware_module()
        import shutil
        verilator, yosys = shutil.which("verilator"), shutil.which("yosys")
        if not verilator or not yosys:
            raise ValueError("Verilator and Yosys required; RTL cannot be skipped")
        result["tools"] = {
            name: {"version": module.run([binary, version], run_out / (name + "-version.log")).strip(),
                   "executable_sha256": sha(Path(binary).resolve().read_bytes())}
            for name, binary, version in [("verilator", verilator, "--version"), ("yosys", yosys, "-V")]
        }
        fixture, tb = inputs / "fixed_linear_fixture.sv", inputs / "tb.sv"
        netlist = run_out / "synthesized.v"
        if any('"' in str(path) or '\n' in str(path) for path in [rtl, fixture, netlist]):
            raise ValueError("unsupported EDA source path")
        for tag, sources in [("rtl", [rtl, fixture]), ("netlist", [netlist])]:
            if tag == "netlist":
                script = f'read_verilog -sv -DSYNTHESIS "{rtl}" "{fixture}"; synth -top fixed_linear_fixture; check -assert; write_verilog -noattr "{netlist}"'
                module.run([yosys, "-p", script], run_out / "synthesis.log")
                result["checks"]["generic_synthesis"] = "passed"
            obj = run_out / ("obj-" + tag)
            module.run([verilator, "--binary", "--timing", "--assert", "--timescale-override", "1ns/1ps",
                        "--top-module", "tb", "--Mdir", str(obj), "-o", "tile-sim",
                        *map(str, sources), str(tb)], run_out / (tag + "-compile.log"), 180)
            text = module.run([str(obj / "tile-sim")], run_out / (tag + "-simulation.log"))
            if "RTL_PASS " not in text:
                raise ValueError("missing simulator proof marker")
            result["checks"][tag + "_vs_integer_oracle"] = "passed"
        result["status"] = "passed"
        result.pop("failure_stage")
    except BaseException as error:
        result["status"] = "cancelled" if isinstance(error, KeyboardInterrupt) else "failed"
        result["failure"] = str(error)
        if result["input_validation"] == "pending":
            result["input_validation"] = "failed"
        raise
    finally:
        result["artifact_sha256"] = {path.name: sha(path.read_bytes()) for path in run_out.glob("*.log")}
        if (run_out / "synthesized.v").exists():
            result["artifact_sha256"]["synthesized.v"] = sha((run_out / "synthesized.v").read_bytes())
        receipt.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("snapshot", type=Path, nargs="?")
    parser.add_argument("--out", type=lambda value: Path(value).resolve(), required=True)
    parser.add_argument("--verify-existing", action="store_true")
    parser.add_argument("--run-out", type=lambda value: Path(value).resolve(),
                        help="New directory for an immutable verification receipt and outputs")
    parser.add_argument("--toolchain-image", help="Caller-declared immutable container identity")
    args = parser.parse_args()
    try:
        if args.verify_existing:
            result = verify_existing(args.out, args.toolchain_image, args.run_out)
        else:
            if args.snapshot is None:
                raise ValueError("snapshot required")
            with args.snapshot.open("rb") as source:
                raw = source.read(inventory.MAX_HEADER + 1)
            if len(raw) > inventory.MAX_HEADER:
                raise ValueError("snapshot exceeds metadata bound")
            result = compile_tile(inventory.parse_json(raw), args.out)
        print(json.dumps({"evidence": result["evidence"], "out": result.get("run_directory", str(args.out)), "status": result.get("status", "compiled")}, sort_keys=True))
    except (ValueError, OSError, KeyError, RuntimeError, subprocess.SubprocessError) as error:
        receipt = f"; receipt: {args.run_out / 'verification.json'}" if args.run_out and not isinstance(error, FileExistsError) and (args.run_out / "verification.json").exists() else ""
        parser.exit(1, f"weight tile failed: {error}{receipt}\n")


if __name__ == "__main__":
    main()
