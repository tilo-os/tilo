#!/usr/bin/env python3
"""Queue one compact Forgejo CI result after a Codex-initiated git push."""

from __future__ import annotations

import datetime as dt
import hashlib
import json
import os
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
VISIBLE_STATUSES = TERMINAL_STATUSES | {"pending", "queued", "running", "waiting", "unknown"}
SAFE_LABEL = re.compile(r"[A-Za-z0-9._/-]{1,128}")
SAFE_SESSION_ID = re.compile(r"[A-Za-z0-9_-]{1,128}")
MAX_MESSAGE_CHARS = 4_000


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


def watch_directory(cwd: Path) -> Path:
    common = run(["git", "rev-parse", "--git-common-dir"], cwd=cwd).stdout.strip()
    return (cwd / common).resolve() / "codex-ci-watch"


def claim_watch(
    directory: Path, *, cwd: Path, session_id: str, sha: str
) -> WatchReceipt | None:
    key = hashlib.sha256(f"{session_id}:{sha}".encode()).hexdigest()
    return acquire(directory / f"{key}.json", cwd=cwd, session_id=session_id, sha=sha)


def configured_pushes_current_branch(
    cwd: Path, arguments: list[str], branch: str
) -> bool:
    if arguments not in ([], ["origin"]):
        return pushes_current_branch(arguments, branch)
    result = run(
        ["git", "rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{push}"],
        cwd=cwd,
        check=False,
    )
    return result.returncode == 0 and result.stdout.strip() == f"origin/{branch}"


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
        parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
        return parsed if parsed.tzinfo is not None else None
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
        if created is not None and created >= earliest:
            runs.append(item)
    return runs


def run_key(item: dict[str, Any]) -> str:
    value = item.get("index_in_repo") or item.get("id")
    if type(value) is int and value >= 0:
        return str(value)
    if isinstance(value, str) and len(value) <= 20 and value.isascii() and value.isdigit():
        return value
    return "?"


def terminal(item: dict[str, Any]) -> bool:
    return str(item.get("status", "")).lower() in TERMINAL_STATUSES


def watch(
    *, cwd: Path, host: str, repo: str, sha: str, started: dt.datetime
) -> tuple[str, list[dict[str, Any]]]:
    began = time.monotonic()
    discovery_deadline = began + DISCOVERY_TIMEOUT_SECONDS
    deadline = began + WATCH_TIMEOUT_SECONDS
    last_snapshot: set[tuple[str, str]] = set()
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
        snapshot = {
            (run_key(item), str(item.get("status", "")).lower()) for item in latest
        }
        if snapshot != last_snapshot:
            last_snapshot = snapshot
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
    candidate = item.get("workflow_id") or item.get("title")
    workflow = str(candidate) if isinstance(candidate, (str, int)) else "workflow"
    if SAFE_LABEL.fullmatch(workflow) is None:
        workflow = "workflow"
    number = run_key(item)
    status = str(item.get("status") or "unknown").lower()
    if status not in VISIBLE_STATUSES:
        status = "unknown"
    return f"{workflow} #{number}: {status}"


def bounded_message(message: str) -> str:
    if len(message) <= MAX_MESSAGE_CHARS:
        return message
    suffix = " [truncated]"
    return message[: MAX_MESSAGE_CHARS - len(suffix)] + suffix


def result_message(
    *, outcome: str, runs: list[dict[str, Any]], sha: str, branch: str
) -> str:
    short = sha[:12]
    display_branch = branch if SAFE_LABEL.fullmatch(branch) else "branch"
    target = f"{display_branch} ({short})" if branch else short
    if outcome == "missing":
        return bounded_message(
            f"[CI watcher] No Forgejo Actions runs appeared for push {target} within 90 seconds."
        )

    summary = "; ".join(describe_run(item) for item in sorted(runs, key=run_key))
    if outcome == "timeout":
        return bounded_message(
            f"[CI watcher] Timed out waiting for CI for {target}. Latest status: {summary or 'unknown'}."
        )

    failed = [
        item
        for item in runs
        if str(item.get("status", "")).lower() not in GREEN_STATUSES
    ]
    if not failed:
        return bounded_message(
            f"[CI watcher] CI for {target} is green: {summary}. No action is needed."
        )

    numbers = ", ".join(run_key(item) for item in failed)
    return bounded_message(
        f"[CI watcher] CI for {target} finished with failures: {summary}. "
        f"Inspect failed output with `fgj actions run view <run> --log-failed` "
        f"(failed run{'s' if len(failed) != 1 else ''}: {numbers})."
    )


