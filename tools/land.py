#!/usr/bin/env python3
"""Land this branch's commits on main through one queue, linted on the tree they land on.

    python3 tools/land.py            queue HEAD's commits, wait for the answer
    python3 tools/land.py status     what is queued, and what the lander is doing

Many people and agents push to main at once, and the push lint takes minutes:
a push that lints, finds main moved, rebases and lints again loses the same race
again. Here every worktree of this clone hands its commits to one lander
instead. The lander takes everything queued, puts it on main's newest head one
change after another, lints the combined range with the push gate
(tools/ci/pre_push_lint.py --range) and pushes it, so one lint lands several
changes, and nobody races. The push itself goes through the pre-push hook as any
push does; it reuses the lint that just passed on the same tree.

The answer is one of:

    landed     the commits are on main; the line names the commit
    conflict   they do not apply on main's head; the line names the files,
               which are never resolved for you: rebase onto main and land again
    lint       the push lint failed on the tree they would land on; its
               diagnostics follow
    error      something else stopped it (a missing commit, a merge commit)

and the exit status says the same: 0, 2, 1, 3. Waiting is bounded (--wait); a
wait that runs out leaves the commits queued (exit 75) and running the command
again waits for the same answer. When the change lands and HEAD is still the
commit queued, the branch is moved onto the landed head, as a rebase onto main
would.

A plain `git push` keeps working: the lander's push is refused like anyone's
when main moved under it, and it puts its changes on the new head and lints
again.

The queue and the lander's log live in the clone's Git directory
(`land/<branch>/`), the lander's own worktree beside the main checkout's other
worktrees (.claude/worktrees/land-<branch>). The lander runs detached and
stops when the queue has been empty for a while.
"""
from __future__ import annotations

import argparse
import fcntl
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time

# Changes put on main and linted together in one round. A lint that fails is
# retried on the first half, so a bad change costs a few rounds, not everyone's.
BATCH = 8
# How long the lander stays up with nothing queued before it stops.
IDLE_SECONDS = 120
# How long the command waits for its answer before it says to run it again; under
# the ten minutes an agent's tool call may take.
DEFAULT_WAIT = 540
# How much of a failed lint is handed back; the lander's log keeps all of it.
SHOWN_LINES = 120
# A lint that failed because the machine that compiles was not there says nothing about the
# changes: the round is tried again rather than answered. rcargo says so in these words.
NO_BUILDER = ("remote builder unavailable",)
NO_BUILDER_RETRY_SECONDS = 30
EXIT = {"landed": 0, "lint": 1, "conflict": 2, "error": 3}
STILL_QUEUED = 75


def say(message: str) -> None:
    print(message, file=sys.stderr, flush=True)


def git(cwd: Path, *args: str, check: bool = False) -> subprocess.CompletedProcess:
    result = subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True)
    if check and result.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)}: {result.stderr.strip()}")
    return result


def out(cwd: Path, *args: str) -> str:
    return git(cwd, *args, check=True).stdout.strip()


def now() -> float:
    return time.time()


