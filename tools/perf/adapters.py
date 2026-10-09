#!/usr/bin/env python3
"""Turn existing benchmark output into records, without changing the benchmark.

Each adapter reads what a benchmark already prints and emits records through
`record.from_samples`. A benchmark that writes records itself needs no adapter.

- `fs-gate LOG...`: the tilo-fs performance gate
  (`crates/system/fs/tests/performance_gate.rs`, `TILO_FS_PERF_GATE=1`) prints
  `PERF <name>: <x> ms (SLO <= <y> ms)` per scenario. Each log is one run;
  repeated runs become the samples of `fs.<name>`.
- `tapp-sizes ENTRIES_JSON`: the public app registry's signed `entries`
  index (`https://apps.tilo.so/entries`). The newest version of every app per
  architecture is one sample of `footprint.tapp.bytes.<arch>`: the bytes a
  user downloads.
- `workspace-switch LOG...`: the compositor's `WORKSPACE-SWITCH` session-log
  lines (`crates/session/shell/src/compositor/switch_latency.rs`), one per
  switch, become `latency.workspace_switch.first_frame` (chord to the first
  frame shown), `.settled` (chord to the resting frame shown after the frame
  run) and `.dropped_frames` (refresh intervals in the run that showed no new
  frame). `scripts/perf/latency_bench.py` (headless, nightly) and
  `scripts/perf/device_latency_bench.py` (reference hardware) call it.

Usage:

    python3 tools/perf/adapters.py fs-gate --env-json env.json run-*.log >> results.jsonl
    python3 tools/perf/adapters.py tapp-sizes --env-json env.json entries.json >> results.jsonl
    python3 tools/perf/adapters.py workspace-switch --env-json env.json session.log >> results.jsonl

An adapter that finds nothing to record fails: a benchmark whose output format
drifted must not turn into a silently empty night.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

import record  # noqa: E402

# Searched, not anchored: with --nocapture, libtest's "test NAME ... " prefix can
# share the line with the test's first stderr line when both streams are merged.
FS_GATE_LINE = re.compile(r"(?:^|\s)PERF (?P<name>[a-z0-9][a-z0-9-]*): (?P<value>[0-9.]+) ms \(SLO <= [0-9.]+ ms\)$")


def fs_gate(logs: list[str], env: dict[str, Any]) -> list[record.Record]:
    """Records for the scenarios of one or more tilo-fs gate runs."""
    samples: dict[str, list[float]] = {}
    for log in logs:
        seen = set()
        for line in log.splitlines():
            match = FS_GATE_LINE.search(line.strip())
            if match is None:
                continue
            name = match.group("name")
            if name in seen:
                raise ValueError(f"fs gate scenario {name} printed twice in one run")
            seen.add(name)
            samples.setdefault(name, []).append(float(match.group("value")))
    if not samples:
        raise ValueError("no `PERF <name>: <x> ms (SLO <= <y> ms)` lines: did the gate run with TILO_FS_PERF_GATE=1?")
    counts = {name: len(values) for name, values in samples.items()}
    if len(set(counts.values())) != 1:
        raise ValueError(f"fs gate scenarios ran unequal times: {counts}")
    meta = {**env.get("meta", {}), "runs": str(len(logs))}
    base = {name: value for name, value in env.items() if name != "meta"}
    return [record.from_samples(f"fs.{name}", "ms", values, meta=meta, **base)
            for name, values in sorted(samples.items())]


def tapp_sizes(entries: dict[str, Any], env: dict[str, Any]) -> list[record.Record]:
    """Records for the newest published tapp of every app, per architecture."""
    newest: dict[tuple[str, str], dict[str, Any]] = {}
    for entry in entries.get("entries", []):
        for version in entry.get("versions", []):
            target = version.get("target", {})
            if target.get("platform") != "tilo-os":
                continue
            key = (entry["package_id"], target["architecture"])
            if key not in newest or version["released_at_unix"] > newest[key]["released_at_unix"]:
                newest[key] = version
    by_arch: dict[str, list[float]] = {}
    for (_, arch), version in newest.items():
        size = version["tapp_size_bytes"]
        if not isinstance(size, int) or size <= 0:
            raise ValueError(f"invalid tapp_size_bytes {size!r}")
        by_arch.setdefault(arch, []).append(float(size))
    if not by_arch:
        raise ValueError("the registry index lists no tilo-os tapp")
    meta = {**env.get("meta", {}), "registry_sequence": str(entries.get("sequence", ""))}
    base = {name: value for name, value in env.items() if name != "meta"}
    return [record.from_samples(f"footprint.tapp.bytes.{arch}", "bytes", sizes, meta=meta, **base)
            for arch, sizes in sorted(by_arch.items())]


SWITCH_FIELDS = ("first_queued_us", "first_shown_us", "settled_queued_us", "settled_shown_us",
                 "frames", "dropped", "interval_us")
SWITCH_LINE = re.compile(r"(?:^|\s)WORKSPACE-SWITCH ((?:[a-z_]+=[0-9]+ ?)+)$")


def switch_samples(text: str) -> list[dict[str, int]]:
    """Every `WORKSPACE-SWITCH` line of a session log, in order."""
    samples = []
    for line in text.splitlines():
        match = SWITCH_LINE.search(line.rstrip())
        if match is None:
            continue
        fields = dict(pair.split("=", 1) for pair in match.group(1).split())
        missing = [name for name in SWITCH_FIELDS if name not in fields]
        if missing:
            raise ValueError(f"WORKSPACE-SWITCH line lacks {missing}: {line.strip()}")
        samples.append({name: int(fields[name]) for name in SWITCH_FIELDS})
    return samples


def workspace_switch(samples: list[dict[str, int]], env: dict[str, Any],
                     kpi: str = "latency.workspace_switch.{}") -> list[record.Record]:
    """Records for the workspace switches of one run; `kpi` names them from the metric."""
    if not samples:
        raise ValueError("no `WORKSPACE-SWITCH` lines: did the compositor switch workspace, and is it "
                         "new enough to time switches?")
    intervals = sorted({sample["interval_us"] for sample in samples})
    meta = {**env.get("meta", {}), "interval_us": ",".join(str(value) for value in intervals),
            "frames_min": str(min(sample["frames"] for sample in samples))}
    base = {name: value for name, value in env.items() if name != "meta"}
    series = {
        "first_frame": ("ms", [sample["first_shown_us"] / 1000 for sample in samples]),
        "settled": ("ms", [sample["settled_shown_us"] / 1000 for sample in samples]),
        "dropped_frames": ("count", [float(sample["dropped"]) for sample in samples]),
    }
    return [record.from_samples(kpi.format(name), unit, values, meta=meta, **base)
            for name, (unit, values) in series.items()]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    gate = commands.add_parser("fs-gate", help="records from tilo-fs performance gate logs")
    gate.add_argument("--env-json", required=True, type=Path)
    gate.add_argument("logs", nargs="+", type=Path)
    sizes = commands.add_parser("tapp-sizes", help="records from the registry entries index")
    sizes.add_argument("--env-json", required=True, type=Path)
    sizes.add_argument("entries", type=Path)
    switch = commands.add_parser("workspace-switch", help="records from WORKSPACE-SWITCH session-log lines")
    switch.add_argument("--env-json", required=True, type=Path)
    switch.add_argument("logs", nargs="+", type=Path)
    args = parser.parse_args(argv)
    try:
        env = record.load_env(args.env_json)
        if args.command == "fs-gate":
            records = fs_gate([path.read_text(encoding="utf-8", errors="replace") for path in args.logs], env)
        elif args.command == "workspace-switch":
            samples = [sample for path in args.logs
                       for sample in switch_samples(path.read_text(encoding="utf-8", errors="replace"))]
            records = workspace_switch(samples, env)
        else:
            records = tapp_sizes(json.loads(args.entries.read_text(encoding="utf-8")), env)
    except (ValueError, KeyError) as error:
        print(f"adapters.py: {error}", file=sys.stderr)
        return 1
    for item in records:
        print(item.to_json())
    return 0


if __name__ == "__main__":
    sys.exit(main())
