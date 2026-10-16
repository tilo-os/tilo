#!/usr/bin/env python3
"""Prewarm standard test harness entry; the subsequent Cargo test run owns coverage.

macOS checks newly written executables before entering libtest. Listing them in
a bounded pool pays that cost without serializing every first launch or changing
test scheduling. Custom harnesses are never launched by this optimization.
"""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import threading
import time
import tomllib


MAX_BYTES = 16 * 1024 * 1024
MAX_EXECUTABLES = 512
ACTIVE = set()
ACTIVE_LOCK = threading.Lock()
CANCELLED = threading.Event()


@dataclass(frozen=True)
class Harness:
    executable: Path
    directory: Path


def harnesses(artifacts: Path, source: Path, target: Path) -> list[Harness]:
    source, target = source.resolve(strict=True), target.resolve(strict=True)
    if artifacts.stat().st_size > MAX_BYTES:
        raise ValueError("Cargo artifact metadata exceeds the bounded budget")
    found: dict[Path, Harness] = {}
    manifests = {}
    with artifacts.open(encoding="utf-8") as stream:
        consumed = 0
        while line := stream.readline(MAX_BYTES + 1):
            consumed += len(line.encode("utf-8"))
            if consumed > MAX_BYTES:
                raise ValueError("Cargo artifact metadata grew past its budget")
            row = json.loads(line)
            if (row.get("reason") != "compiler-artifact" or not row.get("executable")
                    or row.get("profile", {}).get("test") is not True):
                continue
            kind = row["target"]["kind"]
            if len(kind) != 1 or kind[0] not in ("lib", "bin", "test"):
                continue
            manifest = Path(row["manifest_path"]).resolve(strict=True)
            executable = Path(row["executable"]).resolve(strict=True)
            if not manifest.is_relative_to(source) or not executable.is_relative_to(target):
                raise ValueError("test artifact escapes its source or target directory")
            if not executable.is_file() or not os.access(executable, os.X_OK):
                raise ValueError("compiled test executable is unavailable")
            if manifest not in manifests:
                manifests[manifest] = tomllib.loads(manifest.read_text(encoding="utf-8"))
            document = manifests[manifest]
            definitions = ([document.get("lib", {})] if kind[0] == "lib"
                           else document.get(kind[0], []))
            custom = any(definition.get("harness") is False and (
                kind[0] == "lib" or definition.get("name") == row["target"]["name"]
                or (definition.get("path") and
                    (manifest.parent / definition["path"]).resolve() ==
                    Path(row["target"]["src_path"]).resolve())) for definition in definitions)
            if custom:
                continue
            found[executable] = Harness(executable, manifest.parent)
            if len(found) > MAX_EXECUTABLES:
                raise ValueError("too many compiled test executables")
    return list(found.values())


def listing_environment(items: list[Harness], rust_library: Path | None) -> dict[str, str]:
    environment = os.environ.copy()
    key = "DYLD_FALLBACK_LIBRARY_PATH" if sys.platform == "darwin" else "LD_LIBRARY_PATH"
    directories = dict.fromkeys(str(path) for item in items for path in (
        item.executable.parent, item.executable.parent.parent))
    if rust_library is not None:
        directories[str(rust_library.resolve(strict=True))] = None
    existing = environment.get(key, "")
    environment[key] = ":".join([*directories, *([existing] if existing else [])])
    return environment


def stop_owned(process):
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=2)
    except subprocess.TimeoutExpired:
        pass
    # The leader exiting on TERM does not mean its group is gone: a listing
    # helper may ignore TERM and retain resources after the leader is reaped.
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait(timeout=2)


def cancel_listings(signum, frame):
    CANCELLED.set()


def list_one(item: Harness, seconds: float, environment: dict[str, str]) -> str:
    process = subprocess.Popen([str(item.executable), "--list"], cwd=item.directory,
                               env=environment, stdout=subprocess.DEVNULL,
                               stderr=subprocess.DEVNULL, start_new_session=True)
    with ACTIVE_LOCK:
        ACTIVE.add(process)
    deadline = time.monotonic() + seconds
    try:
        while True:
            remaining = deadline - time.monotonic()
            if CANCELLED.is_set() or remaining <= 0:
                # Only this newly created listing group; Cargo runs meaningful tests later.
                stop_owned(process)
                return "cancelled" if CANCELLED.is_set() else "timed_out"
            try:
                code = process.wait(timeout=min(remaining, 0.25))
                return "successful" if code == 0 else "failed"
            except subprocess.TimeoutExpired:
                continue
    finally:
        with ACTIVE_LOCK:
            ACTIVE.discard(process)


def prewarm(items: list[Harness], jobs: int, timeout: float, budget: float,
            environment: dict[str, str]) -> dict:
    start = time.monotonic()
    deadline = start + budget

    def run(item):
        if CANCELLED.is_set():
            return "cancelled"
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return "budget_exhausted"
        try:
            return list_one(item, min(timeout, remaining), environment)
        except OSError:
            return "failed"

    counts = dict.fromkeys(("successful", "failed", "timed_out", "budget_exhausted", "cancelled"), 0)
    with ThreadPoolExecutor(max_workers=jobs) as pool:
        for result in pool.map(run, items):
            counts[result] += 1
    return {"executables": len(items), "jobs": jobs, **counts,
            "wall_seconds": round(time.monotonic() - start, 3), "meaningful_tests_run": False}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifacts", type=Path, required=True)
    parser.add_argument("--source", type=Path, default=Path.cwd())
    parser.add_argument("--target", type=Path, required=True)
    parser.add_argument("--rust-library", type=Path)
    parser.add_argument("--jobs", type=int, default=4)
    parser.add_argument("--timeout", type=float, default=90)
    parser.add_argument("--budget", type=float, default=1200)
    args = parser.parse_args()
    if not 1 <= args.jobs <= 4 or not 0 < args.timeout <= 90 or not 0 < args.budget <= 1200:
        parser.error("prewarm limits exceed the bounded pool or time budgets")
    try:
        items = harnesses(args.artifacts, args.source, args.target)
        if not items:
            raise ValueError("no standard test harness artifacts observed")
        handlers = {sig: signal.signal(sig, cancel_listings) for sig in (signal.SIGINT, signal.SIGTERM)}
        try:
            print(json.dumps(prewarm(items, args.jobs, args.timeout, args.budget,
                                     listing_environment(items, args.rust_library))))
        finally:
            for sig, handler in handlers.items():
                signal.signal(sig, handler)
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"test entry prewarm unavailable: {type(error).__name__}", file=sys.stderr)
        return 1
    # This is an optimization, never a test verdict. Cargo still executes every test.
    return 143 if CANCELLED.is_set() else 0


if __name__ == "__main__":
    raise SystemExit(main())