class Place:
    """Where one branch's queue lives: shared by every worktree of the clone."""

    def __init__(self, repo: Path, remote: str, branch: str):
        self.repo = repo
        self.remote = remote
        self.branch = branch
        common = Path(out(repo, "rev-parse", "--path-format=absolute", "--git-common-dir"))
        self.common = common
        self.dir = common / "land" / branch.replace("/", "%")
        self.queue = self.dir / "queue"
        self.results = self.dir / "results"
        self.lock = self.dir / "lock"
        self.log = self.dir / "lander.log"
        self.events = self.dir / "events.jsonl"
        self.state = self.dir / "state.json"
        for directory in (self.queue, self.results):
            directory.mkdir(parents=True, exist_ok=True)

    @property
    def tracking(self) -> str:
        return f"refs/remotes/{self.remote}/{self.branch}"

    def fetch(self, cwd: Path) -> str:
        """Fetch the branch's head into its remote-tracking ref; retried, since every worktree fetches."""
        spec = f"+refs/heads/{self.branch}:{self.tracking}"
        for attempt in range(6):
            result = git(cwd, "fetch", "--quiet", "--no-tags", self.remote, spec)
            if result.returncode == 0:
                return out(cwd, "rev-parse", self.tracking)
            time.sleep(1 + attempt)
        raise RuntimeError(f"git fetch {self.remote} {self.branch}: {result.stderr.strip()}")

    def event(self, **fields) -> None:
        with self.events.open("a") as events:
            events.write(json.dumps({"at": now(), **fields}) + "\n")

    def main_checkout(self) -> Path:
        """The clone's main checkout: the one worktree no one removes when their work is done."""
        listing = out(self.repo, "worktree", "list", "--porcelain").splitlines()
        return Path(listing[0].split(" ", 1)[1])

    def worktree(self) -> Path:
        """The lander's own worktree, beside the main checkout's other worktrees."""
        return self.main_checkout() / ".claude" / "worktrees" / f"land-{self.branch.replace('/', '-')}"


def write_json(path: Path, value: dict) -> None:
    partial = path.with_suffix(f".{os.getpid()}.partial")
    partial.write_text(json.dumps(value, indent=1) + "\n")
    partial.replace(path)


def read_json(path: Path) -> dict | None:
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return None


# The command: queue, wait, answer.

def queued_commits(repo: Path, upstream: str) -> list[str]:
    """HEAD's commits the branch has not got, oldest first, leaving out ones already applied there."""
    lines = out(repo, "cherry", upstream, "HEAD").splitlines()
    return [line[2:] for line in lines if line.startswith("+ ")]


def lander_running(place: Place) -> bool:
    with place.lock.open("a") as handle:
        try:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return True
        fcntl.flock(handle, fcntl.LOCK_UN)
        return False


def start_lander(place: Place) -> None:
    """Start a detached lander from a copy of this script, so a worktree removed later cannot pull it away."""
    if lander_running(place):
        return
    program = place.dir / "lander.py"
    partial = place.dir / f"lander.{os.getpid()}.partial"
    shutil.copyfile(__file__, partial)
    partial.replace(program)
    with place.log.open("a") as log:
        subprocess.Popen(
            [sys.executable, "-I", str(program), "--serve", "--remote", place.remote,
             "--branch", place.branch, "--repo", str(place.main_checkout())],
            cwd=place.main_checkout(), stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True,
        )


def answer(result: dict, as_json: bool) -> int:
    if as_json:
        print(json.dumps(result))
        return EXIT.get(result["status"], 3)
    status = result["status"]
    if status == "landed":
        say(f"land: landed as {result['commit'][:12]} on {result['branch']} "
            f"({result['waited']:.0f}s after it was queued, with {result['batch']} change(s) in one lint)")
    elif status == "conflict":
        say(f"land: does not apply on {result['branch']}'s head {result['onto'][:12]}; "
            f"conflicting files:")
        for path in result["files"]:
            say(f"  {path}")
        say(f"land: nothing was resolved for you: rebase onto {result['branch']}, fix them, land again")
    elif status == "lint":
        say(f"land: the push lint failed on the tree this would land on "
            f"({result['branch']} {result['onto'][:12]} with this change):")
        for line in result["output"]:
            say(f"  {line}")
        say("land: fix it, commit, land again")
    else:
        say(f"land: {result['detail']}")
    return EXIT.get(status, 3)


