#!/usr/bin/env python3
"""Lint the packages a push touches before its refs leave, natively and for the page.

Git runs `.githooks/pre-push` with one line per ref it is updating on stdin:

    <local ref> <local oid> <remote ref> <remote oid>

The workspace packages those commits touch are then linted the way CI lints
them: `cargo fmt --check`, Clippy over every target of the host, and Clippy for
`wasm32-unknown-unknown` on the touched packages that build for the page. A
change can compile on the host and still break the page - std's clocks and
files are not there - and pushes that had only been built natively broke main
that way repeatedly.

Only the touched packages are linted, and the Clippy runs start together, so a
one-crate push costs about one build. Cargo compiles wherever `$CARGO` points
when it is set - any cargo drop-in, which may build on another machine - and
otherwise the `cargo` on PATH. The wasm32 pass needs the target, so it is
skipped with a line saying so where the cargo in use cannot build it.

There is no switch to skip the lint: `git push --no-verify` is Git's own
escape, and CONTRIBUTING.md asks contributors never to use it.

    tools/ci/pre_push_lint.py --plan < pre-push-stdin    print what a push would lint
"""
from __future__ import annotations

import argparse
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from typing import Iterable, NamedTuple

TOOLS = Path(__file__).resolve().parents[1]
if str(TOOLS) not in sys.path:
    sys.path.append(str(TOOLS))
from ci.package_sets import wasm_crates
from ci.workspace_graph import cargo_metadata, owner_for, package_maps

WASM_TARGET = "wasm32-unknown-unknown"
# A changed file is Rust or Cargo when it can change what a package compiles.
RUST_SUFFIXES = (".rs", ".tl")
RUST_NAMES = ("Cargo.toml", "Cargo.lock", "build.rs")
# Files that steer the whole workspace's build: a change to one can break every
# crate, which is more than a lint of the touched ones can speak for.
WORKSPACE_INPUTS = {"Cargo.toml", "Cargo.lock"}
# How much of a failed run's output is shown before the rest is counted. The
# rerun command in the failure line prints all of it.
SHOWN_DIAGNOSTIC_LINES = 80
# Cargo's own remarks about the workspace, which every run prints whether it
# fails or not: a failure report is about the diagnostics the person has to fix.
CARGO_REMARKS = (
    "includes semver metadata",
    "found to be present in multiple build targets",
    "`bin` target `",
    "Blocking waiting for file lock on",
)
REF_TIPS = ("refs/remotes/{remote}/main", "refs/remotes/origin/main",
            "refs/remotes/origin/HEAD", "refs/heads/main")
ZERO_OID = {"0"}


def say(message: str) -> None:
    """Say something to the person pushing, on stderr where Git shows it in order."""
    print(message, file=sys.stderr, flush=True)


def cargo_command() -> list[str]:
    """The cargo that builds: any drop-in named in $CARGO (which may build on another machine), else cargo."""
    return shlex.split(os.environ.get("CARGO", "")) or ["cargo"]


def wasm_target_installed() -> bool:
    """Whether the toolchain here has the page's target, so the wasm32 pass can run."""
    if shutil.which("rustup") is None:
        return False
    installed = subprocess.run(["rustup", "target", "list", "--installed"],
                               capture_output=True, text=True, check=False)
    return installed.returncode == 0 and WASM_TARGET in installed.stdout.split()


class Lint(NamedTuple):
    """One lint run of the push gate: what it is called, and the command that reruns it."""
    label: str
    command: list[str]


