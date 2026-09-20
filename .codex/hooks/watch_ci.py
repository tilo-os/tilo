#!/usr/bin/env python3
"""Queue one compact Forgejo CI result after a Codex-initiated git push."""

from __future__ import annotations

import datetime as dt
import hashlib
import json
import re
import subprocess
import sys
import time
from pathlib import Path
from typing import Any
from urllib.parse import urlparse

from push_event import push_target, pushes_current_branch
from watch_receipt import WatchReceipt, acquire


DISCOVERY_TIMEOUT_SECONDS = 90
SETTLE_SECONDS = 15
POLL_SECONDS = 10
WATCH_TIMEOUT_SECONDS = 25 * 60
TERMINAL_STATUSES = {
    "success",
    "failure",
    "cancelled",
    "canceled",
    "skipped",
    "neutral",
    "blocked",
}
GREEN_STATUSES = {"success", "skipped", "neutral"}


def run(
    args: list[str], *, cwd: Path, timeout: int = 30, check: bool = True
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        args,
        cwd=cwd,
        text=True,
        capture_output=True,
        timeout=timeout,
        check=check,
    )


def pushed_successfully(event: dict[str, Any]) -> bool:
    return push_target(event) is not None


def claim_watch(cwd: Path, session_id: str, sha: str) -> WatchReceipt | None:
    common = run(["git", "rev-parse", "--git-common-dir"], cwd=cwd).stdout.strip()
    directory = (cwd / common).resolve() / "codex-ci-watch"
    key = hashlib.sha256(f"{session_id}:{sha}".encode()).hexdigest()
    return acquire(directory / f"{key}.json", cwd=cwd, session_id=session_id, sha=sha)


def branch_is_on_remote(cwd: Path, sha: str, branch: str) -> bool:
    if not branch:
        return False
    result = run(
        ["git", "ls-remote", "--exit-code", "origin", f"refs/heads/{branch}"],
        cwd=cwd,
        check=False,
    )
    if result.returncode != 0:
        return False
    remote_sha = result.stdout.partition("\t")[0].strip()
    return remote_sha == sha


def forgejo_target(cwd: Path) -> tuple[str, str]:
    remote = run(["git", "remote", "get-url", "origin"], cwd=cwd).stdout.strip()
    if "://" in remote:
        parsed = urlparse(remote)
        host = parsed.hostname or ""
        repo = parsed.path.lstrip("/")
    else:
        match = re.fullmatch(r"(?:[^@]+@)?([^:]+):(.+)", remote)
        if match is None:
            raise ValueError("origin does not identify a Forgejo owner/repository")
        host, repo = match.groups()
    repo = repo.removesuffix(".git").strip("/")
    if not host or repo.count("/") != 1:
        raise ValueError("origin does not identify a Forgejo owner/repository")
    return host, repo


def parse_time(value: object) -> dt.datetime | None:
    if not isinstance(value, str):
        return None
    try:
        return dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


def recent_runs(
    *, cwd: Path, host: str, repo: str, sha: str, started: dt.datetime
) -> list[dict[str, Any]]:
    result = run(
        [
            "fgj",
            "--hostname",
            host,
            "actions",
            "run",
            "list",
            "-R",
            repo,
            "--commit",
            sha,
            "--json",
            "-L",
            "50",
        ],
        cwd=cwd,
    )
    data = json.loads(result.stdout or "[]")
    if not isinstance(data, list):
        raise ValueError("fgj returned a non-list run result")

    earliest = started - dt.timedelta(seconds=30)
    runs = []
    for item in data:
        if not isinstance(item, dict):
            continue
        created = parse_time(item.get("created"))
        if created is None or created >= earliest:
            runs.append(item)
    return runs


def run_key(item: dict[str, Any]) -> str:
    return str(
        item.get("index_in_repo") or item.get("id") or item.get("workflow_id") or "?"
    )


def terminal(item: dict[str, Any]) -> bool:
    return str(item.get("status", "")).lower() in TERMINAL_STATUSES


