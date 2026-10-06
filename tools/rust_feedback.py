#!/usr/bin/env python3
"""Measure one focused Cargo check/test, keeping output live and evidence scoped.

Uses the chosen Cargo command unchanged; no flags/cache changes or CI dispatch.
Client timestamps include transport; Cargo artifacts are not test results.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import time
import tomllib
import uuid

ROOT = Path(__file__).resolve().parents[1]


def duration_seconds(text: str) -> float | None:
    compact = text.replace(" ", "")
    if not re.fullmatch(r"(?:\d+(?:\.\d+)?[hms])+", compact):
        return None
    return sum(float(n) * {"h": 3600, "m": 60, "s": 1}[unit]
               for n, unit in re.findall(r"(\d+(?:\.\d+)?)([hms])", compact))


class Observation:
    def __init__(self, requested=()):
        self.requested = set(requested)
        self.first_requested_artifact_seconds = None
        self.package_cache_wait_messages = 0
        self.first_diagnostic_seconds = None
        self.first_error_seconds = None
        self.first_artifact_seconds = None
        self.first_test_result_seconds = None
        self.units = {}
        self.tests = {"passed": 0, "failed": 0, "ignored": 0, "filtered_out": 0}
        self.test_summaries = 0
        self.build_success = None
        self.reported_stages = {}

    def observe(self, line: str, elapsed: float) -> None:
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            message = None
        if isinstance(message, dict):
            reason = message.get("reason")
            if reason == "compiler-message":
                level = message.get("message", {}).get("level")
                if level in {"warning", "error"} and self.first_diagnostic_seconds is None:
                    self.first_diagnostic_seconds = elapsed
                if level == "error" and self.first_error_seconds is None:
                    self.first_error_seconds = elapsed
            elif reason == "compiler-artifact":
                if self.first_artifact_seconds is None:
                    self.first_artifact_seconds = elapsed
                target = message.get("target", {})
                package_name = None
                try:
                    manifest = Path(message["manifest_path"])
                    package_name = tomllib.loads(manifest.read_text())["package"]["name"]
                except (OSError, KeyError, ValueError):
                    pass
                if package_name in self.requested and self.first_requested_artifact_seconds is None:
                    self.first_requested_artifact_seconds = elapsed
                key = (message.get("package_id"), target.get("name"),
                       tuple(target.get("kind", [])), message.get("profile", {}).get("test"))
                self.units[key] = {"package": message.get("package_id"),
                                   "package_name": package_name, "target": target.get("name"), "kind": target.get("kind"),
                                   "test": message.get("profile", {}).get("test"),
                                   "fresh": message.get("fresh"), "observed_seconds": elapsed}
            elif reason == "build-finished":
                self.build_success = message.get("success")
        # Cargo's stable libtest summary; never infer execution from artifacts or exit 0.
        match = re.search(r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;.*? (\d+) filtered out", line)
        if match:
            self.test_summaries += 1
            counts = [int(value) for value in match.groups()]
            for key, count in zip(self.tests, counts):
                self.tests[key] += count
            if counts[0] + counts[1] and self.first_test_result_seconds is None:
                self.first_test_result_seconds = elapsed
        match = re.search(r"Summary\s*\[.*?\]\s*(\d+) tests? run: (\d+) passed(?:.*?[, ] (\d+) failed)?", line)
        if match:
            total, passed, failed = match.groups()
            self.test_summaries += 1
            self.tests["passed"] += int(passed)
            self.tests["failed"] += int(failed or 0)
            if int(total) and self.first_test_result_seconds is None:
                self.first_test_result_seconds = elapsed
        if "Blocking waiting for file lock on package cache" in line:
            self.package_cache_wait_messages += 1
        match = re.search(r"Finished `.*? target\(s\) in (.+?)\s*$", line)
        if match:
            seconds = duration_seconds(match[1])
            if seconds is not None:
                self.reported_stages["cargo"] = seconds
        match = re.search(r"^[^: ]+: (.+?) in (.+?)\s*$", line)
        if match and (seconds := duration_seconds(match[2])) is not None:
            label = match[1]
            # Only named transport/lock milestones; never retain arbitrary console text.
            for phrase, stage in (("got a build slot", "builder_admission_queue"),
                                  ("synced sources", "source_sync"),
                                  ("seeded builder worktree", "worktree_seed"),
                                  ("got the lock", "worktree_lock_wait"),
                                  ("fetched", "artifact_fetch")):
                if label.startswith(phrase):
                    self.reported_stages[stage] = self.reported_stages.get(stage, 0) + seconds

    def receipt(self) -> dict:
        return {"first_diagnostic_seconds": self.first_diagnostic_seconds,
                "first_error_seconds": self.first_error_seconds,
                "first_requested_artifact_seconds": self.first_requested_artifact_seconds,
                "package_cache_wait_messages": self.package_cache_wait_messages,
                "first_artifact_seconds": self.first_artifact_seconds,
                "first_test_result_seconds": self.first_test_result_seconds,
                "build_success": self.build_success,
                "artifact_units": list(self.units.values()),
                "fresh_units": sum(unit["fresh"] is True for unit in self.units.values()),
                "rebuilt_units": sum(unit["fresh"] is False for unit in self.units.values()),
                "tests": self.tests, "test_summaries": self.test_summaries,
                "driver_reported_seconds": self.reported_stages,
                "unknown": (["builder_admission_queue_seconds"] if "builder_admission_queue" not in self.reported_stages else []) + ["link_seconds", "per_run_cache_hit_rate",
                            "compiler_internal_concurrency"],
                "clock": "client monotonic; delivery includes transport/buffering"}


def command(args) -> list[str]:
    argv = shlex.split(getattr(args, "cargo_command", None) or os.environ.get("CARGO", "cargo"))
    if not argv:
        raise ValueError("Cargo command must not be empty")
    argv.extend([args.operation, "--locked", "--color=never"])
    for package in args.package:
        argv.extend(["-p", package])
    named_tests = [args.test] if isinstance(args.test, str) else (args.test or [])
    if not (args.lib or args.tests or named_tests):
        raise ValueError("select --lib, --tests, or at least one --test target")
    if args.tests and (args.lib or named_tests):
        raise ValueError("--tests already includes library and integration targets; choose all or explicit targets")
    if args.lib:
        argv.append("--lib")
    if args.tests:
        argv.append("--tests")
    for name in named_tests:
        argv.extend(["--test", name])
    # A Cargo drop-in may use the project's nextest command, whose message formats
    # differ from Cargo. Preserve its normal test runner and human summary.
    if args.operation == "check":
        argv.append("--message-format=json")
    if args.filter is not None:
        if args.operation != "test":
            raise ValueError("--filter requires test")
        argv.append(args.filter)
        if args.exact:
            argv.extend(["--", "--exact"])
    elif args.exact:
        raise ValueError("--exact requires --filter")
    return argv


def identity(root: Path) -> dict:
    def git(*args):
        return subprocess.check_output(["git", *args], cwd=root)
    return {"head": git("rev-parse", "HEAD").decode().strip(),
            "tracked_diff_sha256": hashlib.sha256(git("diff", "HEAD", "--binary")).hexdigest(),
            "untracked_files": [{"path": name, "sha256": hashlib.sha256((root / name).read_bytes()).hexdigest()}
                                for name in git("ls-files", "--others", "--exclude-standard").decode().splitlines()
                                if (root / name).is_file()]}


def run(args, root: Path = ROOT) -> int:
    argv = command(args)
    source = identity(root)
    out = args.out or root / "target/rust-feedback" / (uuid.uuid4().hex + ".json")
    out.parent.mkdir(parents=True, exist_ok=True)
    # Claim evidence before starting a build; an existing receipt must stay intact.
    with out.open("x", encoding="utf-8") as stream:
        base = {"schema": 1, "argv": argv, "source": source,
                "started_at": datetime.now(timezone.utc).isoformat(),
                "scope": "focused-feedback", "integration_qualification": False,
                "status": "running"}
        json.dump(base, stream, indent=2)
    observed = Observation(args.package)
    started = time.monotonic()
    status = 2
    error = None
    try:
        with subprocess.Popen(argv, cwd=root, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                              text=True, encoding="utf-8", errors="replace") as child:
            try:
                for line in child.stdout:
                    observed.observe(line, round(time.monotonic() - started, 3))
                    # Keep normal diagnostics visible without pages of artifact JSON.
                    try:
                        message = json.loads(line)
                    except json.JSONDecodeError:
                        message = None
                    if isinstance(message, dict) and message.get("reason") == "compiler-message":
                        print(message.get("message", {}).get("rendered", ""), end="", flush=True)
                    elif not isinstance(message, dict):
                        print(line, end="", flush=True)
                status = child.wait()
            except KeyboardInterrupt:
                child.terminate()
                try:
                    child.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
                status = 130
                error = "interrupted; cancellation is handled by the selected Cargo command"
    except OSError as exc:
        error = type(exc).__name__
    data = {**base, **observed.receipt(), "elapsed_seconds": round(time.monotonic() - started, 3),
            "exit_code": status, "error": error}
    # A successful empty filter, absent build-finished, or zero observations is not proof.
    verified = (status == 0 and observed.build_success is True and bool(observed.units))
    if args.operation == "test":
        verified = status == 0 and observed.tests["passed"] > 0 and observed.tests["failed"] == 0
        data["unknown"].append("test runner compile artifacts/diagnostic timestamps when no Cargo JSON is emitted")
    try:
        data["source_changed_during_run"] = identity(root) != source
    except (OSError, subprocess.CalledProcessError):
        data["source_changed_during_run"] = True
        data["unknown"].append("source identity unavailable at completion")
    verified = verified and not data["source_changed_during_run"]
    data["status"] = "success" if verified else "failure"
    temporary = out.with_suffix(out.suffix + ".tmp")
    temporary.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    temporary.replace(out)
    print(f"{data['status']}: {out}", flush=True)
    return (0 if verified else 1) if status == 0 else (status if status > 0 else 1)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("check", "test"))
    parser.add_argument("--cargo-command", help="Cargo/drop-in command with arguments (default: $CARGO or cargo)")
    parser.add_argument("-p", "--package", action="append", required=True)
    targets = parser.add_argument_group("targets (select all tests or combine explicit targets)")
    targets.add_argument("--lib", action="store_true")
    targets.add_argument("--tests", action="store_true")
    targets.add_argument("--test", action="append", help="named integration target (repeatable; may be combined with --lib)")
    parser.add_argument("--filter")
    parser.add_argument("--exact", action="store_true")
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()
    try:
        return run(args)
    except (ValueError, OSError, subprocess.CalledProcessError) as exc:
        print(f"rust feedback: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
