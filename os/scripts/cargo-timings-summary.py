#!/usr/bin/env python3
"""Print where a `cargo build --timings` run spent its time.

    cargo-timings-summary.py TARGET_DIR/cargo-timings/cargo-timing.html [--top N]

Lists the slowest units, the time build scripts, proc-macros and links took,
and the longest dependency chain, from the UNIT_DATA table Cargo embeds in
the report.
"""
import argparse
import json
import re
import sys


def load(path):
    text = open(path, encoding="utf-8").read()
    match = re.search(r"const UNIT_DATA = (\[.*?\]);\n", text, re.S)
    if not match:
        raise SystemExit(f"{path}: no UNIT_DATA")
    return json.loads(match.group(1))


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("report")
    parser.add_argument("--top", type=int, default=20)
    args = parser.parse_args()
    units = load(args.report)
    by_index = {unit["i"]: unit for unit in units}
    wall = max(unit["start"] + unit["duration"] for unit in units)
    kinds = {}
    for unit in units:
        target = unit.get("target", "")
        kind = ("build script" if "build script" in target
                else "run build script" if "run-custom-build" in unit.get("mode", "")
                else "proc-macro" if "proc-macro" in target
                else "bin/link" if " bin " in f" {target} " or "bin" == target.strip()
                else "lib")
        kinds.setdefault(kind, [0, 0.0])
        kinds[kind][0] += 1
        kinds[kind][1] += unit["duration"]
    # Longest chain by finish time through unlocked units.
    finish = {}
    parent = {}
    for unit in sorted(units, key=lambda u: u["start"]):
        finish[unit["i"]] = unit["start"] + unit["duration"]
    for unit in units:
        for child in unit.get("unlocked_units", []) + unit.get("unlocked_rmeta_units", []):
            if child in by_index and (child not in parent or finish[unit["i"]] > finish[parent[child]]):
                parent[child] = unit["i"]
    last = max(units, key=lambda u: finish[u["i"]])["i"]
    chain = []
    while last is not None:
        chain.append(by_index[last])
        last = parent.get(last)
    print(f"cargo timings: {len(units)} units, {wall:.0f} s wall")
    for kind, (count, seconds) in sorted(kinds.items(), key=lambda item: -item[1][1]):
        print(f"  {kind:18} {count:5d} units {seconds:8.0f} s summed")
    print(f"slowest {args.top} units:")
    for unit in sorted(units, key=lambda u: -u["duration"])[:args.top]:
        print(f"  {unit['duration']:7.1f} s  {unit['name']} {unit.get('version', '')} {unit.get('target', '').strip()} {unit.get('mode', '')}")
    print("longest chain (last first):")
    for unit in chain[:args.top]:
        print(f"  finish {finish[unit['i']]:7.1f} s  took {unit['duration']:6.1f} s  {unit['name']} {unit.get('target', '').strip()}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
