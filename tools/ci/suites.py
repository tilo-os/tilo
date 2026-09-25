#!/usr/bin/env python3
"""Select and run the hermetic test suites that live outside Cargo.

tools/ci/suites.toml declares each suite with the repository paths it reads.
affected_packages.py selects the suites whose inputs a change touches; the
check workflow's "suites" job runs them with `run`. `check` is the repository
policy that keeps every test-shaped file accounted for.
"""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
import fnmatch
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import tomllib

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = "tools/ci/suites.toml"
SECURITY_MANIFEST = "tools/security-boundary-invariants.txt"
# Files whose change can alter which suites run or how, so every suite runs.
CONTROL_INPUTS = frozenset({MANIFEST, "tools/ci/suites.py", "tools/tests/test_suites.py"})
# Always-run gates: a suite referenced here already runs on every applicable change.
ALWAYS_RUN_SOURCES = (
    "tools/policy/run.sh",
    ".forgejo/workflows/check.yml",
    ".forgejo/workflows/lint.yml",
)
# Test-shaped files outside these directories must be classified. Python tests
# in tools/tests and .codex/hooks are discovered by tools/policy/run.sh.
DISCOVERED_PYTHON_DIRS = ("tools/tests", ".codex/hooks")
TEST_NAME_PATTERNS = (
    "test_*.py", "*_test.py", "*_test.sh", "test-*.py", "test-*.sh", "*-test.sh",
    "*_invariants.sh",
)
SUITE_TIMEOUT_SECONDS = 600


@dataclass(frozen=True)
class Group:
    name: str
    inputs: tuple[str, ...]
    suites: tuple[str, ...]


@dataclass(frozen=True)
class Manifest:
    groups: tuple[Group, ...]
    covered: dict[str, str]
    unrequired: dict[str, str]

    def suites(self) -> list[str]:
        return sorted(suite for group in self.groups for suite in group.suites)


def _strings(value: object, where: str) -> tuple[str, ...]:
    if not isinstance(value, list) or not value or not all(isinstance(item, str) and item for item in value):
        raise ValueError(f"{where} must be a non-empty list of strings")
    return tuple(value)


def _path(value: object, where: str) -> str:
    if (not isinstance(value, str) or not value or value.startswith("/") or "\\" in value
            or ".." in value.split("/") or re.search(r"\s", value)):
        raise ValueError(f"{where} is not a repository-relative path: {value!r}")
    return value


def parse(text: str) -> Manifest:
    """Parse the manifest strictly: unknown keys and duplicate suites are errors."""
    data = tomllib.loads(text)
    unknown = set(data) - {"group", "covered", "unrequired"}
    if unknown:
        raise ValueError(f"unknown manifest tables: {', '.join(sorted(unknown))}")
    groups: list[Group] = []
    seen: dict[str, str] = {}

    def claim(suite: str, owner: str) -> None:
        if suite in seen:
            raise ValueError(f"{suite} is listed twice ({seen[suite]} and {owner})")
        seen[suite] = owner

    names: set[str] = set()
    for index, entry in enumerate(data.get("group", [])):
        where = f"group {index + 1}"
        if not isinstance(entry, dict) or set(entry) != {"name", "inputs", "suites"}:
            raise ValueError(f"{where} must have exactly name, inputs and suites")
        name = entry["name"]
        if not isinstance(name, str) or not re.fullmatch(r"[a-z0-9][a-z0-9-]*", name) or name in names:
            raise ValueError(f"{where} has an invalid or duplicate name: {name!r}")
        names.add(name)
        inputs = tuple(_path(item, f"{where} input") for item in _strings(entry["inputs"], f"{where} inputs"))
        suites = tuple(_path(item, f"{where} suite") for item in _strings(entry["suites"], f"{where} suites"))
        for suite in suites:
            if not suite.endswith((".py", ".sh")):
                raise ValueError(f"{suite}: suites must be .py or .sh files")
            claim(suite, f"group {name}")
        groups.append(Group(name, inputs, suites))
    covered: dict[str, str] = {}
    for entry in data.get("covered", []):
        if not isinstance(entry, dict) or set(entry) != {"suite", "by"}:
            raise ValueError("covered entries must have exactly suite and by")
        suite = _path(entry["suite"], "covered suite")
        claim(suite, "covered")
        covered[suite] = _path(entry["by"], f"{suite} covering suite")
    unrequired: dict[str, str] = {}
    for entry in data.get("unrequired", []):
        if not isinstance(entry, dict) or not {"suite", "reason"} <= set(entry) <= {"suite", "reason", "workflow"}:
            raise ValueError("unrequired entries must have suite and reason, and optionally workflow")
        suite = _path(entry["suite"], "unrequired suite")
        reason = entry["reason"]
        if not isinstance(reason, str) or not reason.strip():
            raise ValueError(f"{suite}: unrequired entries need a reason")
        claim(suite, "unrequired")
        unrequired[suite] = reason
    return Manifest(tuple(groups), covered, unrequired)


