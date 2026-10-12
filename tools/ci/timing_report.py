#!/usr/bin/env python3
"""Read-only Forgejo CI experience report; never stores raw job logs.

Offline: --input evidence.json (object containing runs, each with jobs and optional log).
Live: --repo owner/repo --run 123 --run 124 [--hostname code.tilo.so].
Run numbers match the Forgejo web URL and fgj, not global API database IDs.
Recent stream: --repo owner/repo --recent 20 [--workflow check.yml] [--head SHA].
JSON goes to --output or stdout; a compact human summary goes to stderr.
Pull-to-create is an observed interval, including registry/auth/setup overhead,
not a measurement of network transfer alone. Parallel job times are never summed.
Measured phases also retain bounded CPU/quota/pressure and cumulative job peak
memory readings; missing readings stay unknown. Process RSS and container peak
memory are different metrics. Checkout's own markers cover missing runner steps.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
import http.client
import json
from pathlib import Path
import re
import subprocess
import sys
from urllib.parse import urlsplit


EVENTS = {
    "runner_received": re.compile(r"^Runner .+ received task \d+ of job "),
    "pull_requested": re.compile(r"^\s*🐳\s+docker pull image="),
    "container_create": re.compile(r"^\s*🐳\s+docker create image="),
    "container_run": re.compile(r"^\s*🐳\s+docker run image="),
    "job_finished": re.compile(r"^🏁\s+Job (?:succeeded|failed)\s*$"),
}

TERMINAL = {"success", "failure", "cancelled", "skipped"}
ACTIVE = {"waiting", "queued", "running", "blocked"}
MEASUREMENT = re.compile(r"^==> ([A-Za-z0-9 ._()/:+-]{1,120}) measurements$")
STEP_START = re.compile(r"^\s*⭐\s+Run Main (.{1,160})$")
STEP_FINISH = re.compile(r"^\s*(?:✅\s+Success|❌\s+Failure)\s+-\s+Main (.{1,160})$")
CHECKOUT_START = re.compile(r"^Syncing repository: ")
CHECKOUT_FINISH = re.compile(r"^::remove-matcher owner=checkout-git::$")
SCCACHE = {
    "compile_requests": re.compile(r"^Compile requests\s+([0-9]+)\s*$"),
    "cache_hits": re.compile(r"^Cache hits\s+([0-9]+)\s*$"),
    "cache_misses": re.compile(r"^Cache misses\s+([0-9]+)\s*$"),
}
PHASE_LABELS = ("checkout", "toolchain", "cache", "artifact", "setup", "build", "check", "clippy", "compile", "test")
SCALAR_METRICS = re.compile(
    r"^\s*(wall_seconds|user_seconds|system_seconds|max_rss_kib|cgroup_memory_peak|cgroup_memory_max)="
    r"([0-9]{1,20}(?:\.[0-9]{1,6})?|unavailable|max)\s*$"
)
CPU_COUNTERS = ("usage_usec", "user_usec", "system_usec", "nr_periods", "nr_throttled", "throttled_usec")


def stamp(value):
    if not value or value.startswith("1970-01-01") or value.startswith("0001-"):
        return None
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        raise ValueError("timestamp must include timezone")
    return parsed


def canonical_timestamp(value):
    """Render an already-validated timestamp without retaining its raw text."""
    return value.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")


def elapsed(start, end):
    left, right = stamp(start), stamp(end)
    if left is None or right is None:
        return None
    seconds = (right - left).total_seconds()
    if seconds < 0:
        raise ValueError("timestamps are out of order")
    return round(seconds, 3)


def fixed_phase_label(value):
    """Classify an untrusted log label without retaining any of its text."""
    words = set(re.findall(r"[a-z]+", value.casefold()))
    if words & {"downloads", "restore", "sccache"}:
        return "cache"
    if words & {"upload", "artifact", "artifacts"}:
        return "artifact"
    if words & {"package", "packaging"}:
        return "build"
    if words & {"tests", "suites"}:
        return "test"
    return next((label for label in PHASE_LABELS if label in words), "other")


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
                    if container_image is None:
                        container_image = matched_image
                    elif matched_image != container_image:
                        continue
                    # A later action pull is not startup, even when it uses the
                    # same image as a job whose initial create needed no pull.
                    if event == "pull_requested" and "container_create" in found:
                        continue
                parsed_timestamp = stamp(timestamp)
                if parsed_timestamp is not None:
                    found[event] = canonical_timestamp(parsed_timestamp)
    return found


def resource_metric(message):
    """Accept only named, bounded numbers; never retain arbitrary log values."""
    match = SCALAR_METRICS.fullmatch(message)
    if match:
        key, value = match.groups()
        if value in {"unavailable", "max"}:
            return key, None
        if key.endswith("seconds"):
            return key, round(float(value), 3)
        if value.isdigit():
            return key, int(value)
        return None
    match = re.fullmatch(
        r"\s*cgroup_(cpu_max|cpuset_cpus_effective|cpu_stat_before|cpu_stat_after|"
        r"(?:cpu|memory|io)_pressure_(?:before|after))=unavailable\s*", message)
    if match:
        key = match.group(1)
        return {"cpu_max": "cpu_limit", "cpuset_cpus_effective": "cpuset_cpu_count"}.get(key, key), None
    match = re.fullmatch(r"\s*cgroup_cpu_max=(max|[0-9]{1,20}) ([0-9]{1,20})\s*", message)
    if match:
        quota, period = match.groups()
        if int(period) == 0:
            return None
        return "cpu_limit", None if quota == "max" else round(int(quota) / int(period), 3)
    match = re.fullmatch(r"\s*cgroup_cpuset_cpus_effective=([0-9,-]{1,1000})\s*", message)
    if match:
        count = 0
        for group in match.group(1).split(","):
            bounds = group.split("-")
            if not all(value.isdigit() for value in bounds) or len(bounds) > 2:
                return None
            first, last = int(bounds[0]), int(bounds[-1])
            if last < first or last > 1048576:
                return None
            count += last - first + 1
        return "cpuset_cpu_count", count
    match = re.fullmatch(r"\s*cgroup_(cpu|memory|io)_pressure_(before|after)=(.{1,1000})", message)
    if match:
        totals = {}
        for kind in ("some", "full"):
            found = re.search(r"(?:^| )" + kind + r" (?:avg(?:10|60|300)=[0-9]+(?:\.[0-9]+)? )*total=([0-9]{1,20})(?: |$)", match.group(3))
            if found:
                totals[kind] = int(found.group(1))
        if totals:
            return match.group(1) + "_pressure_" + match.group(2), totals
        return None
    match = re.fullmatch(r"\s*cgroup_cpu_stat_(before|after)=(.{1,1000})", message)
    if match:
        counters = {}
        for key in CPU_COUNTERS:
            found = re.search(r"(?:^| )" + key + r" ([0-9]{1,20})(?: |$)", match.group(2))
            if found:
                counters[key] = int(found.group(1))
        if counters:
            return "cpu_stat_" + match.group(1), counters
    return None


def extract_job_metrics(log):
    """Extract bounded timing/cache values while discarding arbitrary log text."""
    events = extract_events(log)
    measurements = []
    sccache = {}
    steps = []
    current_step = None
    current_step_raw = None
    pending_measurement = None
    pending_lines = 0
    last_timestamp = None
    for line in log.splitlines():
        parts = line.split(" ", 1)
        if len(parts) != 2:
            continue
        timestamp, message = parts
        try:
            parsed_timestamp = stamp(timestamp)
        except ValueError:
            # Forgejo may include untimestamped runner diagnostics. They are
            # neither phase boundaries nor safe evidence for elapsed time.
            continue
        if parsed_timestamp is None:
            continue
        timestamp = canonical_timestamp(parsed_timestamp)
        last_timestamp = timestamp

        match = STEP_START.match(message)
        if match:
            pending_measurement = None
            if current_step:
                current_step["stopped"] = timestamp
                current_step["seconds"] = elapsed(current_step["started"], timestamp)
                steps.append(current_step)
            current_step_raw = match.group(1)
            current_step = {"name": fixed_phase_label(current_step_raw), "started": timestamp,
                            "stopped": None, "seconds": None}
            continue
        match = STEP_FINISH.match(message)
        if match and current_step and match.group(1) == current_step_raw:
            current_step["stopped"] = timestamp
            current_step["seconds"] = elapsed(current_step["started"], timestamp)
            steps.append(current_step)
            current_step = None
            current_step_raw = None
        elif current_step and current_step["name"] == "checkout" and CHECKOUT_FINISH.match(message):
            current_step["stopped"] = timestamp
            current_step["seconds"] = elapsed(current_step["started"], timestamp)
            steps.append(current_step)
            current_step = None
            current_step_raw = None

        # Some runner versions omit Main boundaries for later JS actions.
        # Checkout's own bounded markers still identify every checkout, without
        # retaining its repository name or inferring a missing finish time.
        if CHECKOUT_START.match(message) and current_step is None:
            current_step = {"name": "checkout", "started": timestamp,
                            "stopped": None, "seconds": None}
            current_step_raw = None

        match = MEASUREMENT.match(message)
        if match:
            pending_measurement = {"name": fixed_phase_label(match.group(1))}
            pending_lines = 0
            continue
        if pending_measurement is not None:
            pending_lines += 1
            metric = resource_metric(message)
            if pending_lines > 32 or metric is None:
                pending_measurement = None
            else:
                key, value = metric
                if key == "wall_seconds" and "wall_seconds" not in pending_measurement and value is not None:
                    measurements.append(pending_measurement)
                pending_measurement[key] = value

        for key, pattern in SCCACHE.items():
            match = pattern.match(message)
            if match and key not in sccache:
                sccache[key] = int(match.group(1))

    for measurement in measurements:
        before = measurement.get("cpu_stat_before") or {}
        after = measurement.get("cpu_stat_after") or {}
        for key, output in (("usage_usec", "cgroup_cpu_seconds"),
                            ("throttled_usec", "cgroup_throttled_seconds")):
            if key in before and key in after and after[key] >= before[key]:
                measurement[output] = round((after[key] - before[key]) / 1e6, 3)

    if "cache_hits" in sccache and "cache_misses" in sccache:
        # Compiler probes and uncacheable requests are not cache lookups.
        lookups = sccache["cache_hits"] + sccache["cache_misses"]
        sccache["hit_percent"] = None if lookups == 0 else round(100 * sccache["cache_hits"] / lookups, 1)
    return {"events": events, "measurements": measurements, "steps": steps,
            "sccache": sccache or None, "last_timestamp": last_timestamp}


def experience_summary(runs):
    """Summarize waste visible in a bounded, newest-first run snapshot."""
    newer = set()
    stale = []
    cancelled_seconds = 0.0
    cancelled_with_compute = 0
    orphan_task_candidates = []
    started_without_runner = []
    current_queues = []
    collection_error_ids = []
    for run in runs:
        key = (run.get("workflow_id"), run.get("event"), run.get("ref"))
        if run.get("status") in ACTIVE and key in newer:
            stale.append(run["id"])
        newer.add(key)
        if run.get("collection_errors"):
            collection_error_ids.append(run["id"])
        if run.get("current_queue_seconds") is not None:
            current_queues.append(run["current_queue_seconds"])
        if run.get("status") == "cancelled":
            for job in run.get("jobs", []):
                observed = job.get("runner_observed_seconds")
                if observed is not None:
                    cancelled_with_compute += 1
                    cancelled_seconds += observed
        for job in run.get("jobs", []):
            if run.get("status") in TERMINAL and job.get("status") in ACTIVE:
                orphan_task_candidates.append(job["id"])
        if (run.get("status") in ACTIVE and stamp(run.get("started")) is not None
                and run.get("jobs")
                and not any(job.get("events", {}).get("runner_received") for job in run["jobs"])):
            started_without_runner.append(run["id"])
    return {
        "stale_active_runs": len(stale),
        "stale_active_run_ids": stale,
        "cancelled_jobs_with_observed_compute": cancelled_with_compute,
        "wasted_cancelled_runner_seconds": round(cancelled_seconds, 3),
        "orphan_task_candidates": len(orphan_task_candidates),
        "orphan_task_candidate_ids": orphan_task_candidates,
        "started_runs_without_runner": len(started_without_runner),
        "started_run_ids_without_runner": started_without_runner,
        "oldest_current_queue_seconds": max(current_queues, default=None),
        "runs_with_collection_errors": len(collection_error_ids),
        "run_ids_with_collection_errors": collection_error_ids,
    }


def phase_seconds(metrics):
    checkout = sum(step["seconds"] for step in metrics["steps"]
                   if step["seconds"] is not None and step["name"].casefold() == "checkout")
    phase_words = ("build", "check", "clippy", "compile", "test")
    compile_test = sum(item["wall_seconds"] for item in metrics["measurements"]
                       if any(word in item["name"].casefold() for word in phase_words))
    return round(checkout, 3), round(compile_test, 3)


def attempt_identity(run):
    """Return the current attempt and its observable creation timestamp."""
    attempts = [job.get("attempt") for job in run.get("jobs", [])
                if isinstance(job.get("attempt"), int) and job["attempt"] > 0]
    attempt = max(attempts, default=1)
    if attempt == 1:
        return attempt, run.get("created")
    candidates = [job.get("created_at") for job in run.get("jobs", [])
                  if job.get("attempt") == attempt and stamp(job.get("created_at")) is not None]
    return attempt, min(candidates, key=stamp) if candidates else None


def report(evidence):
    observed_at = evidence.get("observed_at")
    output = {"schema_version": 3, "observed_at": observed_at, "runs": []}
    for run in evidence["runs"]:
        created, started, stopped = (run.get(key) for key in ("created", "started", "stopped"))
        attempt, attempt_created = attempt_identity(run)
        item = {"id": run["id"], "index_in_repo": run.get("index_in_repo"),
                "workflow_id": run.get("workflow_id"),
                "commit_sha": run.get("commit_sha"), "event": run.get("event"),
                "ref": run.get("prettyref", run.get("ref")), "status": run.get("status"),
                "collection_errors": run.get("collection_errors", []),
                "attempt": attempt, "created": created, "attempt_created": attempt_created,
                "started": started, "stopped": stopped,
                "queue_seconds": elapsed(attempt_created, started),
                "current_queue_seconds": (elapsed(attempt_created, observed_at)
                    if run.get("status") in ACTIVE and stamp(started) is None else None),
                "wall_seconds": elapsed(started, stopped),
                "total_seconds": elapsed(attempt_created, stopped), "jobs": []}
        for job in run.get("jobs", []):
            metrics = extract_job_metrics(job.get("log", ""))
            events = metrics["events"]
            checkout_seconds, compile_test_seconds = phase_seconds(metrics)
            item["jobs"].append({
                "id": job["id"], "name": job.get("name"), "events": events,
                "status": job.get("status"), "steps": metrics["steps"],
                "collection_error": job.get("collection_error"),
                "task_materialized": bool(job.get("task_id")),
                "measurements": metrics["measurements"], "sccache": metrics["sccache"],
                "checkout_seconds": checkout_seconds or None,
                "compile_test_seconds": compile_test_seconds or None,
                "startup_pull_observed": (None if job.get("collection_error")
                                            else "pull_requested" in events),
                "queue_seconds": elapsed(job.get("created_at"), job.get("started_at")),
                "run_created_to_runner_received_seconds": (
                    elapsed(created, events.get("runner_received")) if attempt == 1 else None
                ),
                "attempt_created_to_runner_received_seconds": elapsed(
                    attempt_created, events.get("runner_received")
                ),
                "run_started_to_runner_received_seconds": elapsed(started, events.get("runner_received")),
                "runner_to_create_seconds": elapsed(events.get("runner_received"), events.get("container_create")),
                "pull_to_create_seconds": elapsed(events.get("pull_requested"), events.get("container_create")),
                "create_to_run_seconds": elapsed(events.get("container_create"), events.get("container_run")),
                "runner_to_run_seconds": elapsed(events.get("runner_received"), events.get("container_run")),
                "runner_to_finish_seconds": elapsed(events.get("runner_received"), events.get("job_finished")),
                "runner_observed_seconds": elapsed(events.get("runner_received"), metrics["last_timestamp"]),
            })
        output["runs"].append(item)
    output["experience"] = experience_summary(output["runs"])
    return output


def comparison(result):
    """Compare two explicit runs without inventing values for missing evidence."""
    if len(result["runs"]) != 2:
        raise ValueError("comparison requires exactly two runs")
    before, after = result["runs"]

    def delta(field, left=before, right=after):
        old, new = left.get(field), right.get(field)
        return None if old is None or new is None else round(new - old, 3)

    compared = {
        "before_run": before["id"],
        "after_run": after["id"],
        "queue_seconds_delta": delta("queue_seconds"),
        "wall_seconds_delta": delta("wall_seconds"),
        "jobs": [],
    }

    def unique_named(jobs):
        grouped = {}
        for job in jobs:
            if job.get("name"):
                grouped.setdefault(job["name"], []).append(job)
        return {name: matches[0] for name, matches in grouped.items() if len(matches) == 1}

    old_jobs = unique_named(before["jobs"])
    new_jobs = unique_named(after["jobs"])
    for name in sorted(old_jobs.keys() & new_jobs.keys()):
        old, new = old_jobs[name], new_jobs[name]
        compared["jobs"].append({
            "name": name,
            "queue_seconds_delta": delta("queue_seconds", old, new),
            "run_to_runner_seconds_delta": delta("run_started_to_runner_received_seconds", old, new),
            "runner_to_create_seconds_delta": delta("runner_to_create_seconds", old, new),
            "create_to_run_seconds_delta": delta("create_to_run_seconds", old, new),
            "checkout_seconds_delta": delta("checkout_seconds", old, new),
            "compile_test_seconds_delta": delta("compile_test_seconds", old, new),
            "runner_to_finish_seconds_delta": delta("runner_to_finish_seconds", old, new),
        })
    return compared


def latest_comparison(result):
    """Compare the newest run with the newest older run of the same lane."""
    runs = result["runs"]
    for index, current in enumerate(runs):
        if (current.get("status") not in TERMINAL or current.get("wall_seconds") is None
                or current.get("collection_errors")):
            continue
        for baseline in runs[index + 1:]:
            if (baseline.get("workflow_id"), baseline.get("event")) == (
                    current.get("workflow_id"), current.get("event")) \
                    and baseline.get("status") in TERMINAL \
                    and baseline.get("wall_seconds") is not None \
                    and not baseline.get("collection_errors"):
                return comparison({"runs": [baseline, current]})
    raise ValueError("no comparable workflow/event pair in recent runs")


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


def command_json(args, command):
    result = subprocess.run([args.fgj, "--hostname", args.hostname, *command],
                            capture_output=True, timeout=90, check=False)
    if result.returncode:
        raise RuntimeError("Forgejo command failed")
    return json.loads(result.stdout.decode("utf-8-sig"))


class EvidenceGap(RuntimeError):
    def __init__(self, kind):
        super().__init__(kind)
        self.kind = kind


def forgejo_token(args):
    result = subprocess.run(
        [args.fgj, "--hostname", args.hostname, "auth", "token"],
        capture_output=True, timeout=15, check=False,
    )
    token = result.stdout.decode("utf-8-sig").strip()
    if result.returncode or not token:
        raise RuntimeError("Forgejo authentication is unavailable")
    return token


class ForgejoClient:
    """Small persistent client: one TLS session serves every request for a run."""
    def __init__(self, hostname, token):
        parsed = urlsplit(hostname if "://" in hostname else f"https://{hostname}")
        self.scheme = parsed.scheme
        self.host = parsed.hostname
        self.port = parsed.port
        self.token = token
        self.connection = None

    def close(self):
        if self.connection:
            self.connection.close()
            self.connection = None

    def _connect(self):
        connection_type = (http.client.HTTPSConnection if self.scheme == "https"
                           else http.client.HTTPConnection)
        self.connection = connection_type(self.host, self.port, timeout=30)

    def get(self, endpoint, raw=False):
        path = endpoint if endpoint.startswith("/api/v1/") else f"/api/v1/{endpoint.lstrip('/')}"
        for attempt in range(2):
            try:
                if self.connection is None:
                    self._connect()
                self.connection.request("GET", path, headers={
                    "Authorization": f"token {self.token}",
                    "Accept": "application/json" if not raw else "text/plain",
                    "User-Agent": "tilo-ci-experience/1",
                })
                response = self.connection.getresponse()
                body = response.read(50_000_001)
                if len(body) > 50_000_000:
                    self.close()
                    raise EvidenceGap("response_too_large")
                if response.status == 404:
                    raise EvidenceGap("not_found")
                if response.status >= 500:
                    raise EvidenceGap("server_error")
                if response.status >= 400:
                    raise EvidenceGap("request_rejected")
                try:
                    value = body.decode("utf-8-sig")
                    return value if raw else json.loads(value)
                except (UnicodeDecodeError, json.JSONDecodeError) as exc:
                    raise EvidenceGap("invalid_response") from exc
            except EvidenceGap:
                raise
            except (OSError, http.client.HTTPException):
                self.close()
                if attempt:
                    raise EvidenceGap("connection_failed") from None
        raise AssertionError("unreachable")


def hydrate_recent_run(args, run, token):
    client = ForgejoClient(args.hostname, token)
    prefix = f"repos/{args.repo}/actions"
    errors = []
    try:
        try:
            jobs = client.get(f"{prefix}/runs/{run['id']}/jobs")
            if isinstance(jobs, dict):
                jobs = jobs["jobs"]
        except (EvidenceGap, KeyError, TypeError) as exc:
            kind = exc.kind if isinstance(exc, EvidenceGap) else "invalid_response"
            run["jobs"] = []
            run["collection_errors"] = [{"stage": "jobs", "kind": kind}]
            return run
        for job in jobs:
            if not job.get("task_id"):
                job["log"] = ""
                continue
            try:
                job["log"] = client.get(f"{prefix}/jobs/{job['id']}/logs", raw=True)
            except EvidenceGap as exc:
                job["log"] = ""
                job["collection_error"] = {"stage": "log", "kind": exc.kind}
                errors.append({"stage": "log", "job_id": job.get("id"), "kind": exc.kind})
        run["jobs"] = jobs
        run["collection_errors"] = errors
        return run
    finally:
        client.close()


def collect(args):
    runs = []
    for number in args.run:
        if number < 1:
            raise ValueError("run number must be positive")
        prefix = f"repos/{args.repo}/actions"
        resolved = command_json(args, ["actions", "run", "view", str(number),
                                       "--repo", args.repo, "--json"]).get("run", {})
        run_id = resolved.get("id")
        if (type(run_id) is not int or run_id < 1
                or resolved.get("index_in_repo") != number
                or not resolved.get("commit_sha")):
            raise ValueError("resolved run does not match requested repository run number")
        run = api(args, f"{prefix}/runs/{run_id}")
        if (run.get("id") != run_id or run.get("index_in_repo") != number
                or run.get("repository", {}).get("full_name") != args.repo
                or run.get("commit_sha") != resolved["commit_sha"]):
            raise ValueError("returned run identity does not match resolved repository run")
        # Resolve the UI number once, then use its verified database ID for jobs.
        jobs = api(args, f"{prefix}/runs/{run_id}/jobs")
        if isinstance(jobs, dict):
            jobs = jobs["jobs"]
        for job in jobs:
            job["log"] = api(args, f"{prefix}/jobs/{job['id']}/logs", raw=True) if job.get("task_id") else ""
        run["jobs"] = jobs
        runs.append(run)
    return {"runs": runs}


def collect_recent(args):
    listed = command_json(args, ["actions", "run", "list", "--repo", args.repo,
                                 "--limit", str(args.recent), "--json"])
    selected = [run for run in listed
                if (not args.workflow or run.get("workflow_id") == args.workflow)
                and (not args.head or run.get("commit_sha") == args.head)]
    for run in selected:
        # The compact listing's updated timestamp is Forgejo's terminal stop
        # timestamp. Keep active wall time unknown rather than measuring it at
        # collection time.
        run["stopped"] = run.get("updated") if run.get("status") in TERMINAL else None
    token = forgejo_token(args)
    # Four independent keep-alive connections bound load and avoid one CLI/TLS
    # setup per endpoint. map() preserves Forgejo's newest-first ordering.
    with ThreadPoolExecutor(max_workers=min(4, max(1, len(selected)))) as pool:
        selected = list(pool.map(lambda run: hydrate_recent_run(args, run, token), selected))
    return {"observed_at": datetime.now(timezone.utc).isoformat(), "runs": selected}


def human_summary(result):
    def fmt(value):
        return "unknown" if value is None else f"{value:.3f}s"
    def percent(value):
        return "unknown" if value is None else f"{value:.1f}%"
    lines = []
    for run in result["runs"]:
        identity = " / ".join(value for value in (run.get("workflow_id"), run.get("commit_sha")) if value)
        attempt = f" attempt {run['attempt']}" if run.get("attempt", 1) > 1 else ""
        number = run.get("index_in_repo")
        label = f"Run {number} (API ID {run['id']})" if number else f"Run {run['id']}"
        lines.append(f"{label}{attempt} ({identity or 'unknown'}): queue {fmt(run['queue_seconds'])}; wall {fmt(run['wall_seconds'])}")
        if run["collection_errors"]:
            gaps = ", ".join(f"{gap['stage']}:{gap['kind']}" for gap in run["collection_errors"])
            lines.append(f"  Evidence gap: {gaps}")
        for job in run["jobs"]:
            label = f"{job['name']} ({job['id']})" if job.get("name") else str(job["id"])
            lines.append(f"  Job {label}: job queue {fmt(job['queue_seconds'])}; received→create {fmt(job['runner_to_create_seconds'])}; pull→create {fmt(job['pull_to_create_seconds'])}")
            lines.append(
                f"    run→runner {fmt(job['run_started_to_runner_received_seconds'])}; "
                f"create→run {fmt(job['create_to_run_seconds'])}; "
                f"checkout {fmt(job['checkout_seconds'])}; "
                f"compile/test {fmt(job['compile_test_seconds'])}; "
                f"total {fmt(job['runner_to_finish_seconds'])}"
            )
            for measurement in job["measurements"]:
                resource = ""
                if "cgroup_cpu_seconds" in measurement:
                    resource += f"; CPU {fmt(measurement['cgroup_cpu_seconds'])}"
                if measurement.get("cpu_limit") is not None:
                    resource += f"; quota {measurement['cpu_limit']} CPUs"
                if measurement.get("cgroup_memory_peak") is not None:
                    resource += f"; cumulative job peak {measurement['cgroup_memory_peak']} bytes"
                lines.append(f"    {measurement['name']}: {fmt(measurement['wall_seconds'])}{resource}")
            if job["sccache"]:
                lines.append(f"    sccache hit rate {percent(job['sccache'].get('hit_percent'))}")
    experience = result.get("experience", {})
    lines.append(
        f"Snapshot: {experience.get('stale_active_runs', 0)} stale active; "
        f"cancelled runner time "
        f"{fmt(experience.get('wasted_cancelled_runner_seconds'))}; "
        f"{experience.get('orphan_task_candidates', 0)} orphan candidates; "
        f"{experience.get('started_runs_without_runner', 0)} started without runner; "
        f"oldest live queue {fmt(experience.get('oldest_current_queue_seconds'))}; "
        f"{experience.get('runs_with_collection_errors', 0)} collection gaps"
    )
    if "comparison" in result:
        item = result["comparison"]
        lines.append(
            f"Change {item['before_run']}→{item['after_run']}: "
            f"queue {fmt(item['queue_seconds_delta'])}; wall {fmt(item['wall_seconds_delta'])}"
        )
        for job in item["jobs"]:
            lines.append(
                f"  {job['name']}: job queue {fmt(job['queue_seconds_delta'])}; "
                f"run→runner {fmt(job['run_to_runner_seconds_delta'])}; "
                f"received→create {fmt(job['runner_to_create_seconds_delta'])}; "
                f"checkout {fmt(job['checkout_seconds_delta'])}; "
                f"compile/test {fmt(job['compile_test_seconds_delta'])}; "
                f"received→finish {fmt(job['runner_to_finish_seconds_delta'])}"
            )
    elif result.get("comparison_error"):
        lines.append(f"Comparison unavailable: {result['comparison_error']}")
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--repo")
    parser.add_argument("--run", type=int, action="append", default=[],
                        help="repository run number from Forgejo URL or fgj (not API database ID)")
    parser.add_argument("--recent", type=int, help="inspect this many newest runs (maximum 100)")
    parser.add_argument("--workflow", help="with --recent, select one workflow filename")
    parser.add_argument("--head", help="with --recent, select one exact commit SHA")
    parser.add_argument("--hostname", default="code.tilo.so")
    parser.add_argument("--fgj", default="fgj")
    parser.add_argument("--compare", action="store_true", help="compare exactly two runs in the order supplied")
    parser.add_argument("--compare-latest", action="store_true",
                        help="with --recent, compare the newest matching workflow/event pair")
    args = parser.parse_args()
    if args.recent is not None and not 1 <= args.recent <= 100:
        parser.error("--recent must be between 1 and 100")
    if args.input and (args.run or args.repo or args.recent):
        parser.error("use either local input or explicit live runs")
    if args.run and args.recent:
        parser.error("use either --run or --recent")
    if args.compare and args.compare_latest:
        parser.error("use either --compare or --compare-latest")
    if args.compare_latest and not args.recent:
        parser.error("--compare-latest requires --recent")
    if (args.workflow or args.head) and not args.recent:
        parser.error("--workflow and --head require --recent")
    if not args.input and (not args.repo or (not args.run and not args.recent)):
        parser.error("live mode requires --repo and --run or --recent")
    try:
        evidence = (json.loads(args.input.read_text(encoding="utf-8-sig")) if args.input
                    else collect_recent(args) if args.recent else collect(args))
        result = report(evidence)
        if args.compare:
            result["comparison"] = comparison(result)
        if args.compare_latest:
            try:
                result["comparison"] = latest_comparison(result)
            except ValueError:
                # A partial/outage snapshot is still the evidence needed to
                # diagnose CI. The optional delta must not discard it.
                result["comparison_error"] = "no complete matching workflow/event pair"
        serialized = json.dumps(result, indent=2) + "\n"
        if args.output:
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(serialized, encoding="utf-8")
        else:
            print(serialized, end="")
        print(human_summary(result), file=sys.stderr)
    except (ValueError, KeyError, OSError, RuntimeError, subprocess.TimeoutExpired) as exc:
        parser.exit(1, f"Timing report failed: {type(exc).__name__}\n")


if __name__ == "__main__":
    main()
