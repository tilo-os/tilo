#!/usr/bin/env python3
"""What a public room host holds, and what one collaborator on it costs.

`sweep` runs `tilo-room-load bench` (crates/dev/room-load) once per entry of
a plan, one after another on one machine, and keeps each run's JSON report
and log. `summarize` reads a sweep's reports and derives:

- resident MiB per guest (the slope of the host's peak memory over joined
  guests, plain runs in which every guest joined) and per spectator (the
  slope over the spectator runs, at a fixed number of guests);
- host CPU cores per guest during the hold, and guests per vCPU;
- host egress per guest;
- the host's memory and CPU at the deployed caps (the `caps` run: as many
  guests and spectators as the caps admit, at once);
- what 100 concurrent guests cost on each `[room_node.*]` of
  tools/perf/prices.toml.

The Perf / room host capacity workflow (.forgejo/workflows/perf-room-capacity.yml)
runs both on a runner of known size; docs/benchmarks/room-host-capacity-2026-10.md
records a run.

    python3 tools/perf/room_capacity.py sweep --bin target/release/tilo-room-load --out sweep
    python3 tools/perf/room_capacity.py summarize sweep --json summary.json --markdown summary.md
"""

from __future__ import annotations

import argparse
import json
import math
import os
import re
import signal
import subprocess
import sys
import threading
import time
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Any

PRICES = Path(__file__).with_name("prices.toml")
CGROUP = Path("/sys/fs/cgroup")
# The share of the job's memory a run may take before it is stopped.
MEMORY_GUARD = 0.8
# What every run shares: production's allocator setting (the room service
# starts its relay with MALLOC_ARENA_MAX=2) and a minute of steady state.
COMMON = ("--malloc-arenas", "2", "--hold", "60")
GUESTS = (50, 100, 300, 500, 800)
SPECTATORS = (500, 1000, 2000, 4000)
# Guests beside the spectator runs, so the slope is the spectators' alone.
SPECTATOR_BASE_GUESTS = 50
# eu1's deployed caps (TILO_ROOMS_MAX_GUESTS, TILO_ROOMS_MAX_SPECTATORS).
CAP_GUESTS = 500
CAP_SPECTATORS = 2000


@dataclass(frozen=True)
class Run:
    name: str
    kind: str
    args: tuple[str, ...]
    # Environment the bench (and the host it starts) runs with.
    env: tuple[tuple[str, str], ...] = ()


def plan(which: str) -> list[Run]:
    """The runs of a sweep, in order. `full` is the capacity write-up's."""
    if which == "quick":
        return [Run("plain-g300", "plain", ("--guests", "300"))]
    runs = [Run("idle", "idle", ("--guests", "0"))]
    runs += [Run(f"plain-g{n}", "plain", ("--guests", str(n))) for n in GUESTS]
    runs += [Run(f"rooms4-g{n}", "rooms4", ("--guests", str(n), "--rooms", "4", "--shard", "125", "--chat-room"))
             for n in GUESTS]
    runs += [Run(f"cap{CAP_GUESTS}-g{n}", "capped", ("--guests", str(n), "--max-guests", str(CAP_GUESTS)))
             for n in GUESTS if n >= CAP_GUESTS]
    # Guests arriving at a quarter of the rate: whether joining is bound by
    # the arrival rate or by the room's size.
    runs += [Run(f"ramp5-g{n}", "ramp5", ("--guests", str(n), "--ramp", "5", "--join-timeout", "120"))
             for n in (300, 500)]
    # The host's state on memory instead of the runner's disk: whether the
    # room waits on storage (every commit is made durable before it counts).
    # 100 guests only: the job container's /dev/shm is 64 MiB, which 300
    # guests' state outgrows.
    runs.append(Run("tmpfs-g100", "tmpfs", ("--guests", "100"), (("TMPDIR", "/dev/shm"),)))
    # Spectators admitted as Viewers of their own: what a watching session
    # costs. The homepage embed's path (one spectator person, every page a
    # device of it) is the `embed` run.
    runs += [Run(f"spect-s{n}", "spectators",
                 ("--guests", str(SPECTATOR_BASE_GUESTS), "--spectators", str(n), "--spectator-members"))
             for n in SPECTATORS]
    runs.append(Run("embed-s100", "embed", ("--guests", str(SPECTATOR_BASE_GUESTS), "--spectators", "100")))
    runs.append(Run("caps", "caps", ("--guests", str(CAP_GUESTS), "--spectators", str(CAP_SPECTATORS),
                                     "--spectator-members", "--max-guests", str(CAP_GUESTS),
                                     "--max-spectators", str(CAP_SPECTATORS))))
    if which != "full":
        pattern = re.compile(which)
        runs = [run for run in runs if pattern.search(run.name)]
        if not runs:
            raise ValueError(f"no run of the full plan matches {which!r}")
    return runs


