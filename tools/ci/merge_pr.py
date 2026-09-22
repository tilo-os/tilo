#!/usr/bin/env python3
"""Land one approved routine PR from a trusted checkout of current main."""

import argparse
from datetime import datetime, timezone
import errno
import hashlib
import hmac
import json
import os
from pathlib import Path
import re
import subprocess
import urllib.error
import urllib.request


HOST = "code.podesta.ai"
REPO = "qos/qos"
BRANCH = "main"
INTEGRATOR = "tilo-integrator"
REVIEWERS = {"claude"}
FETCHED_MAIN_REF = "refs/tilo-integration/observed-main"
REQUIRED_CONTEXTS = {
    "CI / affected packages / checks (pull_request)",
    "CI / fast quality / quality (pull_request)",
}
SHA = re.compile(r"[0-9a-f]{40}")
SHA256 = re.compile(r"[0-9a-f]{64}")
ROUTINE = re.compile(
    r"apps/(?P<app>[A-Za-z0-9_-]+)/src/(?:[A-Za-z0-9_.-]+/)*[A-Za-z0-9_.-]+\.rs"
)


def bound_token(environ=None):
    environ = os.environ if environ is None else environ
    token = environ.get("FORGEJO_TOKEN", "")
    expected = environ.get("FORGEJO_TOKEN_SHA256", "")
    if not token or not SHA256.fullmatch(expected):
        return ""
    actual = hashlib.sha256(token.encode("utf-8")).hexdigest()
    return token if hmac.compare_digest(actual, expected) else ""


class Api:
    def __init__(self, token):
        self.token = token

    def __call__(self, path, method="GET", body=None):
        data = None if body is None else json.dumps(body).encode()
        endpoint = f"/api/v1/repos/{REPO}{path}"
        request = urllib.request.Request(
            f"https://{HOST}{endpoint}", data=data, method=method,
            headers={"Authorization": "token " + self.token,
                     "Content-Type": "application/json"},
        )
        opener = urllib.request.build_opener(NoRedirect)
        with opener.open(request, timeout=30) as response:
            raw = response.read()
            return response.status, json.loads(raw) if raw else None


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, file_pointer, code, message, headers, new_url):
        raise urllib.error.HTTPError(request.full_url, code, "redirect denied", headers, file_pointer)


def git(root, *args):
    return subprocess.run(
        ["git", "--no-replace-objects", "-C", str(root), *args],
        check=True, capture_output=True, timeout=60,
    ).stdout


def routine_scope_errors(root, base, head):
    errors = []
    records = git(root, "diff", "--raw", "-z", "--no-renames", base, head).split(b"\0")
    paths = []
    for index in range(0, len(records) - 1, 2):
        header, raw_path = records[index], records[index + 1]
        fields = header.split()
        if len(fields) != 5:
            errors.append("unsupported Git diff record")
            continue
        before, after, status = fields[0][1:], fields[1], fields[4]
        try:
            path = raw_path.decode("utf-8")
        except UnicodeDecodeError:
            errors.append("non-UTF-8 path")
            continue
        paths.append(path)
        if (status not in (b"A", b"M") or after not in (b"100644", b"100755")
                or (before != b"000000" and before != after)):
            errors.append(f"non-routine change type: {path}")
        routine = ROUTINE.fullmatch(path)
        if not routine:
            errors.append(f"non-routine path: {path}")
        else:
            try:
                git(root, "cat-file", "-e", f"{base}:apps/{routine['app']}/Cargo.toml")
            except subprocess.CalledProcessError:
                errors.append(f"source is outside an existing app package: {path}")
    if not paths:
        errors.append("candidate has no changes")
    return errors


def routine_errors(root, base, head):
    errors = []
    if not SHA.fullmatch(base) or not SHA.fullmatch(head):
        return ["missing full base or head SHA"]
    try:
        git(root, "merge-base", "--is-ancestor", base, head)
    except subprocess.CalledProcessError:
        errors.append("candidate does not contain current main")
    if git(root, "rev-parse", "HEAD").decode().strip() != base:
        errors.append("trusted checkout is not current main")
    if git(root, "status", "--porcelain", "--untracked-files=all").strip():
        errors.append("trusted checkout is not clean")
    return errors + routine_scope_errors(root, base, head)


def routine_prefilter_errors(root, base, head):
    """Return only safe candidate rejections; raise on controller-state faults."""
    if not SHA.fullmatch(base) or not SHA.fullmatch(head):
        raise ValueError("missing full base or head SHA")
    if git(root, "rev-parse", "HEAD").decode().strip() != base:
        raise ValueError("trusted checkout moved during scan")
    if git(root, "status", "--porcelain", "--untracked-files=all").strip():
        raise ValueError("trusted checkout became dirty during scan")
    try:
        git(root, "merge-base", "--is-ancestor", base, head)
    except subprocess.CalledProcessError as error:
        if error.returncode == 1:
            return ["candidate does not contain current main"]
        raise
    return routine_scope_errors(root, base, head)


