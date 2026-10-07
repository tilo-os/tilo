#!/usr/bin/env python3
"""Map a compiled checkpoint slice to a pinned, non-manufacturable research library."""

import argparse
import json
import math
from collections import Counter
from pathlib import Path
import shutil
import urllib.request

from compile_weight_tile import HARDWARE, hardware_module, sha
from tensor_inventory import parse_json
from weight_tile_artifact import validate_artifacts

REVISION = "e97f852b01dfcaaa1031510d1bc1f4df7bb90139"
BASE = f"https://raw.githubusercontent.com/The-OpenROAD-Project/OpenROAD-flow-scripts/{REVISION}/flow/platforms/nangate45/"
LIBRARY = "NangateOpenCellLibrary_typical.lib"
SOURCES = {
    LIBRARY: ("lib/" + LIBRARY, "8d540a4d4cf6d09d27c87ad067857a9c0c2eeb023ab7a56e058cd3113db4e9b1"),
    "LICENSE": ("LICENSE", "0d542e0c8804e39aa7f37eb00da5a762149dc682d7829451287e11b938e94594"),
    "README.md": ("README.md", "0e3c8a8cbe5429552764c6531dc454d64f6aa7c1f7128175b5f06070a3de578d"),
}
BOUND = 8 * 1024 * 1024


def bounded(path):
    with path.open("rb") as stream:
        data = stream.read(BOUND + 1)
    if len(data) > BOUND:
        raise ValueError("mapping input exceeds byte bound")
    return data


def fetch_library(out):
    out.mkdir(parents=True, exist_ok=False)
    for name, (relative, expected) in SOURCES.items():
        with urllib.request.urlopen(BASE + relative, timeout=30) as response:
            data = response.read(BOUND + 1)
        if len(data) > BOUND or sha(data) != expected:
            raise ValueError("pinned research library identity mismatch")
        (out / name).write_bytes(data)


def snapshot(sample, library, out):
    raw = bounded(sample / "sample.json")
    artifacts = {name: bounded(sample / name)
                 for name in ["sample.bf16", "fixed_linear_fixture.sv", "tb.sv", "vectors.json"]}
    manifest = validate_artifacts(raw, artifacts)
    identities = {"sample.json": sha(raw)}
    (out / "sample.json").write_bytes(raw)
    for name, data in artifacts.items():
        (out / name).write_bytes(data)
        identities[name] = sha(data)
    data = bounded(HARDWARE / "rtl/integer_group_tile.sv")
    if sha(data) != manifest["source_sha256"]["hardware/llm-asic/rtl/integer_group_tile.sv"]:
        raise ValueError("production RTL identity mismatch")
    (out / "integer_group_tile.sv").write_bytes(data)
    identities["integer_group_tile.sv"] = sha(data)
    for name, (_, expected) in SOURCES.items():
        data = bounded(library / name)
        if sha(data) != expected:
            raise ValueError("pinned research library identity mismatch")
        (out / name).write_bytes(data)
        identities[name] = sha(data)
    return identities



def validate_mapping(stats, mapped, models):
    """Every physical instance needs a functional pinned model and full area."""
    types = stats.get("num_cells_by_type")
    if not isinstance(types, dict) or not types:
        raise ValueError("mapping has no cell types")
    if any(not isinstance(name, str) or type(count) is not int or count <= 0 for name, count in types.items()):
        raise ValueError("invalid mapped cell counts")
    if any(name.startswith("$") and name != "$scopeinfo" for name in types):
        raise ValueError("mapping left generic hardware cells")
    for field in ["num_memories", "num_memory_bits", "num_processes", "num_submodules"]:
        if type(stats.get(field)) is not int or stats[field] != 0:
            raise ValueError("incomplete flat mapped cell statistics")
    if type(stats.get("num_cells")) is not int or stats["num_cells"] != sum(types.values()):
        raise ValueError("mapped cell count mismatch")
    top = mapped["modules"]["fixed_linear_fixture"]
    actual = Counter(cell["type"] for cell in top["cells"].values())
    if dict(actual) != types:
        raise ValueError("statistics disagree with actual mapped instances")
    area = 0.0
    count = 0
    for name, quantity in types.items():
        if name == "$scopeinfo":
            continue
        model = models.get("modules", {}).get(name)
        if model is None:
            raise ValueError("mapped cell lacks functional library model")
        ports = model.get("ports", {})
        if not isinstance(ports, dict) or not any(port.get("direction") == "output" for port in ports.values()):
            raise ValueError("mapped cell model lacks functional outputs")
        blackbox = str(model.get("attributes", {}).get("blackbox", "0"))
        if any(bit != "0" for bit in blackbox):
            raise ValueError("mapped cell model is a black box")
        cell_area = float(model.get("attributes", {}).get("area", "nan"))
        if not math.isfinite(cell_area) or cell_area <= 0:
            raise ValueError("mapped cell has missing or invalid library area")
        area += quantity * cell_area
        count += quantity
    reported = stats.get("area")
    if (not count or type(reported) not in (int, float) or not math.isfinite(reported)
            or reported <= 0 or not math.isclose(reported, area, rel_tol=1e-9, abs_tol=1e-6)):
        raise ValueError("reported mapped area is incomplete or inconsistent")
    return count, reported