def cgroup_memory() -> tuple[int, int] | None:
    """The job's memory use and limit (cgroup v2), or None outside one."""
    try:
        current = int(CGROUP.joinpath("memory.current").read_text())
        limit = CGROUP.joinpath("memory.max").read_text().strip()
    except (OSError, ValueError):
        return None
    return (current, int(limit)) if limit.isdigit() else None


def bench(command: list[str], log: Path, timeout_s: float,
          env: tuple[tuple[str, str], ...] = ()) -> tuple[int | None, str, str | None]:
    """Run one bench in its own process group. It is stopped, and the stop
    named, when it outlives `timeout_s` or the job's memory passes
    MEMORY_GUARD of its limit: a run that would take the runner down with it
    is a failure point to record, not a lost sweep."""
    with log.open("w", encoding="utf-8") as stderr:
        child = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=stderr, text=True, start_new_session=True,
                                 env={**os.environ, **dict(env)})
        output: list[str] = []
        reader = threading.Thread(target=lambda: output.append(child.stdout.read() if child.stdout else ""))
        reader.start()
        began = time.monotonic()
        stopped = None
        while child.poll() is None:
            memory = cgroup_memory()
            if memory and memory[0] > MEMORY_GUARD * memory[1]:
                stopped = f"stopped at {memory[0] / 2**30:.1f} GiB of the job's {memory[1] / 2**30:.1f} GiB"
            elif time.monotonic() - began > timeout_s:
                stopped = f"stopped after {timeout_s:.0f} s"
            if stopped:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait()
                break
            time.sleep(1)
        reader.join()
        if child.stdout:
            child.stdout.close()
    return (None if stopped else child.returncode), "".join(output), stopped


def run_seconds(run: Run) -> float:
    """How long a run may take: its ramps, the hold and the join timeout,
    and five minutes to spare."""
    args = dict(zip(run.args[::2], run.args[1::2]))
    guests = float(args.get("--guests", "50")) / float(args.get("--ramp", "20"))
    spectators = float(args.get("--spectators", "0")) / 50.0
    return max(guests, spectators) + 60 + float(args.get("--join-timeout", "60")) + 300


def sweep(binary: Path, out: Path, which: str) -> int:
    """Run the plan; a run that fails is kept as a failure, not a stop."""
    out.mkdir(parents=True, exist_ok=True)
    failed = []
    for run in plan(which):
        command = [str(binary), "bench", *COMMON, *run.args]
        prefix = "".join(f"{name}={value} " for name, value in run.env)
        print(f"== {run.name}: {prefix}{' '.join(command)}", flush=True)
        code, stdout, stopped = bench(command, out / f"{run.name}.log", run_seconds(run), run.env)
        report = json.loads(stdout) if code == 0 else None
        (out / f"{run.name}.json").write_text(
            json.dumps({"name": run.name, "kind": run.kind, "args": list(run.args), "env": dict(run.env), "exit": code,
                        "stopped": stopped, "report": report}, indent=2),
            encoding="utf-8")
        if report is None:
            failed.append(run.name)
            print(f"   {run.name}: {stopped or f'exited {code}'}; see {run.name}.log", flush=True)
    if failed:
        print(f"failed runs: {', '.join(failed)}", file=sys.stderr)
    return 1 if failed else 0


