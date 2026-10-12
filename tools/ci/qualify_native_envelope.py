#!/usr/bin/env python3
"""Fail closed when a diagnostic cannot prove its actual native job envelope."""
from __future__ import annotations

import argparse
import json
from pathlib import Path

import cgroup_memory

EVENTS = ("max", "oom", "oom_kill")


def observe() -> dict:
    record = cgroup_memory.snapshot()
    directory = cgroup_memory.current_cgroup()
    record["cpu.max"] = (directory / "cpu.max").read_text().strip() if directory else None
    record["memory.swap.max"] = (directory / "memory.swap.max").read_text().strip() if directory else None
    record["memory.swap.current"] = (directory / "memory.swap.current").read_text().strip() if directory else None
    return record


def validate_resources(record: dict, memory_gib: int, cpus: int) -> None:
    if not record.get("identity") or not record.get("cgroup"):
        raise ValueError("cannot observe the job's cgroup identity")
    if record.get("memory.max") != str(memory_gib * 1024**3):
        raise ValueError("the actual memory limit differs from the requested envelope")
    quota, period = (record.get("cpu.max") or "").split()
    if quota == "max" or int(period) <= 0 or int(quota) <= 0 or int(quota) != cpus * int(period):
        raise ValueError("the actual CPU quota differs from the requested envelope")
    if not str(record.get("memory.peak", "")).isdigit():
        raise ValueError("cannot observe peak memory")
    counters = record.get("memory.events")
    if not isinstance(counters, dict) or any(
        not isinstance(counters.get(name), int) or counters[name] < 0 for name in EVENTS
    ):
        raise ValueError("cannot observe memory limit and OOM counters")


def validate(record: dict, memory_gib: int, cpus: int) -> None:
    validate_resources(record, memory_gib, cpus)
    if record.get("memory.swap.max") != "0" or record.get("memory.swap.current") != "0":
        raise ValueError("the actual envelope must prohibit swap and have no swap in use")


def prohibit_swap(memory_gib: int, cpus: int) -> dict:
    before = observe()
    validate_resources(before, memory_gib, cpus)
    if before.get("memory.swap.current") != "0":
        raise ValueError("cannot tighten a diagnostic already using swap")
    maximum = before.get("memory.swap.max")
    if maximum != "max" and not str(maximum).isdigit():
        raise ValueError("cannot observe the existing swap limit")
    directory = cgroup_memory.current_cgroup()
    if not directory or str(directory) != before["cgroup"]:
        raise ValueError("cannot observe this diagnostic job's cgroup")
    # Only lower this job's swap limit; never raise memory/CPU caps, touch a
    # parent cgroup, reset counters, change mounts or relax permissions.
    if maximum != "0":
        (directory / "memory.swap.max").write_text("0\n")
    after = observe()
    if before["identity"] != after["identity"] or before["cgroup"] != after["cgroup"]:
        raise ValueError("the job cgroup changed while tightening swap")
    validate(after, memory_gib, cpus)
    return {"before": before, "after": after}


def qualify(before: dict, after: dict, memory_gib: int, cpus: int) -> dict:
    validate(before, memory_gib, cpus)
    validate(after, memory_gib, cpus)
    if before["identity"] != after["identity"] or before["cgroup"] != after["cgroup"]:
        raise ValueError("the measured cgroup changed during qualification")
    deltas = {name: after["memory.events"][name] - before["memory.events"][name] for name in EVENTS}
    if any(delta != 0 for delta in deltas.values()):
        raise ValueError("memory limit or OOM counters changed during qualification")
    peak = int(after["memory.peak"])
    if not 0 < peak < memory_gib * 1024**3:
        raise ValueError("peak memory did not remain below the enforced limit")
    return {"before": before, "after": after, "event_deltas": deltas, "peak_bytes": peak}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase", choices=("prepare", "before", "after"))
    parser.add_argument("--memory-gib", type=int, choices=(22, 36), required=True)
    parser.add_argument("--cpus", type=int, choices=(24,), required=True)
    parser.add_argument("--state", type=Path, required=True)
    args = parser.parse_args()
    if args.phase == "prepare":
        print("CI_NATIVE_ENVELOPE_SWAP=" + json.dumps(prohibit_swap(args.memory_gib, args.cpus), sort_keys=True))
        return
    record = observe()
    print("CI_NATIVE_ENVELOPE_OBSERVATION=" + json.dumps(record, sort_keys=True))
    validate(record, args.memory_gib, args.cpus)
    if args.phase == "before":
        args.state.write_text(json.dumps(record, sort_keys=True) + "\n")
        print("CI_NATIVE_ENVELOPE_BEFORE=" + json.dumps(record, sort_keys=True))
    else:
        before = json.loads(args.state.read_text())
        result = qualify(before, record, args.memory_gib, args.cpus)
        print("CI_NATIVE_ENVELOPE_QUALIFICATION=" + json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()
