#!/usr/bin/env python3
"""Run the unittest files of some directories side by side, a process each.

One `python3 -m unittest discover -s tools/tests` runs every test there in one process, one after
another: some 1,500 tests that mostly wait on Git and on the subprocesses they start, which took
the policy suite 8 to 30 minutes at a fraction of a core. Run a file per process, several at
once, and the suite takes about as long as its slowest file.

Each file runs as discover runs it (`-s DIR -p FILE`): DIR is the top of the import path, and the
file is imported as a module of that name, so a test passes or fails here as it does there.
Files share nothing but the machine: a test that needs a scratch directory makes its own.

At the end each file is reported as libtest reports a test: a file that passed as
`test <file> ... ok`, each test that failed in one as `test <file>::<test id> ... FAILED` with the
file's output under it, and a file that failed without naming a test (it crashed, or did not
import) as `test <file> ... FAILED`.

A file whose tests each run for seconds (end-to-end script tests) would hold the suite for as
long as all of them take one after another, so the files SHARDS names run in several processes,
each a share of the file's tests as discover finds them (`--shard`), and those shares start
first; the file is still reported as one. The slowest files are listed at the end of every run:
one that holds the run up belongs in SHARDS.

Usage: run_unit_tests.py [--jobs N] DIR|FILE [DIR|FILE ...]
       run_unit_tests.py --shard I N FILE    (the I-th of N shares of FILE's tests, from 0)
"""
from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, replace
from pathlib import Path
import os
import re
import subprocess
import sys
import time

PATTERN = "test_*.py"
# The tests mostly wait, so more of them run at once than there are cores to give them.
DEFAULT_JOBS = 32
RAN = re.compile(r"^Ran (\d+) tests? in ", re.MULTILINE)
# Files whose tests are run in this many processes, by name: those whose tests each take
# seconds, measured in CI (the run's "Slowest files"). A share should take under ten seconds there.
SHARDS = {
    "test_app_artifact_handoff.py": 12,
    "test_extension_publish.py": 8,
    "test_commit_hooks.py": 6,
    "test_publish_chain.py": 4,
    "test_ci_run.py": 3,
    "test_component_manifest.py": 3,
    "test_unused_dependencies.py": 6,
    "test_changed_rust_comparison.py": 2,
    "test_browser_runtime_release.py": 2,
    "test_stage_sdk_services.py": 2,
    "test_workflow_package_paths.py": 2,
    "test_font_artifacts.py": 3,
    "test_native_envelope.py": 3,
    "test_font_catalog.py": 2,
    "test_chrome_process_cleanup.py": 2,
    "test_copy_language.py": 2,
    "test_browser_cleanup_lifecycle.py": 2,
    "test_thin_app_compiler_cache.py": 2,
}
# unittest's header of a failed test: `FAIL: test_x (module.Class.test_x)`, before Python 3.11
# `FAIL: test_x (module.Class)`.
FAILED = re.compile(r"^(?:FAIL|ERROR|UNEXPECTED SUCCESS): (\S+) \(([^)\s]+)\)", re.MULTILINE)


@dataclass(frozen=True)
class Result:
    path: Path
    ok: bool
    seconds: float
    tests: int
    output: str
    # The processor time the file's processes and those they waited for took: on a loaded node
    # the suite is bound by the sum of these, not by its slowest file.
    cpu: float = 0.0


def files(arguments: list[Path]) -> list[Path]:
    """The test files of the directories named, and the files named."""
    found = [path for argument in arguments
             for path in (sorted(argument.glob(PATTERN)) if argument.is_dir() else [argument]) if path.is_file()]
    missing = [str(argument) for argument in arguments if not argument.exists()]
    if missing:
        raise SystemExit(f"run_unit_tests.py: no such file or directory: {', '.join(missing)}")
    return found


def run(path: Path, shard: tuple[int, int] | None = None) -> Result:
    start = time.monotonic()
    command = ([sys.executable, str(Path(__file__).resolve()), "--shard", str(shard[0]), str(shard[1]), str(path)]
               if shard else [sys.executable, "-m", "unittest", "discover", "-s", str(path.parent), "-p", path.name])
    with subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                          text=True, errors="replace") as process:
        output = process.stdout.read()
        _, status, usage = os.wait4(process.pid, 0)
        process.returncode = os.waitstatus_to_exitcode(status)
    tests = sum(int(n) for n in RAN.findall(output))
    return Result(path, process.returncode == 0, time.monotonic() - start, tests, output,
                  usage.ru_utime + usage.ru_stime)


