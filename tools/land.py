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

The pre-push hook refuses a push to main from anywhere but the lander's
worktree, so nothing races its lint. Main still moves under it when the forge
merges a pull request: the lander's push is refused then like anyone's, and it
puts its changes on the new head and lints again.

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
import re
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time

# Changes put on main and linted together in one round. A lint of a widely used crate
# reaches the whole workspace and takes many minutes on a loaded builder while main
# gets a change a minute, so a round takes everything that queued meanwhile: one
# lint for all of it. A lint that fails is retried on the first half, so a bad
# change costs a few rounds, not everyone's.
BATCH = 24
# How long the lander stays up with nothing queued before it stops.
IDLE_SECONDS = 120
# How long the command waits for its answer before it says to run it again; under
# the ten minutes an agent's tool call may take.
DEFAULT_WAIT = 540
# How much of a failed lint is handed back; the lander's log keeps all of it.
SHOWN_LINES = 120
# A failed lint that names no diagnostic, file or package says nothing about the changes: the
# machine that compiles was away, or failed after compiling (a container it could not save).
# The round is tried again, a few times, rather than answered.
UNEXPLAINED_RETRIES = 3
# Rounds whose whole lint output is kept, to tell where a round's minutes went.
KEPT_LINT_LOGS = 40
UNEXPLAINED_RETRY_SECONDS = 30
# How a failed lint names what broke: the packages that did not build, and the files a
# diagnostic points at or the format check found unformatted.
NOT_BUILT = re.compile(r"could not compile `([^`]+)`")
POINTED_AT = re.compile(r"(?:-->|Diff in) (\S+?):\d+:")
# The command a failed run of the push lint prints to run it again.
RERUN = re.compile(r"^pre-push: (.+?) failed - rerun: (.+)$", re.MULTILINE)
# A diagnostic and the file it is about, in rustc's and Clippy's rendering.
DIAGNOSTIC = re.compile(r"^(?:error|warning)(?:\[[^\]]+\])?: (.+)$")
LOCATION = re.compile(r"^\s*--> (\S+?):\d+:\d+")
NOT_A_DIAGNOSTIC = ("could not compile", "aborting due to", "build failed")
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
        return main_checkout(self.repo)

    def worktree(self) -> Path:
        return lander_worktree(self.repo, self.branch)


def main_checkout(repo: Path) -> Path:
    """The clone's main checkout: the one worktree no one removes when their work is done."""
    listing = out(repo, "worktree", "list", "--porcelain").splitlines()
    return Path(listing[0].split(" ", 1)[1])


