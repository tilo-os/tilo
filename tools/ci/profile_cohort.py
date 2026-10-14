"""Observe the existing producer without changing its inputs or cache settings."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time


def totals(value: dict) -> dict:
    result = {}
    for kind in ("cache_hits", "cache_misses"):
        counts = value["stats"][kind]["counts"]
        if not isinstance(counts, dict) or any(type(n) is not int or n < 0 for n in counts.values()):
            raise ValueError("invalid cache counters")
        result[kind] = sum(counts.values())
    return result


def cache_snapshot() -> dict:
    wrapper = os.environ.get("RUSTC_WRAPPER", "")
    path = wrapper if Path(wrapper).name == "sccache" else shutil.which("sccache")
    if not path:
        return {"available": False, "reason": "client_unavailable"}
    try:
        response = subprocess.run([path, "--show-stats", "--stats-format=json"],
                                  capture_output=True, timeout=2, check=False)
        if response.returncode or len(response.stdout) > 128 * 1024:
            return {"available": False, "reason": "stats_unavailable"}
        return {"available": True, **totals(json.loads(response.stdout))}
    except (OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError):
        # Unobservable counters are unknown, never zero. Raw tool output may
        # contain backend URLs; only validated numeric totals leave this call.
        return {"available": False, "reason": "stats_unavailable"}


def delta(before: dict, after: dict) -> dict | None:
    if not before["available"] or not after["available"]:
        return None
    keys = ("cache_hits", "cache_misses")
    if any(after[key] < before[key] for key in keys):
        return None
    return {key: after[key] - before[key] for key in keys}


def run_profile(path: Path, producer_args: list[str]) -> int:
    before = cache_snapshot()
    started = time.monotonic()
    status = None
    try:
        # Keep stdout (the authenticated receipt digest) and stderr streaming
        # directly. Do not change any producer argument, environment or exit.
        command = [sys.executable, str(Path(__file__).resolve().parents[2] / "scripts/vm-acceptance/layer_artifacts.py"), *producer_args]
        status = subprocess.run(command, check=False).returncode
        # subprocess uses a negative signal number; shell commands report
        # 128 + signal. Keep cancellation/failure status at that boundary.
        return 128 - status if status < 0 else status
    finally:
        elapsed = time.monotonic() - started
        after = cache_snapshot()
        observation = {
            "schema": "tilo.vm.cohort-cache-observation/v1",
            "producer_exit_code": status,
            "producer_seconds": elapsed,
            "before": before, "after": after, "delta": delta(before, after),
            # Namespace identity only; never collect cache URLs, credentials,
            # arbitrary environment variables or compiler process arguments.
            "configured_namespace": os.environ.get("SCCACHE_GHA_VERSION"),
            "gha_backend_enabled": os.environ.get("SCCACHE_GHA_ENABLED") == "on",
            "scope": "job-local server totals during the unchanged cohort producer; includes Rust and native compilers",
        }
        try:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(json.dumps(observation, indent=2) + "\n")
            print(f"cohort cache observation: {observation['delta']}; namespace={observation['configured_namespace']!r}",
                  file=sys.stderr)
        except OSError:
            print("cohort cache observation unavailable: cannot write report", file=sys.stderr)


def qualify_observation(path: Path) -> dict:
    """Require real, monotonic job-local cache counters for native qualification."""
    observed = json.loads(path.read_text())
    if observed.get("schema") != "tilo.vm.cohort-cache-observation/v1" \
            or type(observed.get("producer_exit_code")) is not int \
            or observed["producer_exit_code"] != 0:
        raise ValueError("producer did not complete successfully")
    for point in ("before", "after"):
        counts = observed[point]
        if counts.get("available") is not True or any(
                type(counts.get(key)) is not int or counts[key] < 0
                for key in ("cache_hits", "cache_misses")):
            raise ValueError("cache counters unavailable")
    measured = delta(observed["before"], observed["after"])
    recorded = observed.get("delta")
    if not isinstance(recorded, dict) or any(type(v) is not int for v in recorded.values()) \
            or measured is None or recorded != measured:
        raise ValueError("cache counters reset or delta differs")
    if sum(measured.values()) == 0:
        raise ValueError("no compiler cache activity observed during the producer")
    return measured


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--telemetry", required=True, type=Path)
    parser.add_argument("--check-telemetry", action="store_true")
    parser.add_argument("producer_args", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if args.check_telemetry:
        if args.producer_args:
            parser.error("telemetry verification does not run a producer")
        print(json.dumps(qualify_observation(args.telemetry), sort_keys=True))
        return 0
    producer_args = args.producer_args
    if producer_args[:1] == ["--"]:
        producer_args = producer_args[1:]
    if not producer_args:
        parser.error("the existing producer arguments are required")
    return run_profile(args.telemetry, producer_args)


if __name__ == "__main__":
    sys.exit(main())
