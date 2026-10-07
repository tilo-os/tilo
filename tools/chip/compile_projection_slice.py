#!/usr/bin/env python3
"""Bounded eight-row/full-input-width W4A8 numerical experiment, not a layer."""

import argparse
from fractions import Fraction
import importlib.util
import json
import math
from pathlib import Path
import random
import re
import shutil
import struct

import compile_weight_tile as single
import tensor_inventory as inventory
from weight_tile_artifact import CONFIG_SHA256

ROWS, COLUMNS, GROUP = 8, 5120, 128
PAYLOAD_BYTES = ROWS * COLUMNS * 2
SEED = 20261006


def reference():
    path = single.HARDWARE / "scaled_linear_reference.py"
    spec = importlib.util.spec_from_file_location("scaled_linear_reference", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def bounded(path, maximum=2 * 1024 * 1024):
    with path.open("rb") as stream:
        data = stream.read(maximum + 1)
    if len(data) > maximum:
        raise ValueError("projection artifact exceeds byte bound")
    return data


def pack_codes(rows):
    values = [value & 15 for row in rows for value in row]
    return bytes(values[index] | values[index + 1] << 4 for index in range(0, len(values), 2))


def compiled_data(raw):
    """Compiler route uses existing exact BF16 decoder and W4 quantizer."""
    rows = [[single.bf16(word[0]) for word in struct.iter_unpack("<H", raw[row * COLUMNS * 2:(row + 1) * COLUMNS * 2])]
            for row in range(ROWS)]
    codes, scales = [], []
    for row in rows:
        groups = [single.quantize(row[start:start + GROUP]) for start in range(0, COLUMNS, GROUP)]
        codes.append([value for integers, _ in groups for value in integers])
        scales.append([single.ratio(scale) for _, scale in groups])
    return pack_codes(codes), scales


def fetch(snapshot, out):
    if snapshot.get("model") != single.MODEL or snapshot.get("revision") != single.REVISION or snapshot.get("config_sha256") != CONFIG_SHA256:
        raise ValueError("full-input sample requires pinned model/config identity")
    metadata = inventory.inventory(snapshot)
    tensor = metadata["tensors"].get(single.TENSOR)
    if tensor is None or tensor["shape"] != single.SHAPE or tensor["dtype"] != "BF16":
        raise ValueError("target tensor must have pinned BF16 shape")
    shard = metadata["shards"][tensor["shard"]]
    url = shard["source_url"]
    prefix, total = inventory.read_range(url, 0, 7)
    length = struct.unpack("<Q", prefix)[0]
    if not 0 < length <= inventory.MAX_HEADER or 8 + length > total:
        raise ValueError("invalid shard header length")
    header, observed_total = inventory.read_range(url, 8, 7 + length)
    if observed_total != total or total - length - 8 != shard["payload_bytes"] or single.sha(header) != shard["header_sha256"]:
        raise ValueError("refetched shard identity changed")
    descriptor = inventory.parse_json(header).get(single.TENSOR)
    inventory.tensor_storage(descriptor)
    if descriptor["shape"] != single.SHAPE or descriptor["dtype"] != "BF16":
        raise ValueError("refetched tensor layout changed")
    begin, end = descriptor["data_offsets"]
    start = 8 + length + begin
    finish = start + PAYLOAD_BYTES - 1
    if not 0 <= begin <= end <= shard["payload_bytes"] or finish >= 8 + length + end:
        raise ValueError("bounded projection exceeds tensor span")
    raw, observed_total = inventory.read_range(url, start, finish)
    if observed_total != total or len(raw) != PAYLOAD_BYTES:
        raise ValueError("projection payload length or shard size changed")
    codes, scales = compiled_data(raw)
    artifacts = {"weights.bf16": raw, "coefficients.w4": codes,
                 "weight-scales.json": (json.dumps(scales, indent=2, sort_keys=True) + "\n").encode()}
    for name, data in artifacts.items():
        (out / name).write_bytes(data)
    manifest = {"schema_version": 1, "evidence": "bounded_full_input_projection_slice",
                "model": single.MODEL, "revision": single.REVISION, "config_sha256": CONFIG_SHA256,
                "index_canonical_sha256": metadata["index_canonical_sha256"], "header_sha256": shard["header_sha256"],
                "source_url": url, "tensor": single.TENSOR, "tensor_shape": single.SHAPE,
                "rows": ROWS, "columns": COLUMNS, "group_size": GROUP, "first_row": 0, "first_column": 0,
                "source_dtype": "BF16", "sample_bytes": PAYLOAD_BYTES, "shard_total_bytes": total,
                "header_length": length, "tensor_data_offsets": [begin, end],
                "range": {"start": start, "end": finish, "sha256": single.sha(raw)},
                "quantization": "experimental exact absmax/7 symmetric [-7,7], nearest ties even, zero scale1",
                "artifact_sha256": {name: single.sha(data) for name, data in artifacts.items()}}
    (out / "sample.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")


def validate_slice(sample):
    """Independent binary32 BF16 decode and exact quantization regeneration."""
    raw_manifest = bounded(sample / "sample.json")
    m = inventory.parse_json(raw_manifest)
    expected = {"schema_version": 1, "evidence": "bounded_full_input_projection_slice", "model": single.MODEL,
                "revision": single.REVISION, "config_sha256": CONFIG_SHA256, "tensor": single.TENSOR,
                "tensor_shape": single.SHAPE, "rows": ROWS, "columns": COLUMNS, "group_size": GROUP,
                "first_row": 0, "first_column": 0, "source_dtype": "BF16", "sample_bytes": PAYLOAD_BYTES,
                "quantization": "experimental exact absmax/7 symmetric [-7,7], nearest ties even, zero scale1"}
    if any(type(m.get(k)) is not type(v) or m[k] != v for k, v in expected.items()):
        raise ValueError("projection source identity/shape/quantization mismatch")
    base = f"https://huggingface.co/{single.MODEL}/resolve/{single.REVISION}/"
    url = m.get("source_url")
    if not isinstance(url, str) or not url.startswith(base) or not re.fullmatch(r"[A-Za-z0-9_.-]+\.safetensors", url[len(base):]):
        raise ValueError("projection source URL mismatch")
    for key in ["index_canonical_sha256", "header_sha256"]:
        if not isinstance(m.get(key), str) or not re.fullmatch(r"[0-9a-f]{64}", m[key]):
            raise ValueError("missing source metadata hash")
    length, total, offsets, span = m.get("header_length"), m.get("shard_total_bytes"), m.get("tensor_data_offsets"), m.get("range")
    if (type(length) is not int or not 0 < length <= inventory.MAX_HEADER or type(total) is not int or not 0 < total < 2**64
            or not isinstance(offsets, list) or len(offsets) != 2 or any(type(v) is not int for v in offsets)
            or not 0 <= offsets[0] < offsets[1] <= total - length - 8
            or offsets[1] - offsets[0] != single.SHAPE[0] * COLUMNS * 2
            or not isinstance(span, dict) or set(span) != {"start", "end", "sha256"}
            or type(span["start"]) is not int or span["start"] != length + 8 + offsets[0]
            or type(span["end"]) is not int or span["end"] != span["start"] + PAYLOAD_BYTES - 1):
        raise ValueError("projection storage range mismatch")
    names = ["weights.bf16", "coefficients.w4", "weight-scales.json"]
    if not isinstance(m.get("artifact_sha256"), dict) or set(m["artifact_sha256"]) != set(names):
        raise ValueError("projection artifact names mismatch")
    artifacts = {name: bounded(sample / name) for name in names}
    if any(single.sha(data) != m["artifact_sha256"][name] for name, data in artifacts.items()) or single.sha(artifacts["weights.bf16"]) != span["sha256"]:
        raise ValueError("projection artifact identity mismatch")
    raw = artifacts["weights.bf16"]
    if len(raw) != PAYLOAD_BYTES:
        raise ValueError("projection sample byte count mismatch")
    decoded = []
    for row in range(ROWS):
        values = []
        for word, in struct.iter_unpack("<H", raw[row * COLUMNS * 2:(row + 1) * COLUMNS * 2]):
            value = struct.unpack("<f", struct.pack("<I", word << 16))[0]
            if not math.isfinite(value):
                raise ValueError("non-finite BF16 projection weight")
            values.append(Fraction.from_float(value))
        decoded.append(values)
    weights, scales = [], []
    for row in decoded:
        integers, row_scales = [], []
        for start in range(0, COLUMNS, GROUP):
            group = row[start:start + GROUP]
            scale = max(map(abs, group)) / 7 or Fraction(1)
            integers.extend(max(-7, min(7, round(value / scale))) for value in group)
            row_scales.append(scale)
        weights.append(integers)
        scales.append(row_scales)
    canonical_scales = [[single.ratio(value) for value in row] for row in scales]
    if pack_codes(weights) != artifacts["coefficients.w4"] or inventory.parse_json(artifacts["weight-scales.json"]) != canonical_scales:
        raise ValueError("independent projection quantization regeneration mismatch")
    return m, decoded, weights, scales


def host_value(word):
    return struct.unpack("<f", struct.pack("<I", word))[0]


def host_bits(value):
    return struct.unpack("<I", struct.pack("<f", value))[0]


def independent_oracle(activation_words, weights, scales):
    """Host FP32 boundaries, separately decoded activations/ordered sums."""
    values = [Fraction.from_float(host_value(word << 16)) for word in activation_words]
    groups, scale_bits = [], []
    for start in range(0, COLUMNS, GROUP):
        group = values[start:start + GROUP]
        scale = max(map(abs, group)) / 127 or Fraction(1)
        groups.append([max(-127, min(127, round(value / scale))) for value in group])
        scale_bits.append(host_bits(float(scale)))
    outputs, partials = [], []
    for row, row_scales in zip(weights, scales):
        accumulator, row_partials = 0, []
        for group_index, activation in enumerate(groups):
            start = group_index * GROUP
            partial = sum(activation[index] * row[start + index] for index in range(GROUP))
            row_partials.append(partial)
            joint = host_bits(host_value(host_bits(float(row_scales[group_index]))) * host_value(scale_bits[group_index]))
            contribution = host_bits(host_value(host_bits(partial)) * host_value(joint))
            accumulator = host_bits(host_value(accumulator) + host_value(contribution))
        bf16 = (accumulator + 0x7fff + ((accumulator >> 16) & 1)) >> 16
        if bf16 & 0x7f80 == 0x7f80:
            raise FloatingPointError("independent BF16 oracle overflow")
        outputs.append({"fp32_bits": accumulator, "bf16_bits": bf16})
        partials.append(row_partials)
    return outputs, partials, values


def profiles(module):
    rng = random.Random(SEED)
    random_values = [rng.randrange(-128, 128) for _ in range(COLUMNS)]
    values = {"zero": [0] * COLUMNS,
              "sparse": [Fraction(3, 2) if index % 31 == 0 else (-2 if index % 31 == 1 else 0) for index in range(COLUMNS)],
              "uniform_signed_integer": random_values,
              "group_dynamic_range": [value * module.power_two(index // GROUP % 8 - 4) for index, value in enumerate(random_values)]}
    return {name: [module.round_bits(value, 7) for value in row] for name, row in values.items()}


def evaluate(sample, out):
    manifest, decoded, weights, scales = validate_slice(sample)
    module = reference()
    stimuli = profiles(module)
    (out / "synthetic-activations.json").write_text(json.dumps(stimuli, indent=2, sort_keys=True) + "\n")
    results = {}
    for name, words in stimuli.items():
        observed = module.linear_projection(words, weights, scales)
        outputs, partials, activation = independent_oracle(words, weights, scales)
        if observed["outputs"] != outputs or observed["integer_partials"] != partials:
            raise ValueError("independent FP32/BF16/partial oracle mismatch")
        differences = []
        for row, result in zip(decoded, outputs):
            direct = sum(a * w for a, w in zip(activation, row))
            fp32_error = module.finite_value(result["fp32_bits"], 23) - direct
            bf16_error = module.finite_value(result["bf16_bits"], 7) - direct
            differences.append({"unquantized_sample_dot_exact": single.ratio(direct),
                                "fp32_error_exact": single.ratio(fp32_error), "bf16_error_exact": single.ratio(bf16_error),
                                "bf16_absolute_error": float(abs(bf16_error))})
        results[name] = {"synthetic": True, "independent_numerical_oracle": "passed",
                         "projection": observed, "differences": differences}
    (out / "profiles.json").write_text(json.dumps(results, indent=2, sort_keys=True) + "\n")
    return manifest, results


def rtl_fixture(weights):
    """Forty baseline tiles form a verification harness, not chosen silicon."""
    instances = []
    for group in range(40):
        packed = 0
        for lane in range(ROWS):
            for index in range(GROUP):
                packed |= (weights[lane][group * GROUP + index] & 15) << (4 * (lane * GROUP + index))
        instances.append(f"""
    integer_group_tile #(.LANES(8), .GROUP_SIZE(128), .WEIGHTS(4096'h{packed:x})) tile_{group} (
        .clk(clk), .reset_n(reset_n), .activation(activation),
        .in_valid(in_valid && in_ready && input_group == 6'd{group}), .in_ready(bank_ready[{group}]),
        .out_valid(bank_valid[{group}]), .out_sums(bank_sums[{group}]),
        .out_ready(out_ready && pending_valid && output_group == 6'd{group}));
""")
    return """// GENERATED VERIFICATION HARNESS: duplicates 40 fixed group tiles.
// This elaboration is not a proposed accelerator architecture or area result.
module fixed_linear_fixture (
    input logic clk, reset_n, in_valid,
    output logic in_ready,
    input logic signed [7:0] activation,
    output logic out_valid,
    input logic out_ready,
    output logic [255:0] out_sums
);
    logic [5:0] input_group, output_group;
    logic [6:0] input_index;
    logic pending_valid;
    wire [39:0] bank_ready, bank_valid;
    wire [255:0] bank_sums [40];
    assign in_ready = reset_n && (!pending_valid || out_ready) && bank_ready[input_group];
    assign out_valid = pending_valid;
    assign out_sums = bank_sums[output_group];
    always_ff @(posedge clk) begin
        if (!reset_n) begin
            input_group <= 0; output_group <= 0; input_index <= 0; pending_valid <= 0;
        end else begin
            if (out_valid && out_ready) pending_valid <= 0;
            if (in_valid && in_ready) begin
                if (input_index == 127) begin
                    input_index <= 0; output_group <= input_group; pending_valid <= 1;
                    input_group <= input_group == 39 ? 6'd0 : input_group + 6'd1;
                end else input_index <= input_index + 7'd1;
            end
        end
    end
""" + "".join(instances) + "endmodule\n"


def verify_integer_rtl(sample, observations, out):
    _, _, weights, scales = validate_slice(sample)
    module, runner = reference(), single.hardware_module()
    verilator = shutil.which("verilator")
    if not verilator:
        raise ValueError("Verilator required; grouped RTL observation cannot be skipped")
    activations, expected = [], []
    for name, words in profiles(module).items():
        _, partials, _ = independent_oracle(words, weights, scales)
        integers = observations[name]["projection"]["activation_integers"]
        for group in range(40):
            activations.append(integers[group * GROUP:(group + 1) * GROUP])
            expected.append([row[group] for row in partials])
    fixture, bench = out / "grouped_fixture.sv", out / "grouped_tb.sv"
    fixture.write_text(rtl_fixture(weights))
    # Break the large activation literal before Verilator's per-line token limit.
    bench.write_text(runner.bench(activations, expected).replace(",8'h", ",\n8'h"))
    obj = (out / "obj-rtl").resolve()
    version = runner.run([verilator, "--version"], out / "verilator-version.log").strip()
    runner.run([verilator, "--binary", "--timing", "--assert", "--timescale-override", "1ns/1ps",
                "--top-module", "tb", "--Mdir", str(obj), "-o", "sim",
                str((single.HARDWARE / "rtl/integer_group_tile.sv").resolve()), str(fixture.resolve()), str(bench.resolve())],
               out / "rtl-compile.log", 240)
    text = runner.run([str(obj / "sim")], out / "rtl-simulation.log")
    if "RTL_PASS " not in text:
        raise ValueError("grouped RTL did not demonstrate oracle/protocol observations")
    return {"version": version, "executable_sha256": single.sha(Path(verilator).resolve().read_bytes()),
            "rtl_sha256": single.sha((single.HARDWARE / "rtl/integer_group_tile.sv").read_bytes()),
            "bench_generator_sha256": single.sha((single.HARDWARE / "verify_tile.py").read_bytes()),
            "groups": len(expected), "integer_output_checks": len(expected) * ROWS}


def observe(snapshot, out, existing=None, rtl=False, toolchain_image=None):
    out.mkdir(parents=True, exist_ok=False)
    receipt = out / "projection.json"
    result = {"schema_version": 1, "evidence": "bounded_full_input_projection_experiment", "status": "running", "checks": {},
              "compiler_sha256": single.sha(Path(__file__).read_bytes()),
              "single_group_compiler_sha256": single.sha(Path(single.__file__).read_bytes()),
              "range_reader_sha256": single.sha(Path(inventory.__file__).read_bytes()),
              "numerical_reference_sha256": single.sha((single.HARDWARE / "scaled_linear_reference.py").read_bytes()),
              "synthetic_activations": True, "seed": SEED, "declared_toolchain_image": toolchain_image,
              "excludes": ["complete_layer", "model_quality", "activation_calibration", "framework_gemm_equivalence", "hardware_scaler", "physical_ppa", "offline_checkpoint_payload_authentication"]}
    receipt.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    try:
        sample = out / "sample"
        sample.mkdir()
        if existing:
            validate_slice(existing)
            for name in ["sample.json", "weights.bf16", "coefficients.w4", "weight-scales.json"]:
                (sample / name).write_bytes(bounded(existing / name))
            result["network"] = "none; copied independently validated existing sample"
        else:
            fetch(snapshot, sample)
            result["network"] = "81,920 unique BF16 weight bytes plus index/header metadata; transfer exceeds weight payload"
        manifest, observations = evaluate(sample, out)
        result["source"] = manifest
        result["contract"] = reference().CONTRACT
        result["checks"] = {"independent_quantization_regeneration": "passed", "grouped_partials_vs_independent_oracle": "passed", "fp32_bf16_vs_independent_oracle": "passed"}
        if rtl:
            result["rtl_observation"] = verify_integer_rtl(sample, observations, out)
            result["checks"]["grouped_integer_rtl_vs_independent_oracle"] = "passed"
        else:
            result["excludes"].append("rtl_execution")
        result["profile_summaries"] = {name: {"max_bf16_absolute_error": max(row["bf16_absolute_error"] for row in profile["differences"])} for name, profile in observations.items()}
        result["status"] = "passed"
    except BaseException as error:
        result["status"] = "cancelled" if isinstance(error, KeyboardInterrupt) else "failed"
        result["failure"] = str(error)
        raise
    finally:
        result["artifact_sha256"] = {str(p.relative_to(out)): single.sha(p.read_bytes()) for p in out.rglob("*") if p.is_file() and p != receipt and "obj-rtl/" not in str(p.relative_to(out))}
        receipt.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--snapshot", type=Path)
    parser.add_argument("--existing", type=Path)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--rtl", action="store_true")
    parser.add_argument("--toolchain-image")
    args = parser.parse_args()
    try:
        if bool(args.snapshot) == bool(args.existing):
            raise ValueError("choose snapshot fetching or offline existing sample")
        snapshot = inventory.parse_json(bounded(args.snapshot)) if args.snapshot else None
        observe(snapshot, args.out, args.existing, args.rtl, args.toolchain_image)
    except (ValueError, OSError, KeyError, TypeError, RuntimeError, FloatingPointError) as error:
        parser.exit(2, f"projection experiment: {error}\n")


if __name__ == "__main__":
    main()