def run_shard(index: int, count: int, path: Path) -> int:
    """Run the `index`-th of `count` shares of the tests discover finds in `path`, as
    `python3 -m unittest discover -s DIR -p FILE` would run them, with its output."""
    import unittest

    def flat(suite):
        for test in suite:
            yield from flat(test) if isinstance(test, unittest.TestSuite) else (test,)

    # As `python3 -m unittest` has it: the working directory first, not this script's (discover
    # adds the test directory itself).
    sys.path[0] = ""
    found = list(flat(unittest.defaultTestLoader.discover(str(path.parent), pattern=path.name)))
    if not found:
        print(f"run_unit_tests.py: no tests in {path}", file=sys.stderr)
        return 5
    share = unittest.TestSuite(found[index::count])
    result = unittest.TextTestRunner(stream=sys.stderr, verbosity=1).run(share)
    return 0 if result.wasSuccessful() else 1


def joined(results: list[Result]) -> Result:
    """A file's shards as one result: the report names a file's test once."""
    first = results[0]
    return replace(first, ok=all(r.ok for r in results), seconds=max(r.seconds for r in results),
                   tests=sum(r.tests for r in results), output="\n".join(r.output for r in results),
                   cpu=sum(r.cpu for r in results))


def failed_tests(output: str) -> dict[str, str]:
    """The tests a file's unittest output says failed, by id in order, each with its report: its
    header and traceback, without unittest's rules of dashes."""
    found: dict[str, str] = {}
    matches = list(FAILED.finditer(output))
    for index, match in enumerate(matches):
        name, where = match[1], match[2]
        test = where if where.endswith("." + name) else f"{where}.{name}"
        end = matches[index + 1].start() if index + 1 < len(matches) else len(output)
        block = [line for line in output[match.start():end].splitlines()
                 if line.strip("-=") or not line.strip()]
        found.setdefault(test, "\n".join(block).strip())
    return found


def report(results: list[Result]) -> str:
    """Each file and failed test in libtest's form (see the module's docstring)."""
    lines, sections = [""], []
    for result in sorted(results, key=lambda r: str(r.path)):
        if result.ok:
            lines.append(f"test {result.path} ... ok")
            continue
        tests = failed_tests(result.output)
        named = {f"{result.path}::{test}": text for test, text in tests.items()} or {
            str(result.path): "\n".join(line for line in result.output.splitlines() if line.strip("-=") or not line.strip())}
        for name, text in named.items():
            lines.append(f"test {name} ... FAILED")
            sections += [f"---- {name} stdout ----", text.rstrip(), ""]
    passed = sum(1 for r in results if r.ok)
    failed = len(lines) - 1 - passed
    return "\n".join([*lines, "", "failures:", "", *sections,
                      f"test result: {'FAILED' if failed else 'ok'}. {passed} passed; {failed} failed", ""])


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    if argv[:1] == ["--shard"]:
        if len(argv) != 4:
            parser.error("--shard I N FILE")
        return run_shard(int(argv[1]), int(argv[2]), Path(argv[3]))
    parser.add_argument("--jobs", type=int, default=DEFAULT_JOBS)
    parser.add_argument("directories", nargs="+", type=Path)
    args = parser.parse_args(argv)
    if args.jobs < 1:
        parser.error("--jobs must be at least 1")
    paths = files(args.directories)
    if not paths:
        print(f"run_unit_tests.py: no {PATTERN} in {', '.join(map(str, args.directories))}", file=sys.stderr)
        return 1

    # The sharded files' shares first, the slowest ahead, then the rest: the run ends about when
    # its slowest share does.
    pieces = sorted(((path, (index, SHARDS[path.name])) for path in paths if path.name in SHARDS
                     for index in range(SHARDS[path.name])), key=lambda piece: -piece[1][1])
    pieces += [(path, None) for path in paths if path.name not in SHARDS]
    start = time.monotonic()
    shares: dict[Path, list[Result]] = {}
    with ThreadPoolExecutor(max_workers=args.jobs) as pool:
        for future in as_completed([pool.submit(run, *piece) for piece in pieces]):
            result = future.result()
            shares.setdefault(result.path, []).append(result)
            if not result.ok:
                print(f"\n---- {result.path} failed after {result.seconds:.1f}s:\n{result.output.rstrip()}\n", flush=True)
    results = [joined(shares[path]) for path in paths]
    for result in results:
        if result.ok and result.tests == 0 and result.path.name in SHARDS:
            # Every share of a sharded file ran none: discover would have found nothing either.
            results[results.index(result)] = replace(result, ok=False, output=result.output + "\nno tests ran\n")

    failed = sorted(str(r.path) for r in results if not r.ok)
    print(report(results), flush=True)
    print("\nSlowest files:")
    for result in sorted(results, key=lambda r: r.seconds, reverse=True)[:10]:
        print(f"  {result.seconds:7.1f}s  {result.path}")
    print(f"\nMost processor time ({sum(r.cpu for r in results):.0f}s in all):")
    for result in sorted(results, key=lambda r: r.cpu, reverse=True)[:15]:
        print(f"  {result.cpu:7.1f}s  {result.path}")
    print(f"\nRan {sum(r.tests for r in results)} tests in {len(results)} files in "
          f"{time.monotonic() - start:.1f}s, {args.jobs} at a time")
    if failed:
        print(f"{len(failed)} failed: {' '.join(failed)}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