def pages(api, path, field=None):
    rows, seen = [], set()
    for page in range(1, 101):
        separator = "&" if "?" in path else "?"
        _, response = api(f"{path}{separator}limit=50&page={page}")
        batch = response[field] if field else response
        if not isinstance(batch, list):
            raise ValueError("invalid paginated response")
        for item in batch:
            identifier = item.get("id")
            if type(identifier) is not int or identifier in seen:
                raise ValueError("invalid or repeated API record")
            seen.add(identifier)
            rows.append(item)
        if field and type(response.get("total_count")) is int:
            if len(rows) == response["total_count"]:
                return rows
            if not batch or len(rows) > response["total_count"]:
                raise ValueError("incomplete paginated response")
        elif len(batch) < 50:
            return rows
    raise ValueError("pagination limit exceeded")


def protection_errors(branch):
    expected = {
        "protected": True,
        "enable_status_check": True,
        "status_check_contexts": REQUIRED_CONTEXTS,
        "user_can_merge": True,
        "user_can_push": False,
    }
    errors = []
    for key, wanted in expected.items():
        actual = branch.get(key)
        if isinstance(wanted, set):
            matches = isinstance(actual, list) and set(actual) == wanted
        elif isinstance(wanted, bool):
            matches = type(actual) is bool and actual is wanted
        else:
            matches = actual == wanted
        if not matches:
            errors.append("branch protection mismatch: " + key)
    approvals = branch.get("required_approvals")
    if type(approvals) is not int or approvals < 1:
        errors.append("branch protection requires no independent approval")
    return errors


def protection_rule_errors(rule):
    expected = {
        "enable_push": False,
        "apply_to_admins": True,
        "enable_merge_whitelist": False,
        "merge_whitelist_usernames": [],
        "merge_whitelist_teams": [],
        "enable_status_check": True,
        "status_check_contexts": REQUIRED_CONTEXTS,
        "enable_approvals_whitelist": True,
        "approvals_whitelist_username": sorted(REVIEWERS),
        "approvals_whitelist_teams": [],
        "block_on_rejected_reviews": True,
        "block_on_official_review_requests": True,
        "block_on_outdated_branch": True,
        "dismiss_stale_approvals": True,
    }
    errors = []
    for key, wanted in expected.items():
        actual = rule.get(key)
        if isinstance(wanted, set):
            matches = isinstance(actual, list) and set(actual) == wanted
        else:
            matches = actual == wanted
        if not matches:
            errors.append("branch protection rule mismatch: " + key)
    approvals = rule.get("required_approvals")
    if type(approvals) is not int or approvals < 1:
        errors.append("branch protection rule requires no independent approval")
    return errors


def run_errors(api, runs, head, workflow, required_job):
    choices = [run for run in runs if run.get("workflow_id") == workflow
               and run.get("event") == "pull_request" and run.get("commit_sha") == head]
    if not choices:
        return [f"missing exact-head {workflow} run"]
    run = max(choices, key=lambda item: item.get("id", 0))
    errors = []
    if run.get("status") != "success":
        errors.append(f"latest exact-head {workflow} run is not successful")
    _, jobs = api(f"/actions/runs/{run['id']}/jobs")
    matches = [job for job in jobs if job.get("name") == required_job]
    if len(matches) != 1 or matches[0].get("status") != "success":
        errors.append(f"{workflow} job {required_job!r} is not uniquely successful")
    if any(job.get("status") in ("failure", "cancelled") for job in jobs):
        errors.append(f"{workflow} retains a failed or cancelled job")
    return errors


def observe(api, number, root):
    _, repository = api("")
    _, pr = api(f"/pulls/{number}")
    _, branch = api(f"/branches/{BRANCH}")
    _, rule = api(f"/branch_protections/{BRANCH}")
    head = pr.get("head", {}).get("sha", "")
    base = branch.get("commit", {}).get("id", "")
    errors = []
    if repository.get("full_name") != REPO or repository.get("private") is not True:
        errors.append("repository identity mismatch")
    if (pr.get("state") != "open" or pr.get("merged") is not False or pr.get("draft") is not False
            or pr.get("base", {}).get("ref") != BRANCH):
        errors.append("PR is not an open, non-draft main candidate")
    if pr.get("user", {}).get("login") in REVIEWERS | {INTEGRATOR}:
        errors.append("author is not independent of reviewer and integrator")
    errors += protection_errors(branch)
    errors += protection_rule_errors(rule)
    errors += routine_errors(root, base, head)
    reviews = pages(api, f"/pulls/{number}/reviews")
    approved = {review.get("user", {}).get("login") for review in reviews
                if review.get("state") == "APPROVED" and review.get("official") is True
                and review.get("dismissed") is False and review.get("commit_id") == head}
    if not approved & REVIEWERS:
        errors.append("missing allowed independent approval on exact head")
    if any(review.get("state") == "REQUEST_CHANGES" and review.get("dismissed") is False
           for review in reviews):
        errors.append("outstanding change request")
    runs = pages(api, f"/actions/runs?head_sha={head}", "workflow_runs")
    errors += run_errors(api, runs, head, "check.yml", "checks")
    errors += run_errors(api, runs, head, "lint.yml", "quality")
    return {"pr": number, "head": head, "base": base, "errors": sorted(set(errors))}