def load(root: Path = ROOT) -> Manifest:
    return parse((root / MANIFEST).read_text(encoding="utf-8"))


def matches(path: str, pattern: str) -> bool:
    if pattern.endswith("/"):
        return path.startswith(pattern)
    if any(char in pattern for char in "*?["):
        return fnmatch.fnmatchcase(path, pattern)
    return path == pattern


def select(manifest: Manifest, changes: list[str], *, run_all: bool = False) -> list[str]:
    """Return the suites a change must run, in stable order."""
    if run_all or any(path in CONTROL_INPUTS for path in changes):
        return manifest.suites()
    selected = {
        suite
        for group in manifest.groups
        if any(matches(path, pattern) for path in changes for pattern in group.inputs)
        for suite in group.suites
    }
    selected.update(suite for suite in manifest.suites() if suite in changes)
    return sorted(selected)


def command(suite: str) -> list[str]:
    return ["python3", suite] if suite.endswith(".py") else ["bash", suite]


def run(root: Path, suites: list[str], jobs: int) -> int:
    known = set(load(root).suites())
    unknown = [suite for suite in suites if suite not in known]
    if unknown:
        print(f"not declared in {MANIFEST}: {', '.join(unknown)}", file=sys.stderr)
        return 2
    if not suites:
        print("no suites selected", file=sys.stderr)
        return 2

    def one(suite: str) -> tuple[str, int, float, str]:
        started = time.monotonic()
        try:
            result = subprocess.run(command(suite), cwd=root, stdin=subprocess.DEVNULL,
                                    stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                    text=True, errors="replace", timeout=SUITE_TIMEOUT_SECONDS)
            code, output = result.returncode, result.stdout
        except subprocess.TimeoutExpired as error:
            code = 124
            output = (error.stdout or "") if isinstance(error.stdout, str) else ""
            output += f"\ntimed out after {SUITE_TIMEOUT_SECONDS}s\n"
        return suite, code, time.monotonic() - started, output

    failed: list[str] = []
    started = time.monotonic()
    with ThreadPoolExecutor(max_workers=max(1, jobs)) as pool:
        for suite, code, elapsed, output in pool.map(one, suites):
            status = "ok" if code == 0 else f"FAILED (exit {code})"
            print(f"::group::{suite}: {status} in {elapsed:.1f}s", flush=True)
            print(output.rstrip(), flush=True)
            print("::endgroup::", flush=True)
            if code:
                failed.append(suite)
    print(f"{len(suites) - len(failed)}/{len(suites)} suites passed in {time.monotonic() - started:.1f}s")
    for suite in failed:
        print(f"::error::{suite} failed")
    return 1 if failed else 0


def in_scope(path: str) -> bool:
    """Whether a tracked file looks like a test that must be classified."""
    if path.startswith("vendor/"):
        return False
    directory, _, name = path.rpartition("/")
    if directory == "os/tests" and name.endswith((".py", ".sh")):
        return True
    if directory == "tools/tests" and name.endswith(".sh"):
        return True
    if directory in DISCOVERED_PYTHON_DIRS and name.endswith(".py"):
        return False
    return any(fnmatch.fnmatchcase(name, pattern) for pattern in TEST_NAME_PATTERNS)


