#!/usr/bin/env python3
"""Offline, immutable shared-multiples comparison; research-library area only."""

import argparse
import json
from pathlib import Path
import shutil

import map_weight_tile as mapping
from compile_weight_tile import HARDWARE, hardware_module, sha
from tensor_inventory import parse_json

ALTERNATIVE = HARDWARE / "rtl/integer_group_tile_shared.sv"


def alternative_wrapper(raw):
    text = raw.decode()
    old = "integer_group_tile #(.LANES("
    if text.count(old) != 1:
        raise ValueError("expected exactly one canonical tile instantiation")
    return text.replace(old, "integer_group_tile_shared #(.LANES(", 1).encode()


def simulate(runner, verilator, sources, bench, out, name):
    obj = (out / ("obj-" + name)).resolve()
    runner.run([verilator, "--binary", "--timing", "--assert", "--timescale-override", "1ns/1ps",
                "--top-module", "tb", "--Mdir", str(obj), "-o", "sim",
                *[str(p.resolve()) for p in sources], str(bench.resolve())],
               out / (name + "-compile.log"), 180)
    result = runner.run([str(obj / "sim")], out / (name + "-simulation.log"))
    if "RTL_PASS " not in result:
        raise ValueError("simulation lacks integer/protocol oracle proof")


def exhaustive_fixture(runner, out):
    # GROUP_SIZE=1 observes every signed W4*A8 product separately, without
    # cancellation between products. Existing bench adds stalls/reset/overlap.
    weights = [[weight] for weight in range(-8, 8)]
    inputs = [[activation] for activation in range(-128, 128)]
    expected = [[activation[0] * row[0] for row in weights] for activation in inputs]
    fixture, bench = out / "exhaustive_fixture.sv", out / "exhaustive_tb.sv"
    fixture.write_bytes(alternative_wrapper(runner.wrapper(weights).encode()))
    bench.write_text(runner.bench(inputs, expected))
    return fixture, bench


def map_variant(runner, yosys, verilator, inputs, out, shared):
    out.mkdir()
    rtl = inputs / ("integer_group_tile_shared.sv" if shared else "integer_group_tile.sv")
    fixture = inputs / ("shared_fixture.sv" if shared else "fixed_linear_fixture.sv")
    lib, netlist, cells = inputs / mapping.LIBRARY, out / "mapped.v", out / "library_cells.v"
    q = mapping.quote
    script = (f"read_verilog -sv -DSYNTHESIS {q(rtl)} {q(fixture)}\n"
              "synth -top fixed_linear_fixture -flatten\n"
              f"dfflibmap -liberty {q(lib)}\nabc -liberty {q(lib)} -D 1000\n"
              f"read_liberty -lib {q(lib)}\nhierarchy -check -top fixed_linear_fixture\nclean\ncheck -assert\n"
              f"tee -o {q(out / 'stat.json')} stat -json -liberty {q(lib)}\n"
              f"write_json {q(out / 'mapped.json')}\nwrite_verilog -noattr {q(netlist)}\ndesign -reset\n"
              f"read_liberty -ignore_miss_func {q(lib)}\nproc\ncheck -assert\n"
              f"write_json {q(out / 'library_cells.json')}\nwrite_verilog -noattr {q(cells)}\n")
    (out / "map.ys").write_text(script)
    simulate(runner, verilator, [rtl, fixture], inputs / "tb.sv", out, "rtl")
    runner.run([yosys, "-Q", "-q", "-s", str((out / "map.ys").resolve())], out / "mapping.log", 180)
    count, area = mapping.validate_mapping(parse_json(mapping.bounded(out / "stat.json"))["design"],
                                          parse_json(mapping.bounded(out / "mapped.json")),
                                          parse_json(mapping.bounded(out / "library_cells.json")))
    simulate(runner, verilator, [netlist, cells], inputs / "tb.sv", out, "mapped")
    return {"status": "passed", "mapped_cell_count": count, "reported_library_area": area,
            "checks": {"rtl_vs_integer_oracle": "passed", "library_mapping": "passed",
                       "mapped_netlist_vs_integer_oracle": "passed"}}


