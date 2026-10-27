#!/usr/bin/env python3
"""Per-package test times from libtest's JSON events, and the regression warning.

t's test wrapper (tools/ci/t-nextest.sh) runs the tests with libtest's own JSON
events on. Those events name each test's package and how long it took, so the
wrapper passes them through `record`, which forwards every line to the job's log
unchanged and records, per package, how many tests ran, how long they took and
the slowest of them. Several jobs writing one file are summed: a package one job
tested is not tested by another.

  test_timings.py record [--json FILE]           # stdin -> stdout, records to FILE

  test_timings.py summarise [--json FILE] [--baseline FILE] [--slowest N]
                            [--tolerance FRACTION] [--third-party] [--warn-only]

`summarise` prints the table (the job log in CI and on t), warns about every
package at least two seconds and `--tolerance` (20% by default) slower than the
baseline, and adds what the run recorded to `--json`. A run that ended without
finishing its tests (nextest cancelled it, or hit its hang bound) is not
compared and is not taken as a baseline.

  test_timings.py write-baseline --json FILE --baseline FILE [--force]
  test_timings.py compare --json FILE --baseline FILE [--tolerance FRACTION]
                          [--third-party]

`write-baseline` starts the baseline from a finished run and leaves an existing
one alone: it is committed (`docs/measurements/test-timings.json`) and only moved
deliberately, by `--force`. `compare` does what `summarise --baseline` does, on
its own, for a job that only compares.

A package is named as libtest's events name it, in the test's name before its
first `::`. The time is the run's own measurement of the test, which starts at
the test function and leaves out starting its binary; a job's wall time is t's
to report, and the build is neither.

`--third-party` keeps the tests of crates.io packages beside the workspace's own
instead of dropping them: an external job tests a dependency, which is not
Tilo's test, and the baseline holds only the workspace's.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys
from typing import Any, Iterable, Iterator, TextIO

# Below this, a difference is the reporting's own noise rather than a change.
FLOOR_SECONDS = 2.0
SLOWEST = 5
# One package is one run: this marker separates them from a job's records.
FINISHED = "__finished__"


def events(lines: Iterable[str]) -> Iterator[tuple[str, dict[str, Any] | None]]:
    """Each line with the libtest event it carries, if any.

    libtest writes one JSON object a line. A build tool may prefix a line's
    beginning with its own (cargo's compiler messages arrive that way); the
    event itself always runs to the line's end, and a suite's own `ok` and a
    test that did not start carry no time to record.
    """
    for line in lines:
        if "{" not in line:
            yield line, None
            continue
        start = line.rfind("{") if line.rstrip().endswith("}") else line.find("{")
        try:
            event = json.loads(line[start:])
        except json.JSONDecodeError:
            yield line, None
            continue
        if not isinstance(event, dict):
            yield line, None
            continue
        yield line, event


def record(record: dict[str, dict[str, Any]], event: dict[str, Any]) -> None:
    """One event's test added to the package's record."""
    name = event.get("name")
    if event.get("type") not in ("test", "bench") or event.get("event") != "ok" or not name:
        return
    duration = event.get("exec_time")
    if not isinstance(duration, (int, float)):
        return
    package, _, test = name.partition("::")
    entry = record.setdefault(
        package, {"tests": 0, "wall_seconds": 0.0, "test_seconds": 0.0, "slowest": []})
    entry["tests"] += 1
    entry["test_seconds"] += float(duration)
    entry["wall_seconds"] += float(duration)
    entry["slowest"].append({"test": test or name, "seconds": float(duration)})


def load(path: Path) -> dict[str, dict[str, Any]]:
    document = json.loads(path.read_text(encoding="utf-8"))
    return {**document["packages"], FINISHED: document.get("finished", True)}


def save(path: Path, records: dict[str, dict[str, Any]]) -> None:
    packages = {package: record for package, record in sorted(records.items()) if package != FINISHED}
    document = {"finished": bool(records.get(FINISHED, True)), "packages": packages}
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def merge(into: dict[str, dict[str, Any]], extra: dict[str, dict[str, Any]],
          finished: bool) -> dict[str, dict[str, Any]]:
    """`extra`'s records added to `into`'s; one package is one run, across jobs."""
    merged = {package: record for package, record in into.items() if package != FINISHED}
    for package, entry in extra.items():
        current = merged.setdefault(
            package, {"tests": 0, "wall_seconds": 0.0, "test_seconds": 0.0, "slowest": []})
        current["tests"] += entry["tests"]
        current["wall_seconds"] += entry["wall_seconds"]
        current["test_seconds"] += entry["test_seconds"]
        current["slowest"] += entry["slowest"]
    merged[FINISHED] = bool(into.get(FINISHED, True)) and finished
    return merged


def is_workspace_package(root: Path, package: str) -> bool:
    """Whether a manifest in the tree names the package: a member or a path dependency."""
    needle = f'name = "{package}"'
    for manifest in root.glob("**/Cargo.toml"):
        if any(part in ("target", "vendor", "node_modules") for part in manifest.parts):
            continue
        try:
            if needle in manifest.read_text(encoding="utf-8"):
                return True
        except OSError:
            continue
    return False


def rows(records: dict[str, dict[str, Any]], root: Path, third_party: bool,
         slowest: int) -> list[dict[str, Any]]:
    kept = []
    for package, entry in records.items():
        if package == FINISHED or (not third_party and not is_workspace_package(root, package)):
            continue
        kept.append({"package": package, "tests": entry["tests"],
                     "wall_seconds": entry["wall_seconds"],
                     "test_seconds": entry["test_seconds"],
                     "slowest": sorted(entry["slowest"], key=lambda item: item["seconds"],
                                       reverse=True)[:slowest]})
    return sorted(kept, key=lambda row: (-row["wall_seconds"], row["package"]))


def regressions(table: list[dict[str, Any]], baseline: dict[str, dict[str, Any]],
                tolerance: float) -> list[dict[str, Any]]:
    found = []
    for row in table:
        before = baseline.get(row["package"])
        if not before or before["wall_seconds"] <= 0:
            continue
        change = row["wall_seconds"] - before["wall_seconds"]
        if change < FLOOR_SECONDS or change < tolerance * before["wall_seconds"]:
            continue
        found.append({**row, "baseline_seconds": before["wall_seconds"],
                      "change_seconds": change,
                      "change_fraction": change / before["wall_seconds"]})
    return found


def format_seconds(value: float) -> str:
    if value < 60:
        return f"{value:.1f}s"
    minutes, rest = divmod(value, 60)
    return f"{int(minutes)}m{rest:04.1f}s"


def print_table(table: list[dict[str, Any]], out: TextIO) -> None:
    if not table:
        print("Test time per package: no tests ran", file=out)
        return
    print(f"{len(table)} packages, {sum(row['tests'] for row in table)} tests, "
          f"{format_seconds(sum(row['wall_seconds'] for row in table))} of test time", file=out)
    print(f"{'package':<36}{'tests':>8}{'time':>10}{'slowest':>10}  slowest test", file=out)
    for row in table:
        slowest = row["slowest"][0] if row["slowest"] else {"test": "", "seconds": 0.0}
        print(f"{row['package']:<36}{row['tests']:>8}{format_seconds(row['wall_seconds']):>10}"
              f"{format_seconds(slowest['seconds']):>10}  {slowest['test']}", file=out)
    for row in table:
        if len(row["slowest"]) > 1:
            print(f"{row['package']} slowest: " + ", ".join(
                f"{entry['test']} {format_seconds(entry['seconds'])}"
                for entry in row["slowest"]), file=out)


def report(found: list[dict[str, Any]], warn_only: bool, out: TextIO) -> int:
    if not found:
        print("No package is slower than its baseline.", file=out)
        return 0
    for row in found:
        print(f"::warning title=Test time regression::{row['package']} took "
              f"{format_seconds(row['wall_seconds'])} against a baseline of "
              f"{format_seconds(row['baseline_seconds'])}: "
              f"{row['change_fraction'] * 100:.0f}% slower", file=out)
    return 0 if warn_only else 1


def main(argv: list[str]) -> int:
    root = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)

    def shared(sub: argparse.ArgumentParser) -> None:
        sub.add_argument("--json", type=Path,
                         help="the file this job records its packages in, across its runs")
        sub.add_argument("--third-party", action="store_true",
                         help="keep the tests of registry packages, which an external job runs")
        sub.add_argument("--slowest", type=int, default=SLOWEST)

    record_command = commands.add_parser(
        "record", help="pass a run's events through, recording each test's package and time")
    record_command.add_argument("--json", type=Path, required=True)

    summarise = commands.add_parser("summarise", help="print the table and warn on a regression")
    summarise.add_argument("--baseline", type=Path)
    summarise.add_argument("--tolerance", type=float, default=0.20)
    summarise.add_argument("--warn-only", action="store_true",
                           help="warn without failing, where the host cannot show a failure")
    shared(summarise)

    write = commands.add_parser("write-baseline", help="start the baseline from a finished run")
    write.add_argument("--baseline", type=Path, required=True)
    write.add_argument("--force", action="store_true", help="replace an existing baseline")
    shared(write)

    compare = commands.add_parser("compare", help="warn on a regression against the baseline")
    compare.add_argument("--baseline", type=Path, required=True)
    compare.add_argument("--tolerance", type=float, default=0.20)
    compare.add_argument("--warn-only", action="store_true")
    shared(compare)

    arguments = parser.parse_args(argv)
    if arguments.command == "record":
        found: dict[str, dict[str, Any]] = {}
        for line, event in events(sys.stdin):
            sys.stdout.write(line if line.endswith("\n") else line + "\n")
            sys.stdout.flush()
            if event is not None:
                record(found, event)
        # The jobs of one run share this file; each adds what it tested.
        before = load(arguments.json) if arguments.json.is_file() else {}
        save(arguments.json, merge(before, found, True))
        return 0
    if arguments.command == "summarise":
        records = load(arguments.json) if arguments.json and arguments.json.is_file() else {}
        table = rows(records, root, arguments.third_party, arguments.slowest)
        print_table(table, sys.stdout)
        if arguments.baseline and arguments.baseline.is_file():
            return report(regressions(table, load(arguments.baseline), arguments.tolerance),
                          arguments.warn_only, sys.stdout)
        return 0
    if not arguments.json:
        parser.error(f"{arguments.command} needs --json")
    if arguments.command == "write-baseline":
        if arguments.baseline.is_file() and not arguments.force:
            print(f"{arguments.baseline} exists: move it deliberately with --force", file=sys.stderr)
            return 0
        records = load(arguments.json)
        if not records[FINISHED]:
            print("Refusing a partial run as the baseline.", file=sys.stderr)
            return 1
        save(arguments.baseline, records)
        print(f"Wrote the baseline to {arguments.baseline}")
        return 0
    records = load(arguments.json)
    if not records[FINISHED]:
        print("The run ended before its tests finished: not comparing it.")
        return 0
    table = rows(records, root, arguments.third_party, arguments.slowest)
    return report(regressions(table, load(arguments.baseline), arguments.tolerance),
                  arguments.warn_only, sys.stdout)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
