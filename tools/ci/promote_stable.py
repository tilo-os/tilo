#!/usr/bin/env python3
"""Fast-forward the promotion ref `stable` to a commit main has just verified.

Main is allowed to be unstable: a commit is tested after it lands, and `stable`
names the newest commit whose required checks all passed. This script is the
only thing that moves it, and it moves it forward only:

- the target must be on main (an ancestor of the fetched main tip), so no other
  object can be promoted;
- `stable` must be an ancestor of the target, which is what makes the push a
  fast-forward; git itself refuses the push otherwise, and the push carries no
  `--force`;
- an older target than `stable` (a run that finished after a later one) is
  left alone: `stable` is already ahead and needs nothing.

The push authenticates with the STABLE_PUSH_TOKEN secret, a bot account's token
with write access to this repository (docs/ci/stable-promotion.md). The job's
own token is not used: branch protection on `stable` admits only that bot.

Called from the `promote` job of .forgejo/workflows/check.yml.
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import subprocess
import sys
from urllib.parse import urlsplit, urlunsplit

STABLE = "refs/heads/stable"
MAIN = "refs/heads/main"
REMOTE = "origin"


def git(*args: str, check: bool = True, capture: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(["git", *args], check=check, text=True, capture_output=capture)


def resolve(rev: str) -> str | None:
    result = git("rev-parse", "--verify", "--quiet", f"{rev}^{{commit}}", check=False)
    return result.stdout.strip() if result.returncode == 0 else None


def fetch(remote: str) -> None:
    """Fetch both refs in full so ancestry questions get real answers."""
    git("fetch", "--no-tags", remote, f"+{MAIN}:refs/remotes/{remote}/main")
    # The first promotion creates the ref, so its absence is not an error.
    git("fetch", "--no-tags", remote, f"+{STABLE}:refs/remotes/{remote}/stable", check=False)


def push_url(remote_url: str, token: str) -> str:
    """The remote's URL with the bot's token, never printed."""
    parts = urlsplit(remote_url.strip())
    if parts.scheme not in {"http", "https"}:
        raise SystemExit(f"cannot authenticate to a non-HTTP remote: {remote_url}")
    # Forgejo accepts any user name with a token as the password.
    netloc = f"ci-bot:{token}@{parts.hostname}"
    if parts.port:
        netloc += f":{parts.port}"
    return urlunsplit(parts._replace(netloc=netloc))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Fast-forward stable to a verified main commit.")
    parser.add_argument("--target", default=os.environ.get("GITHUB_SHA", ""),
                        help="the commit whose checks passed (default: $GITHUB_SHA)")
    parser.add_argument("--remote", default=REMOTE)
    parser.add_argument("--dry-run", action="store_true",
                        help="print the decision without pushing")
    args = parser.parse_args(argv)
    summary = Path(os.environ["GITHUB_STEP_SUMMARY"]) if "GITHUB_STEP_SUMMARY" in os.environ else None

    def report(verdict: str, message: str) -> None:
        line = f"stable: {verdict} - {message}"
        print(line)
        if summary:
            with summary.open("a") as handle:
                handle.write(line + "\n")

    target = resolve(args.target)
    if target is None:
        print(f"::error::target {args.target!r} is not a commit in this checkout", file=sys.stderr)
        return 2
    # Fail before any Git work when the only credential that can move the ref is
    # missing; a fork or a new repository has no token configured yet.
    remote_url = git("remote", "get-url", args.remote).stdout.strip()
    destination = args.remote
    if urlsplit(remote_url).scheme in {"http", "https"}:
        token = os.environ.get("STABLE_PUSH_TOKEN", "").strip()
        if not token and not args.dry_run:
            print("::error::STABLE_PUSH_TOKEN is not set; cannot promote "
                  "(docs/ci/stable-promotion.md)", file=sys.stderr)
            return 2
        destination = push_url(remote_url, token or "-")
    fetch(args.remote)
    main_tip = resolve(f"refs/remotes/{args.remote}/main")
    stable = resolve(f"refs/remotes/{args.remote}/stable")
    if main_tip is None:
        print("::error::main is missing from the remote", file=sys.stderr)
        return 2
    if git("merge-base", "--is-ancestor", target, main_tip, check=False).returncode != 0:
        print(f"::error::{target} is not on main; only a landed commit can be promoted", file=sys.stderr)
        return 2
    if stable == target:
        report("unchanged", f"{target[:12]} is already promoted")
        return 0
    if stable is not None and git("merge-base", "--is-ancestor", target, stable, check=False).returncode == 0:
        report("ahead", f"{stable[:12]} already contains {target[:12]}; nothing to do")
        return 0
    if stable is not None and git("merge-base", "--is-ancestor", stable, target, check=False).returncode != 0:
        # Not a fast-forward: stable carries commits this run never tested.
        print(f"::error::stable ({stable[:12]}) is not an ancestor of {target[:12]}; "
              "refusing a promotion that would drop verified commits", file=sys.stderr)
        return 1
    if args.dry_run:
        report("would advance", f"{stable[:12] if stable else '(unborn)'} -> {target[:12]}")
        return 0
    result = git("push", "--porcelain", destination,
                 f"{target}:{STABLE}", check=False, capture=False)
    if result.returncode != 0:
        print("::error::the stable push was rejected; run again after re-reading stable", file=sys.stderr)
        return 1
    report("advanced", f"{stable[:12] if stable else '(unborn)'} -> {target[:12]}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