def save(path, value, *, exclusive=False):
    text = json.dumps(value, indent=2) + "\n"
    if exclusive:
        with path.open("x", encoding="utf-8") as stream:
            stream.write(text)
            stream.flush()
            os.fsync(stream.fileno())
        fsync_parent(path)
        return
    temporary = path.with_suffix(path.suffix + ".tmp")
    with temporary.open("w", encoding="utf-8") as stream:
        stream.write(text)
        stream.flush()
        os.fsync(stream.fileno())
    temporary.replace(path)
    fsync_parent(path)


def fsync_parent(path):
    """Persist the directory entry on systems that support directory fsync."""
    descriptor = None
    try:
        flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
        descriptor = os.open(path.parent, flags)
        os.fsync(descriptor)
    except OSError as error:
        # Windows and some filesystems do not permit opening/fsyncing directories.
        if error.errno not in {errno.EACCES, errno.EINVAL, errno.ENOTSUP, errno.EPERM}:
            raise
    finally:
        if descriptor is not None:
            os.close(descriptor)


def reconcile(api, journal_path, root):
    journal = json.loads(journal_path.read_text(encoding="utf-8"))
    if (journal.get("repo"), journal.get("state")) not in {
            (REPO, "merge_intent_do_not_retry"),
            (REPO, "not_verified_do_not_retry"),
    } or type(journal.get("pr")) is not int or not SHA.fullmatch(journal.get("head", "")):
        raise ValueError("journal is not a valid unresolved merge intent")
    _, pr = api(f"/pulls/{journal['pr']}")
    _, branch = api(f"/branches/{BRANCH}")
    main = branch.get("commit", {}).get("id", "")
    fetched_main, contained = "", False
    if SHA.fullmatch(main):
        try:
            git(root, "fetch", "--force", "--no-tags", "origin",
                f"refs/heads/{BRANCH}:{FETCHED_MAIN_REF}")
            fetched_main = git(root, "rev-parse", FETCHED_MAIN_REF).decode().strip()
            if fetched_main != main:
                raise ValueError("fetched main differs from API main")
            git(root, "merge-base", "--is-ancestor", journal["head"], fetched_main)
            contained = True
        except (subprocess.CalledProcessError, ValueError):
            pass
    verified = (pr.get("merged") is True and pr.get("merge_commit_sha") == journal["head"]
                and (pr.get("merged_by") or {}).get("login") == INTEGRATOR
                and contained)
    journal["readback"] = {"merged": pr.get("merged"), "merge_commit_sha": pr.get("merge_commit_sha"),
                           "merged_by": (pr.get("merged_by") or {}).get("login"),
                           "main": main, "fetched_main": fetched_main,
                           "head_contained_in_main": contained}
    journal["state"] = "merged_verified" if verified else "not_verified_do_not_retry"
    save(journal_path, journal)
    return 0 if verified else 3


def merge(api, number, root, journal_path, execute=False):
    journal = {"schema": 1, "repo": REPO, "pr": number, "execute": execute,
               "created_at": datetime.now(timezone.utc).isoformat(), "state": "observing"}
    save(journal_path, journal, exclusive=True)
    first = observe(api, number, root)
    second = observe(api, number, root)
    errors = sorted(set(first["errors"] + second["errors"]))
    if (first["head"], first["base"]) != (second["head"], second["base"]):
        errors.append("candidate or main changed during admission")
    journal.update(head=second["head"], base=second["base"], errors=errors)
    if errors:
        journal["state"] = "rejected"
        save(journal_path, journal)
        return 2
    if not execute:
        journal["state"] = "dry_run_ready"
        save(journal_path, journal)
        return 0
    journal["state"] = "merge_intent_do_not_retry"
    save(journal_path, journal)
    try:
        status, _ = api(f"/pulls/{number}/merge", "POST", {
            "Do": "fast-forward-only", "head_commit_id": second["head"],
            "force_merge": False, "merge_when_checks_succeed": False,
        })
        journal["response_status"] = status
    except (OSError, urllib.error.HTTPError) as error:
        journal["response_error"] = type(error).__name__
    save(journal_path, journal)
    return reconcile(api, journal_path, root)


def main(argv=None, environ=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pr", type=int, required=True)
    parser.add_argument("--git-root", type=Path, required=True)
    parser.add_argument("--journal", type=Path, required=True)
    parser.add_argument("--execute", action="store_true")
    parser.add_argument("--reconcile", action="store_true")
    args = parser.parse_args(argv)
    token = bound_token(environ)
    if args.pr <= 0 or not token or (args.execute and args.reconcile):
        parser.error("positive PR, bound FORGEJO_TOKEN and one operation are required")
    if args.reconcile:
        if not args.journal.exists():
            parser.error("reconciliation requires an existing journal")
        return reconcile(Api(token), args.journal, args.git_root.resolve())
    if args.journal.exists():
        parser.error("a new journal path is required")
    return merge(Api(token), args.pr, args.git_root.resolve(), args.journal, args.execute)


if __name__ == "__main__":
    raise SystemExit(main())