def slope(points: list[tuple[float, float]]) -> float:
    """Least-squares slope of y over x."""
    if len(points) < 2:
        raise ValueError("a slope needs two runs")
    mean_x = sum(x for x, _ in points) / len(points)
    mean_y = sum(y for _, y in points) / len(points)
    spread = sum((x - mean_x) ** 2 for x, _ in points)
    if spread == 0:
        raise ValueError("a slope needs runs of different sizes")
    return sum((x - mean_x) * (y - mean_y) for x, y in points) / spread


def failures(part: dict[str, Any]) -> int:
    return sum(int(count) for count in part.get("failures", {}).values())


def row(run: dict[str, Any]) -> dict[str, Any]:
    """One run's line of the table."""
    report = run["report"]
    host = report["host"]
    spectators = report.get("spectators", {})
    return {
        "name": run["name"],
        "kind": run["kind"],
        "guests": report["config"]["guests"],
        "joined": report["joined"],
        "refused_full": report["full"]["count"],
        "spectators": report["config"].get("spectators", 0),
        "watching": spectators.get("joined", 0),
        "spectators_refused_full": spectators.get("full", {}).get("count", 0),
        "failures": failures(report) + failures(spectators),
        "join_p50_ms": report["join_ms"]["p50"],
        "join_p99_ms": report["join_ms"].get("p99", report["join_ms"]["max"]),
        "chat_p99_ms": report["chat_ms"].get("p99", report["chat_ms"]["max"]),
        # Chat changes the senders heard `Committed` for, of those sent.
        "chat_committed": report.get("committed", 0),
        "chat_sent": report.get("chat_tx", 0),
        "spectator_join_p99_ms": spectators.get("join_ms", {}).get("p99", 0.0),
        "spectator_chat_p99_ms": spectators.get("chat_ms", {}).get("p99", 0.0),
        "cpu_cores": host["hold_cpu_cores"],
        "busiest_thread_cores": host.get("hold_busiest_thread_cores", 0.0),
        "processes": host.get("processes", 1),
        "rss_mib_peak": host["rss_mib_peak"],
        "egress_mbit_s": host.get("egress_bytes_per_s", 0.0) * 8 / 1e6,
        "driver_cores": host["driver_cpu_cores_avg"],
        "machine_cores": host["machine_cores"],
        # Where the host's time went (tilo_relay::timing), per phase.
        "timings": report.get("timings") or {},
    }


def node_cost(node: dict[str, Any], prices: dict[str, Any], per_guest: dict[str, float],
              idle_mib: float) -> dict[str, Any]:
    """Guests one node carries at the sheet's target utilisation, the
    resource that binds it, and the price of 100 concurrent guests."""
    target = float(prices["target_utilisation"])
    by_cpu = float(node["vcpus"]) * target / per_guest["cores"]
    by_memory = (float(node["memory_gib"]) * 1024.0 * target - idle_mib) / per_guest["mib"]
    # The node's network port, which the guests' egress may fill first.
    port = float(node.get("port_mbit_s", 0.0)) * 1e6 / 8
    egress = per_guest["egress_bytes_s"]
    by_network = port * target / egress if port and egress > 0 else math.inf
    capacity, binding = min((by_cpu, "cpu"), (by_memory, "memory"), (by_network, "network"))
    hours = float(prices["hours_per_month"])
    egress_gb_month = per_guest["egress_bytes_s"] * 3600 * hours / 1e9 * capacity
    billable = max(0.0, egress_gb_month - float(node.get("included_egress_gb_month", 0.0)))
    monthly = float(node["monthly_usd"]) + billable * float(node["egress_usd_per_gb"])
    return {
        "description": node["description"],
        "guests_per_node": math.floor(capacity),
        "binding": binding,
        "usd_per_month_per_100_guests": monthly / capacity * 100,
        "usd_per_guest_hour": monthly / hours / capacity,
    }