def submit(options) -> int:
    repo = Path(out(Path.cwd(), "rev-parse", "--show-toplevel"))
    place = Place(repo, options.remote, options.branch)
    head = out(repo, "rev-parse", "HEAD")
    identity = head[:20]
    result = read_json(place.results / f"{identity}.json")
    if result is None and not (place.queue / f"{identity}.json").exists():
        upstream = place.fetch(repo)
        if git(repo, "status", "--porcelain", "--untracked-files=no").stdout.strip():
            say("land: uncommitted changes stay here; only commits land")
        merges = out(repo, "rev-list", "--merges", f"{upstream}..HEAD")
        if merges:
            return answer({"status": "error", "detail": "land puts a linear series of commits on "
                           f"{options.branch}; {merges.split()[0][:12]} is a merge: rebase onto "
                           f"{options.remote}/{options.branch} and land again"}, options.json)
        commits = queued_commits(repo, upstream)
        if not commits:
            return answer({"status": "landed", "commit": upstream, "branch": options.branch,
                           "waited": 0, "batch": 0, "detail": "already there"}, options.json)
        write_json(place.queue / f"{identity}.json", {
            "id": identity, "head": head, "commits": commits, "worktree": str(repo),
            "submitted": now(),
        })
        place.event(kind="queued", id=identity, commits=len(commits))
        say(f"land: queued {len(commits)} commit(s) for {options.branch}")
    deadline = now() + options.wait
    shown = None
    while result is None:
        start_lander(place)
        if options.no_wait:
            say("land: queued; `python3 tools/land.py` again waits for the answer")
            return STILL_QUEUED
        state = read_json(place.state) or {}
        line = state.get("doing")
        if line and line != shown and not options.json:
            say(f"land: {line}")
            shown = line
        if now() > deadline:
            say("land: still queued; run the same command again to keep waiting for this answer")
            return STILL_QUEUED
        time.sleep(2)
        result = read_json(place.results / f"{identity}.json")
    # An answer is given once: landing the same commit again after a conflict or a failed lint
    # (main may have moved past what caused it) queues it again.
    (place.results / f"{identity}.json").unlink(missing_ok=True)
    if result["status"] == "landed" and result.get("commit"):
        follow(repo, head, result["commit"], options)
    return answer(result, options.json)


def follow(repo: Path, head: str, landed: str, options) -> None:
    """Move the branch onto the landed head when it still is the commit that was queued."""
    if out(repo, "rev-parse", "HEAD") != head:
        return
    if git(repo, "cat-file", "-e", landed).returncode != 0:
        git(repo, "fetch", "--quiet", "--no-tags", options.remote,
            f"+refs/heads/{options.branch}:refs/remotes/{options.remote}/{options.branch}")
    moved = git(repo, "reset", "--quiet", "--keep", landed)
    if moved.returncode == 0:
        say(f"land: this branch now points at {landed[:12]}")


def status(options) -> int:
    repo = Path(out(Path.cwd(), "rev-parse", "--show-toplevel"))
    place = Place(repo, options.remote, options.branch)
    entries = sorted((read_json(path) or {} for path in place.queue.glob("*.json")),
                     key=lambda entry: entry.get("submitted", 0))
    state = read_json(place.state) or {}
    report = {"lander": lander_running(place), "doing": state.get("doing"),
              "queued": [{"id": entry.get("id"), "worktree": entry.get("worktree"),
                          "commits": len(entry.get("commits", [])),
                          "waiting": round(now() - entry.get("submitted", now()))}
                         for entry in entries]}
    if options.json:
        print(json.dumps(report))
        return 0
    print(f"lander: {'running' if report['lander'] else 'not running'}"
          f"{' - ' + report['doing'] if report['lander'] and report['doing'] else ''}")
    for entry in report["queued"]:
        print(f"  {entry['id'][:12]}  {entry['commits']} commit(s), {entry['waiting']}s  {entry['worktree']}")
    if not report["queued"]:
        print("  nothing queued")
    print(f"log: {place.log}")
    return 0


# The lander.

