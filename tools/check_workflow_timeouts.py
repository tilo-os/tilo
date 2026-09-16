#!/usr/bin/env python3
"""Require bounded jobs in the repository's fast feedback workflows."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import re


ROOT = Path(__file__).resolve().parents[1]
WORKFLOWS = ROOT / ".forgejo" / "workflows"
REQUIRED = ("lint.yml", "check.yml", "legacy-app-e2e.yml")
JOB = re.compile(r"^  ([A-Za-z0-9_-]+):\s*$")
TIMEOUT = re.compile(r"^    timeout-minutes:\s*([0-9]+)\s*$")


@dataclass(frozen=True)
class Violation:
    workflow: str
    job: str

    def __str__(self) -> str:
        return f"{self.workflow}: job {self.job!r} has no positive timeout-minutes"


def jobs_without_timeouts(path: Path) -> list[str]:
    """Return job names without a positive job-level timeout."""
    jobs: dict[str, bool] = {}
    current_job: str | None = None
    in_jobs = False

    for line in path.read_text(encoding="utf-8").splitlines():
        if line == "jobs:":
            in_jobs = True
            continue
        if not in_jobs:
            continue
        if line and not line.startswith(" "):
            break

        job_match = JOB.match(line)
        if job_match:
            current_job = job_match.group(1)
            jobs[current_job] = False
            continue

        timeout_match = TIMEOUT.match(line)
        if current_job is not None and timeout_match:
            jobs[current_job] = int(timeout_match.group(1)) > 0

    return [job for job, bounded in jobs.items() if not bounded]


def audit(
    workflows: Path = WORKFLOWS, required: tuple[str, ...] = REQUIRED
) -> list[Violation]:
    violations: list[Violation] = []
    for workflow in required:
        path = workflows / workflow
        if not path.is_file():
            violations.append(Violation(workflow, "<missing workflow>"))
            continue
        violations.extend(
            Violation(workflow, job) for job in jobs_without_timeouts(path)
        )
    return violations


def main() -> int:
    violations = audit()
    if violations:
        for violation in violations:
            print(violation)
        return 1
    print("Fast workflow timeout policy OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