def always_run(root: Path) -> set[str]:
    """Suites that already run on every applicable change."""
    gated = {
        line.strip() for line in (root / SECURITY_MANIFEST).read_text(encoding="utf-8").splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    }
    for source in ALWAYS_RUN_SOURCES:
        gated.update(re.findall(r"[A-Za-z0-9_./-]+\.(?:py|sh)\b", (root / source).read_text(encoding="utf-8")))
    return gated


def audit(root: Path, tracked: list[str], manifest: Manifest | None = None) -> list[str]:
    """Return policy violations for the manifest against the tracked tree."""
    manifest = manifest or load(root)
    problems: list[str] = []
    tracked_set = set(tracked)
    gated = always_run(root) | {
        path for path in tracked
        if path.rpartition("/")[0] in DISCOVERED_PYTHON_DIRS
        and fnmatch.fnmatchcase(path.rpartition("/")[2], "test_*.py")
    }
    required = set(manifest.suites())
    for suite in sorted(required | set(manifest.covered) | set(manifest.unrequired)):
        if suite not in tracked_set:
            problems.append(f"{suite}: listed in {MANIFEST} but not a tracked file")
        elif suite in gated:
            problems.append(f"{suite}: already runs on every change; remove it from {MANIFEST}")
    for suite in sorted(required & tracked_set):
        if suite.endswith(".py"):
            text = (root / suite).read_text(encoding="utf-8")
            if re.search(r"\bTestCase\b", text) and "unittest.main(" not in text:
                problems.append(f"{suite}: defines unittest cases but never calls unittest.main()")
    for suite, by in sorted(manifest.covered.items()):
        if by not in gated and by not in required:
            problems.append(f"{suite}: covering suite {by} is not itself gated")
        elif (root / by).is_file() and not re.search(
                rf"\b{re.escape(Path(suite).stem)}\b", (root / by).read_text(encoding="utf-8")):
            problems.append(f"{suite}: {by} does not reference it")
    classified = gated | required | set(manifest.covered) | set(manifest.unrequired)
    for path in sorted(tracked):
        if in_scope(path) and path not in classified:
            problems.append(
                f"{path}: test-shaped file is neither gated nor listed in {MANIFEST} "
                "(add it to a group, or to [[covered]]/[[unrequired]] with a reason)")
    return problems


def tracked_files(root: Path) -> list[str]:
    output = subprocess.run(["git", "ls-files", "-z"], cwd=root, check=True,
                            stdout=subprocess.PIPE).stdout.decode("utf-8")
    return [path for path in output.split("\0") if path]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    run_parser = commands.add_parser("run", help="run the named suites")
    run_parser.add_argument("suites", nargs="*")
    run_parser.add_argument("--all", action="store_true", help="run every declared suite")
    run_parser.add_argument("--jobs", type=int, default=os.cpu_count() or 1)
    commands.add_parser("list", help="print every declared suite")
    commands.add_parser("check", help="verify every test-shaped file is accounted for")
    args = parser.parse_args(argv)
    if args.command == "list":
        print("\n".join(load(ROOT).suites()))
        return 0
    if args.command == "check":
        problems = audit(ROOT, tracked_files(ROOT))
        for problem in problems:
            print(problem, file=sys.stderr)
        if not problems:
            manifest = load(ROOT)
            print(f"suite manifest OK: {len(manifest.suites())} path-selected, "
                  f"{len(manifest.covered)} covered, {len(manifest.unrequired)} unrequired")
        return 1 if problems else 0
    if args.all and args.suites:
        parser.error("run takes either suite paths or --all")
    # An empty selection is an error, never "everything": the workflow passes
    # the selector's list, and an empty list there means selection broke.
    return run(ROOT, load(ROOT).suites() if args.all else args.suites, args.jobs)


if __name__ == "__main__":
    sys.exit(main())
