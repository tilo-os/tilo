#!/usr/bin/env python3
"""Read-only alpha integration audit. Run from a trusted checkout, never a PR checkout.

This deliberately does not merge or publish statuses. A passing receipt proves
only the named observations; runner isolation and authority separation require
separate qualification. Policy and expected jobs must come from trusted tooling.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import re
import subprocess
from urllib.parse import quote


SHA = re.compile(r"[0-9a-f]{40}")


def api(host, repo, suffix):
    result = subprocess.run(
        ["fgj", "--hostname", host, "api", f"repos/{repo}{suffix}"],
        check=True, capture_output=True, text=True, timeout=60,
    )
    return json.loads(result.stdout)


def review_errors(reviews, head, author, reviewers):
    errors = []
    approved = set()
    for review in reviews:
        user = review.get("user", {}).get("login")
        if review.get("dismissed") is not False or review.get("official") is not True:
            continue
        if review.get("state") == "REQUEST_CHANGES":
            errors.append(f"outstanding change request by {user}")
        if (review.get("state") == "APPROVED" and review.get("commit_id") == head
                and user in reviewers and user != author):
            approved.add(user)
    if not approved:
        errors.append("no allowed independent official approval on exact head")
    return errors


def run_errors(run, jobs, repo, head, requirement):
    errors = []
    repository = run.get("repository") or {}
    run_id, repo_id = run.get("id"), repository.get("id")
    if type(run_id) is not int or run_id != requirement["run_id"]:
        errors.append("run ID mismatch")
    if type(repo_id) is not int or repo_id <= 0 or repository.get("full_name") != repo:
        errors.append("run repository mismatch")
    if run.get("commit_sha") != head:
        errors.append("run head mismatch")
    if run.get("workflow_id") != requirement["workflow"]:
        errors.append("workflow mismatch")
    if run.get("event") not in requirement["events"]:
        errors.append("unexpected run event")
    if run.get("status") != "success":
        errors.append("run not successful")
    if not isinstance(jobs, list) or not jobs:
        return errors + ["missing job records"]
    names = [j.get("name") for j in jobs]
    if len(names) != len(set(names)) or set(names) != set(requirement["jobs"]):
        errors.append("job set differs from trusted expectation")
    ids = [j.get("id") for j in jobs]
    if any(type(i) is not int or i <= 0 for i in ids) or len(ids) != len(set(ids)):
        errors.append("missing or duplicate job IDs")
    attempts = {j.get("attempt") for j in jobs}
    if len(attempts) != 1 or any(type(a) is not int or a < 1 for a in attempts):
        errors.append("ambiguous job attempt")
    for job in jobs:
        if job.get("run_id") != run_id or job.get("repo_id") != repo_id:
            errors.append("foreign job record")
        if job.get("status") != "success":
            errors.append(f"job {job.get('name')!r} not successful")
    return errors


def policy_errors(policy):
    errors = []
    if not isinstance(policy, dict):
        return ["policy must be an object"]
    if not SHA.fullmatch(str(policy.get("trusted_revision", ""))):
        errors.append("trusted_revision must be a full commit SHA")
    paths = policy.get("protected_paths", [])
    if not isinstance(paths, list) or not paths:
        errors.append("protected_paths must explicitly name the complete trusted harness")
    elif any(not isinstance(p, str) or not p or p.startswith(("/", ":"))
             or ".." in p.split("/") or "\\" in p for p in paths):
        errors.append("protected_paths must be literal repository-relative paths")
    reviewers = policy.get("reviewers", [])
    if not isinstance(reviewers, list) or not reviewers or any(not isinstance(r, str) or not r for r in reviewers):
        errors.append("reviewers must be a nonempty login list")
    runs = policy.get("runs", [])
    if not isinstance(runs, list) or not runs:
        return errors + ["runs must explicitly identify every required workflow and job"]
    workflows = []
    for run in runs:
        if not isinstance(run, dict):
            errors.append("invalid run requirement")
            continue
        workflow = run.get("workflow")
        workflows.append(workflow)
        if not isinstance(workflow, str) or not re.fullmatch(r"[A-Za-z0-9_.-]+\.ya?ml", workflow):
            errors.append("invalid workflow filename")
        elif isinstance(paths, list) and not any(
                f".forgejo/workflows/{workflow}" == p or f".forgejo/workflows/{workflow}".startswith(p.rstrip("/") + "/")
                for p in paths if isinstance(p, str)):
            errors.append("required workflow is outside protected_paths")
        if type(run.get("run_id")) is not int or run["run_id"] <= 0:
            errors.append("invalid run ID")
        for field in ("jobs", "events"):
            values = run.get(field)
            if (not isinstance(values, list) or not values
                    or any(not isinstance(v, str) or not v for v in values)
                    or len(values) != len(set(values))):
                errors.append(f"invalid {field} expectation")
    if len(workflows) != len(set(str(w) for w in workflows)):
        errors.append("duplicate workflow requirement")
    return errors


def git(root, *args):
    return subprocess.run(["git", "--no-replace-objects", "-C", str(root), *args],
                          check=True, capture_output=True, timeout=60).stdout


def provenance_errors(root, base, head, policy):
    errors = []
    if git(root, "rev-parse", "--is-shallow-repository").strip() != b"false":
        return ["shallow repository cannot establish ancestry"]
    for ancestor, descendant, label in (
        (base, head, "current main is not an ancestor of candidate"),
        (policy["trusted_revision"], base, "trusted policy revision is not an ancestor of main"),
    ):
        try:
            git(root, "merge-base", "--is-ancestor", ancestor, descendant)
        except subprocess.CalledProcessError as error:
            if error.returncode != 1:
                raise
            errors.append(label)
    for path in policy["protected_paths"]:
        spec = ":(literal)" + path.rstrip("/")
        before = git(root, "ls-tree", "-r", "-z", policy["trusted_revision"], "--", spec)
        after = git(root, "ls-tree", "-r", "-z", head, "--", spec)
        if not before:
            errors.append(f"trusted protected path is absent: {path}")
        if before != after:
            errors.append(f"candidate changes trusted harness: {path}")
    return errors


def collect(host, repo, number, branch, policy, read=None):
    read = read or api
    prefix = f"/pulls/{number}"
    pr = read(host, repo, prefix)
    base = read(host, repo, "/branches/" + quote(branch, safe=""))
    reviews = []
    # Bound pagination and fail rather than silently accept an incomplete review set.
    for page in range(1, 101):
        batch = read(host, repo, prefix + f"/reviews?limit=50&page={page}")
        if not isinstance(batch, list):
            raise ValueError("invalid review response")
        reviews.extend(batch)
        if len(batch) < 50:
            break
    else:
        raise ValueError("review pagination exceeded bound")
    runs = []
    for expected in policy["runs"]:
        path = f"/actions/runs/{expected['run_id']}"
        # Installed Forgejo 16 API defines this as an unpaginated array (no
        # page/limit parameters). Exact expected job-set comparison fails closed
        # if an incomplete server response omits any required job.
        runs.append({"run": read(host, repo, path), "jobs": read(host, repo, path + "/jobs")})
    return {"pr": pr, "base": base, "reviews": reviews, "runs": runs}


def observation_errors(observation, repo, number, branch, policy):
    pr, errors = observation["pr"], []
    if pr.get("number") != number or pr.get("state") != "open" or pr.get("merged") is not False:
        errors.append("PR is not the requested open unmerged candidate")
    if pr.get("draft") is not False:
        errors.append("PR is draft or draft state is unavailable")
    if pr.get("base", {}).get("ref") != branch or pr.get("base", {}).get("repo", {}).get("full_name") != repo:
        errors.append("PR base repository/branch mismatch")
    head = pr.get("head", {}).get("sha", "")
    base = observation["base"].get("commit", {}).get("id", "")
    if not SHA.fullmatch(head) or not SHA.fullmatch(base):
        errors.append("missing full head/base SHA")
    author = pr.get("user", {}).get("login")
    if not author:
        errors.append("missing author identity")
    errors += review_errors(observation["reviews"], head, author, policy["reviewers"])
    if len(observation["runs"]) != len(policy["runs"]):
        errors.append("missing run observations")
    for expected, actual in zip(policy["runs"], observation["runs"]):
        errors += [expected["workflow"] + ": " + e for e in run_errors(actual["run"], actual["jobs"], repo, head, expected)]
    return errors


def fingerprint(observation):
    # Ignore unrelated profile/timing metadata; retain all authorization-relevant state.
    pr = observation["pr"]
    return {
        "pr": {**{k: pr.get(k) for k in ("number", "state", "draft", "merged")},
               **{k: {"sha": pr.get(k, {}).get("sha"), "ref": pr.get(k, {}).get("ref"),
                      "repo": pr.get(k, {}).get("repo", {}).get("full_name")}
                  for k in ("head", "base")}},
        "author": pr.get("user", {}).get("login"),
        "base": observation["base"].get("commit", {}).get("id"),
        "reviews": [{**{k: r.get(k) for k in ("id", "state", "commit_id", "dismissed", "official")},
                     "user": r.get("user", {}).get("login")} for r in observation["reviews"]],
        "runs": [{"run": {**{k: r["run"].get(k) for k in ("id", "status", "commit_sha", "workflow_id", "event", "started", "stopped")},
                          "repository": {k: r["run"].get("repository", {}).get(k) for k in ("id", "full_name")}},
                  "jobs": [{k: j.get(k) for k in ("id", "run_id", "repo_id", "name", "status", "attempt", "task_id")}
                           for j in r["jobs"]]} for r in observation["runs"]],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--hostname", default="code.podesta.ai")
    parser.add_argument("--repo", required=True)
    parser.add_argument("--pr", required=True, type=int)
    parser.add_argument("--branch", default="main")
    parser.add_argument("--git-root", required=True, type=Path)
    parser.add_argument("--policy", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", args.repo) or args.pr <= 0:
        parser.error("invalid repository or PR number")
    if args.out.exists():
        parser.error("output already exists")
    raw = args.policy.read_bytes()
    policy = json.loads(raw)
    errors = policy_errors(policy)
    receipt = {"collected_at": datetime.now(timezone.utc).isoformat(),
               "repo": args.repo, "pr": args.pr, "policy_sha256": hashlib.sha256(raw).hexdigest(),
               "policy": policy, "merge_authorized": False,
               "limitations": ["No runner isolation or credential separation qualification",
                               "Protected path closure must be supplied by trusted policy",
                               "Uniform job attempts and stable snapshots do not independently prove latest attempt",
                               "Snapshots are not an atomic merge authorization; server must recheck exact head and base"]}
    try:
        if not errors:
            first = collect(args.hostname, args.repo, args.pr, args.branch, policy)
            errors += observation_errors(first, args.repo, args.pr, args.branch, policy)
            head = first["pr"].get("head", {}).get("sha", "")
            base = first["base"].get("commit", {}).get("id", "")
            receipt.update(head=head, base=base, evidence=fingerprint(first))
            if SHA.fullmatch(head) and SHA.fullmatch(base):
                errors += provenance_errors(args.git_root, base, head, policy)
            second = collect(args.hostname, args.repo, args.pr, args.branch, policy)
            errors += observation_errors(second, args.repo, args.pr, args.branch, policy)
            if fingerprint(first) != fingerprint(second):
                errors.append("candidate/review/run evidence changed during audit; retry")
            receipt.update(head=head, base=base, evidence=fingerprint(second))
    except (OSError, ValueError, TypeError, AttributeError, KeyError, subprocess.SubprocessError) as error:
        errors.append(f"incomplete observation: {type(error).__name__}")
    receipt.update(errors=sorted(set(errors)), observed_ready=not errors)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    with args.out.open("x", encoding="utf-8") as output:
        json.dump(receipt, output, indent=2)
        output.write("\n")
    print(json.dumps({k: receipt[k] for k in ("observed_ready", "merge_authorized", "errors")}))
    return 0 if not errors else 2


if __name__ == "__main__":
    raise SystemExit(main())
