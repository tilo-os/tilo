#!/usr/bin/env python3
"""Read server-owned run/job records; never infer CI success from commit statuses.

Experimental execution-evidence check only. Workflow identity is a path, not a
trusted definition: callers must separately establish policy/code provenance,
runner isolation, review and freshness before authorizing any merge.
"""
import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import re
import subprocess


def api(host, repo, suffix):
    result = subprocess.run(
        ["fgj", "--hostname", host, "api", f"repos/{repo}{suffix}"],
        capture_output=True, text=True, check=True,
    )
    return json.loads(result.stdout)


def execution_errors(run, jobs, repo, head, workflow, required_jobs):
    errors = []
    repository_id = run.get("repository", {}).get("id")
    if type(repository_id) is not int or repository_id <= 0:
        errors.append("missing valid repository ID")
    if type(run.get("id")) is not int or run["id"] <= 0:
        errors.append("missing valid run ID")
    if run.get("repository", {}).get("full_name") != repo:
        errors.append("run repository mismatch")
    if run.get("commit_sha") != head:
        errors.append("run commit mismatch")
    if run.get("workflow_id") != workflow:
        errors.append("run workflow mismatch")
    if run.get("status") != "success":
        errors.append(f"run is {run.get('status')!r}, not success")
    if not isinstance(jobs, list) or not jobs:
        return errors + ["missing job records"]
    names = [job.get("name") for job in jobs]
    if len(names) != len(set(names)) or set(names) != set(required_jobs):
        errors.append("job set differs from the explicit expected set")
    ids = [job.get("id") for job in jobs]
    if any(type(value) is not int or value <= 0 for value in ids) or len(ids) != len(set(ids)):
        errors.append("missing or duplicate job IDs")
    for job in jobs:
        if job.get("run_id") != run.get("id") or job.get("repo_id") != run.get("repository", {}).get("id"):
            errors.append(f"job {job.get('id')} belongs to another run/repository")
        if job.get("status") != "success":
            errors.append(f"job {job.get('name')!r} is {job.get('status')!r}, not success")
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--hostname", default="code.podesta.ai")
    parser.add_argument("--repo", required=True)
    parser.add_argument("--run", required=True, type=int)
    parser.add_argument("--head", required=True)
    parser.add_argument("--workflow", required=True)
    parser.add_argument("--job", required=True, action="append")
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", args.repo):
        parser.error("invalid repository")
    if not re.fullmatch(r"[0-9a-f]{40}", args.head) or args.run <= 0:
        parser.error("expected a full SHA and positive run ID")
    if args.out.exists():
        parser.error("output already exists; preserve prior observations")
    path = f"/actions/runs/{args.run}"
    run = api(args.hostname, args.repo, path)
    jobs = api(args.hostname, args.repo, path + "/jobs")
    errors = execution_errors(run, jobs, args.repo, args.head, args.workflow, args.job)
    if run.get("id") != args.run:
        errors.append("requested run ID mismatch")
    # Minimize unrelated account metadata while retaining the checked fields.
    kept = {key: run.get(key) for key in (
        "id", "workflow_id", "commit_sha", "event", "status", "started", "stopped", "html_url")}
    kept["repository"] = {key: run.get("repository", {}).get(key) for key in ("id", "full_name")}
    receipt = {
        "collected_at": datetime.now(timezone.utc).isoformat(),
        "hostname": args.hostname,
        "expected": {"repo": args.repo, "head": args.head, "workflow": args.workflow, "jobs": args.job},
        "run": kept, "jobs": jobs, "errors": errors,
        "execution_passed": not errors, "merge_authorized": False,
        "scope": "Server run/job execution only; no workflow provenance, review or runner trust established.",
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    with args.out.open("x", encoding="utf-8") as output:
        json.dump(receipt, output, indent=2)
        output.write("\n")
    print(json.dumps({"execution_passed": not errors, "merge_authorized": False, "errors": errors}))
    return 2 if errors else 0


if __name__ == "__main__":
    raise SystemExit(main())