def deliver(session_id: str, message: str, cwd: Path) -> bool:
    try:
        run(
            ["codex", "queue", "--thread", session_id, "--message", message],
            cwd=cwd,
            timeout=30,
        )
        return True
    except (OSError, subprocess.SubprocessError):
        return False


def spawn_worker(
    *,
    session_id: str,
    cwd: Path,
    sha: str,
    branch: str,
    host: str,
    repo: str,
    receipt_directory: Path,
) -> None:
    command = [
        sys.executable,
        str(Path(__file__).resolve()),
        "--worker",
        session_id,
        str(cwd),
        sha,
        branch,
        host,
        repo,
        str(receipt_directory),
    ]
    options: dict[str, Any] = {
        "cwd": cwd,
        "stdin": subprocess.DEVNULL,
        "stdout": subprocess.DEVNULL,
        "stderr": subprocess.DEVNULL,
        "close_fds": True,
    }
    if os.name == "nt":
        options["creationflags"] = (
            subprocess.CREATE_NEW_PROCESS_GROUP | subprocess.DETACHED_PROCESS
        )
    else:
        options["start_new_session"] = True
    subprocess.Popen(command, **options)


def launch() -> int:
    try:
        event = json.load(sys.stdin)
    except (json.JSONDecodeError, OSError):
        return 0
    if not isinstance(event, dict):
        return 0

    target = push_target(event)
    if target is None:
        return 0
    session_id = event.get("session_id")
    if not isinstance(session_id, str) or SAFE_SESSION_ID.fullmatch(session_id) is None:
        return 0
    cwd = target[0]

    try:
        sha = run(["git", "rev-parse", "HEAD"], cwd=cwd).stdout.strip()
        branch = run(
            ["git", "branch", "--show-current"], cwd=cwd, check=False
        ).stdout.strip()
        if not configured_pushes_current_branch(cwd, target[1], branch):
            return 0
        host, repo = forgejo_target(cwd)
        directory = watch_directory(cwd)
        spawn_worker(
            session_id=session_id,
            cwd=cwd,
            sha=sha,
            branch=branch,
            host=host,
            repo=repo,
            receipt_directory=directory,
        )
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        deliver(session_id, safe_error(error), cwd)
    return 0


def worker(
    *,
    session_id: str,
    cwd: Path,
    sha: str,
    branch: str,
    host: str,
    repo: str,
    receipt_directory: Path,
) -> int:
    try:
        if not branch_is_on_remote(cwd, sha, branch):
            return 0
        receipt = claim_watch(
            receipt_directory, cwd=cwd, session_id=session_id, sha=sha
        )
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
        if deliver(session_id, message, cwd):
            receipt.save(state="reported", message=message)
        else:
            receipt.save(state="delivery_failed", message=message)
    return 0


def main() -> int:
    if len(sys.argv) == 9 and sys.argv[1] == "--worker":
        return worker(
            session_id=sys.argv[2],
            cwd=Path(sys.argv[3]),
            sha=sys.argv[4],
            branch=sys.argv[5],
            host=sys.argv[6],
            repo=sys.argv[7],
            receipt_directory=Path(sys.argv[8]),
        )
    if len(sys.argv) != 1:
        return 0
    return launch()


def safe_error(error: Exception) -> str:
    # Exception strings can include URLs with embedded credentials or command
    # arguments. Report the failure category, never that untrusted text.
    return ("[CI watcher] Could not watch Forgejo CI after the push "
            f"({type(error).__name__}); inspect the hook configuration and Forgejo access.")


if __name__ == "__main__":
    raise SystemExit(main())