def quote(path):
    value = str(path.resolve())
    if any(char in value for char in ['"', '\n', '\r', '\\', '\x00']):
        raise ValueError("unsupported EDA path")
    return '"' + value + '"'


def observe(sample, library, out, toolchain_image=None):
    out.mkdir(parents=True, exist_ok=False)
    result = {"schema_version": 1, "evidence": "research_library_checkpoint_tile",
              "status": "running", "checks": {}, "failure_stage": "input_validation",
              "mapper_sha256": sha(Path(__file__).read_bytes()),
              "artifact_validator_sha256": sha((Path(__file__).parent / "weight_tile_artifact.py").read_bytes()),
              "declared_toolchain_image": toolchain_image,
              "library_revision": REVISION, "manufacturable": False,
              "abc_delay_target_ps": 1000,
              "excludes": ["timing_closure", "physical_layout", "physical_power", "manufacturability",
                           "full_model_area", "group_rescaling", "coding_quality", "offline_checkpoint_payload_authentication"]}
    receipt = out / "mapping.json"
    receipt.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    try:
        inputs = out / "inputs"
        inputs.mkdir()
        result["input_sha256"] = snapshot(sample, library, inputs)
        result["checks"]["input_validation"] = "passed"
        result["failure_stage"] = "mapping"
        runner = hardware_module()
        yosys, verilator = shutil.which("yosys"), shutil.which("verilator")
        if not yosys or not verilator:
            raise ValueError("Yosys and Verilator required; mapped execution cannot be skipped")
        result["tools"] = {name: {"version": runner.run([binary, flag], out / (name + "-version.log")).strip(),
                                  "executable_sha256": sha(Path(binary).resolve().read_bytes())}
                           for name, binary, flag in [("yosys", yosys, "-V"), ("verilator", verilator, "--version")]}
        lib, netlist, cells = inputs / LIBRARY, out / "mapped.v", out / "library_cells.v"
        script = (f"read_verilog -sv -DSYNTHESIS {quote(inputs / 'integer_group_tile.sv')} {quote(inputs / 'fixed_linear_fixture.sv')}\n"
                  "synth -top fixed_linear_fixture -flatten\n"
                  f"dfflibmap -liberty {quote(lib)}\nabc -liberty {quote(lib)} -D 1000\n"
                  f"read_liberty -lib {quote(lib)}\nhierarchy -check -top fixed_linear_fixture\nclean\ncheck -assert\n"
                  f"tee -o {quote(out / 'stat.json')} stat -json -liberty {quote(lib)}\n"
                  f"write_json {quote(out / 'mapped.json')}\nwrite_verilog -noattr {quote(netlist)}\ndesign -reset\n"
                  f"read_liberty -ignore_miss_func {quote(lib)}\nproc\ncheck -assert\n"
                  f"write_json {quote(out / 'library_cells.json')}\nwrite_verilog -noattr {quote(cells)}\n")
        (out / "map.ys").write_text(script)
        runner.run([yosys, "-Q", "-q", "-s", str((out / "map.ys").resolve())], out / "mapping.log", 180)
        stats = parse_json(bounded(out / "stat.json"))["design"]
        count, area = validate_mapping(stats, parse_json(bounded(out / "mapped.json")),
                                       parse_json(bounded(out / "library_cells.json")))
        result["reported_library_area"] = area
        result["mapped_cell_count"] = count
        result["checks"]["library_mapping"] = "passed"
        result["failure_stage"] = "mapped_simulation"
        obj = (out / "obj-mapped").resolve()
        runner.run([verilator, "--binary", "--timing", "--assert", "--timescale-override", "1ns/1ps",
                    "--top-module", "tb", "--Mdir", str(obj), "-o", "mapped-sim",
                    str(netlist.resolve()), str(cells.resolve()), str((inputs / "tb.sv").resolve())],
                   out / "mapped-compile.log", 180)
        text = runner.run([str(obj / "mapped-sim")], out / "mapped-simulation.log")
        if "RTL_PASS " not in text:
            raise ValueError("mapped simulation lacks oracle proof marker")
        result["checks"]["mapped_netlist_vs_integer_oracle"] = "passed"
        result["status"] = "passed"
        result.pop("failure_stage")
    except BaseException as error:
        result["status"] = "cancelled" if isinstance(error, KeyboardInterrupt) else "failed"
        result["failure"] = str(error)
        raise
    finally:
        result["artifact_sha256"] = {p.name: sha(p.read_bytes()) for p in out.iterdir()
                                     if p.is_file() and p != receipt}
        receipt.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fetch-library", type=Path)
    parser.add_argument("--sample", type=Path)
    parser.add_argument("--library", type=Path)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--toolchain-image")
    args = parser.parse_args()
    try:
        if args.fetch_library:
            if any([args.sample, args.library, args.out, args.toolchain_image]):
                raise ValueError("library fetching and offline observation are separate invocations")
            fetch_library(args.fetch_library)
        else:
            if not all([args.sample, args.library, args.out]):
                raise ValueError("offline observation requires --sample, --library and new --out")
            observe(args.sample, args.library, args.out, args.toolchain_image)
    except (OSError, ValueError, KeyError, TypeError, RuntimeError) as error:
        parser.exit(2, f"map_weight_tile: {error}\n")


if __name__ == "__main__":
    main()
