#!/usr/bin/env python3
"""Keep the open RPM snapshot pin pull requests converging on main.

    reconcile-snapshot-pins.py --server URL --repository OWNER/NAME [--dry-run]

Run from a checkout of current main (snapshot-rpms.yml runs it on every push
that can move the pin, and after it opens a pin PR). A pin PR is never closed
just because a newer one exists: closing one whose checks are about to pass,
for one whose checks have not started, is how main went hours without a
landable pin. Instead, for every open pin PR (automation/rpm-snapshot-* and
automation/rpm-kernel-snapshot-* against main):

  * obsolete, when main already pins a snapshot built from the same commit or
    a later one: closed, with a comment naming main's pin;
  * newer than main's pin but no longer applying to main, because another pin
    landed first: its commit is replayed onto main's tip, its pin files
    winning, and force-pushed with a lease;
  * otherwise left as it is.

Every PR it keeps has Forgejo's "merge when checks succeed" scheduled, so it
lands through branch protection's required checks like any other PR, and the
first one to pass wins; the rest become obsolete or are replayed on the next
push.

FORGEJO_TOKEN must read, comment on, close and merge pull requests
(secrets.RELEASE_TOKEN in the workflows). Pushes use the checkout's git
credentials.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request

PREFIXES = ("automation/rpm-snapshot-", "automation/rpm-kernel-snapshot-")
PIN = "os/rpms-snapshot.env"
SOURCE = re.compile(r"^RPMS_SOURCE_REVISION=([0-9a-f]{40})$", re.MULTILINE)


class ReconcileError(RuntimeError):
    """A failure reported as one clear error line."""


def git(*args: str, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(["git", *args], check=check, capture_output=True, text=True)


def source_revision(text: str) -> str | None:
    match = SOURCE.search(text)
    return match.group(1) if match else None


def pin_source(ref: str) -> str | None:
    shown = git("show", f"{ref}:{PIN}", check=False)
    return source_revision(shown.stdout) if shown.returncode == 0 else None


def is_ancestor(older: str, newer: str) -> bool:
    return git("merge-base", "--is-ancestor", older, newer, check=False).returncode == 0


def classify(branch_source: str | None, main_source: str | None) -> str:
    """obsolete, or newer (main's pin predates the branch's)."""
    if branch_source is None:
        return "obsolete"
    if main_source is None:
        return "newer"
    if branch_source == main_source or is_ancestor(branch_source, main_source):
        return "obsolete"
    return "newer"


def applies_to(main: str, branch: str) -> bool:
    """Does the branch still merge into main without conflicts?"""
    return git("merge-tree", "--write-tree", main, branch, check=False).returncode == 0


def replay(main: str, branch_ref: str, branch: str, dry_run: bool) -> None:
    """Rebuild the pin commit on main's tip, the branch's pin files winning."""
    tip = git("rev-parse", branch_ref).stdout.strip()
    git("checkout", "--quiet", "--detach", main)
    picked = git("cherry-pick", "--strategy-option=theirs", tip, check=False)
    if picked.returncode != 0:
        git("cherry-pick", "--abort", check=False)
        raise ReconcileError(f"could not replay {branch} onto main: {picked.stderr.strip()}")
    if dry_run:
        print(f"would force-push {branch} onto main's tip")
        return
    pushed = git("push", f"--force-with-lease=refs/heads/{branch}:{tip}", "origin",
                 f"HEAD:refs/heads/{branch}", check=False)
    if pushed.returncode != 0:
        raise ReconcileError(f"could not push the replayed {branch}: {pushed.stderr.strip()}")
    print(f"replayed {branch} onto main's tip")


class Forge:
    def __init__(self, server: str, repository: str, token: str, dry_run: bool):
        self.base = f"{server.rstrip('/')}/api/v1/repos/{repository}"
        self.repository = repository
        self.token = token
        self.dry_run = dry_run

    def call(self, path: str, method: str = "GET", payload: dict | None = None):
        data = None if payload is None else json.dumps(payload).encode()
        headers = {"Authorization": f"token {self.token}", "Accept": "application/json"}
        if data is not None:
            headers["Content-Type"] = "application/json"
        request = urllib.request.Request(self.base + path, data=data, headers=headers, method=method)
        with urllib.request.urlopen(request, timeout=30) as response:
            body = response.read()
        return json.loads(body) if body else None

    def open_pins(self) -> list[dict]:
        pins, page = [], 1
        while True:
            pulls = self.call(f"/pulls?state=open&limit=50&page={page}")
            for pull in pulls:
                head = pull.get("head", {})
                if (head.get("ref", "").startswith(PREFIXES)
                        and head.get("repo", {}).get("full_name") == self.repository
                        and pull.get("base", {}).get("ref") == "main"):
                    pins.append(pull)
            if len(pulls) < 50:
                return pins
            page += 1

    def close(self, number: int, reason: str) -> None:
        if self.dry_run:
            print(f"would close #{number}: {reason}")
            return
        self.call(f"/issues/{number}/comments", "POST", {"body": reason})
        self.call(f"/pulls/{number}", "PATCH", {"state": "closed"})
        print(f"closed #{number}: {reason}")

    def schedule_merge(self, number: int) -> None:
        """Forgejo merges it once branch protection's required checks pass."""
        if self.dry_run:
            print(f"would schedule #{number} to merge when its checks succeed")
            return
        try:
            self.call(f"/pulls/{number}/merge", "POST", {
                "Do": "merge",
                "merge_when_checks_succeed": True,
                "delete_branch_after_merge": True,
            })
        except urllib.error.HTTPError as error:
            # 405/409: already scheduled, or mergeable only after a push; the
            # next reconcile run schedules it again.
            print(f"::warning::could not schedule #{number} to merge (HTTP {error.code})", file=sys.stderr)
            return
        print(f"#{number} merges when its checks succeed")


def reconcile(forge: Forge, main: str = "HEAD") -> int:
    main_source = pin_source(main)
    pins = forge.open_pins()
    if not pins:
        print("no open snapshot pin pull requests")
        return 0
    git("fetch", "--no-tags", "--quiet", "origin",
        *[f"+refs/heads/{pull['head']['ref']}:refs/remotes/origin/{pull['head']['ref']}" for pull in pins])
    main_commit = git("rev-parse", main).stdout.strip()
    failures = 0
    for pull in sorted(pins, key=lambda p: p["number"]):
        number, branch = pull["number"], pull["head"]["ref"]
        ref = f"refs/remotes/origin/{branch}"
        if classify(pin_source(ref), main_source) == "obsolete":
            forge.close(number, f"Obsolete: main already pins an RPM snapshot built from "
                                f"{main_source or 'a later commit'}, at least as new as this one.")
            continue
        try:
            if not applies_to(main_commit, ref):
                replay(main_commit, ref, branch, forge.dry_run)
        except ReconcileError as error:
            print(f"::error::#{number}: {error}", file=sys.stderr)
            failures += 1
            continue
        forge.schedule_merge(number)
    git("checkout", "--quiet", "--detach", main_commit, check=False)
    return 1 if failures else 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--server", required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    token = os.environ.get("FORGEJO_TOKEN", "")
    if not token:
        parser.error("FORGEJO_TOKEN is empty; pass secrets.RELEASE_TOKEN")
    try:
        return reconcile(Forge(args.server, args.repository, token, args.dry_run))
    except (ReconcileError, urllib.error.URLError, subprocess.CalledProcessError) as error:
        print(f"::error::{error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
