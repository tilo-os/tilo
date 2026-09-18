#!/usr/bin/env python3
"""Read-only timing report for explicit Forgejo runs; never stores raw job logs.

Offline: --input evidence.json (object containing runs, each with jobs and optional log).
Live: --repo owner/repo --run 123 --run 124 [--hostname code.podesta.ai].
JSON goes to --output or stdout; a compact human summary goes to stderr.
Pull-to-create is an observed interval, including registry/auth/setup overhead,
not a measurement of network transfer alone. Parallel job times are never summed.
"""
import argparse
from datetime import datetime
import json
from pathlib import Path
import re
import subprocess
import sys


EVENTS = {
    "runner_received": re.compile(r"^Runner .+ received task \d+ of job "),
    "pull_requested": re.compile(r"^\s*🐳\s+docker pull image="),
    "container_create": re.compile(r"^\s*🐳\s+docker create image="),
    "container_run": re.compile(r"^\s*🐳\s+docker run image="),
    "job_finished": re.compile(r"^🏁\s+Job (?:succeeded|failed)\s*$"),
}


def stamp(value):
    if not value or value.startswith("1970-01-01") or value.startswith("0001-"):
        return None
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        raise ValueError("timestamp must include timezone")
    return parsed


def elapsed(start, end):
    left, right = stamp(start), stamp(end)
    if left is None or right is None:
        return None
    seconds = (right - left).total_seconds()
    if seconds < 0:
        raise ValueError("timestamps are out of order")
    return round(seconds, 3)


def extract_events(log):
    """Keep only timestamp/event names, never command text or arbitrary log values."""
    found = {}
    container_image = None
    for line in log.splitlines():
        parts = line.split(" ", 1)
        if len(parts) != 2:
            continue
        timestamp, message = parts
        for event, pattern in EVENTS.items():
            if event not in found and pattern.match(message):
                if event in ("pull_requested", "container_create", "container_run"):
                    matched_image = re.search(r"\bimage=(\S+)", message).group(1)
                    if event == "pull_requested":
                        container_image = matched_image
                    elif container_image is None or matched_image != container_image:
                        continue
                if stamp(timestamp) is not None:
                    found[event] = timestamp
    return found


def report(evidence):
    output = {"schema_version": 1, "runs": []}
    for run in evidence["runs"]:
        created, started, stopped = (run.get(key) for key in ("created", "started", "stopped"))
        item = {"id": run["id"], "created": created, "started": started,
                "stopped": stopped, "queue_seconds": elapsed(created, started),
                "wall_seconds": elapsed(started, stopped), "jobs": []}
        for job in run.get("jobs", []):
            events = extract_events(job.get("log", ""))
            item["jobs"].append({
                "id": job["id"], "events": events,
                "queue_seconds": elapsed(job.get("created_at"), job.get("started_at")),
                "run_created_to_runner_received_seconds": elapsed(created, events.get("runner_received")),
                "pull_to_create_seconds": elapsed(events.get("pull_requested"), events.get("container_create")),
                "create_to_run_seconds": elapsed(events.get("container_create"), events.get("container_run")),
                "runner_to_finish_seconds": elapsed(events.get("runner_received"), events.get("job_finished")),
            })
        output["runs"].append(item)
    return output


def api(args, endpoint, raw=False):
    result = subprocess.run([args.fgj, "--hostname", args.hostname, "api", endpoint],
                            capture_output=True, timeout=90, check=False)
    if result.returncode:
        # stderr can contain response bodies: never echo it into the report.
        raise RuntimeError(f"Forgejo API request failed for {endpoint}")
    if len(result.stdout) > 50_000_000:
        raise RuntimeError("response exceeds report size limit")
    value = result.stdout.decode("utf-8-sig")
    return value if raw else json.loads(value)


def collect(args):
    runs = []
    for run_id in args.run:
        prefix = f"repos/{args.repo}/actions"
        run = api(args, f"{prefix}/runs/{run_id}")
        if run["id"] != run_id:
            raise ValueError("returned run ID does not match request")
        # Explicit run endpoint: no global run listing or polling.
        jobs = api(args, f"{prefix}/runs/{run_id}/jobs")
        if isinstance(jobs, dict):
            jobs = jobs["jobs"]
        for job in jobs:
            job["log"] = api(args, f"{prefix}/jobs/{job['id']}/logs", raw=True) if job.get("task_id") else ""
        run["jobs"] = jobs
        runs.append(run)
    return {"runs": runs}


def human_summary(result):
    def fmt(value):
        return "unknown" if value is None else f"{value:.3f}s"
    lines = []
    for run in result["runs"]:
        lines.append(f"Run {run['id']}: queue {fmt(run['queue_seconds'])}; wall {fmt(run['wall_seconds'])}")
        for job in run["jobs"]:
            lines.append(f"  Job {job['id']}: job queue {fmt(job['queue_seconds'])}; pull→create {fmt(job['pull_to_create_seconds'])}")
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--repo")
    parser.add_argument("--run", type=int, action="append", default=[])
    parser.add_argument("--hostname", default="code.podesta.ai")
    parser.add_argument("--fgj", default="fgj")
    args = parser.parse_args()
    if args.input and (args.run or args.repo):
        parser.error("use either local input or explicit live runs")
    if not args.input and (not args.run or not args.repo):
        parser.error("live mode requires --repo and at least one --run")
    try:
        result = report(json.loads(args.input.read_text(encoding="utf-8-sig")) if args.input else collect(args))
        serialized = json.dumps(result, indent=2) + "\n"
        if args.output:
            args.output.write_text(serialized, encoding="utf-8")
        else:
            print(serialized, end="")
        print(human_summary(result), file=sys.stderr)
    except (ValueError, KeyError, OSError, RuntimeError, subprocess.TimeoutExpired) as exc:
        parser.exit(1, f"Timing report failed: {type(exc).__name__}\n")


if __name__ == "__main__":
    main()
