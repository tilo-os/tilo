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
  user downloads. The apps are those with a budget in
  `tools/perf/tapp-size-budget/` (the default collection); runtime and
  provider stubs and withdrawn packages are left out and named in `meta`.
- `runtime-sizes MEASUREMENTS_JSON`: what `runtime_size_ratchet.py published
  --measurements` read from the newest published shared runtime
  (`libruntime_dylib.so`) of each architecture. Each of its numbers is one
  sample of `footprint.runtime.<metric>.<arch>`: `bytes`, `relocation_bytes`,
  `relocated_bytes` (bytes) and `symbol_relocations` (count).
- `workspace-switch LOG...`: the compositor's `WORKSPACE-SWITCH` session-log
  lines (`crates/session/shell/src/compositor/switch_latency.rs`), one per
  switch, become `latency.workspace_switch.first_frame` (chord to the first
  frame shown), `.settled` (chord to the resting frame shown after the frame
  run) and `.dropped_frames` (refresh intervals in the run that showed no new
  frame). `scripts/perf/latency_bench.py` (headless, nightly) and
  `scripts/perf/device_latency_bench.py` (reference hardware) call it.
- `idle-wakeups LOG...`: the idle-wakeup gate
  (`crates/session/shell/tests/idle_wakeups.rs`) prints one
  `IDLE-WAKEUPS total_per_s=<x> shell_per_s=<y> ...` line per run. Each log is
  one run; repeated runs become the samples of `wakeups.headless_two_apps`
  (the compositor's whole process tree), `.shell` (its own process) and
  `wakeups.frame_loop_ticks.headless_two_apps` (its frame loop's iterations).
- `hosting-load REPORT_JSON`: the relay load bench's `hosting-load.json`
  (`crates/hosting/tilo-relay/examples/relay_load`). Each KPI it measured
  becomes `<kpi>.n<collaborators>`, one sample per measured minute (fan-out
  latency: one per delivered edit), and each provider in
  `tools/perf/prices.toml` adds `hosting.relay.cost_usd_per_collab_hour.<provider>`
  and `hosting.relay.cost_usd_per_user_month.<provider>`, priced per minute.

Usage:

    python3 tools/perf/adapters.py fs-gate --env-json env.json run-*.log >> results.jsonl
    python3 tools/perf/adapters.py tapp-sizes --env-json env.json entries.json >> results.jsonl
    python3 tools/perf/adapters.py runtime-sizes --env-json env.json runtime.json >> results.jsonl
    python3 tools/perf/adapters.py workspace-switch --env-json env.json session.log >> results.jsonl
    python3 tools/perf/adapters.py idle-wakeups --env-json env.json run-*.log >> results.jsonl
    python3 tools/perf/adapters.py hosting-load --env-json env.json hosting-load.json >> results.jsonl

An adapter that finds nothing to record fails: a benchmark whose output format
drifted must not turn into a silently empty night.
"""

from __future__ import annotations

import argparse
import json
import math
import re
import sys
import tomllib
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

import record  # noqa: E402
import runtime_size_ratchet  # noqa: E402
import tapp_size_ratchet  # noqa: E402

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


def tapp_sizes(entries: dict[str, Any], env: dict[str, Any],
               population: set[str] | None = None) -> list[record.Record]:
    """Records for the newest published tapp of every app, per architecture.

    `population` is the package ids that count as apps; every other package is
    left out and named in the record's meta. None counts every package.
    """
    by_arch: dict[str, list[float]] = {}
    outside = set()
    for (package, arch), size in tapp_size_ratchet.newest_tapps(entries).items():
        if population is not None and package not in population:
            outside.add(package)
            continue
        by_arch.setdefault(arch, []).append(float(size))
    if not by_arch:
        raise ValueError("the registry index lists no tapp of a budgeted app")
    meta = {**env.get("meta", {}), "registry_sequence": str(entries.get("sequence", ""))}
    if population is not None:
        meta.update(population=tapp_size_ratchet.DIRECTORY, not_counted=",".join(sorted(outside)))
    base = {name: value for name, value in env.items() if name != "meta"}
    return [record.from_samples(f"footprint.tapp.bytes.{arch}", "bytes", sizes, meta=meta, **base)
            for arch, sizes in sorted(by_arch.items())]


def runtime_sizes(measured: dict[str, Any], env: dict[str, Any]) -> list[record.Record]:
    """Records for the published shared runtime of each architecture."""
    runtimes = measured.get("runtimes") or {}
    if not runtimes:
        raise ValueError("the runtime measurements name no architecture")
    base = {name: value for name, value in env.items() if name != "meta"}
    records = []
    for arch, values in sorted(runtimes.items()):
        meta = {**env.get("meta", {}), "registry_sequence": str(measured.get("sequence", "")),
                "version": str(values.get("version", "")), "runtime": str(values.get("runtime", ""))}
        for metric in runtime_size_ratchet.METRICS:
            value = values[metric]
            if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
                raise ValueError(f"runtime {arch} {metric}: invalid measurement {value!r}")
            unit = "count" if metric == "symbol_relocations" else "bytes"
            records.append(record.from_samples(f"footprint.runtime.{metric}.{arch}", unit, [float(value)],
                                               meta=meta, **base))
    return records


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


IDLE_WAKEUPS_LINE = re.compile(r"(?:^|\s)IDLE-WAKEUPS ((?:[a-z_]+=[0-9.]+ ?)+)$")
IDLE_WAKEUPS_KPIS = {"total_per_s": "wakeups.headless_two_apps",
                     "shell_per_s": "wakeups.headless_two_apps.shell",
                     "loop_ticks_per_s": "wakeups.frame_loop_ticks.headless_two_apps"}


def idle_wakeups(logs: list[str], env: dict[str, Any]) -> list[record.Record]:
    """Records for one or more runs of the idle-wakeup gate."""
    samples: dict[str, list[float]] = {kpi: [] for kpi in IDLE_WAKEUPS_KPIS.values()}
    windows = []
    for log in logs:
        lines = [match for line in log.splitlines() if (match := IDLE_WAKEUPS_LINE.search(line.rstrip()))]
        if len(lines) != 1:
            raise ValueError(f"expected one `IDLE-WAKEUPS` line per run, found {len(lines)}: "
                             "did the gate run with --nocapture?")
        fields = dict(pair.split("=", 1) for pair in lines[0].group(1).split())
        missing = [name for name in (*IDLE_WAKEUPS_KPIS, "window_ms") if name not in fields]
        if missing:
            raise ValueError(f"IDLE-WAKEUPS line lacks {missing}")
        for field, kpi in IDLE_WAKEUPS_KPIS.items():
            samples[kpi].append(float(fields[field]))
        windows.append(fields["window_ms"])
    if not logs:
        raise ValueError("no idle-wakeup gate logs")
    meta = {**env.get("meta", {}), "runs": str(len(logs)), "window_ms": ",".join(sorted(set(windows)))}
    base = {name: value for name, value in env.items() if name != "meta"}
    return [record.from_samples(kpi, "per_s", values, meta=meta, **base) for kpi, values in samples.items()]


PRICES = Path(__file__).resolve().parent / "prices.toml"
# The relay load bench's per-minute KPIs, as `relay_load` names them.
HOSTING_KPIS = {
    "hosting.relay.cpu_s_per_collab_hour": "s",
    "hosting.relay.rss_mib_per_collab": "MiB",
    "hosting.relay.egress_bytes_per_collab_hour": "bytes",
    "hosting.relay.frame_bytes_per_collab_hour": "bytes",
    "hosting.relay.fanout_ms": "ms",
}


def collab_hour_usd(provider: dict[str, Any], prices: dict[str, Any], cpu_s: float, rss_mib: float,
                    egress_bytes: float, idle_rss_mib: float) -> tuple[float, float, str]:
    """Dollars per active collaborator-hour on one provider's VM, with the
    collaborators that VM carries and the resource that binds it."""
    target = float(prices["target_utilisation"])
    hours = float(prices["hours_per_month"])
    by_cpu = float(provider["vcpus"]) * 3600.0 * target / cpu_s if cpu_s > 0 else math.inf
    free_mib = float(provider["memory_gib"]) * 1024.0 * target - idle_rss_mib
    if free_mib <= 0:
        raise ValueError(f"the idle relay ({idle_rss_mib:.0f} MiB) does not fit {provider['description']}")
    by_memory = free_mib / rss_mib if rss_mib > 0 else math.inf
    capacity, binding = (by_cpu, "cpu") if by_cpu <= by_memory else (by_memory, "memory")
    if not math.isfinite(capacity):
        raise ValueError("the run measured neither CPU time nor memory per collaborator")
    compute = float(provider["monthly_usd"]) / hours / capacity
    included_gb = float(provider["included_egress_gb_month"]) / (capacity * hours)
    billable_gb = max(0.0, egress_bytes / 1e9 - included_gb)
    return compute + billable_gb * float(provider["egress_usd_per_gb"]), capacity, binding


def hosting_load(report: dict[str, Any], prices: dict[str, Any], env: dict[str, Any]) -> list[record.Record]:
    """Records for one relay load bench run, priced with `prices`."""
    collaborators = int(report["collaborators"])
    variant = f"n{collaborators}"
    kpis = report.get("kpis", {})
    if set(kpis) != set(HOSTING_KPIS):
        raise ValueError(f"the bench reported {sorted(kpis)}, not {sorted(HOSTING_KPIS)}")
    for kpi, unit in HOSTING_KPIS.items():
        if kpis[kpi]["unit"] != unit or not kpis[kpi]["samples"]:
            raise ValueError(f"{kpi}: expected samples in {unit}")
    meta = {**env.get("meta", {}), "collaborators": str(collaborators), "rooms": str(report["rooms"]),
            "room_size": str(report["room_size"]), "window_s": str(report["window_s"]),
            "edits": str(report["edits"]), "presence": str(report["presence"]),
            "undelivered_edits": str(report["undelivered_edits"]), "prices_checked": str(prices["checked"])}
    base = {name: value for name, value in env.items() if name != "meta"}
    records = [record.from_samples(f"{kpi}.{variant}", unit, kpis[kpi]["samples"], meta=meta, **base)
               for kpi, unit in HOSTING_KPIS.items()]
    minutes = list(zip(kpis["hosting.relay.cpu_s_per_collab_hour"]["samples"],
                       kpis["hosting.relay.rss_mib_per_collab"]["samples"],
                       kpis["hosting.relay.egress_bytes_per_collab_hour"]["samples"], strict=True))
    idle = float(report["relay_idle_rss_mib"])
    for name, provider in sorted(prices["provider"].items()):
        priced = [collab_hour_usd(provider, prices, *minute, idle) for minute in minutes]
        hourly = [usd for usd, _, _ in priced]
        capacity = min(collaborators for _, collaborators, _ in priced)
        bindings = sorted({binding for _, _, binding in priced})
        provider_meta = {**meta, "provider": provider["description"],
                         "collaborators_per_vm": f"{capacity:.0f}", "binding": ",".join(bindings)}
        records.append(record.from_samples(f"hosting.relay.cost_usd_per_collab_hour.{name}.{variant}", "usd",
                                           hourly, meta=provider_meta, **base))
        monthly = [usd * float(prices["active_hours_per_user_month"]) for usd in hourly]
        records.append(record.from_samples(f"hosting.relay.cost_usd_per_user_month.{name}.{variant}", "usd",
                                           monthly, meta=provider_meta, **base))
    return records


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    gate = commands.add_parser("fs-gate", help="records from tilo-fs performance gate logs")
    gate.add_argument("--env-json", required=True, type=Path)
    gate.add_argument("logs", nargs="+", type=Path)
    sizes = commands.add_parser("tapp-sizes", help="records from the registry entries index")
    sizes.add_argument("--env-json", required=True, type=Path)
    sizes.add_argument("entries", type=Path)
    runtime = commands.add_parser("runtime-sizes", help="records from the published runtime's measurements")
    runtime.add_argument("--env-json", required=True, type=Path)
    runtime.add_argument("measurements", type=Path)
    switch = commands.add_parser("workspace-switch", help="records from WORKSPACE-SWITCH session-log lines")
    switch.add_argument("--env-json", required=True, type=Path)
    switch.add_argument("logs", nargs="+", type=Path)
    idle = commands.add_parser("idle-wakeups", help="records from idle-wakeup gate logs")
    idle.add_argument("--env-json", required=True, type=Path)
    idle.add_argument("logs", nargs="+", type=Path)
    hosting = commands.add_parser("hosting-load", help="records from the relay load bench's hosting-load.json")
    hosting.add_argument("--env-json", required=True, type=Path)
    hosting.add_argument("--prices", type=Path, default=PRICES)
    hosting.add_argument("report", type=Path)
    args = parser.parse_args(argv)
    try:
        env = record.load_env(args.env_json)
        if args.command == "hosting-load":
            records = hosting_load(json.loads(args.report.read_text(encoding="utf-8")),
                                   tomllib.loads(args.prices.read_text(encoding="utf-8")), env)
            for item in records:
                if item.unit == "usd":
                    print(f"{item.kpi}: p50 ${item.p50:.6f}, max ${item.max:.6f} "
                          f"({item.meta['collaborators_per_vm']} collaborators per VM, "
                          f"bound by {item.meta['binding']})", file=sys.stderr)
        elif args.command == "fs-gate":
            records = fs_gate([path.read_text(encoding="utf-8", errors="replace") for path in args.logs], env)
        elif args.command == "workspace-switch":
            samples = [sample for path in args.logs
                       for sample in switch_samples(path.read_text(encoding="utf-8", errors="replace"))]
            records = workspace_switch(samples, env)
        elif args.command == "runtime-sizes":
            records = runtime_sizes(json.loads(args.measurements.read_text(encoding="utf-8")), env)
        elif args.command == "idle-wakeups":
            records = idle_wakeups([path.read_text(encoding="utf-8", errors="replace") for path in args.logs], env)
        else:
            records = tapp_sizes(json.loads(args.entries.read_text(encoding="utf-8")), env,
                                 tapp_size_ratchet.load().apps())
    except (ValueError, KeyError) as error:
        print(f"adapters.py: {error}", file=sys.stderr)
        return 1
    for item in records:
        print(item.to_json())
    return 0


if __name__ == "__main__":
    sys.exit(main())