def summarize(runs: list[dict[str, Any]], prices: dict[str, Any]) -> dict[str, Any]:
    order = {item.name: index for index, item in enumerate(plan("full"))}
    runs = sorted(runs, key=lambda run: order.get(run["name"], len(order)))
    ok = [run for run in runs if run.get("report")]
    rows = [row(run) for run in ok]
    by_kind: dict[str, list[dict[str, Any]]] = {}
    for item in rows:
        by_kind.setdefault(item["kind"], []).append(item)
    plain = by_kind.get("plain", [])
    failed = [run["name"] for run in runs if not run.get("report")]
    if len(plain) < 2:
        # A partial sweep (`--plan quick` or a regex) is reported as measured.
        return {"prices_checked": prices.get("room_node_checked", prices["checked"]), "rows": rows,
                "failed_runs": failed, "derived": {}, "cost": {}}
    idle = by_kind.get("idle", [{"rss_mib_peak": 0.0}])[0]["rss_mib_peak"]
    # Runs in which every guest joined, when there are two: a guest still
    # joining holds a session but none of the steady state.
    complete = [r for r in plain if r["joined"] == r["guests"] and r["failures"] == 0]
    basis = complete if len(complete) >= 2 else plain
    mib_per_guest = slope([(r["joined"], r["rss_mib_peak"]) for r in basis])
    cores_per_guest = slope([(r["joined"], r["cpu_cores"]) for r in basis])
    egress_per_guest = slope([(r["joined"], r["egress_mbit_s"] * 1e6 / 8) for r in basis])
    derived: dict[str, Any] = {
        "idle_rss_mib": idle,
        "per_guest_runs": [r["name"] for r in basis],
        "mib_per_guest": mib_per_guest,
        "cores_per_guest": cores_per_guest,
        "guests_per_vcpu": 1 / cores_per_guest if cores_per_guest > 0 else math.inf,
        "egress_kbit_s_per_guest": egress_per_guest * 8 / 1e3,
    }
    # Only runs whose spectators all got in say what a spectator costs: one
    # still waiting holds a session and a replay but none of the steady
    # state, so a partial run's memory is not per spectator.
    spectators = [r for r in by_kind.get("spectators", [])
                  if r["watching"] == r["spectators"] and r["joined"] == r["guests"]]
    if len({r["watching"] for r in spectators}) >= 2:
        derived["mib_per_spectator"] = slope([(r["watching"], r["rss_mib_peak"]) for r in spectators])
        derived["cores_per_spectator"] = slope([(r["watching"], r["cpu_cores"]) for r in spectators])
        derived["egress_kbit_s_per_spectator"] = slope(
            [(r["watching"], r["egress_mbit_s"] * 1e3) for r in spectators])
    caps = by_kind.get("caps")
    if caps:
        derived["at_caps"] = {key: caps[0][key] for key in
                              ("joined", "watching", "rss_mib_peak", "cpu_cores", "egress_mbit_s", "failures")}
    derived["predicted_rss_mib_at_caps"] = (
        idle + CAP_GUESTS * mib_per_guest + CAP_SPECTATORS * derived.get("mib_per_spectator", 0.0))
    per_guest = {"cores": cores_per_guest, "mib": mib_per_guest, "egress_bytes_s": egress_per_guest}
    costs = {name: node_cost(node, prices, per_guest, idle)
             for name, node in sorted(prices.get("room_node", {}).items())} if cores_per_guest > 0 else {}
    return {"prices_checked": prices.get("room_node_checked", prices["checked"]), "rows": rows,
            "failed_runs": failed,
            "stopped_runs": {run["name"]: run["stopped"] for run in runs if run.get("stopped")},
            "derived": derived, "cost": costs}


def timing_lines(rows: list[dict[str, Any]]) -> list[str]:
    """Each run's phase timings, as the bench summarized them: per-join and
    per-commit phases by arrival tenth, owner and lock phases as
    milliseconds spent per second of the run."""
    out: list[str] = []
    for r in rows:
        if not r.get("timings"):
            continue
        out += ["", f"{r['name']} phase timings:", ""]
        out += [f"- `{step}`: {value}" for step, value in sorted(r["timings"].items())]
    return out


