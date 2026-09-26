#!/usr/bin/env python3
"""Print the selection base of a main-branch check.yml push run.

Queued main check runs are coalesced (tools/ci/queue_coalesce.py): a run that
never started is cancelled once a newer one is queued. The newest run must then
test the skipped commits' changes too, so its base is the nearest first-parent
ancestor of HEAD whose own check run actually started, not github.event.before.
Ancestors whose run is queued, cancelled or missing fall inside the range.

The coalescer cancels only runs that never started, so it never cancels a run
chosen as a base here. A started run that fails or is cancelled by hand leaves
its own range uncovered, as the push-before base did; `receipt.sh covers` says
which commits a green run covers.

Without the runs API the base falls back to the nearest ancestor with a CI
receipt (a completed run), which is never newer than a started one, so the
range only grows. Exit 3 when neither finds a base within --limit commits; the
caller then selects the full workspace.
"""

import argparse
import json
import os
import subprocess
import sys
from urllib.error import URLError
from urllib.parse import urlencode, urlsplit
from urllib.request import HTTPRedirectHandler, Request, build_opener

WORKFLOW = "check.yml"
MAIN = "refs/heads/main"
UNSTARTED = {None, "", "1970-01-01T00:00:00Z", "0001-01-01T00:00:00Z"}
STARTED_STATUSES = {"running", "success", "failure"}
NOTES_REF = "refs/notes/tilo-check-ci"


def started_main_check(runs):
    """Whether one of a commit's runs is a main check.yml push run that started."""
    for run in runs:
        try:
            payload = json.loads(run.get("event_payload") or "{}")
        except ValueError:
            continue
        if (run.get("workflow_id") == WORKFLOW and run.get("event") == "push"
                and isinstance(payload, dict) and payload.get("ref") == MAIN
                and run.get("status") in STARTED_STATUSES
                and run.get("started") not in UNSTARTED):
            return True
    return False


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ValueError("API redirects refused")


class Forgejo:
    def __init__(self, base_url, token, repository, timeout):
        parsed = urlsplit(base_url)
        if parsed.scheme not in {"https", "http"} or not parsed.netloc or parsed.username:
            raise ValueError("invalid API URL")
        if len(repository.split("/")) != 2 or any(not part for part in repository.split("/")):
            raise ValueError("invalid repository")
        self.base_url = base_url.rstrip("/")
        self.repository = repository
        self.token = token
        self.timeout = timeout
        self.opener = build_opener(NoRedirect())

    def runs(self, sha):
        query = urlencode(dict(head_sha=sha, event="push", limit=50))
        headers = {"Accept": "application/json"}
        if self.token:
            headers["Authorization"] = f"token {self.token}"
        request = Request(f"{self.base_url}/repos/{self.repository}/actions/runs?{query}", headers=headers)
        with self.opener.open(request, timeout=self.timeout) as response:
            rows = json.loads(response.read())["workflow_runs"]
        if not isinstance(rows, list) or len(rows) >= 50:
            raise ValueError("unexpected runs page")
        return rows


def git(*args, check=True):
    result = subprocess.run(["git", *args], capture_output=True, text=True)
    if check and result.returncode:
        raise RuntimeError(f"git {args[0]} failed: {result.stderr.strip()[-300:]}")
    return result


def ancestors(head, limit):
    """First-parent ancestors of head, nearest first, head excluded."""
    return git("rev-list", "--first-parent", f"--max-count={limit + 1}", head).stdout.split()[1:]


def base_from_runs(api, commits):
    for sha in commits:
        if started_main_check(api.runs(sha)):
            return sha
    return None


def base_from_receipts(commits):
    for sha in commits:
        if git("notes", f"--ref={NOTES_REF}", "show", sha, check=False).returncode == 0:
            return sha
    return None


def select_base(api, commits):
    """(base or None, source): runs API first, CI receipts when it is unavailable."""
    try:
        return base_from_runs(api, commits), "started check run"
    except (ValueError, KeyError, TypeError, OSError, URLError) as error:
        # Never echo server payloads; the type is enough to diagnose.
        print(f"runs API unavailable ({type(error).__name__}); using CI receipts", file=sys.stderr)
        return base_from_receipts(commits), "CI receipt"


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--head", required=True)
    parser.add_argument("--limit", type=int, default=50)
    parser.add_argument("--timeout", type=int, default=20)
    args = parser.parse_args()
    server = os.environ.get("GITHUB_SERVER_URL", "")
    base_url = os.environ.get("GITHUB_API_URL") or (f"{server}/api/v1" if server else "")
    commits = ancestors(args.head, args.limit)
    try:
        api = Forgejo(base_url, os.environ.get("FORGEJO_TOKEN", "").strip(),
                      os.environ.get("GITHUB_REPOSITORY", ""), args.timeout)
    except ValueError as error:
        print(f"runs API not configured: {error}", file=sys.stderr)
        base, source = base_from_receipts(commits), "CI receipt"
    else:
        base, source = select_base(api, commits)
    if base is None:
        print(f"no checked ancestor within {args.limit} commits", file=sys.stderr)
        return 3
    print(f"selection base {base} ({source}, {commits.index(base) + 1} commits back)", file=sys.stderr)
    print(base)
    return 0


if __name__ == "__main__":
    sys.exit(main())
