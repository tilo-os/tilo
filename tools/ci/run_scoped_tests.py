#!/usr/bin/env python3
"""Run the scoped reader tests an affected-package shard selected.

A package that reads repository files outside its directory is selected when
those files change (tools/ci/suites.toml). When the change touches only inputs
the manifest scopes to a few tests, affected_packages.py emits those tests in
the shard's `scoped_tests` instead of adding the whole package to the
`test_packages` suite. This runs each with the shard's environment and profile,
and folds its JUnit report into the shard's report so the retry summary and the
uploaded artifact cover it.

SCOPED_TESTS is the JSON list from the matrix; GOLDEN_FILTER keeps visual
goldens out, as in the main test step. A filter that selects no test fails.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import xml.etree.ElementTree as ET

JUNIT = Path("target/nextest/ci/junit.xml")
NAME = re.compile(r"[A-Za-z0-9_-]+")


def entries(text: str) -> list[dict[str, str]]:
    data = json.loads(text)
    if not isinstance(data, list) or not data:
        raise ValueError("SCOPED_TESTS must be a non-empty JSON list")
    for entry in data:
        if (not isinstance(entry, dict) or set(entry) != {"package", "test", "filter"}
                or not all(isinstance(entry[key], str) for key in entry)
                or not NAME.fullmatch(entry["package"]) or not NAME.fullmatch(entry["test"])
                or not entry["filter"].strip() or "\n" in entry["filter"]):
            raise ValueError(f"invalid scoped test entry: {entry!r}")
    return data


def command(entry: dict[str, str], golden: str) -> list[str]:
    expression = f"({entry['filter']})"
    if golden.strip():
        expression += f" and not ({golden.strip()})"
    return ["bash", "tools/test-env.sh", "cargo", "nextest", "run", "--locked", "--profile", "ci",
            "--no-tests=fail", "-p", entry["package"], "--test", entry["test"], "-E", expression]


def merge(reports: list[Path], output: Path) -> None:
    """Fold every report's test suites into one nextest-shaped JUnit document."""
    root = None
    for report in reports:
        tree = ET.parse(report).getroot()
        if root is None:
            root = tree
            continue
        for suite in tree.findall("testsuite"):
            root.append(suite)
        for key in ("tests", "failures", "errors"):
            if key in root.attrib or key in tree.attrib:
                root.set(key, str(int(root.get(key, "0")) + int(tree.get(key, "0"))))
    if root is not None:
        output.parent.mkdir(parents=True, exist_ok=True)
        ET.ElementTree(root).write(output, encoding="utf-8", xml_declaration=True)


def main() -> int:
    scoped = entries(os.environ.get("SCOPED_TESTS", ""))
    golden = os.environ.get("GOLDEN_FILTER", "")
    reports: list[Path] = []
    if JUNIT.exists():
        reports.append(JUNIT.with_name("junit-affected.xml"))
        shutil.move(JUNIT, reports[-1])
    status = 0
    for index, entry in enumerate(scoped):
        argv = command(entry, golden)
        print("==> " + " ".join(argv), flush=True)
        result = subprocess.run(argv, check=False)
        if JUNIT.exists():
            reports.append(JUNIT.with_name(f"junit-scoped-{index}.xml"))
            shutil.move(JUNIT, reports[-1])
        elif result.returncode == 0:
            print(f"::error::Scoped tests for {entry['package']}::{entry['test']} "
                  f"succeeded without the required JUnit report at {JUNIT}", file=sys.stderr)
            status = status or 1
        status = status or result.returncode
    merge(reports, JUNIT)
    return status


if __name__ == "__main__":
    sys.exit(main())