def markdown(summary: dict[str, Any]) -> str:
    lines = ["| run | guests joined | spectators | refused (full) | failures | join p50 / p99 ms | chat p99 ms "
             "| chat committed | spectator join p99 ms | host CPU cores (busiest thread) | peak memory MiB "
             "| egress Mbit/s | driver cores |",
             "|---|---|---|---|---|---|---|---|---|---|---|---|---|"]
    for r in summary["rows"]:
        lines.append(
            f"| {r['name']} | {r['joined']}/{r['guests']} | {r['watching']}/{r['spectators']} "
            f"| {r['refused_full'] + r['spectators_refused_full']} | {r['failures']} "
            f"| {r['join_p50_ms']:.0f} / {r['join_p99_ms']:.0f} | {r['chat_p99_ms']:.0f} "
            f"| {r.get('chat_committed', 0)}/{r.get('chat_sent', 0)} "
            f"| {r['spectator_join_p99_ms']:.0f} | {r['cpu_cores']:.2f} ({r['busiest_thread_cores']:.2f}) "
            f"| {r['rss_mib_peak']} "
            f"| {r['egress_mbit_s']:.1f} | {r['driver_cores']:.1f} |")
    lines += timing_lines(summary["rows"])
    d = summary["derived"]
    if not d:
        return "\n".join(lines) + "\n"
    lines += ["", f"- idle host: {d['idle_rss_mib']} MiB",
              f"- MiB per guest: {d['mib_per_guest']:.2f} (from {', '.join(d['per_guest_runs'])})",
              f"- guests per vCPU: {d['guests_per_vcpu']:.0f} ({d['cores_per_guest'] * 1000:.2f} millicores each)",
              f"- egress per guest: {d['egress_kbit_s_per_guest']:.1f} kbit/s"]
    if "mib_per_spectator" in d:
        lines += [f"- MiB per spectator: {d['mib_per_spectator']:.3f}",
                  f"- egress per spectator: {d['egress_kbit_s_per_spectator']:.1f} kbit/s"]
    else:
        lines.append("- MiB per spectator: not derived, no two spectator runs admitted every spectator")
    counted = f" + {CAP_SPECTATORS} spectators" if "mib_per_spectator" in d else " (spectators left out)"
    lines.append(f"- predicted memory at {CAP_GUESTS} guests{counted}: {d['predicted_rss_mib_at_caps']:.0f} MiB")
    if "at_caps" in d:
        c = d["at_caps"]
        lines.append(f"- measured at the caps ({c['joined']} guests, {c['watching']} spectators): "
                     f"{c['rss_mib_peak']} MiB peak, {c['cpu_cores']:.2f} cores, {c['egress_mbit_s']:.1f} Mbit/s")
    if summary["cost"]:
        lines += ["", f"| node (prices checked {summary['prices_checked']}) | guests per node | bound by "
                  "| USD per month per 100 concurrent guests | USD per guest-hour |", "|---|---|---|---|---|"]
        for cost in summary["cost"].values():
            lines.append(f"| {cost['description']} | {cost['guests_per_node']} | {cost['binding']} "
                         f"| {cost['usd_per_month_per_100_guests']:.2f} | {cost['usd_per_guest_hour']:.6f} |")
    if summary["failed_runs"]:
        lines += ["", f"Runs that did not finish: {', '.join(summary['failed_runs'])}"]
        lines += [f"- {name}: {why}" for name, why in summary.get("stopped_runs", {}).items()]
    return "\n".join(lines) + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    run = commands.add_parser("sweep", help="run the plan's benches and keep their reports")
    run.add_argument("--bin", required=True, type=Path)
    run.add_argument("--out", required=True, type=Path)
    run.add_argument("--plan", default="full", help="full, quick, or a regex over the full plan's run names")
    summary = commands.add_parser("summarize", help="derive per-guest costs from a sweep")
    summary.add_argument("dir", type=Path)
    summary.add_argument("--prices", type=Path, default=PRICES)
    summary.add_argument("--json", type=Path)
    summary.add_argument("--markdown", type=Path)
    args = parser.parse_args(argv)
    try:
        if args.command == "sweep":
            return sweep(args.bin, args.out, args.plan)
        runs = [json.loads(path.read_text(encoding="utf-8")) for path in sorted(args.dir.glob("*.json"))]
        result = summarize(runs, tomllib.loads(args.prices.read_text(encoding="utf-8")))
    except (ValueError, KeyError) as error:
        print(f"room_capacity.py: {error}", file=sys.stderr)
        return 1
    text = markdown(result)
    if args.json:
        args.json.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    if args.markdown:
        args.markdown.write_text(text, encoding="utf-8")
    print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