def observe(sample, library, out, toolchain_image=None):
    out.mkdir(parents=True, exist_ok=False)
    receipt = out / "comparison.json"
    result = {"schema_version": 1, "evidence": "shared_w4_products_comparison", "status": "running",
              "checks": {}, "variants": {}, "failure_stage": "input_validation",
              "comparison_tool_sha256": sha(Path(__file__).read_bytes()),
              "oracle_generator_sha256": sha((HARDWARE / "verify_tile.py").read_bytes()),
              "integer_reference_sha256": sha((HARDWARE / "integer_reference.py").read_bytes()),
              "mapping_tool_sha256": sha(Path(mapping.__file__).read_bytes()),
              "artifact_validator_sha256": sha((Path(__file__).parent / "weight_tile_artifact.py").read_bytes()),
              "declared_toolchain_image": toolchain_image, "library_revision": mapping.REVISION,
              "abc_delay_target_ps": 1000, "manufacturable": False,
              "excludes": ["physical_area", "physical_power", "timing_closure", "full_model_area",
                           "group_rescaling", "coding_quality", "offline_checkpoint_payload_authentication"]}
    receipt.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    try:
        inputs = out / "inputs"
        inputs.mkdir()
        # Reject modified, rehashed or marker-only source fixtures before EDA.
        result["input_sha256"] = mapping.snapshot(sample, library, inputs)
        manifest = parse_json(mapping.bounded(inputs / "sample.json"))
        result["sample"] = {key: manifest[key] for key in
                            ["model", "revision", "rows", "columns", "sample_bytes", "tensor", "first_row", "first_column"]}
        alt = mapping.bounded(ALTERNATIVE)
        (inputs / ALTERNATIVE.name).write_bytes(alt)
        (inputs / "shared_fixture.sv").write_bytes(alternative_wrapper((inputs / "fixed_linear_fixture.sv").read_bytes()))
        result["checks"]["canonical_input_validation"] = "passed"
        runner = hardware_module()
        yosys, verilator = shutil.which("yosys"), shutil.which("verilator")
        if not yosys or not verilator:
            raise ValueError("Yosys and Verilator required; execution cannot be skipped")
        result["tools"] = {name: {"version": runner.run([binary, flag], out / (name + "-version.log")).strip(),
                                  "executable_sha256": sha(Path(binary).resolve().read_bytes())}
                           for name, binary, flag in [("yosys", yosys, "-V"), ("verilator", verilator, "--version")]}
        result["failure_stage"] = "exhaustive_signed_products"
        fixture, bench = exhaustive_fixture(runner, inputs)
        simulate(runner, verilator, [inputs / ALTERNATIVE.name, fixture], bench, out, "exhaustive")
        result["checks"]["all_4096_signed_products_and_protocol"] = "passed"
        for shared, name in [(False, "baseline"), (True, "shared_multiples")]:
            result["failure_stage"] = name
            result["variants"][name] = map_variant(runner, yosys, verilator, inputs, out / name, shared)
        base = result["variants"]["baseline"]["reported_library_area"]
        alt_area = result["variants"]["shared_multiples"]["reported_library_area"]
        result["area_change_percent"] = 100 * (alt_area / base - 1)
        result["status"] = "passed"
        result.pop("failure_stage")
    except BaseException as error:
        result["status"] = "cancelled" if isinstance(error, KeyboardInterrupt) else "failed"
        result["failure"] = str(error)
        raise
    finally:
        inputs = out / "inputs"
        if inputs.exists():
            result["input_sha256"] = {p.name: sha(p.read_bytes()) for p in inputs.iterdir() if p.is_file()}
        result["artifact_sha256"] = {str(p.relative_to(out)): sha(p.read_bytes()) for p in out.rglob("*")
                                     if p.is_file() and p != receipt and "obj-" not in str(p.relative_to(out))
                                     and not str(p.relative_to(out)).startswith("inputs/")}
        receipt.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ["sample", "library", "out"]:
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--toolchain-image")
    args = parser.parse_args()
    try:
        observe(args.sample, args.library, args.out, args.toolchain_image)
    except (OSError, ValueError, KeyError, TypeError, RuntimeError) as error:
        parser.exit(2, f"compare_product_tiles: {error}\n")


if __name__ == "__main__":
    main()