def git(root: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(["git", *args], cwd=root, capture_output=True, text=True, check=False)


def known(root: Path, revision: str) -> bool:
    return git(root, "cat-file", "-e", revision).returncode == 0


def diff(root: Path, base: str, head: str) -> set[str]:
    """The files that differ between two commits, both sides of a move included."""
    result = subprocess.run(
        ["git", "diff", "--no-renames", "--name-only", "-z", "--diff-filter=ACDMRTUXB",
         f"{base}..{head}", "--"], cwd=root, capture_output=True, check=False,
    )
    return {path for path in result.stdout.decode("utf-8", "surrogateescape").split("\0") if path}


def start_of(root: Path, remote: str, head: str) -> str | None:
    """The commit a ref being created started from: its merge base with main.

    A branch push says nothing about where the branch began, so the merge base
    with the branch it will be reviewed against is what the push adds.
    """
    for tip in (pattern.format(remote=remote) for pattern in REF_TIPS):
        if not known(root, tip):
            continue
        merged = git(root, "merge-base", tip, head)
        if merged.returncode == 0 and merged.stdout.strip():
            return merged.stdout.strip()
    return None


def unreferenced(root: Path, head: str) -> set[str]:
    """The files of the pushed commits no remote has yet: the last resort for a branch start."""
    commits = git(root, "rev-list", head, "--not", "--remotes")
    if commits.returncode != 0:
        return set()
    files: set[str] = set()
    for commit in commits.stdout.split():
        result = subprocess.run(
            ["git", "diff-tree", "-r", "--root", "--no-commit-id", "--no-renames", "--name-only",
             "-z", commit], cwd=root, capture_output=True, check=False,
        )
        files |= {path for path in result.stdout.decode("utf-8", "surrogateescape").split("\0") if path}
    return files


def changed_files(root: Path, remote: str, remote_oid: str, head: str, ref: str) -> set[str] | None:
    """What a pushed ref changes, or None when there is nothing to compare it with."""
    if set(remote_oid) != ZERO_OID and known(root, remote_oid):
        return diff(root, remote_oid, head)
    start = start_of(root, remote, head)
    if start:
        return diff(root, start, head)
    files = unreferenced(root, head)
    if files:
        return files
    say(f"pre-push: cannot tell what {ref} adds (no main to compare it with); "
        "the lint is skipped for this push")
    return None


def pushed(root: Path, remote: str, lines: Iterable[str]) -> set[str]:
    """Every file the pushed refs change."""
    files: set[str] = set()
    for line in lines:
        parts = line.split()
        if len(parts) != 4:
            continue
        ref, head, _remote_ref, remote_oid = parts
        if set(head) == ZERO_OID:
            continue  # a deleted ref changes nothing that is built
        changed = changed_files(root, remote, remote_oid, head, ref)
        if changed is not None:
            files |= changed
    return files


def rust_changes(files: set[str]) -> set[str]:
    return {path for path in files if path.endswith(RUST_SUFFIXES) or Path(path).name in RUST_NAMES}


def touched(root: Path, files: set[str]) -> tuple[list[str], list[str]]:
    """The packages owning the changed files, and which of them build for the page."""
    metadata = cargo_metadata(root)
    packages, by_dir = package_maps(metadata, root)
    owners = {owner_for(path, by_dir) for path in files} & set(packages)
    web = wasm_crates(packages)
    return sorted(owners), sorted(owners & web)


def lint_runs(native: list[str], web: list[str], formatting: bool) -> list[Lint]:
    """The runs the gate makes: formatting, the host lint, and the page lint where it can run.

    `web` is what is actually linted for the page, so a toolchain without the
    target leaves it empty. Each command is also the line a failure prints to
    rerun it.
    """
    cargo = cargo_command()
    selected = [item for name in native for item in ("-p", name)]
    runs = []
    if formatting:
        runs.append(Lint("rustfmt", [*cargo, "fmt", "--check", *selected]))
    runs.append(Lint("clippy (native)",
                     [*cargo, "clippy", "--locked", *selected, "--all-targets", "--", "-D", "warnings"]))
    if web:
        page = [item for name in web for item in ("-p", name)]
        runs.append(Lint("clippy (wasm32)", [
            *cargo, "clippy", "--locked", "--target", WASM_TARGET, *page, "--", "-D", "warnings",
        ]))
    return runs


def rustfmt_available() -> bool:
    return subprocess.run([*cargo_command(), "fmt", "--version"], capture_output=True,
                          check=False).returncode == 0


def run_together(root: Path, runs: list[Lint]) -> tuple[list[tuple[Lint, int, str]], float]:
    """Start every run at once and collect them, so the wall time is the slowest one."""
    started = time.monotonic()
    running = []
    for run in runs:
        # A file, not a pipe: a run whose output outgrows the pipe buffer must
        # not wait on a reader that is still waiting on another run.
        log = tempfile.TemporaryFile(mode="w+")
        process = subprocess.Popen(run.command, cwd=root, stdout=log, stderr=subprocess.STDOUT, text=True)
        running.append((run, process, log))
    results = []
    for run, process, log in running:
        status = process.wait()
        log.seek(0)
        results.append((run, status, log.read()))
        log.close()
    return results, time.monotonic() - started


def packages(count: int) -> str:
    return f"{count} package" if count == 1 else f"{count} packages"


def report(results: list[tuple[Lint, int, str]], elapsed: float, checked: str) -> int:
    failed = [(run, output) for run, status, output in results if status != 0]
    if not failed:
        say(f"pre-push: lint passed in {elapsed:.0f}s ({checked})")
        return 0
    for run, output in failed:
        say(f"pre-push: {run.label} failed - rerun: {' '.join(run.command)}")
        lines = [line for line in output.strip().splitlines()
                 if not any(remark in line for remark in CARGO_REMARKS)]
        for line in lines[:SHOWN_DIAGNOSTIC_LINES]:
            say(f"  {line}")
        if len(lines) > SHOWN_DIAGNOSTIC_LINES:
            say(f"  ... {len(lines) - SHOWN_DIAGNOSTIC_LINES} more line(s); the rerun above prints them")
    say("pre-push: refusing the push until the lint passes")
    return 1


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("remote", nargs="?", default="origin", help="the remote being pushed to")
    parser.add_argument("--plan", action="store_true", help="print what the push would lint, and lint nothing")
    parser.add_argument("--stdin", type=argparse.FileType("r"), default=sys.stdin,
                        help="the ref lines Git writes for a pre-push hook")
    options = parser.parse_args(argv)
    root = Path(git(Path.cwd(), "rev-parse", "--show-toplevel").stdout.strip())
    files = rust_changes(pushed(root, options.remote, options.stdin))
    if not files:
        say("pre-push: no Rust or Cargo file changed; nothing to lint")
        return 0
    try:
        native, web = touched(root, files)
    except (subprocess.CalledProcessError, RuntimeError) as error:
        say(f"pre-push: cargo metadata could not name the packages this push touches, so the lint is "
            f"skipped: {error}")
        return 0
    if not native:
        say("pre-push: the changed Rust files belong to no workspace package (a workspace manifest or "
            "the lock); CI builds the whole workspace for that")
        return 0
    if options.plan:
        print(f"native: {' '.join(native)}")
        print(f"wasm32: {' '.join(web)}")
        return 0
    if workspace_wide := sorted(files & WORKSPACE_INPUTS):
        say(f"pre-push: {', '.join(workspace_wide)} changed too: only the touched packages are linted "
            "here, CI builds the whole workspace")
    if shutil.which(cargo_command()[0]) is None:
        say(f"pre-push: there is no cargo to lint with ({cargo_command()[0]}); the lint is skipped for "
            "this push")
        return 0
    if web and not (os.environ.get("CARGO", "").strip() or wasm_target_installed()):
        say(f"pre-push: {WASM_TARGET} is not installed here, so the page build is not linted "
            f"(rustup target add {WASM_TARGET} adds it)")
        web = []
    if git(root, "status", "--porcelain").stdout.strip():
        say("pre-push: the worktree has uncommitted changes; the lint covers the files on disk, which "
            "is not quite the pushed commit")
    formatting = rustfmt_available()
    if not formatting:
        say("pre-push: rustfmt is not installed, so formatting is not checked here")
    page = f", {len(web)} of them for the page" if web else ""
    say(f"pre-push: linting {packages(len(native))}{page}")
    runs = lint_runs(native, web, formatting)
    checked = f"{'clippy and rustfmt' if formatting else 'clippy'} for {packages(len(native))}{page}"
    return report(*run_together(root, runs), checked)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
