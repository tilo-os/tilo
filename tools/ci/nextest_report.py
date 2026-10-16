#!/usr/bin/env python3
"""Summarise a cargo-nextest JUnit report for a CI job summary.

The CI profile does not retry: a first-run failure fails the job. This still
lists any retried case (a profile that turns retries back on marks a pass
after a failure as FLAKY, which would otherwise hide in the log) and the 20
slowest tests, the ones that decide a shard's wall time and are worth
speeding up or starting first (.config/nextest.toml priorities), and emits one
warning annotation per flaky test.

Usage: nextest_report.py --label NAME [--summary PATH] junit.xml

Markdown is appended to --summary (default: $GITHUB_STEP_SUMMARY, else
stdout). Reporting never changes the job result: a missing report, e.g.
because compilation failed before any test ran, is noted and exits 0.
"""
from __future__ import annotations

import argparse
from dataclasses import dataclass
import os
from pathlib import Path
import sys
import xml.etree.ElementTree as ET

FAILURE_TAGS = ("failure", "error")
RERUN_TAGS = ("rerunFailure", "rerunError")
FLAKY_TAGS = ("flakyFailure", "flakyError")
SLOWEST = 20


@dataclass(frozen=True)
class Case:
    binary: str
    name: str
    seconds: float
    failed: bool
    failed_attempts: int

    @property
    def flaky(self) -> bool:
        return not self.failed and self.failed_attempts > 0

    @property
    def retried(self) -> bool:
        return self.failed_attempts > (1 if self.failed else 0)


def parse(path: Path) -> list[Case]:
    cases = []
    for element in ET.parse(path).getroot().iter("testcase"):
        tags = [child.tag for child in element]
        failed = any(tag in FAILURE_TAGS for tag in tags)
        attempts = sum(tag in FAILURE_TAGS + RERUN_TAGS + FLAKY_TAGS for tag in tags)
        try:
            seconds = float(element.get("time", "0"))
        except ValueError:
            seconds = 0.0
        cases.append(Case(
            binary=element.get("classname", ""),
            name=element.get("name", ""),
            seconds=seconds,
            failed=failed,
            failed_attempts=attempts,
        ))
    return cases


def cell(text: str) -> str:
    return "`" + text.replace("`", "'").replace("|", "\\|") + "`"


def annotation(text: str) -> str:
    # Workflow commands end at a newline and treat % specially.
    return text.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


def render(label: str, cases: list[Case]) -> str:
    flaky = [case for case in cases if case.flaky]
    failed = [case for case in cases if case.failed]
    lines = [
        f"### nextest: {label}",
        "",
        f"{len(cases)} tests, {len(failed)} failed, {len(flaky)} flaky "
        "(failed, then passed on retry).",
        "",
    ]
    if flaky or any(case.retried for case in failed):
        lines += ["| Result | Binary | Test | Failed attempts |", "|---|---|---|---|"]
        for case in sorted(flaky + failed, key=lambda c: (not c.failed, c.binary, c.name)):
            result = "FAIL" if case.failed else "FLAKY"
            lines.append(
                f"| {result} | {cell(case.binary)} | {cell(case.name)} | {case.failed_attempts} |")
        lines.append("")
    slowest = sorted(cases, key=lambda c: c.seconds, reverse=True)[:SLOWEST]
    if slowest:
        lines += [f"Slowest tests ({len(slowest)}):", "",
                  "| Seconds | Binary | Test |", "|---|---|---|"]
        lines += [f"| {case.seconds:.1f} | {cell(case.binary)} | {cell(case.name)} |"
                  for case in slowest]
        lines.append("")
    return "\n".join(lines) + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--label", required=True)
    parser.add_argument("--summary", type=Path,
                        default=Path(os.environ["GITHUB_STEP_SUMMARY"])
                        if os.environ.get("GITHUB_STEP_SUMMARY") else None)
    parser.add_argument("junit", type=Path)
    args = parser.parse_args(argv)

    if not args.junit.is_file():
        text = f"### nextest: {args.label}\n\nNo JUnit report at `{args.junit}`; no test ran.\n\n"
        cases = []
    else:
        try:
            cases = parse(args.junit)
        except (ET.ParseError, OSError, LookupError) as error:
            text = f"### nextest: {args.label}\n\nUnreadable JUnit report: {error}\n\n"
            cases = []
        else:
            text = render(args.label, cases)
    for case in cases:
        if case.flaky:
            print(f"::warning title=Flaky test::{annotation(case.binary)} "
                  f"{annotation(case.name)} failed {case.failed_attempts} time(s) "
                  "before passing on retry")
    if args.summary is None:
        sys.stdout.write(text)
    else:
        try:
            with args.summary.open("a", encoding="utf-8") as handle:
                handle.write(text)
        except OSError as error:
            # Reporting is diagnostic: never replace the test phase's verdict
            # with a failure to append its optional summary.
            print(f"::warning::Cannot write nextest summary: {annotation(str(error))}", file=sys.stderr)
            sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