def watch(
    *, cwd: Path, host: str, repo: str, sha: str, started: dt.datetime
) -> tuple[str, list[dict[str, Any]]]:
    began = time.monotonic()
    discovery_deadline = began + DISCOVERY_TIMEOUT_SECONDS
    deadline = began + WATCH_TIMEOUT_SECONDS
    last_keys: set[str] = set()
    last_change = began
    latest: list[dict[str, Any]] = []

    while time.monotonic() < deadline:
        try:
            latest = recent_runs(
                cwd=cwd, host=host, repo=repo, sha=sha, started=started
            )
        except (OSError, ValueError, subprocess.SubprocessError):
            if not latest and time.monotonic() >= discovery_deadline:
                raise
            time.sleep(POLL_SECONDS)
            continue
        keys = {run_key(item) for item in latest}
        if keys != last_keys:
            last_keys = keys
            last_change = time.monotonic()

        if latest:
            if (
                all(terminal(item) for item in latest)
                and time.monotonic() - last_change >= SETTLE_SECONDS
            ):
                return "complete", latest
        elif time.monotonic() >= discovery_deadline:
            return "missing", []

        time.sleep(POLL_SECONDS)
    return "timeout", latest


def describe_run(item: dict[str, Any]) -> str:
    workflow = str(item.get("workflow_id") or item.get("title") or "workflow")
    number = run_key(item)
    status = str(item.get("status") or "unknown").lower()
    return f"{workflow} #{number}: {status}"


def result_message(
    *, outcome: str, runs: list[dict[str, Any]], sha: str, branch: str
) -> str:
    short = sha[:12]
    target = f"{branch} ({short})" if branch else short
    if outcome == "missing":
        return f"[CI watcher] No Forgejo Actions runs appeared for push {target} within 90 seconds."

    summary = "; ".join(describe_run(item) for item in sorted(runs, key=run_key))
    if outcome == "timeout":
        return f"[CI watcher] Timed out waiting for CI for {target}. Latest status: {summary or 'unknown'}."

    failed = [
        item
        for item in runs
        if str(item.get("status", "")).lower() not in GREEN_STATUSES
    ]
    if not failed:
        return f"[CI watcher] CI for {target} is green: {summary}. No action is needed."

    numbers = ", ".join(run_key(item) for item in failed)
    return (
        f"[CI watcher] CI for {target} finished with failures: {summary}. "
        f"Inspect failed output with `fgj actions run view <run> --log-failed` "
        f"(failed run{'s' if len(failed) != 1 else ''}: {numbers})."
    )


def deliver(session_id: str, message: str, cwd: Path) -> None:
    try:
        run(
            ["codex", "queue", "--thread", session_id, "--message", message],
            cwd=cwd,
            timeout=30,
        )
    except (OSError, subprocess.SubprocessError):
        # If queueing is unavailable, async hook output still reaches the next
        # safe model request or the user's next turn.
        print(
            json.dumps(
                {
                    "hookSpecificOutput": {
                        "hookEventName": "PostToolUse",
                        "additionalContext": message,
                    }
                }
            )
        )


def main() -> int:
    try:
        event = json.load(sys.stdin)
    except (json.JSONDecodeError, OSError):
        return 0
    if not isinstance(event, dict) or not pushed_successfully(event):
        return 0

    session_id = event.get("session_id")
    target = push_target(event)
    cwd_value = str(target[0]) if target else None
    if (
        not isinstance(session_id, str)
        or not session_id
        or not isinstance(cwd_value, str)
    ):
        return 0
    cwd = Path(cwd_value)

    try:
        sha = run(["git", "rev-parse", "HEAD"], cwd=cwd).stdout.strip()
        branch = run(
            ["git", "branch", "--show-current"], cwd=cwd, check=False
        ).stdout.strip()
        if target is None or not pushes_current_branch(target[1], branch):
            return 0
        if not branch_is_on_remote(cwd, sha, branch):
            return 0
        host, repo = forgejo_target(cwd)
        receipt = claim_watch(cwd, session_id, sha)
        if receipt is None:
            return 0
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        deliver(session_id, safe_error(error), cwd)
        return 0

    with receipt:
        try:
            outcome, runs = watch(
                cwd=cwd, host=host, repo=repo, sha=sha, started=receipt.started,
            )
            message = result_message(outcome=outcome, runs=runs, sha=sha, branch=branch)
        except (OSError, ValueError, subprocess.SubprocessError) as error:
            message = safe_error(error)
        # A crash after queueing but before marking reported is ambiguous. Never
        # recover that state automatically and risk duplicate user notifications.
        receipt.save(state="delivering")
        deliver(session_id, message, cwd)
        receipt.save(state="reported", message=message)
    return 0


def safe_error(error: Exception) -> str:
    # Exception strings can include URLs with embedded credentials or command
    # arguments. Report the failure category, never that untrusted text.
    return ("[CI watcher] Could not watch Forgejo CI after the push "
            f"({type(error).__name__}); inspect the hook configuration and Forgejo access.")


if __name__ == "__main__":
    raise SystemExit(main())
