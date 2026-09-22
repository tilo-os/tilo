#!/usr/bin/env python3
"""Run at most one trusted routine-PR merge worker per invocation."""

from __future__ import annotations

import argparse
from contextlib import AbstractContextManager
from datetime import datetime, timezone
import importlib.util
import json
import os
from pathlib import Path
import secrets
import subprocess
import sys

RECONCILABLE = {"merge_intent_do_not_retry", "not_verified_do_not_retry"}
TERMINAL = {"rejected", "merged_verified", "dry_run_ready"}
LEASE_BUSY = 0
UNCERTAIN = 3
WORKER = Path(__file__).resolve().with_name("merge_pr.py")
WORKER_SPEC = importlib.util.spec_from_file_location("tilo_merge_pr", WORKER)
if WORKER_SPEC is None or WORKER_SPEC.loader is None:
    raise ImportError(f"cannot load trusted merge worker: {WORKER}")
merge_pr = importlib.util.module_from_spec(WORKER_SPEC)
WORKER_SPEC.loader.exec_module(merge_pr)


def event(name, **fields):
    """Write one bounded, credential-free controller event."""
    print(json.dumps({"event": name, **fields}, sort_keys=True), file=sys.stderr, flush=True)


def uncertain(stage, **fields):
    event("uncertain", stage=stage, **fields)
    return UNCERTAIN


class LeaseUnavailable(Exception):
    """Another dispatcher process owns the lease."""