def lander_worktree(repo: Path, branch: str) -> Path:
    """The lander's own worktree, beside the main checkout's other worktrees.

    The pre-push hook lets only a push from here onto main (tools/ci/pre_push_lint.py).
    """
    return main_checkout(repo) / ".claude" / "worktrees" / f"land-{branch.replace('/', '-')}"


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
    """Start a detached lander from a copy of the branch's own land.py, else of this script.

    The branch's copy, in the lander's worktree, is the same for every submitter: worktrees
    branched off at different times would otherwise each start their own version. A copy, so
    a worktree removed later cannot pull it away.
    """
    if lander_running(place):
        return
    program = place.dir / "lander.py"
    partial = place.dir / f"lander.{os.getpid()}.partial"
    branch_copy = place.worktree() / "tools" / "land.py"
    shutil.copyfile(branch_copy if branch_copy.is_file() else __file__, partial)
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
        # Changes a failed lint points at: each is linted alone, after the others.
        # Kept beside the queue, so a lander that takes up a newer one keeps them.
        self.suspects: set[str] = set((read_json(place.dir / "suspects.json") or {}).get("ids", []))
        self.unexplained = 0

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
        self.keep_suspects(self.suspects - {entry["id"]})
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
        self.keep_suspects(self.suspects & {entry["id"] for entry in entries})
        # The changes nothing points at go first; the ones a failure pointed at go together after
        # them, and each failure of theirs narrows them again.
        innocent = [entry for entry in entries if entry["id"] not in self.suspects]
        entries = innocent or entries
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
        lint = self.lint(upstream)
        seconds = now() - started
        output = (lint.stdout + lint.stderr).strip()
        print(output, flush=True)
        self.place.event(kind="lint", ok=lint.returncode == 0, seconds=seconds, batch=len(batch),
                         onto=upstream, status=lint.returncode)
        explained = bool(NOT_BUILT.search(output) or POINTED_AT.search(output)
                         or any(DIAGNOSTIC.match(line.strip()) and not any(
                             words in line for words in NOT_A_DIAGNOSTIC)
                                for line in output.splitlines()))
        if lint.returncode != 0 and not explained and self.unexplained < UNEXPLAINED_RETRIES:
            self.unexplained += 1
            self.doing(f"the lint failed without a diagnostic (the builder?); linting again in "
                       f"{UNEXPLAINED_RETRY_SECONDS}s")
            time.sleep(UNEXPLAINED_RETRY_SECONDS)
            return True
        self.unexplained = 0
        inherited = False
        if lint.returncode != 0:
            # Setting aside the changes the failure points at costs nothing; comparing with the
            # branch's head alone costs two more lints, so it is for a failure no change explains.
            if self.set_aside(batch, upstream, output):
                return True
            inherited = self.inherited(len(batch), upstream, output)
            if not inherited:
                self.failed(batch, upstream, output)
                return True
        self.doing(f"pushing {len(batch)} change(s) onto {upstream[:12]}")
        # The hook lints again and refuses on the branch's own failures, which the lander has just
        # shown the batch adds nothing to; Git's own switch is the only way past it.
        verify = ["--no-verify"] if inherited else []
        push = git(self.tree, "push", "--quiet", *verify, self.place.remote,
                   f"HEAD:refs/heads/{self.place.branch}")
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

    def lint(self, upstream: str) -> subprocess.CompletedProcess:
        """The push lint of what the worktree adds to `upstream`, in a process group of its own.

        A lander stopped mid-lint (by a newer one taking its place) stops the lint's compiles
        too, rather than leave them loading the builder for minutes, for nobody.
        """
        process = subprocess.Popen(
            [sys.executable, "-I", "tools/ci/pre_push_lint.py", "--range", f"{upstream}..HEAD",
             "--keep-logs", str(self.logs())],
            cwd=self.tree, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            start_new_session=True,
        )

        def stop(signum, _frame):
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            sys.exit(128 + signum)

        previous = signal.signal(signal.SIGTERM, stop)
        try:
            stdout, stderr = process.communicate()
        finally:
            signal.signal(signal.SIGTERM, previous)
        return subprocess.CompletedProcess(process.args, process.returncode, stdout, stderr)

    def logs(self) -> Path:
        """Where this round's lint keeps the whole output of each of its runs; the last few rounds stay."""
        directory = self.place.dir / "lint-logs"
        directory.mkdir(exist_ok=True)
        rounds = sorted(directory.iterdir())
        for old in rounds[:-KEPT_LINT_LOGS]:
            shutil.rmtree(old, ignore_errors=True)
        return directory / time.strftime("%Y%m%d-%H%M%S")

    def inherited(self, size: int, upstream: str, output: str) -> bool:
        """Whether every diagnostic of the failed lint is one the branch's head has without the batch.

        A lint reaches the packages built on what a change touched, and when the branch itself
        does not build there, every change that reaches them fails, whoever broke it. The failed
        Clippy runs are made again on the batch and on the head alone; when the batch adds no
        diagnostic, it is not what is broken. A failed format check is always the batch's: it
        reads only the files the batch changed.
        """
        reruns = RERUN.findall(output)
        if not reruns or any(not label.startswith("clippy") for label, _command in reruns):
            return False
        batch_head = out(self.tree, "rev-parse", "HEAD")
        self.doing(f"the lint of {size} change(s) failed; comparing it with {upstream[:12]} alone")
        mine = self.diagnostics(reruns)
        try:
            out(self.tree, "checkout", "--quiet", "--detach", upstream)
            theirs = self.diagnostics(reruns)
        finally:
            out(self.tree, "checkout", "--quiet", "--detach", batch_head)
        added = mine - theirs
        self.place.event(kind="inherited", onto=upstream, batch=size, head=len(theirs),
                         added=len(added))
        print(f"diagnostics with the batch {len(mine)}, on {upstream[:12]} alone {len(theirs)}, "
              f"added by the batch {len(added)}", flush=True)
        for message, path in sorted(added)[:20]:
            print(f"  added: {path}: {message}", flush=True)
        return bool(mine) and not added

    def diagnostics(self, reruns: list[tuple[str, str]]) -> set[tuple[str, str]]:
        """The diagnostics the push lint's Clippy runs report on the tree checked out, by message and file."""
        found: set[tuple[str, str]] = set()
        page = self.tree / "target" / "wasm-clippy"
        for _label, command in reruns:
            environment = dict(os.environ)
            if "wasm32" in command and (page / "clippy.toml").is_file():
                environment["CLIPPY_CONF_DIR"] = str(page)
            result = subprocess.run(command, shell=True, cwd=self.tree, env=environment,
                                 capture_output=True, text=True)
            message = None
            before = len(found)
            for line in (result.stdout + result.stderr).splitlines():
                diagnostic = DIAGNOSTIC.match(line)
                if diagnostic:
                    text = diagnostic.group(1)
                    message = None if any(words in text for words in NOT_A_DIAGNOSTIC) else text
                    continue
                location = LOCATION.match(line)
                if location and message:
                    found.add((message, location.group(1)))
                    message = None
            if result.returncode != 0 and len(found) == before:
                tail = (result.stdout + result.stderr).strip().splitlines()[-15:]
                print(f"a failed lint named no diagnostic (exit {result.returncode}): {command[:200]}",
                      *tail, sep="\n  ", flush=True)
        return found

    def moved(self, upstream: str) -> bool:
        try:
            return self.place.fetch(self.place.repo) != upstream
        except RuntimeError:
            return False

    def keep_suspects(self, suspects: set[str]) -> None:
        if suspects != self.suspects:
            self.suspects = suspects
            write_json(self.place.dir / "suspects.json", {"ids": sorted(suspects)})

    def set_aside(self, batch: list[tuple[dict, str]], upstream: str, output: str) -> bool:
        """Set aside the changes a failed lint of several points at; whether there were any."""
        if len(batch) < 2:
            return False
        blamed = self.blamed(batch, upstream, output)
        if not blamed or len(blamed) == len(batch):
            return False
        # The others are linted without them next, then they are, together.
        self.keep_suspects((self.suspects - {entry["id"] for entry, _commit in batch}) | blamed)
        self.place.event(kind="set-aside", ids=sorted(blamed), batch=len(batch))
        return True

    def failed(self, batch: list[tuple[dict, str]], upstream: str, output: str) -> None:
        if len(batch) > 1:
            # Which change broke it is not known: the next round tries the first half.
            self.limit = max(1, len(batch) // 2)
            return
        lines = output.splitlines()
        shown = lines[-SHOWN_LINES:] if len(lines) > SHOWN_LINES else lines
        self.finish(batch[0][0], {"status": "lint", "onto": upstream, "output": shown})

    def blamed(self, batch: list[tuple[dict, str]], upstream: str, output: str) -> set[str]:
        """The changes that touch a file the failed lint points at, or a package it could not build."""
        tree = self.tree.resolve()
        pointed = set()
        for path in POINTED_AT.findall(output):
            resolved = Path(path) if Path(path).is_absolute() else tree / path
            try:
                pointed.add(str(resolved.resolve().relative_to(tree)))
            except ValueError:
                continue
        failing = set(NOT_BUILT.findall(output))
        directories = self.directories(failing) if failing else []
        changed = {}
        previous = upstream
        for entry, commit in batch:
            changed[entry["id"]] = set(out(self.tree, "diff", "--name-only", previous, commit).splitlines())
            previous = commit
        # A change to the very file a diagnostic is about is the likeliest cause; a package that
        # did not build is touched by many changes, so it only decides when no file does.
        exact = {identity for identity, files in changed.items() if pointed & files}
        if exact:
            return exact
        return {identity for identity, files in changed.items()
                if any(path.startswith(directory) for path in files for directory in directories)}

    def directories(self, packages: set[str]) -> list[str]:
        """The repository directories of these packages."""
        try:
            metadata = json.loads(subprocess.run(
                ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
                cwd=self.tree, capture_output=True, text=True, check=True).stdout)
        except (OSError, subprocess.CalledProcessError, ValueError):
            return []
        tree = self.tree.resolve()
        return [str(Path(package["manifest_path"]).resolve().parent.relative_to(tree)) + "/"
                for package in metadata.get("packages", []) if package["name"] in packages]

    def prune(self) -> None:
        """Answers nobody came back for within a day."""
        for path in self.place.results.glob("*.json"):
            try:
                if now() - path.stat().st_mtime > 86400:
                    path.unlink(missing_ok=True)
            except FileNotFoundError:
                continue

    def upgrade(self) -> None:
        """Become the lander the branch itself holds now, so a busy lander still takes its fixes.

        A lander only stops once nothing is queued, which with a change a minute may be never.
        """
        fresh = self.tree / "tools" / "land.py"
        try:
            code = fresh.read_bytes()
            if code == Path(__file__).read_bytes():
                return
        except OSError:
            return
        program = self.place.dir / "lander.py"
        partial = self.place.dir / f"lander.{os.getpid()}.partial"
        partial.write_bytes(code)
        partial.replace(program)
        self.doing("taking up the lander on the branch's head")
        # The lock's descriptor closes on exec; the new lander takes it again.
        os.execv(sys.executable, [sys.executable, "-I", str(program), *sys.argv[1:]])

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
                self.upgrade()
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
