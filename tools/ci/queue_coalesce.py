#!/usr/bin/env python3
"""Coalesce superseded, unstarted main push runs. Dry-run unless --apply.

Uses FORGEJO_TOKEN or --token-file authentication. Does not cancel manual, tag, PR, other-branch,
unknown-workflow, or already-started runs. Run periodically outside Actions.
Forgejo has no atomic cancel-if-still-waiting API: the final read narrows but
cannot eliminate the scheduler race between inspection and cancellation.
"""

import argparse
import json
import os
from pathlib import Path
import sys
from urllib.parse import urlencode, urlsplit
from urllib.request import Request, build_opener, HTTPRedirectHandler
from urllib.error import URLError

WORKFLOWS = frozenset({
    "build-image.yml", "package-desktop.yml",
    "release-apps.yml", "fs-fuse.yml", "legacy-app-e2e.yml",
})
UNSTARTED = {None, "", "1970-01-01T00:00:00Z", "0001-01-01T00:00:00Z"}


def eligible(run):
    try:
        payload = json.loads(run.get("event_payload", ""))
    except (ValueError, TypeError):
        return False
    return (
        isinstance(payload, dict)
        and run.get("event") == "push"
        and run.get("trigger_event") == "push"
        and payload.get("ref") == "refs/heads/main"
        and run.get("status") == "waiting"
        and "started" in run and run["started"] in UNSTARTED
        and run.get("workflow_id") in WORKFLOWS
        and isinstance(run.get("id"), int) and run["id"] > 0
    )


def plan(runs):
    """Keep newest queued run per workflow; IDs are Forgejo creation order."""
    latest = {}
    candidates = []
    for run in sorted(runs, key=lambda row: row["id"], reverse=True):
        if not eligible(run):
            continue
        workflow = run["workflow_id"]
        if workflow not in latest:
            latest[workflow] = run["id"]
        else:
            candidates.append((run, latest[workflow]))
    return latest, candidates


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ValueError("API redirects refused")


class Forgejo:
    def __init__(self, base_url, token, repository, timeout):
        parsed = urlsplit(base_url)
        if parsed.scheme not in {"https", "http"} or not parsed.netloc or parsed.username or parsed.query or parsed.fragment:
            raise ValueError("invalid configured API URL")
        if len(repository.split("/")) != 2 or any(not part or part in {".", ".."} for part in repository.split("/")):
            raise ValueError("invalid repository")
        self.base_url = base_url.rstrip("/")
        self.token = token
        self.opener = build_opener(NoRedirect())
        self.base = f"repos/{repository}/actions/runs"
        self.timeout = timeout

    def call(self, endpoint, method="GET"):
        request = Request(f"{self.base_url}/{endpoint}", method=method,
                          headers={"Authorization": f"token {self.token}", "Accept": "application/json"})
        with self.opener.open(request, timeout=self.timeout) as response:
            body = response.read()
        return json.loads(body) if body.strip() else None

    def collect(self, max_pages):
        runs = {}
        for page in range(1, max_pages + 1):
            query = urlencode(dict(limit=30, page=page, status="waiting",
                                   event="push", ref="refs/heads/main"))
            response = self.call(f"{self.base}?{query}")
            rows = response["workflow_runs"]
            if not isinstance(rows, list):
                raise ValueError("workflow_runs is not a list")
            if len(rows) > 30:
                raise ValueError("server ignored page size; no cancellations performed")
            for row in rows:
                runs[row["id"]] = row
            if len(rows) < 30:
                return list(runs.values())
        raise ValueError("page bound reached; no cancellations performed")


def coalesce(api, max_pages, max_cancel, apply):
    keep, candidates = plan(api.collect(max_pages))
    report = {"apply": apply, "keep": keep, "candidates": [
        {"id": row["id"], "workflow": row["workflow_id"], "replacement": newer}
        for row, newer in candidates], "cancelled": [], "changed": []}
    if len(candidates) > max_cancel:
        raise ValueError("cancellation bound exceeded; no cancellations performed")
    if apply:
        for original, newer in candidates:
            # Preserve the replacement if another operator cancelled it meanwhile.
            replacement = api.call(f"{api.base}/{newer}")
            identity = dict(replacement, status="waiting", started="1970-01-01T00:00:00Z")
            if (not eligible(identity) or replacement.get("id") != newer
                    or replacement.get("workflow_id") != original["workflow_id"]
                    or replacement.get("status") not in {"waiting", "running", "success"}):
                report["changed"].append(original["id"])
                continue
            jobs = api.call(f"{api.base}/{original['id']}/jobs")
            if not isinstance(jobs, list) or not jobs:
                raise ValueError("missing or invalid jobs response; refusing cancellation")
            if any(job.get("status") not in {"waiting", "blocked"} for job in jobs):
                report["changed"].append(original["id"])
                continue
            current = api.call(f"{api.base}/{original['id']}")
            if not eligible(current) or current["workflow_id"] != original["workflow_id"]:
                report["changed"].append(original["id"])
                continue
            api.call(f"{api.base}/{original['id']}/cancel", "POST")
            report["cancelled"].append(original["id"])
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default="https://code.podesta.ai/api/v1")
    parser.add_argument("--token-file")
    parser.add_argument("--repository", default="qos/qos")
    parser.add_argument("--max-pages", type=int, default=30)
    parser.add_argument("--max-cancel", type=int, default=300)
    parser.add_argument("--timeout", type=int, default=30)
    parser.add_argument("--apply", action="store_true")
    args = parser.parse_args()
    if min(args.max_pages, args.max_cancel, args.timeout) < 1:
        parser.error("bounds must be positive")
    try:
        token = Path(args.token_file).read_text().strip() if args.token_file else os.environ.get("FORGEJO_TOKEN", "").strip()
        if not token:
            raise ValueError("FORGEJO_TOKEN or --token-file required")
        api = Forgejo(args.base_url, token, args.repository, args.timeout)
        print(json.dumps(coalesce(api, args.max_pages, args.max_cancel, args.apply), indent=2))
    except (ValueError, KeyError, TypeError, OSError, URLError) as error:
        # Avoid emitting server payloads or credential-bearing CLI diagnostics.
        print(f"Queue coalescing stopped: {type(error).__name__}; inspect API availability and bounds", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