class ProcessLease(AbstractContextManager):
    """An operating-system lease released automatically when the process exits."""

    def __init__(self, path: Path):
        self.path = path
        self.stream = None

    def __enter__(self):
        self.stream = self.path.open("a+b", buffering=0)
        try:
            if os.name == "nt":
                import msvcrt

                if self.path.stat().st_size == 0:
                    self.stream.write(b"\0")
                    os.fsync(self.stream.fileno())
                self.stream.seek(0)
                msvcrt.locking(self.stream.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                import fcntl

                fcntl.flock(self.stream.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except (OSError, BlockingIOError) as error:
            self.stream.close()
            self.stream = None
            raise LeaseUnavailable from error
        return self

    def __exit__(self, exc_type, exc_value, traceback):
        if self.stream is not None:
            if os.name == "nt":
                import msvcrt

                self.stream.seek(0)
                msvcrt.locking(self.stream.fileno(), msvcrt.LK_UNLCK, 1)
            else:
                import fcntl

                fcntl.flock(self.stream.fileno(), fcntl.LOCK_UN)
            self.stream.close()
        return False


def journals(state_dir: Path):
    records = []
    for path in sorted(state_dir.glob("merge-*.json")):
        try:
            record = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, UnicodeError, json.JSONDecodeError) as error:
            raise ValueError(f"unreadable journal: {path.name}") from error
        if (not isinstance(record, dict) or record.get("schema") != 1
                or record.get("repo") != merge_pr.REPO
                or type(record.get("pr")) is not int or record["pr"] <= 0):
            raise ValueError(f"invalid journal: {path.name}")
        if record.get("state") in TERMINAL | RECONCILABLE:
            if not merge_pr.SHA.fullmatch(record.get("head", "")):
                raise ValueError(f"invalid journal: {path.name}")
        records.append((path, record))
    return records


def run_worker(root: Path, journal: Path, number: int, *, reconcile=False):
    command = [
        sys.executable,
        "-I",
        str(WORKER),
        "--pr",
        str(number),
        "--git-root",
        str(root),
        "--journal",
        str(journal),
        "--reconcile" if reconcile else "--execute",
    ]
    return subprocess.run(command, check=False, timeout=180).returncode


def fetch_candidate(root: Path, number: int, listed_head: str):
    ref = f"refs/tilo-integration/candidates/{number}"
    try:
        cached = merge_pr.git(root, "rev-parse", "--verify", ref).decode().strip()
        if cached == listed_head:
            merge_pr.git(root, "cat-file", "-e", listed_head + "^{commit}")
            return True
    except subprocess.SubprocessError:
        pass
    try:
        merge_pr.git(
            root,
            "fetch",
            "--force",
            "--no-tags",
            "origin",
            f"refs/pull/{number}/head:{ref}",
        )
        fetched = merge_pr.git(root, "rev-parse", ref).decode().strip()
    except subprocess.SubprocessError:
        return False
    return fetched == listed_head


def refresh_main(api, root: Path):
    """Move a clean trusted checkout to the exact main commit reported by Forgejo."""
    try:
        if merge_pr.git(root, "status", "--porcelain", "--untracked-files=all").strip():
            return False
        _, branch = api(f"/branches/{merge_pr.BRANCH}")
        observed = branch.get("commit", {}).get("id", "")
        if not merge_pr.SHA.fullmatch(observed):
            return False
        if merge_pr.git(root, "rev-parse", "HEAD").decode().strip() == observed:
            return True
        merge_pr.git(
            root,
            "fetch",
            "--force",
            "--no-tags",
            "origin",
            f"refs/heads/{merge_pr.BRANCH}:{merge_pr.FETCHED_MAIN_REF}",
        )
        fetched = merge_pr.git(root, "rev-parse", merge_pr.FETCHED_MAIN_REF).decode().strip()
        if fetched != observed:
            return False
        merge_pr.git(root, "reset", "--hard", merge_pr.FETCHED_MAIN_REF)
        return (merge_pr.git(root, "rev-parse", "HEAD").decode().strip() == observed
                and not merge_pr.git(
                    root, "status", "--porcelain", "--untracked-files=all"
                ).strip())
    except (OSError, subprocess.SubprocessError, TypeError, ValueError):
        return False


def journal_path(state_dir: Path, number: int, head: str):
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
    nonce = secrets.token_hex(8)
    return state_dir / f"merge-{number}-{head[:12]}-{stamp}-{nonce}.json"


def dispatch(api, root: Path, state_dir: Path, worker=run_worker, fetch=fetch_candidate,
             refresh=refresh_main, classify=merge_pr.routine_prefilter_errors):
    try:
        with ProcessLease(state_dir / "dispatcher.lock"):
            try:
                existing = journals(state_dir)
            except ValueError:
                return uncertain("journal-read")

            unresolved = [(path, record) for path, record in existing
                          if record.get("state") in RECONCILABLE]
            unknown = [(path, record) for path, record in existing
                       if record.get("state") not in TERMINAL | RECONCILABLE]
            if unknown or len(unresolved) > 1:
                return uncertain("journal-state", unknown=len(unknown), unresolved=len(unresolved))
            if unresolved:
                path, record = unresolved[0]
                number = record.get("pr")
                head = record.get("head")
                if type(number) is not int or not merge_pr.SHA.fullmatch(head or ""):
                    return uncertain("journal-identity")
                return worker(root, path, number, reconcile=True)

            if not refresh(api, root):
                return uncertain("refresh-main")

            try:
                base = merge_pr.git(root, "rev-parse", "HEAD").decode().strip()
            except (OSError, subprocess.SubprocessError, UnicodeError):
                return uncertain("read-main")

            recorded_heads = {record.get("head") for _, record in existing
                              if merge_pr.SHA.fullmatch(record.get("head", ""))}
            try:
                open_prs = merge_pr.pages(api, "/pulls?state=open")
            except (OSError, TypeError, ValueError):
                return uncertain("list-pulls")

            candidates = []
            seen_numbers = set()
            for candidate in open_prs:
                if not isinstance(candidate, dict) or not isinstance(candidate.get("head"), dict):
                    return uncertain("pull-record")
                number = candidate.get("number")
                listed_head = candidate.get("head", {}).get("sha", "")
                if (type(number) is not int or number <= 0 or number in seen_numbers
                        or not merge_pr.SHA.fullmatch(listed_head)):
                    return uncertain("pull-record")
                seen_numbers.add(number)
                candidates.append((number, listed_head))

            fetched = routine = 0
            for number, listed_head in sorted(candidates):
                if listed_head in recorded_heads:
                    continue
                if not fetch(root, number, listed_head):
                    return uncertain("fetch-candidate", pr=number)
                fetched += 1
                try:
                    if classify(root, base, listed_head):
                        continue
                except (OSError, subprocess.SubprocessError, TypeError, ValueError):
                    return uncertain("classify-candidate", pr=number)
                routine += 1
                try:
                    observation = merge_pr.observe(api, number, root)
                except (OSError, subprocess.SubprocessError, TypeError, ValueError):
                    return uncertain("observe-candidate", pr=number)
                if (observation.get("head") != listed_head or observation.get("errors")):
                    continue
                path = journal_path(state_dir, number, listed_head)
                event("selected", pr=number, head=listed_head)
                return worker(root, path, number)
            try:
                _, final_branch = api(f"/branches/{merge_pr.BRANCH}")
                final_base = final_branch.get("commit", {}).get("id", "")
            except (OSError, TypeError, ValueError):
                return uncertain("recheck-main")
            if final_base != base:
                return uncertain("main-changed")
            event("scan", open=len(candidates), fetched=fetched, routine=routine)
            return 0
    except LeaseUnavailable:
        return LEASE_BUSY


def main(argv=None, environ=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--git-root", type=Path, required=True)
    parser.add_argument("--state-dir", type=Path, required=True)
    parser.add_argument("--execute", action="store_true")
    args = parser.parse_args(argv)
    token = merge_pr.bound_token(environ)
    root = args.git_root.resolve()
    state_dir = args.state_dir.resolve()
    if not args.execute or not token or not state_dir.is_dir():
        parser.error("--execute, bound FORGEJO_TOKEN and an existing state directory are required")
    return dispatch(merge_pr.Api(token), root, state_dir)


if __name__ == "__main__":
    raise SystemExit(main())
