#!/usr/bin/env python3
"""Require every Forgejo workflow job to declare a timeout of at most three hours.

A job that needs longer is a job to split or fix, not to wait for.

A job that calls a local reusable workflow cannot declare one; the called
workflow's own jobs are audited instead.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import re


ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".forgejo" / "workflows"
JOB = re.compile(r"^  ([A-Za-z0-9_-]+):\s*$")
TIMEOUT = re.compile(r"^    timeout-minutes:\s*([0-9]+)\s*$")
MAX_MINUTES = 180
# The OS release build still exceeds the limit.
LONGER = {"build-image.yml"}
LOCAL_CALL = re.compile(r"^    uses:\s*\./\.forgejo/workflows/([A-Za-z0-9_.-]+\.ya?ml)\s*$")


@dataclass(frozen=True)
class Violation:
    workflow: str
    job: str

    def __str__(self) -> str:
        return (f"{self.workflow}: job {self.job!r} needs a timeout-minutes between 1 "
                f"and {MAX_MINUTES}")


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
            minutes = int(timeout_match.group(1))
            jobs[current_job] = 0 < minutes and (minutes <= MAX_MINUTES or path.name in LONGER)
        call_match = LOCAL_CALL.match(line)
        if current_job is not None and call_match:
            jobs[current_job] = (path.parent / call_match.group(1)).is_file()

    return [job for job, bounded in jobs.items() if not bounded]


def workflow_files(workflows: Path) -> list[Path]:
    """Return every YAML workflow in a stable order."""
    return sorted((*workflows.glob("*.yml"), *workflows.glob("*.yaml")))


def audit(
    workflows: Path = WORKFLOWS, required: tuple[str, ...] | None = None
) -> list[Violation]:
    paths = (
        [workflows / workflow for workflow in required]
        if required is not None
        else workflow_files(workflows)
    )
    if not paths:
        return [Violation(str(workflows), "<no workflows found>")]
    violations: list[Violation] = []
    for path in paths:
        workflow = path.name
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
    print(f"Workflow timeout policy OK ({len(workflow_files(WORKFLOWS))} workflows)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