class Lander:
    def __init__(self, place: Place):
        self.place = place
        self.tree = place.worktree()
        # After a failed lint the next round tries the first half of what failed.
        self.limit = BATCH

    def doing(self, line: str) -> None:
        print(time.strftime("%H:%M:%S ") + line, flush=True)
        write_json(self.place.state, {"doing": line, "pid": os.getpid(), "at": now()})

    def prepare(self, upstream: str) -> None:
        if not (self.tree / ".git").exists():
            self.tree.parent.mkdir(parents=True, exist_ok=True)
            git(self.place.repo, "worktree", "prune")
            out(self.place.repo, "worktree", "add", "--quiet", "--detach", str(self.tree), upstream)
        git(self.tree, "cherry-pick", "--abort")
        out(self.tree, "checkout", "--quiet", "--detach", "--force", upstream)
        out(self.tree, "reset", "--quiet", "--hard", upstream)
        # Keeps target/: the lint's builds stay warm from one round to the next.
        git(self.tree, "clean", "-fdq")

    def entries(self) -> list[dict]:
        found = []
        for path in self.place.queue.glob("*.json"):
            entry = read_json(path)
            if entry:
                found.append(entry)
        return sorted(found, key=lambda entry: entry["submitted"])

    def finish(self, entry: dict, result: dict) -> None:
        result = {"id": entry["id"], "branch": self.place.branch,
                  "waited": now() - entry["submitted"], **result}
        write_json(self.place.results / f"{entry['id']}.json", result)
        (self.place.queue / f"{entry['id']}.json").unlink(missing_ok=True)
        self.place.event(kind=result["status"], id=entry["id"], waited=result["waited"],
                         batch=result.get("batch"))

    def apply(self, entry: dict) -> list[str] | None:
        """Put an entry's commits on the worktree's head; the conflicting files when they do not apply."""
        for commit in entry["commits"]:
            # A commit already on the branch under another hash puts nothing on it.
            picked = git(self.tree, "cherry-pick", "--empty=drop", commit)
            if picked.returncode != 0:
                files = out(self.tree, "diff", "--name-only", "--diff-filter=U").splitlines()
                git(self.tree, "cherry-pick", "--abort")
                return files or [f"(commit {commit[:12]} did not apply: {picked.stderr.strip()[:200]})"]
        return None

    def round(self) -> bool:
        """One round: put what is queued on the head, lint it once, push it. False when nothing is queued."""
        entries = self.entries()
        if not entries:
            return False
        upstream = self.place.fetch(self.place.repo)
        self.prepare(upstream)
        batch: list[tuple[dict, str]] = []
        for entry in entries:
            if len(batch) >= self.limit:
                break
            missing = [commit for commit in entry["commits"]
                       if git(self.tree, "cat-file", "-e", commit).returncode != 0]
            if missing:
                self.finish(entry, {"status": "error", "detail": f"commit {missing[0][:12]} is gone"})
                continue
            before = out(self.tree, "rev-parse", "HEAD")
            files = self.apply(entry)
            if files is None:
                batch.append((entry, out(self.tree, "rev-parse", "HEAD")))
            elif not batch:
                self.finish(entry, {"status": "conflict", "files": files, "onto": upstream})
            else:
                # It may only conflict with a change ahead of it in this round, which may not
                # land: it waits for the next round, on whatever head that gives.
                out(self.tree, "reset", "--quiet", "--hard", before)
        if not batch:
            return True
        names = " ".join(entry["id"][:8] for entry, _ in batch)
        self.doing(f"linting {len(batch)} change(s) on {upstream[:12]}: {names}")
        started = now()
        lint = subprocess.run(
            [sys.executable, "-I", "tools/ci/pre_push_lint.py", "--range", f"{upstream}..HEAD"],
            cwd=self.tree, capture_output=True, text=True,
        )
        seconds = now() - started
        output = (lint.stdout + lint.stderr).strip()
        print(output, flush=True)
        self.place.event(kind="lint", ok=lint.returncode == 0, seconds=seconds, batch=len(batch),
                         onto=upstream)
        if lint.returncode != 0 and any(words in output for words in NO_BUILDER):
            self.doing(f"the builder was unavailable; linting again in {NO_BUILDER_RETRY_SECONDS}s")
            time.sleep(NO_BUILDER_RETRY_SECONDS)
            return True
        if lint.returncode != 0:
            self.failed(batch, upstream, output)
            return True
        self.doing(f"pushing {len(batch)} change(s) onto {upstream[:12]}")
        push = git(self.tree, "push", "--quiet", self.place.remote, f"HEAD:refs/heads/{self.place.branch}")
        self.place.event(kind="push", ok=push.returncode == 0, batch=len(batch),
                         detail=push.stderr.strip()[-400:])
        if push.returncode != 0:
            print(push.stderr, flush=True)
            if self.moved(upstream):
                # Someone pushed past the queue: the next round puts the same changes on their head.
                self.doing("the branch moved during the lint; again on its new head")
                return True
            self.failed(batch, upstream, push.stderr.strip())
            return True
        self.limit = BATCH
        for entry, commit in batch:
            self.finish(entry, {"status": "landed", "commit": commit, "onto": upstream,
                                "batch": len(batch), "lint_seconds": seconds})
        return True

    def moved(self, upstream: str) -> bool:
        try:
            return self.place.fetch(self.place.repo) != upstream
        except RuntimeError:
            return False

    def failed(self, batch: list[tuple[dict, str]], upstream: str, output: str) -> None:
        if len(batch) > 1:
            # Which change broke it is not known yet: the next round tries the first half.
            self.limit = max(1, len(batch) // 2)
            return
        lines = output.splitlines()
        shown = lines[-SHOWN_LINES:] if len(lines) > SHOWN_LINES else lines
        self.finish(batch[0][0], {"status": "lint", "onto": upstream, "output": shown})

    def prune(self) -> None:
        """Answers nobody came back for within a day."""
        for path in self.place.results.glob("*.json"):
            try:
                if now() - path.stat().st_mtime > 86400:
                    path.unlink(missing_ok=True)
            except FileNotFoundError:
                continue

    def serve(self) -> None:
        self.prune()
        idle_since = now()
        while True:
            try:
                busy = self.round()
            except Exception as error:  # a lander that dies leaves every waiter waiting
                print(f"round failed: {error}", flush=True)
                self.doing(f"a round failed ({error}); trying again")
                time.sleep(10)
                busy = True
            if busy:
                idle_since = now()
                continue
            if now() - idle_since > IDLE_SECONDS:
                return
            self.doing("idle")
            time.sleep(2)


def serve(options) -> int:
    place = Place(Path(options.repo), options.remote, options.branch)
    while True:
        with place.lock.open("a") as handle:
            try:
                fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                return 0
            Lander(place).serve()
            fcntl.flock(handle, fcntl.LOCK_UN)
        # Something queued while this lander was deciding to stop is taken by it, or by the
        # lander its submitter started once the lock was free.
        if not any(place.queue.glob("*.json")):
            write_json(place.state, {"doing": None, "at": now()})
            return 0


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", nargs="?", choices=["status"], help="show the queue instead of landing")
    parser.add_argument("--branch", default="main", help="the branch to land on (default: main)")
    parser.add_argument("--remote", default="origin")
    parser.add_argument("--wait", type=float, default=DEFAULT_WAIT,
                        help=f"seconds to wait for the answer (default {DEFAULT_WAIT})")
    parser.add_argument("--no-wait", action="store_true", help="queue and return at once")
    parser.add_argument("--json", action="store_true", help="the answer as one JSON object on stdout")
    parser.add_argument("--serve", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--repo", help=argparse.SUPPRESS)
    options = parser.parse_args(argv)
    if options.serve:
        return serve(options)
    try:
        return status(options) if options.command == "status" else submit(options)
    except RuntimeError as error:
        return answer({"status": "error", "detail": str(error)}, options.json)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
