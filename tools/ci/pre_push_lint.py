#!/usr/bin/env python3
"""Lint what a push can break before its refs leave.

Git runs `.githooks/pre-push` with one line per ref it is updating on stdin:

    <local ref> <local oid> <remote ref> <remote oid>

Every pushed ref is linted for what it adds. A push to main is compared with main's head,
which it must contain: everyone lands on main directly, rebased onto it, and nobody
force-pushes it. A push to another branch is compared with the branch's old head when it
builds on it; after a force-push, with the merge-base of the old head and the new one; and a
new branch with its merge-base with the remote's main (`refs/remotes/<remote>/main`, which a
fetch keeps). A deleted ref changes nothing that is built.

A push lints the packages its commits own: the changed files through the
repository's own format check, and Clippy over every target of the host with the
features the workspace's unified build gives each package (so code behind a
feature that no default build enables is compiled).

A package that builds directly on one of those through a path dependency is compiled
too, a dev-dependency's test included: a type a push changes breaks the package that
constructs it while the package that declares it compiles clean. A dependant is compiled,
never linted: only a compile error there refuses the push. Its lints are its own authors'
and main's, and a push that refused on them refused for code it never touched, which sent
pushes out with `--no-verify` and left their own lints unchecked. A changed package the page
family names is also linted for wasm32, a target the host lint cannot see. A push that changes
a Cargo manifest or the lockfile runs the policy step that keeps the page's wasm32 builds off
the workspace-hack: an ungated dependency there compiles the native feature set into every page
build, and only the page clippy job surfaced it, after the push's own lint passed.
A push that changes only an app's published surface - its sources, its surface file, the
catalogue entry the desktop reads the fleet from - has no package to lint, and runs t's checks
over the app's committed files and the generated SDK types on their own: they are committed, and
an app change that does not regenerate them lands stale for the next person to fix.
The crates that only unify features are neither a source nor a dependant: their
dependency lists name members they hold no code for. Only the direct dependants are
named; what a change reaches through them, and the packages as they ship, is t's
`clippy`, `clippy-{arch}`, `clippy-idle`, `clippy-shipped` and `clippy-page`
families' work, on every change: each is keyed by what it reads, and a miss is named
on the head it broke.

Where t is installed, the lint is t's own per-package checks of the families
that build with those unified features (`clippy/<package>`, `clippy-idle/...`,
`clippy-<arch>/...`: the ones whose `unify` names workspace-unify in t.toml), and
`clippy-page/<package>` for a changed package the page family names, and the
dependants are compiled together in one `t cargo check`: one build of them all
shares the crates they build on, where a check of each rebuilt them in a job of its
own. t keys each check by what that package's build reads, its dependencies included, and
answers a key it has seen from its result cache, whoever ran it: after a rebase
onto commits that reach none of what a package reads, the push is answered in
seconds, and a package main's new commits did reach, through a dependency too,
is linted again. Without t, or with `$CARGO` naming a cargo - any cargo drop-in,
which may build on another machine - that cargo runs Clippy itself, with the
same unified features. The packages are named by the cargo `$CARGO` names, else
the `cargo` on PATH, else t's (`t cargo metadata`). Formatting is checked by the
`rustfmt` on PATH, which must be the version the toolchain pins, or by t's `fmt`
check where there is none.

A failure is not the push's own when main has it at the push's base. t compares each failed
check's findings and tests one by one against the base's verdict of the same key, so a check
that is red on main for one lint does not hide a second, new one here. What t cannot place -
the base has no stored verdict, or the two failures cannot be told apart - is judged against
main's board at that base: the item failing since a commit the base contains, a failed test by
name, a finding one by one against main's own log. A check's build lints the workspace packages
it builds on, so its failure can name a finding in a dependency - a session's check failing on
the runtime it builds on, with the file nowhere in the push. That red is main's when the board
holds it at the base under the dependency's own check or the family's run, compared finding by
finding; where that log is gone, the board's red at the base stands for a finding whose file
the push does not change. A check that generates committed files (the fleet catalogue)
fails naming the files not current, so it is judged by what its generator writes: the generator
runs on the base tree and on this one, and a file whose generated content differs between the
two is the push's to commit, while one it writes the same at both ends is main's. Where that
cannot be done - the check declares no fix, or t cannot run it - the files main's log names
judge the push, and the person pushing is told which check fell back and why. A dependants'
compile that fails in a package whose build reads nothing the push changes is main's too. A
push that passes on reds says which, and whose they are; a board that cannot be read refuses.

A lint that passed stands for the same change after a rebase that leaves it alone: a pass is
remembered by the push's own patch (`git patch-id`), the packages it lints with the files it
writes in each of their own directories, and the kinds of run its lint makes. A push retried
after main moved, whose rebase resolved nothing, has all three the same and is sent without
linting again, however long the lint took and however much main changed elsewhere, dependencies
of those packages included. What main decides about a run - the dependants its manifests name,
the features workspace-unify gives the packages, the checks `t scope` names, the commits a
format check compares - is not part of the key: a retry computes those again, against commits
the push never saw, and keying a pass by them made a push lint from scratch after every rebase.
What main's own commits did to a linted package's files does not enter the key either: t's
clippy families lint those interactions on main, and the next commit fixes a red there. What
only the combination breaks is t's clippy families' to find on main. A rebase that resolved a
conflict into another change writes another patch, so it is linted. The passes are kept in the
repository's git directory.

The lint reads the pushed commit's files, never the ones beside them: an untracked file or an
edit not committed is not sent, so it neither changes what is linted nor keeps the pass from being
remembered. Where the checkout holds such files, the lint checks the pushed commit out in the
repository's git directory and reads that, which is what t uploads and builds, and removes it
after; where it holds none, the lint reads the checkout itself, and a push standing on a
remembered pass checks nothing out. A push is refused when what it sends is not the commit
checked out here. A push whose changed packages the check cannot name is refused too, rather
than passed unread.

The catalogues a push changes are judged over its whole range before any lint,
by `tools/policy/check_catalogue_coverage.py --range`: the commit hook judges
each commit's own catalogues as it is made, and a cherry-pick or a rebase writes
commits it never runs for. A key the range adds to a source catalogue with no
line in a translation, a key it adds to a translation with no source original,
or a catalogue it leaves holding one key twice refuses the push. A tilolang
package's source catalogue is not a file: the check asks the tree's compiler
for the words the package's sources write at each end of the range.

There is no switch to skip the lint: `git push --no-verify` is Git's own
escape, and CONTRIBUTING.md asks contributors never to use it.

    tools/ci/pre_push_lint.py --plan < pre-push-stdin    print what a push would lint
    tools/ci/pre_push_lint.py --range BASE..HEAD         lint what that range changes
"""
from __future__ import annotations

import argparse
import datetime
import hashlib
import importlib.util
import json
import os
import platform
from pathlib import Path
import re
import shlex
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import tomllib
from typing import Callable, Iterable, NamedTuple

TOOLS = Path(__file__).resolve().parents[1]
if str(TOOLS) not in sys.path:
    sys.path.append(str(TOOLS))
from ci import suites
from ci.workspace_graph import file_at, lockfile_change, package_maps, reverse_dependencies

# A changed file is Rust or Cargo when it can change what a package compiles.
RUST_SUFFIXES = (".rs", ".tl")
RUST_NAMES = ("Cargo.toml", "Cargo.lock", "build.rs")
# Files that steer the whole workspace's build: a change to one can reach every
# member, so one the check cannot attribute refuses the push instead.
WORKSPACE_MANIFEST = "Cargo.toml"
WORKSPACE_LOCK = "Cargo.lock"
# The manifest that gives every member the features the workspace's whole build
# resolves for it (tools/ci/workspace_unify.py); a lint without it compiles code
# no other family's build leaves out of a default feature set.
UNIFY_MANIFEST = Path("tools/workspace-unify/Cargo.toml")
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
# Cargo's progress, one line per crate: a lint of the whole workspace prints hundreds before
# its first diagnostic, and a failure report cut to its first lines showed nothing else.
PROGRESS = re.compile(r"^\s*(?:Checking|Compiling|Fresh|Building|Blocking|Downloading|Downloaded"
                      r"|Locking|Updating|Adding|Documenting)\s")
# t's checks that judge generated, committed output: the fleet catalogue and the published
# crates' policy. Their failure names the files that are not current, and a check
# that declares a `fix` is run on the base tree and on this one: a file whose generated content
# differs between the two is the push's to commit, and one it writes the same at both ends is
# main's. A check with no fix, or a generator t cannot run, falls back to the files main's log
# names, and says so.
GENERATED_CHECKS = ("app-surfaces", "sdk-policy")
# How a generated-output check says a file is not current (`tilo sdk gen --check`,
# `tilolang -- surfaces`, `generate_catalogue_fleet.py --check`).
STALE_LINES = (re.compile(r"^\s*not current: (.+?)\s*$"),
               re.compile(r"^\s*(.+?) is stale(?::|$)"))
# Where a base commit's generated output is kept, under this worktree's own git directory: the
# base side of a comparison is paid for once per base, so a worktree pushing twice off one base
# runs its generator once for the base and once for each head, never twice for the base. Another
# worktree has its own directory, so no two share an entry.
GENERATED_CACHE = "tilo-pre-push-generated"

ZERO_OID = {"0"}
# Where the passes a later push may stand on are kept, in the repository's common git directory
# (shared by its worktrees), and how many of the newest are: one line each.
PASSES = "tilo-pre-push-passes"
PASSES_KEPT = 256
# The branch everyone lands on. A push to main that is not a fast-forward is a race already lost:
# nobody force-pushes main, so the remote refuses it after any lint. Another branch may be
# force-pushed, so its push is compared with where it forked instead.
MAIN_REF = "refs/heads/main"


def say(message: str) -> None:
    """Say something to the person pushing, on stderr where Git shows it in order."""
    print(message, file=sys.stderr, flush=True)


# t's cargo: what a machine without a cargo of its own reads the workspace with.
T_CARGO = ["t", "cargo"]
# t's configuration, whose cargo families name the per-package checks the lint asks t for.
T_TOML = "t.toml"
# The package whose manifest gives every member the features the workspace's whole build
# resolves for it: the clippy families that name it in `unify` lint with those features, which is
# the lint a push gets.
UNIFY = "workspace-unify"
# The family that lints the page's wasm32 build. It declares no `unify` - workspace-unify is a
# host crate that builds nothing for wasm32 - so it stands apart from the unified families.
PAGE_FAMILY = "clippy-page"
# The packages that only unify features, generated from the workspace's manifests. Their
# dependency lists name members they never use, so a change to one reaches no code and names no
# set of dependants, and their own lint says nothing about a member that changed.
SYNTHETIC = {"workspace-hack", "workspace-unify"}
# A target kind another package can link against; a package with none cannot be depended on.
LIBRARY_KINDS = {"lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"}
# What an architecture family's `{arch}` is on this machine.
ARCHITECTURES = {"x86_64": "x86_64", "amd64": "x86_64", "aarch64": "aarch64", "arm64": "aarch64"}


def cargo_command() -> list[str]:
    """The cargo that reads the workspace, and lints it where t does not.

    Any drop-in named in $CARGO (which may build on another machine), else the cargo on PATH, else
    t's, which reads it for a machine without a cargo of its own.
    """
    named = shlex.split(os.environ.get("CARGO", ""))
    if named:
        return named
    if shutil.which("cargo") is None and shutil.which(T_CARGO[0]) is not None:
        return T_CARGO
    return ["cargo"]


def lints_through_t() -> bool:
    """Whether t's own checks lint the push: t is installed and no `$CARGO` names a cargo to use."""
    return not os.environ.get("CARGO", "").strip() and shutil.which(T_CARGO[0]) is not None


def cargo_metadata(root: Path) -> dict:
    """The workspace's packages, from the cargo that lints them, with the paths of this checkout."""
    cargo = cargo_command()
    command = [*cargo, "metadata", "--locked", "--no-deps", "--format-version", "1"]
    try:
        result = subprocess.run(command, cwd=root, capture_output=True, text=True, check=False)
    except OSError as error:
        raise RuntimeError(f"{shlex.join(command)}: {error}") from error
    if result.returncode == 0:
        return json.loads(result.stdout)
    raise RuntimeError(f"{shlex.join(command)} failed: {result.stderr.strip()}")


BUILDER_LICENSES = Path("tools/sdk/check_builder_licenses.py")
LICENSE_POLICY = "tools/sdk/license-policy.toml"


def builder_license_findings(root: Path, metadata: dict) -> list[str]:
    """Each Tilo crate a published builder crate's manifests reach whose license LICENSING.md does
    not allow there, or that could not be published with it: t's builder-manifests check, so a
    push that would put AGPL or unpublishable code under tilo-ui, tilo-app or tilolang is refused
    before it lands. The third-party graph on every target stays t's builder-licenses check."""
    spec = importlib.util.spec_from_file_location("check_builder_licenses", root / BUILDER_LICENSES)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    policy = tomllib.loads((root / LICENSE_POLICY).read_text())
    reached = module.manifest_closure(metadata["packages"], policy["builder_roots"])
    return (module.manifest_findings(reached, frozenset(policy["allowed_expressions"]))
            + module.publishable_findings(reached))


def builder_licenses_range(root: Path, push: "Push") -> int:
    """Refuse a push whose manifests put a crate LICENSING.md does not allow under a builder
    crate; a push that changes no manifest and not the policy cannot, and is not asked, and
    neither is a workspace that publishes no builder crates (no policy names any)."""
    if not (root / LICENSE_POLICY).is_file():
        return 0
    if not any(Path(path).name == WORKSPACE_MANIFEST or path == LICENSE_POLICY for path in push.files):
        return 0
    try:
        findings = builder_license_findings(root, cargo_metadata(root))
    except (OSError, RuntimeError, ValueError, KeyError) as error:
        say(f"pre-push: cannot read the builder crates' licenses: {error}")
        return 1
    if not findings:
        return 0
    say("pre-push: refusing the push: a published builder crate would reach code LICENSING.md "
        "keeps AGPL, or a crate that cannot be published with it "
        "(python3 tools/sdk/check_builder_licenses.py --manifests):")
    say_lines(findings)
    return 1


class Lint(NamedTuple):
    """One lint run of the push check: what it is called and the command that reruns it.

    `t` marks a run of t's checks, whose output is t's JSON and is reported from it.
    """
    label: str
    command: list[str]
    t: bool = False


class Push(NamedTuple):
    """What the refs being pushed change, and what the check could not name about them."""
    files: set[str]
    comparisons: list[tuple[str, str]]
    tips: list[str]
    unnamed: list[str]
    # Pushes to main that main's own head, as the remote reported it when the push began, is not
    # part of: the remote refuses them whatever the lint says.
    behind: list[str] = []


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


def comparison(root: Path, remote_oid: str, head: str) -> tuple[str, set[str]] | None:
    """What a push to main changes from main's head, or None when that head is not here."""
    if set(remote_oid) != ZERO_OID and known(root, remote_oid):
        return remote_oid, diff(root, remote_oid, head)
    return None


def contains(root: Path, head: str, commit: str) -> bool:
    """Whether `commit` is `head` or one of its ancestors (False when it is not even here)."""
    return known(root, commit) and git(root, "merge-base", "--is-ancestor", commit, head).returncode == 0


def merge_base(root: Path, one: str, other: str) -> str | None:
    result = git(root, "merge-base", one, other)
    if result.returncode != 0:
        return None
    return result.stdout.strip() or None


def branch_start(root: Path, remote: str, remote_ref: str, head: str,
                 remote_oid: str) -> str | None:
    """Where a push to a branch other than main is compared from, said to the person pushing.

    The branch's old head when the push builds on it; after a force-push, where the old head and
    the new one forked; for a new branch, or an old head that is not here, where the head forked
    from the remote's main. None, said why, when there is nothing to compare with.
    """
    branch = remote_ref.removeprefix("refs/heads/")
    if set(remote_oid) != ZERO_OID and known(root, remote_oid):
        if contains(root, head, remote_oid):
            say(f"pre-push: linting {branch} against {branch}'s {remote_oid[:12]}")
            return remote_oid
        if (fork := merge_base(root, remote_oid, head)) is not None:
            say(f"pre-push: {branch} is force-pushed; linting it against its merge-base with "
                f"{branch}'s {remote_oid[:12]}: {fork[:12]}")
            return fork
    main = f"refs/remotes/{remote}/main"
    tip = rev(root, main)
    if tip is None:
        say(f"pre-push: {branch} is new here and there is no {main} to find where it forked, so "
            f"nothing of it is linted; fetch {remote} and push again to lint it")
        return None
    fork = merge_base(root, tip, head)
    if fork is None:
        say(f"pre-push: {branch} shares no history with {main}, so nothing of it is linted")
        return None
    say(f"pre-push: linting {branch} against its merge-base with main {fork[:12]}")
    return fork


def pushed(root: Path, lines: Iterable[str], remote: str = "origin") -> Push:
    """Every file the pushed refs change, and the comparison each is linted with."""
    files: set[str] = set()
    comparisons: list[tuple[str, str]] = []
    tips: list[str] = []
    unnamed: list[str] = []
    behind: list[str] = []
    for line in lines:
        parts = line.split()
        if len(parts) != 4:
            continue
        ref, head, remote_ref, remote_oid = parts
        if set(head) == ZERO_OID:
            say(f"pre-push: {remote_ref.removeprefix('refs/heads/')} is deleted, which changes "
                "nothing that is built")
            continue
        if remote_ref != MAIN_REF:
            start = branch_start(root, remote, remote_ref, head, remote_oid)
            if start is not None:
                comparisons.append((start, head))
                tips.append(head)
                files |= diff(root, start, head)
            continue
        if set(remote_oid) != ZERO_OID and not contains(root, head, remote_oid):
            behind.append(remote_oid)
            continue
        result = comparison(root, remote_oid, head)
        if result is None:
            unnamed.append(ref)
            say(f"pre-push: main's head {remote_oid[:12]} is not here, so what {ref} adds to it "
                "cannot be named; fetch and push again")
            continue
        start, changed = result
        comparisons.append((start, head))
        tips.append(head)
        files |= changed
    return Push(files, comparisons, tips, unnamed, behind)


def rust_changes(files: set[str]) -> set[str]:
    return {path for path in files if path.endswith(RUST_SUFFIXES) or Path(path).name in RUST_NAMES}


def rev(root: Path, revision: str) -> str | None:
    result = git(root, "rev-parse", "--verify", "--quiet", revision)
    return result.stdout.strip() or None


def unchecked_tree(root: Path, push: Push) -> str | None:
    """The pushed commit whose tree is not the one checked out here, if any.

    The lint reads the files on disk. A ref whose tree they are not would be
    sent unlinted, and a pass recorded for it would be reused for a tree nobody
    read.
    """
    checkout = rev(root, "HEAD^{tree}")
    for tip in push.tips:
        if rev(root, f"{tip}^{{tree}}") != checkout:
            return tip
    return None


def owner_of(path: str, by_directory: dict[str, str]) -> str | None:
    """The package owning a repository path: the nearest directory above it that is one.

    owner_for walks every directory of the workspace for each path; walking up
    from the path instead is what a search that matched thousands of files can
    afford.
    """
    candidate = path
    while candidate:
        owner = by_directory.get(candidate)
        if owner is not None:
            return owner
        parent = candidate.rsplit("/", 1)[0] if "/" in candidate else ""
        candidate = parent
    return by_directory.get("")


def lock_seeds(root: Path, push: Push, members: set[str]) -> set[str] | None:
    """The members a Cargo.lock change reaches, or None when the change cannot be attributed."""
    seeds: set[str] = set()
    for base, head in push.comparisons:
        before = file_at(root, base, WORKSPACE_LOCK)
        after = file_at(root, head, WORKSPACE_LOCK)
        if before is None or after is None:
            return None
        change = lockfile_change(before, after, members)
        if change is None:
            return None
        seeds |= change.seeds & members
    return seeds


def profile_seeds(before: object, after: object, packages: dict[str, dict]) -> set[str] | None:
    """The members a profile change reaches, or None when it reaches all of them.

    A setting at a profile's own level (`[profile.release]`) decides how every
    member of that profile is built; one under `package.<name>` (`[profile.
    release.package.tilo-auth]`) decides only how that member is. A section
    naming a package no longer in the workspace has no build to reach.
    """
    if not isinstance(before, dict) or not isinstance(after, dict):
        return None
    seeds: set[str] = set()
    for name in before.keys() | after.keys():
        old, new = before.get(name), after.get(name)
        if old == new:
            continue
        if not isinstance(old, dict) or not isinstance(new, dict):
            return None
        if {key: value for key, value in old.items() if key != "package"} != {
                key: value for key, value in new.items() if key != "package"}:
            return None
        old_packages, new_packages = old.get("package", {}), new.get("package", {})
        if not isinstance(old_packages, dict) or not isinstance(new_packages, dict):
            return None
        seeds |= {package for package in old_packages.keys() | new_packages.keys()
                  if old_packages.get(package) != new_packages.get(package)
                  and package in packages}
    return seeds


def manifest_seeds(root: Path, push: Push, metadata: dict) -> set[str] | None:
    """The members a root Cargo.toml change reaches, or None when it reaches all of them."""
    packages = {package["name"]: package for package in metadata["packages"]}
    by_dir = {Path(package["manifest_path"]).parent.resolve(): name
              for name, package in packages.items()}
    seeds: set[str] = set()
    for base, head in push.comparisons:
        before, after = file_at(root, base, WORKSPACE_MANIFEST), file_at(root, head, WORKSPACE_MANIFEST)
        if before is None or after is None:
            return None
        try:
            old, new = tomllib.loads(before.decode("utf-8")), tomllib.loads(after.decode("utf-8"))
        except (UnicodeDecodeError, tomllib.TOMLDecodeError):
            return None
        # A change outside the workspace table and the profiles - the lint
        # table, a patch - decides how every member is built.
        if {key: value for key, value in old.items() if key not in ("workspace", "profile")} != {
                key: value for key, value in new.items() if key not in ("workspace", "profile")}:
            return None
        reached = profile_seeds(old.get("profile", {}), new.get("profile", {}), packages)
        if reached is None:
            return None
        seeds |= reached
        old_workspace, new_workspace = old.get("workspace", {}), new.get("workspace", {})
        if not isinstance(old_workspace, dict) or not isinstance(new_workspace, dict):
            return None
        changed = {key for key in old_workspace.keys() | new_workspace.keys()
                   if old_workspace.get(key) != new_workspace.get(key)}
        if not changed <= {"members", "dependencies"}:
            return None
        if "members" in changed:
            added = set(new_workspace.get("members", ())) - set(old_workspace.get("members", ()))
            for member in added:
                owner = by_dir.get((root / member).resolve())
                if owner is None:
                    return None
                seeds.add(owner)
        if "dependencies" in changed:
            old_shared = old_workspace.get("dependencies", {})
            new_shared = new_workspace.get("dependencies", {})
            names: set[str] = set()
            for key in {key for key in old_shared.keys() | new_shared.keys()
                        if old_shared.get(key) != new_shared.get(key)}:
                names.add(key)
                for value in (old_shared.get(key), new_shared.get(key)):
                    if isinstance(value, dict) and value.get("package"):
                        names.add(value["package"])
            seeds |= {name for name, package in packages.items()
                      if any(dependency.get("name") in names or dependency.get("rename") in names
                             for dependency in package.get("dependencies", []))}
    return seeds


class Selection(NamedTuple):
    """What a push is linted as: the packages, the metadata they were named from, and which of
    them no commit of the push changed - the ones selected because they build on a changed one."""
    native: list[str]
    metadata: dict
    dependants: list[str] = []

    @property
    def linted(self) -> list[str]:
        """The packages the push changes: the ones it lints. A dependant is only compiled."""
        built_on = set(self.dependants)
        return [name for name in self.native if name not in built_on]


def reading_packages(root: Path, paths: list[str]) -> list[str]:
    """The packages whose own build reads one of these repository files (tools/ci/suites.toml).

    A tilolang app or a shipped asset is compiled into a package that holds no
    Cargo package of its own, so the package's own files say nothing about it.
    """
    try:
        return suites.select_packages(suites.load(root), paths)
    except (OSError, ValueError, KeyError) as error:
        say(f"pre-push: cannot read which packages read these files: {error}")
        return []


def whole_workspace(members: set[str], metadata: dict) -> Selection:
    """Every member: what a change that reaches how all of them build is linted as."""
    return Selection(sorted(members), metadata)


def library(package: dict) -> bool:
    """Whether a package has a target another member can link against.

    Metadata that names no targets comes from a stub, which says nothing against it.
    """
    targets = package.get("targets")
    return not targets or any(LIBRARY_KINDS & set(target.get("kind", ())) for target in targets)


def dependants(seeds: set[str], packages: dict[str, dict], root: Path) -> set[str]:
    """The members that build directly on one of the seeds through a path dependency.

    A type a push changes breaks the package that constructs it while the package that declares
    it compiles clean, and a dev-dependency's test breaks the same way. Only the direct
    dependants are named: a break further along belongs to the dependant that fails first. The
    feature unification crates are neither a seed's source nor its dependants: their dependency
    lists name members they hold no code for, so their own lint says nothing.
    """
    reverse = reverse_dependencies(packages, root)
    reached: set[str] = set()
    for seed in seeds:
        if seed in SYNTHETIC or not library(packages.get(seed, {})):
            continue
        reached |= reverse.get(seed, set())
    return reached - seeds - SYNTHETIC


def select(root: Path, push: Push, files: set[str]) -> Selection | None:
    """The packages to lint for the host, or None when the push cannot be named.

    Those are the packages the commits own or read the changed files through, and the members
    that build on one of them.
    """
    metadata = cargo_metadata(root)
    packages, by_dir = package_maps(metadata, root)
    members = set(packages)
    by_directory = {directory.as_posix(): name for directory, name in by_dir.items()}
    owned = {path: owner_of(path, by_directory) for path in files}
    owners = {owner for owner in owned.values() if owner} & members
    unowned = sorted(path for path, owner in owned.items()
                     if owner is None and path.endswith(RUST_SUFFIXES))
    if unowned:
        # A package that reads one of these compiles it, so its lint covers it;
        # one no package reads is a file CI builds outside the workspace.
        readers = set(reading_packages(root, unowned))
        if not readers:
            say(f"pre-push: no workspace package reads {', '.join(unowned[:3])}"
                f"{' and more' if len(unowned) > 3 else ''}, so CI builds them outside the check; "
                "they are not linted here")
        owners |= readers & members
    seeds = set(owners)
    if WORKSPACE_LOCK in files:
        reached = lock_seeds(root, push, members)
        if reached is None:
            say(f"pre-push: {WORKSPACE_LOCK}'s change cannot be attributed to the packages it "
                "reaches, so the whole workspace is linted")
            return whole_workspace(members, metadata)
        seeds |= reached
    if WORKSPACE_MANIFEST in files:
        reached = manifest_seeds(root, push, metadata)
        if reached is None:
            say(f"pre-push: {WORKSPACE_MANIFEST}'s change decides how every member is built, so "
                "the whole workspace is linted")
            return whole_workspace(members, metadata)
        seeds |= reached
    built_on = dependants(seeds, packages, root)
    return Selection(sorted(seeds | built_on), metadata, sorted(built_on))


def unified_features(root: Path, selected: Iterable[str]) -> list[str]:
    """The features the workspace's whole build gives the selected packages.

    tools/workspace-unify unifies every family's build for the members, in
    the manifest tools/ci/workspace_unify.py generates. A lint that leaves it out
    compiles those members with their default features only, so code behind a
    feature another family enables goes unlinted.
    """
    manifest = root / UNIFY_MANIFEST
    try:
        document = tomllib.loads(manifest.read_text())
    except (OSError, ValueError):
        return []
    wanted = set(selected)
    features: list[str] = []
    tables = [document.get(table, {}) for table in
              ("dependencies", "build-dependencies", "dev-dependencies")]
    for target, tables_under in document.get("target", {}).items():
        # The members resolve the same features whichever Linux host build they
        # are part of; a Windows or Apple table names what only those hosts link.
        if "linux" in target and "windows" not in target and "apple" not in target:
            tables += [tables_under.get(table, {}) for table in
                       ("dependencies", "build-dependencies")]
    for table in tables:
        if not isinstance(table, dict):
            continue
        for name, spec in table.items():
            if name in wanted and isinstance(spec, dict):
                features += [f"{name}/{feature}" for feature in spec.get("features", ())]
    return ["--features", ",".join(sorted(set(features)))] if features else []


def host_architecture() -> str:
    machine = platform.machine().lower()
    return ARCHITECTURES.get(machine, machine)


def t_checks(root: Path, selected: Iterable[str], members: set[str]) -> tuple[list[str], list[str]]:
    """t's checks that lint the selected packages as the push lint does, and the packages none does.

    Those are the per-package checks (`<family>/<package>`) of t.toml's clippy families that name
    workspace-unify in `unify`, on this machine's architecture: a family's members are its
    `packages`, or every member its `exclude` leaves. A package the page family names is asked for
    there as well: its wasm32 build is one the host lint cannot see, and the family declares no
    `unify`, so it stands apart from the unified families.
    """
    families = tomllib.loads((root / T_TOML).read_text()).get("cargo", [])
    arch = host_architecture()
    owners: dict[str, str] = {}
    page: set[str] = set()
    for family in families:
        if family.get("tool") != "clippy":
            continue
        if family.get("name") == PAGE_FAMILY:
            page |= set(family.get("packages", ()))
            continue
        if family.get("unify") != UNIFY:
            continue
        name = family["name"]
        if "{arch}" in name:
            if arch not in family.get("matrix", {}).get("arch", ()):
                continue
            name = name.replace("{arch}", arch)
        lints = (set(family["packages"]) if "packages" in family
                 else members - set(family.get("exclude", ())))
        for package in lints:
            owners.setdefault(package, name)
    checks, uncovered = [], []
    for package in selected:
        if package in owners:
            checks.append(f"{owners[package]}/{package}")
        elif package not in page:
            uncovered.append(package)
        if package in page:
            checks.append(f"{PAGE_FAMILY}/{package}")
    return checks, uncovered


def lint_runs(root: Path, selection: Selection, formatting: list[str],
              checks: list[str] | None) -> list[Lint]:
    """The runs the check makes: the pinned rustfmt, formatting, the host lint of the changed
    packages and the compile of their dependants.

    With `checks`, the changed packages' lint is t's checks of those names; without, the cargo
    lints them. The dependants are compiled in one cargo check either way, through t's cargo with
    `checks`, without `-D warnings`: a dependant's warnings are not the push's, and one build of
    them all shares the crates they build on. Asked as t's per-package checks, each dependant was a
    job of its own building those crates again: a push changing a shared crate queued about 100
    (`check/*`, 106 for one push on t.tilo.so, 2026-11-24). Each command is also the line a failure
    prints to rerun it.
    """
    runs = [Lint("rustfmt pinned", ["bash", str(TOOLS / "ci/check-rustfmt-pin.sh")])]
    if formatting:
        runs.append(Lint("rustfmt" if formatting[0] != "t" else "fmt (t)", formatting))
    if checks is not None:
        if checks:
            runs.append(Lint("clippy (t)", [T_CARGO[0], "test", *checks, "--json"], True))
        cargo, where = T_CARGO, "t"
    else:
        cargo, where = cargo_command(), "native"
    for label, tool, names, trailing in (
            (f"clippy ({where})", "clippy", [] if checks is not None else selection.linted,
             ["--", "-D", "warnings"]),
            (f"dependants' compile ({where})", "check", selection.dependants, [])):
        if names:
            runs.append(Lint(label, [*cargo, tool, "--locked",
                                     *(item for name in names for item in ("-p", name)),
                                     *unified_features(root, names),
                                     "--all-targets", "--keep-going", *trailing]))
    return runs


def scoped_checks(root: Path, push: Push) -> set[str] | None:
    """The checks `t scope` names for the push's change, or None when t cannot tell.

    `t scope` is what says which checks a change reaches: it asks each check's read sets whether
    the change touches them. A script that names a check from a change checks the same rule t
    does, rather than one of its own.
    """
    if not push.tips:
        return None
    try:
        scoped = subprocess.run(["t", "scope", "--rev", push.tips[0], "--names"],
                                cwd=root, capture_output=True, text=True, check=False)
    except OSError:
        return None
    if scoped.returncode != 0:
        return None
    return set(scoped.stdout.split())


# What an app publishes as its surface: its sources, the surface file written from them, and the
# catalogue entry it commits beside itself (`fleet.json`), which the fleet crate gathers at build
# time.
SURFACE_PREFIXES = ("apps/",)
SURFACE_NAMES = ("surface.json",)
SURFACE_SUFFIXES = (".tl",)
# t's checks that judge what an app publishes: its committed surfaces and catalogue entry, and
# the published crates' policy.
SURFACE_CHECKS = ("app-surfaces", "sdk-policy")


def surface_change(files: set[str]) -> bool:
    """Whether the push changes what an app publishes, which its generated files follow."""
    return any(path.startswith(SURFACE_PREFIXES) or path.endswith(SURFACE_SUFFIXES)
               or Path(path).name in SURFACE_NAMES for path in files)


def surface_checks(push: Push, named: set[str] | None) -> list[str]:
    """t's checks over what an app publishes, when the push reaches them.

    `t scope` names the checks a commit's change touches; a push that reaches one and leaves
    its output stale refuses here, not on main. The surfaces' own gate builds a compiler and an
    app collection, so it is asked only when the push changes an app's published surface; a
    manifest or a crate elsewhere reaches the crates' policy without it.
    """
    wanted = list(SURFACE_CHECKS) if surface_change(push.files) else ["sdk-policy"]
    if named is not None:
        return [name for name in wanted if name in named]
    # t could not tell: an app's surface alone still moves its catalogue entry.
    return wanted if surface_change(push.files) else []


# A dependency line is written in a manifest and versioned in the lockfile: a push that changes
# neither adds none.
MANIFESTS = ("Cargo.toml", "Cargo.lock")
# The file t.toml turns into its `policy/{step}` checks.
POLICY_STEPS = Path("tools/policy/steps.toml")
# t's policy step that keeps the browser's wasm32 builds off workspace-hack, whose native
# feature set (mio, sockets, D-Bus, ...) the page cannot compile.
PAGE_HACK_POLICY = "policy/wasm-workspace-hack"


def policy_step_declared(root: Path, name: str) -> bool:
    """Whether the tree's policy suite declares the step `t.toml` turns into `name`."""
    try:
        steps = tomllib.loads((root / POLICY_STEPS).read_text())
    except (OSError, ValueError):
        return False
    wanted = name.removeprefix("policy/")
    return any(step.get("name") == wanted for step in steps.get("step", ()))


def policy_checks(root: Path, push: Push, named: set[str] | None) -> list[str]:
    """t's policy step a manifest change can break the page's wasm32 builds with.

    ``workspace-hack`` unifies the workspace's native feature sets, and `cargo hakari
    manage-deps` writes it into a new member's ``[dependencies]`` unconditionally. A member the
    page family builds must name it only under a ``cfg(not(target_arch = "wasm32"))`` table;
    ungated, the page's wasm32 build pulls mio and every ``clippy-page/*`` job fails for every
    pusher. The host Clippy and compile a push runs cannot see that build, so the policy step
    runs here for the change that can cause it. A step removed from the pushed tree is one t no
    longer names, so a push cannot ask for a check the tree lacks.
    """
    if not any(Path(path).name in MANIFESTS for path in push.files):
        return []
    if named is not None:
        return [PAGE_HACK_POLICY] if PAGE_HACK_POLICY in named else []
    # t could not tell: the tree says whether it declares the step.
    return [PAGE_HACK_POLICY] if policy_step_declared(root, PAGE_HACK_POLICY) else []


# What the component catalogue (`crates/ui/ui-graph/components.json`) is
# generated from: the toolkit's widgets and the window that builds them by name.
# The catalogue is committed, so a push that changes one of these without
# regenerating it lands it stale.
CATALOGUE_INPUTS = (
    "crates/ui/ui-base/src/",
    "crates/ui/ui-core/src/",
    "crates/ui/ui-app/src/",
    "crates/ui/tilo-rich-edit/src/",
    "crates/sdk/tilo-app-window/",
    "crates/ui/ui-graph/",
)


def catalogue_checks(push: Push) -> list[str]:
    """t's `ui-graph` when the push reaches what the component catalogue is made from.

    The check regenerates the catalogue from the tree and compares it with the
    committed file, which is the same answer the toolkit's own test holds; t keys
    it by its inputs, so a push of a tree whose catalogue was already checked
    costs nothing.
    """
    if any(path.startswith(CATALOGUE_INPUTS) for path in push.files):
        return ["ui-graph"]
    return []


# What the shipped shell is compiled from: its package, the session's declared actions it calls,
# and the tilolang compiler and standard library that compile it.
SHELL_INPUTS = (
    "apps/tl/shell/",
    "apps/tl/session/surface.json",
    "crates/lang/tilolang",
)


def shell_checks(push: Push) -> list[str]:
    """t's `shell-compiles` when the push reaches what the shipped shell is compiled from.

    A session compiles the shell's package without its scenarios at startup, so a shell that
    fails that compile starts no session and every session test and walk fails with it. The
    surface checks compile the package together with its scenarios, so a package file calling a
    helper only the scenarios declare passes them and fails here. t keys the check by its
    inputs, so a rebase that changes none of them reuses the pass.
    """
    if any(path.startswith(SHELL_INPUTS) for path in push.files):
        return ["shell-compiles"]
    return []


# The check that judges the fluent catalogues a change leaves behind.
CATALOGUE_COVERAGE = TOOLS / "policy/check_catalogue_coverage.py"


def catalogue_range(root: Path, push: Push) -> int:
    """Judge the catalogues the push changes over its whole range, and refuse drift.

    The commit hook judges each commit's own catalogues as it is made, through
    the check's `--staged` mode. A cherry-pick or a rebase writes commits the
    hook never runs for, and the translation one of them left behind landed with
    no check in its way. The push judges the range as one change, before any
    lint: a key it adds to a source catalogue with no line in a translation, a
    key it adds to a translation with no source original, and a catalogue it
    leaves holding one key twice. The check reads the range's blobs, not the
    files on disk, so its answer is about what is sent.
    """
    for base, head in push.comparisons:
        result = subprocess.run(
            [sys.executable, str(CATALOGUE_COVERAGE), "--range", f"{base}..{head}"],
            cwd=root, capture_output=True, text=True, check=False)
        if result.returncode == 0:
            continue
        for line in [*result.stdout.splitlines(), *result.stderr.splitlines()]:
            say(line)
        say("pre-push: refusing the push: a cherry-pick or a rebase writes commits the commit "
            "hook never judges, so the push judges the catalogues its whole range changes")
        return 1
    return 0


def catalogue_plan(push: Push) -> None:
    """Say the catalogue judgement the push makes; a plan names it and runs nothing."""
    for base, head in push.comparisons:
        print(f"catalogue: --range {base[:12]}..{head[:12]}")


def rustfmt_available() -> bool:
    """Whether the repository's format check can run here.

    It calls rustfmt on the changed files itself, so it needs that binary and
    nothing from the cargo `$CARGO` may point at.
    """
    return shutil.which("rustfmt") is not None


def landing_env(root: Path) -> dict[str, str]:
    """The environment the lint's t runs get: the push hook's own session, a person's.

    A push lint lands work, so t ranks it as the person at a terminal whoever pushes. Run in an
    agent's session it stood behind main's whole CI backlog, and pushes waited for it for an hour
    while main moved on under them. t ranks a `human:` session as people's; each checkout gets one
    of its own, so pushes from several checkouts share people's cores fairly and a retry keeps the
    session, and the builds t kept for it, of the last attempt.
    """
    env = dict(os.environ)
    checkout = re.sub(r"[^A-Za-z0-9_.-]+", "_", root.resolve().name)
    env["T_SESSION"] = f"human:push.{socket.gethostname()}.{checkout}"
    return env


def run_together(root: Path, runs: list[Lint], keep: Path | None = None,
                 tree: Path | None = None) -> tuple[list[tuple[Lint, int, str]], float]:
    """Start every run at once and collect them, so the wall time is the slowest one.

    With `tree`, the runs read the pushed commit's files from that checkout of it, in this
    checkout's t session. A cargo of this machine builds into this checkout's own target
    directory: the checkout is new for every push, and a target of its own would build the whole
    workspace from nothing each time.

    With `keep`, each run's whole output is written there, headed by its command and how long
    it took: what tells a lint that compiled everything from one that compiled what changed.
    """
    started = time.monotonic()
    running = []
    for run in runs:
        # A file, not a pipe: a run whose output outgrows the pipe buffer must
        # not wait on a reader that is still waiting on another run.
        log = tempfile.TemporaryFile(mode="w+")
        env = landing_env(root)
        if tree is not None:
            env["TILO_REPO_ROOT"] = str(tree)
            env.setdefault("CARGO_TARGET_DIR", str(root / "target"))
        process = subprocess.Popen(run.command, cwd=tree or root, stdout=log,
                                   stderr=subprocess.STDOUT, text=True, env=env)
        running.append((run, process, log))
    finished: dict[int, float] = {}
    while len(finished) < len(running):
        for index, (_run, process, _log) in enumerate(running):
            if index not in finished and process.poll() is not None:
                finished[index] = time.monotonic() - started
        time.sleep(0.2)
    results = []
    for index, (run, process, log) in enumerate(running):
        log.seek(0)
        output = log.read()
        results.append((run, process.returncode, output))
        log.close()
        if keep is not None:
            keep.mkdir(parents=True, exist_ok=True)
            name = re.sub(r"[^a-z0-9]+", "-", run.label.lower()).strip("-")
            (keep / f"{index}-{name}.log").write_text(
                f"$ {shlex.join(run.command)}\n# exit {process.returncode} after {finished[index]:.0f}s\n"
                + output)
    return results, time.monotonic() - started


def packages(count: int) -> str:
    return f"{count} package" if count == 1 else f"{count} packages"


def t_documents(output: str) -> list[dict]:
    """The JSON documents in what `t test --json` printed: one per line while it streams, then the
    whole run's, spread over many lines."""
    decoder = json.JSONDecoder()
    documents = []
    position = 0
    while position < len(output):
        if output.startswith("{", position):
            try:
                document, position = decoder.raw_decode(output, position)
            except ValueError:
                pass
            else:
                if isinstance(document, dict):
                    documents.append(document)
                continue
        newline = output.find("\n", position)
        position = len(output) if newline < 0 else newline + 1
    return documents


def t_outcomes(output: str) -> tuple[list[dict], list[dict]]:
    """Each check's result from t's output, and the errors t gave instead of results.

    A check t answers again after a base probe tells its result twice, first unblamed and then
    with whose failure it is: the later one stands, so a check is judged once.
    """
    finished: dict[str, dict] = {}
    errors = []
    for document in t_documents(output):
        if document.get("type") == "error":
            errors.append(document)
        elif isinstance(document.get("body"), dict) and isinstance(document["body"].get("finished"), dict):
            result = document["body"]["finished"]
            finished[result.get("check")] = result
    return list(finished.values()), errors


def say_lines(lines: list[str]) -> None:
    for line in lines[:SHOWN_DIAGNOSTIC_LINES]:
        say(f"  {line}")
    if len(lines) > SHOWN_DIAGNOSTIC_LINES:
        say(f"  ... {len(lines) - SHOWN_DIAGNOSTIC_LINES} more line(s); the rerun above prints them")


def say_t_results(output: str) -> None:
    """What t answered for each check: from its result cache, or by running it now."""
    finished, _errors = t_outcomes(output)
    for result in finished:
        if result.get("outcome") != "pass":
            continue
        if result.get("cached"):
            say(f"pre-push: {result['check']} passed on these inputs before (t's run {result['cached']})")
        else:
            say(f"pre-push: {result['check']} passed in {result.get('duration_ms', 0) / 1000:.0f}s")


def declared_fixes(root: Path) -> dict[str, str]:
    """The fix `t.toml` declares for each check, by its name: what writes its generated files.

    A check that declares none has no generator this lint can run, so a generated-output
    comparison of it falls back to the names its failure prints.
    """
    try:
        project = tomllib.loads((root / T_TOML).read_text())
    except (OSError, ValueError):
        return {}
    fixes = {}
    for check in project.get("check", ()):
        name, fix = check.get("name"), check.get("fix")
        if isinstance(name, str) and isinstance(fix, str) and fix:
            fixes[name] = fix
    return fixes


def t_fixes(root: Path) -> set[str]:
    """The t checks that declare a `fix` in t.toml, by name.

    `t fix <check>` runs it. A check that compares a generated file is worth naming its fix
    for: the person pushing writes the file back rather than working out which generator does.
    """
    return set(declared_fixes(root))


def say_t_refused(refused: list[tuple[dict, str]], errors: list[dict], fixes: set[str]) -> None:
    """Each failure the push is refused for: t's own error, or a check with its diagnostics.

    A check that names a fix in t.toml is told how to write what it compares, and one the board
    could not inherit says why.
    """
    for result, why in refused:
        say(f"  {result['check']}: {result.get('outcome')} - whole log: t log {result.get('log')}")
        words = blame_words(result)
        if words:
            say(f"  {words}")
        if why and why != words:
            say(f"  {why}")
        say_lines(str(result.get("excerpt") or "").strip().splitlines())
        if result["check"] in fixes:
            say(f"  write the generated files back with: t fix {result['check']}")
    for error in errors:
        say(f"  t: {error.get('message')}")
        if error.get("hint"):
            say(f"  {error['hint']}")


def inherited(result: dict) -> bool:
    """Whether t blamed a failure on main at the run's base."""
    blame = result.get("blame")
    return isinstance(blame, dict) and "inherited" in blame


def blame_words(result: dict) -> str:
    """Whose failure t says a result is, in words: its Blame, naming the base's red."""
    if result.get("fixed") is not None:
        return "t already pushed its fix; pull it"
    blame = result.get("blame")
    if isinstance(blame, dict):
        said = blame.get("inherited")
        if isinstance(said, dict):
            since = str(said.get("since") or "")
            owner = said.get("claimed_by")
            # A finding in a package the check builds on is red under that
            # package's check or its family's run, so the red may not be the
            # failing check's own.
            of = said.get("check")
            subject = "it" if not of or of == result.get("check") else of
            says = (f"main has failed {subject} since {since[:12]}" if since
                    else f"main has {subject} too")
            words = f"{says}, claimed by {owner}" if owner else f"{says}, unclaimed"
            if said.get("unread"):
                words += ("; main's log is gone, but the push changes no file the finding names")
            return words
        if blame.get("elsewhere"):
            return "not yours: only in files this push leaves as they are"
        if "uncompared" in blame:
            return "main's failure and this one can't be told apart one by one: it may be yours"
    return {
        "flaky": "flaky: the same inputs passed before",
        "unknown": "may be yours: main has no result at your base",
        "new": "new: passes on your base",
    }.get(blame if isinstance(blame, str) else "", "")


def say_past_reds(results: list[dict]) -> None:
    """Say the failures a push passes because they are not its own, each with whose it is.

    Main's reds are counted as such; a flaky failure or one t already fixed is said beside them,
    not counted as a red the push pushes past.
    """
    inherited_results = [result for result in results if inherited(result)]
    others = [result for result in results if result not in inherited_results]
    if inherited_results:
        count = len(inherited_results)
        say(f"pre-push: pushing past {count} red{'s' if count != 1 else ''} main already has:")
        for result in inherited_results:
            say(f"  {result['check']}: {blame_words(result)}")
    for result in others:
        say(f"pre-push: {result['check']}: {blame_words(result) or 'not yours'}")


def remote_repo(root: Path) -> str | None:
    """The repository's name on the server, from origin's URL (`code.tilo.so:tilo/tilo.git`)."""
    url = git(root, "remote", "get-url", "origin").stdout.strip()
    if not url:
        return None
    name = url.rstrip("/").rsplit("/", 1)[-1].rsplit(":", 1)[-1]
    return name.removesuffix(".git") or None


def is_ancestor(root: Path, commit: str, of: str) -> bool | None:
    """Whether `commit` is `of` or an ancestor of it; None when either is not here."""
    if not commit or not of or not known(root, commit) or not known(root, of):
        return None
    return git(root, "merge-base", "--is-ancestor", commit, of).returncode == 0


def finding_key(finding: str) -> str:
    """A finding without the count t adds for a repeated lint in one file (`unused_imports a.rs (2)`)."""
    return re.sub(r" \(\d+\)$", "", finding.strip())


def finding_file(finding: str) -> str:
    """The file a finding names (`clippy::arithmetic_side_effects crates/a/src/lib.rs`), or empty."""
    _lint, _, path = finding_key(finding).partition(" ")
    return path.strip()


def stale_files(text: str) -> set[str]:
    """The generated files a check's output names as not current, as each command prints it."""
    found: set[str] = set()
    for line in text.splitlines():
        for pattern in STALE_LINES:
            match = pattern.match(line)
            if match:
                found.add(match.group(1).strip())
                break
    return found


# The file a fix's patch changes, as a git diff names it beside its original.
PATCH_FILE = re.compile(r"^diff --git a/.+? b/(.+)$", re.MULTILINE)


def generated_cache_dir(root: Path) -> Path:
    """This worktree's cache of the base side of a generated comparison.

    It is the worktree's own git directory, so a linked worktree keeps its own entries and no
    two write over each other. A rebase onto a new base is a new key, not a rewritten one.
    """
    own = git(root, "rev-parse", "--path-format=absolute", "--git-dir").stdout.strip()
    return Path(own) / GENERATED_CACHE


def fix_patch(root: Path, revision: str, check: str, fix: str,
              *, cached: bool) -> tuple[str | None, str]:
    """What a check's generator writes at `revision`, as the patch `t fix --rev` answers.

    The generator is the one t builds from that revision's own tree, so the patch says what
    that commit's code writes for the files it compares. The base side (`cached`) is kept in
    this worktree's cache, keyed by the check, its fix command and the commit: a second push
    off the same base does not run it again. Answers the patch, or why it cannot be had.
    """
    entry = generated_cache_dir(root) / (
        hashlib.sha256(f"{check}\n{fix}\n{revision}".encode()).hexdigest() + ".patch")
    if cached:
        try:
            return entry.read_text(), ""
        except FileNotFoundError:
            pass
        except (OSError, UnicodeDecodeError):
            # A cache written by a build that can no longer be read is only a miss.
            pass
    with tempfile.TemporaryDirectory() as scratch:
        patch = Path(scratch) / "generated.patch"
        try:
            run = subprocess.run(["t", "fix", "--rev", revision, f"--patch={patch}", check],
                                 cwd=root, capture_output=True, text=True, check=False)
        except OSError as error:
            return None, f"t is not at hand ({error})"
        if run.returncode != 0:
            said = [line for line in (run.stderr or run.stdout).splitlines() if line.strip()]
            return None, "t could not run its generator" + (f" ({said[-1]})" if said else "")
        try:
            text = patch.read_text()
        except OSError:
            return None, "its generator wrote no patch"
    if cached:
        try:
            entry.parent.mkdir(parents=True, exist_ok=True)
            writing = entry.with_name(f"{entry.name}.{os.getpid()}")
            writing.write_text(text)
            writing.replace(entry)
        except OSError:
            # A cache that cannot be written only makes the next push off this base slower.
            pass
    return text, ""


def generated_content(root: Path, revision: str, patch: str,
                      paths: Iterable[str]) -> dict[str, str | None] | None:
    """What a check's generator writes for `paths` at `revision`: each file, with its fix's
    patch applied, or `None` when the generator deletes it.

    The comparison is of content, so the files are materialized in a scratch tree from the
    revision and the patch applied there. `None` when the patch does not apply, which is no
    comparison at all rather than a false difference.
    """
    wanted = sorted(set(paths))
    with tempfile.TemporaryDirectory() as scratch:
        tree = Path(scratch)
        # Every file the patch names is written first: git applies a patch to a tree, and one
        # the patch creates or deletes is as much part of it as the files compared.
        for path in sorted(set(PATCH_FILE.findall(patch)) | set(wanted)):
            committed = subprocess.run(["git", "show", f"{revision}:{path}"], cwd=root,
                                       capture_output=True, check=False)
            if committed.returncode != 0:
                continue
            target = tree / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(committed.stdout)
        if patch.strip():
            # git applies a patch inside a repository only, so the scratch tree is one. The push
            # hook runs with GIT_DIR naming the pusher's repository, and that wins over the
            # working directory: with it, `init` would turn the pusher's repository bare and
            # `apply` would patch it. So the git variables never reach the scratch tree's git.
            scratch_env = {key: value for key, value in os.environ.items()
                           if not key.startswith("GIT_")}
            subprocess.run(["git", "init", "-q", "."], cwd=tree, env=scratch_env,
                           capture_output=True, check=False)
            applied = subprocess.run(["git", "apply", "-p1", "--binary"], cwd=tree, input=patch,
                                     env=scratch_env, capture_output=True, text=True, check=False)
            if applied.returncode != 0:
                return None
        generated: dict[str, str | None] = {}
        for path in wanted:
            target = tree / path
            try:
                generated[path] = target.read_text() if target.is_file() else None
            except (OSError, UnicodeDecodeError):
                # A file no comparison can read is no comparison: the names judge it instead.
                return None
        return generated


def dependency_closure(packages: dict[str, dict], names: set[str]) -> set[str]:
    """`names` and every package they reach through a path dependency."""
    reached: set[str] = set()
    todo = list(names)
    while todo:
        name = todo.pop()
        if name in reached:
            continue
        reached.add(name)
        for dependency in packages.get(name, {}).get("dependencies", ()):
            if dependency.get("source") is None:
                todo.append(dependency["name"])
    return reached


def compile_is_mains(root: Path, push: Push, metadata: dict, output: str) -> list[str] | None:
    """The failing packages whose build reads nothing the push changes, or None if one may.

    A package's compile is decided by its own directory and the path dependencies it builds; a
    push that changes none of them compiles it as the base did, so a failure there is main's. A
    manifest, the lockfile or a build script can reach any build, so one leaves it unknown.
    """
    if any(Path(path).name in (*MANIFESTS, "build.rs") for path in push.files):
        return None
    failed = set(re.findall(r"error: could not compile `([^`]+)`", output))
    if not failed:
        return None
    packages, by_dir = package_maps(metadata, root)
    directory_of = {name: directory for directory, name in by_dir.items()}
    changed = [Path(path) for path in push.files]
    for name in failed:
        directories = [directory_of[member]
                       for member in dependency_closure(packages, {name}) if member in directory_of]
        if any(path.is_relative_to(directory) for path in changed for directory in directories):
            return None
    return sorted(failed)


def log_findings(root: Path, log: str) -> set[str] | None:
    """The findings a check's log holds, as t's results name them (`lint file`), or None.

    The log's machine messages (`t log --verbose`) are cargo's own JSON, so a primary span's file
    and its lint code are exactly what t read the finding from: no rebase shifts a line here.
    """
    try:
        answer = subprocess.run(["t", "log", str(log), "--verbose"], cwd=root,
                                capture_output=True, text=True, check=False)
    except OSError:
        return None
    if answer.returncode != 0:
        return None
    found: set[str] = set()
    for line in answer.stdout.splitlines():
        if not line.startswith("{"):
            continue
        try:
            message = json.loads(line)
        except ValueError:
            continue
        if message.get("reason") != "compiler-message":
            continue
        diagnostic = message.get("message") or {}
        code = (diagnostic.get("code") or {}).get("code")
        spans = diagnostic.get("spans") or []
        primary = next((span for span in spans if span.get("is_primary")),
                       spans[0] if spans else None)
        if code and primary and primary.get("file_name"):
            found.add(f"{code} {primary['file_name']}")
    return found


def main_board(root: Path) -> list[dict] | None:
    """Main's board from the server, or None when it cannot be read."""
    for repo in (remote_repo(root), None):
        command = ["t", "main", "--json", *(("--repo", repo) if repo else ())]
        try:
            answer = subprocess.run(command, cwd=root, capture_output=True, text=True, check=False)
        except OSError:
            continue
        if answer.returncode != 0:
            continue
        try:
            document = json.loads(answer.stdout)
        except ValueError:
            continue
        if isinstance(document, dict) and isinstance(document.get("items"), list):
            return document["items"]
    return None


# Lints a change can cause in a file it does not touch.
CROSS_FILE_LINTS = {"dead_code"}


def item_blame(item: dict) -> dict:
    """A board item as a Blame, so a red passed on the board is said like t's own.

    `check` names the item's check, which is the failing check's unless the red
    was found under the package it builds on; `log_unread` says the board's own
    log is gone, so only the board's red and the unchanged files stand for it.
    """
    if item.get("untouched"):
        return {"elsewhere": True}
    claim = item.get("claim") or {}
    inherited = {"since": item.get("first_bad") or "",
                 "claimed_by": claim.get("session") or claim.get("note")}
    if item.get("check"):
        inherited["check"] = item["check"]
    if item.get("log_unread"):
        inherited["unread"] = True
    return {"inherited": inherited}


def item_covers_check(item: dict, check: str, covering: bool) -> bool:
    """Whether a board item's red is of `check`, or covers it when asked.

    A check's build lints the workspace packages it builds on, so its failure
    can name a finding in a dependency; main's board records such a red under
    the dependency's own item, or under the family's run, whose `checks` names
    every per-package check it ran. Covering matches those too. The finding is
    still compared one by one, so a red of another check cannot hide a new one.
    """
    named = item.get("check") or ""
    if named == check:
        return True
    if not covering:
        return False
    if check in (item.get("checks") or ()):
        return True
    family, _, package = check.partition("/")
    return bool(package) and named.partition("/")[0] == family


class Board:
    """Main's board at the push's base: what a failure t could not compare is judged against.

    t names a failure at the run's base only where the base has a stored verdict of the same
    key, and holds otherwise: the result says `unknown` before its probe answers, and a probe
    the push does not wait for leaves it there. Main's board knows the red anyway, so this asks
    it, and compares the failure with main's own log finding by finding.
    """

    def __init__(self, root: Path, push: Push, items: list[dict] | None = None,
                 logs: dict[str, set[str] | None] | None = None,
                 texts: dict[str, str | None] | None = None,
                 ancestor: "Callable[[str, str], bool | None] | None" = None):
        self.root = root
        self.push = push
        self.base = push.comparisons[0][0] if push.comparisons else None
        self._items = items
        self._logs = logs or {}
        self._texts = texts or {}
        self._read = items is not None
        self.ancestor = ancestor or (lambda commit, of: is_ancestor(root, commit, of))

    def items(self) -> list[dict] | None:
        if not self._read:
            self._read = True
            self._items = main_board(self.root)
        return self._items

    def findings(self, log: str) -> set[str] | None:
        if log not in self._logs:
            self._logs[log] = log_findings(self.root, log)
        return self._logs[log]

    def log_text(self, log: str) -> str | None:
        """A check's whole log as it prints, or None when it cannot be read."""
        if log not in self._texts:
            try:
                answer = subprocess.run(["t", "log", str(log)], cwd=self.root,
                                        capture_output=True, text=True, check=False)
            except OSError:
                return None
            self._texts[log] = answer.stdout if answer.returncode == 0 else None
        return self._texts[log]

    def items_at_base(self, check: str, covering: bool = False) -> tuple[list[dict], str]:
        """Main's items for `check` whose red the base contains, or why it cannot be said.

        `covering` also takes the items of the packages the check builds on, for
        a finding that package's build named (`item_covers_check`).
        """
        items = self.items()
        if items is None:
            return [], "main's board cannot be read, so whose failure this is cannot be told"
        if self.base is None:
            return [], "this push has no base to compare main's board with"
        named = [item for item in items if item_covers_check(item, check, covering)]
        if not named:
            return [], f"main's board names no red for {check}"
        at_base = []
        for item in named:
            since = item.get("first_bad") or ""
            before = self.ancestor(since, self.base)
            if before is None:
                return [], (f"main's board names {check} red since {since[:12]}, which this "
                            "checkout does not have")
            if before:
                at_base.append(item)
        if not at_base:
            return [], f"main began failing {check} after this push's base"
        return at_base, ""

    def inherit_generated(self, result: dict) -> tuple[dict | None, str]:
        """Main's item a generated-output failure is, by what its generator writes, or why not.

        The check fails with the files that are not current on this tree. A file whose generated
        content this push changes since its base is the push's to commit; one its generator
        writes the same at both ends is main's, whether or not main's log names it. So the
        generator runs at the base and at this tree when the check fails, and the two write-outs
        are compared: the names in main's log judge nothing while that comparison can be made.
        """
        check = result.get("check")
        ours = stale_files(str(result.get("excerpt") or ""))
        at_base, why = self.items_at_base(check)
        if not at_base:
            # Main has no red for the check at the base, or none this checkout can place: the
            # files it names here are the push's to write back.
            return None, ("new: " + ", ".join(sorted(ours))) if ours else why
        if not ours:
            return self.inherit_generated_by_names(check, ours, at_base,
                                                   "its failure names no output file")
        comparisons = self.push.comparisons
        if not comparisons:
            return self.inherit_generated_by_names(check, ours, at_base,
                                                   "this push has no base to compare")
        fix = declared_fixes(self.root).get(check)
        if fix is None:
            return self.inherit_generated_by_names(check, ours, at_base,
                                                   "t.toml declares no `fix` for it")
        base, head = comparisons[0]
        base_patch, reason = fix_patch(self.root, base, check, fix, cached=True)
        if base_patch is None:
            return self.inherit_generated_by_names(check, ours, at_base,
                                                   f"the base's generator is not at hand: {reason}")
        head_patch, reason = fix_patch(self.root, head, check, fix, cached=False)
        if head_patch is None:
            return self.inherit_generated_by_names(
                check, ours, at_base, f"this tree's generator is not at hand: {reason}")
        at_base_content = generated_content(self.root, base, base_patch, ours)
        if at_base_content is None:
            return self.inherit_generated_by_names(check, ours, at_base,
                                                   "the base's generator's patch does not apply")
        here = generated_content(self.root, head, head_patch, ours)
        if here is None:
            return self.inherit_generated_by_names(
                check, ours, at_base, "this tree's generator's patch does not apply")
        changed = [path for path in sorted(ours) if at_base_content.get(path) != here.get(path)]
        if changed:
            return None, "new: " + ", ".join(changed)
        return at_base[0], ""

    def inherit_generated_by_names(self, check: str, ours: set[str], at_base: list[dict],
                                   reason: str) -> tuple[dict | None, str]:
        """Main's item a generated-output failure is by the files main's log names, or why not.

        This is the comparison that needs no generator, and it says when it is the one making
        the judgement: a push judged by names alone is never mistaken for one judged by what it
        writes, and the reason its own output could not be compared is the person's to read.
        """
        say(f"pre-push: {check}: judging by the files main's log names alone ({reason})")
        theirs: set[str] = set()
        for item in at_base:
            text = self.log_text(str(item.get("log") or ""))
            if text is None:
                say(f"pre-push: {check}: main's own log cannot be read either")
                return None, f"main's log for {check} cannot be read"
            theirs |= stale_files(text)
        extra = sorted(ours - theirs)
        if extra:
            return None, "new: " + ", ".join(extra)
        if not ours and theirs:
            return None, (f"{check} names no output file here, but main fails on "
                          + ", ".join(sorted(theirs)))
        return at_base[0], ""

    def judge(self, result: dict) -> tuple[dict | None, str]:
        """Main's item this failure is, at the push's base, or why it cannot be inherited.

        A failed test is compared by name, a finding (a lint and its file) one by one against
        main's log, so a check red on main for one lint does not hide a new one here. A finding
        is sought among the reds of the checks the failing one builds on too: a session check's
        build lints the runtime it builds on, and main's board names that red under the
        runtime's check or the family's run. Where every such log is gone, the board's red at
        the base stands for a finding the push's own files do not name. A failure naming
        neither test nor finding is the whole check's, and inherits another whole one of the
        same check.
        """
        check = result.get("check")
        tests = [test for test in result.get("failed_tests") or [] if test]
        findings = [finding_key(finding) for finding in result.get("findings") or [] if finding]
        if tests:
            at_base, why = self.items_at_base(check)
            if not at_base:
                return None, why
            on_main = {item.get("test") for item in at_base if item.get("test")}
            missing = [test for test in tests if test not in on_main]
            if missing:
                return None, "new: " + ", ".join(missing)
            return at_base[0], ""
        if findings:
            at_base, why = self.items_at_base(check, covering=True)
            if not at_base:
                return self.elsewhere(check, findings, why)
            theirs: set[str] = set()
            unread: list[dict] = []
            for item in at_base:
                if item.get("test"):
                    continue
                on_main = self.findings(str(item.get("log") or ""))
                if on_main is None:
                    unread.append(item)
                else:
                    theirs |= on_main
            missing = [finding for finding in findings if finding not in theirs]
            if not missing:
                return at_base[0], ""
            if unread and self.files_untouched(missing):
                return {**unread[0], "log_unread": True}, ""
            return self.elsewhere(check, missing, "new: " + ", ".join(missing))
        at_base, why = self.items_at_base(check)
        if not at_base:
            return None, why
        for item in at_base:
            if not item.get("test"):
                return item, ""
        return None, f"main's {check} red names a test this failure does not"

    def elsewhere(self, check: str | None, findings: list[str], why: str) -> tuple[dict | None, str]:
        """A lint the push did not write, when main's board cannot say it is main's.

        A lint is reported at the line that breaks it, so a finding in a file the push leaves
        as it is came with whoever last changed that file: main's red the board has not judged
        yet, or a commit after the push's base. Refusing it holds a finished change behind
        someone else's red. `dead_code` is the exception: removing a caller in one file makes
        an item in another unused, so it stays the push's until the board says otherwise.
        """
        if not self.files_untouched(findings):
            return None, why
        if any(finding_key(finding).partition(" ")[0] in CROSS_FILE_LINTS for finding in findings):
            return None, why
        return {"check": check, "untouched": True}, ""

    def files_untouched(self, findings: Iterable[str]) -> bool:
        """Whether every finding names a file this push leaves as it is."""
        paths = [finding_file(finding) for finding in findings]
        return all(path and path not in self.push.files for path in paths)


def report(results: list[tuple[Lint, int, str]], elapsed: float, checked: str,
           fixes: set[str], root: Path, push: Push, metadata: dict) -> int:
    passed_through: list[dict] = []
    failed = []
    board = Board(root, push)
    for run, status, output in results:
        if not run.t:
            if status == 0:
                continue
            if run.label.startswith("dependants' compile"):
                compile_reds = compile_is_mains(root, push, metadata, output)
                if compile_reds is not None:
                    say(f"pre-push: pushing past main's dependants' compile: "
                        f"{', '.join(compile_reds)} (the push changes none of what they build)")
                    continue
            failed.append((run, output, [], []))
            continue
        finished, errors = t_outcomes(output)
        say_t_results(output)
        refused = []
        for result in finished:
            outcome = result.get("outcome")
            if outcome == "pass":
                continue
            if outcome != "fail":
                refused.append((result, ""))
                continue
            if result.get("fixed") is not None or result.get("blame") == "flaky":
                passed_through.append(result)
                continue
            check = result.get("check")
            if check in GENERATED_CHECKS:
                # A generated output is judged by what its generator writes at the base and at
                # this tree, whatever t says of the check whole: a file whose generated content
                # this push leaves alone is not this push's to write back.
                item, why = board.inherit_generated(result)
            elif inherited(result):
                # t compared this failure with the base's verdict and holds it main's.
                passed_through.append(result)
                continue
            elif result.get("blame") == "new":
                item, why = None, blame_words(result)
            else:
                # t could not compare it: unknown before a probe answers, or two failures it
                # cannot tell apart. Main's board can say, finding by finding.
                item, why = board.judge(result)
            if item is not None:
                passed_through.append({**result, "blame": item_blame(item)})
            else:
                refused.append((result, why))
        if refused or errors:
            failed.append((run, output, refused, errors))
        elif status != 0 and not finished:
            failed.append((run, output, [], []))
    if not failed:
        if passed_through:
            say_past_reds(passed_through)
        say(f"pre-push: lint passed in {elapsed:.0f}s ({checked})")
        return 0
    for run, output, refused, errors in failed:
        command = [part for part in run.command if part != "--json"] if run.t else run.command
        say(f"pre-push: {run.label} failed - rerun: {shlex.join(command)}")
        if run.t:
            say_t_refused(refused, errors, fixes)
            continue
        say_lines([line for line in output.strip().splitlines()
                   if not any(remark in line for remark in CARGO_REMARKS)
                   and not PROGRESS.match(line)])
    say("pre-push: refusing the push until the lint passes")
    return 1


def own_key(root: Path, push: Push, selection: Selection,
            runs: list[Lint]) -> tuple[str, str] | None:
    """What a pass of this push's lint is remembered by, and the patches it names.

    The push's own patches, the packages it lints with the files it writes in each of their
    own directories, and the kinds of run its lint makes. A rebase that resolved nothing
    changes none of them, however much main moved: the same change replayed onto newer commits
    has the same patch-ids, lints the same packages, writes the same files in their
    directories, and asks for the same kinds of run. A rebase that resolved a conflict into
    another change writes another patch, so it is linted.

    What main decides about the run does not enter - the dependants its manifests name, the
    features workspace-unify gives the packages, the checks `t scope` names, the commits a
    format check compares: a retry after main moved computes those again, against commits the
    push never saw, and keying a pass by them made a busy package lint from scratch after every
    rebase. What main's commits did to those directories does not enter either: t's clippy
    families lint those interactions on main, and a red main is fixed forward.
    """
    directories = ({name: directory.as_posix()
                    for directory, name in package_maps(selection.metadata, root)[1].items()}
                   if selection.linted else {})
    # Only which kinds of run there are: a command also names the dependants main's manifests
    # give a package, the features workspace-unify gives it, and the commits to compare, none of
    # which is the push's to decide.
    parts = [f"run {run.label}" for run in runs]
    patches = []
    for base, head in push.comparisons:
        patch = subprocess.run(["git", "diff", "--no-renames", "--binary", f"{base}..{head}", "--"],
                               cwd=root, capture_output=True, check=False)
        identity = subprocess.run(["git", "patch-id", "--stable"], cwd=root, input=patch.stdout,
                                  capture_output=True, check=False).stdout.split()
        if patch.returncode != 0 or not identity:
            return None
        patches.append(identity[0].decode())
        parts.append(f"patch {patches[-1]}")
        for name in selection.linted:
            directory = directories.get(name)
            if directory is None:
                return None
            # What the push writes in the package's own directory, blobs included: a rebase onto
            # commits that leave these files alone replays the same bytes, and one whose merge
            # changes them lints again.
            own = subprocess.run(["git", "diff", "--no-renames", "--raw", "-z",
                                  f"{base}..{head}", "--", directory or "."],
                                 cwd=root, capture_output=True, check=False)
            if own.returncode != 0:
                return None
            parts.append(f"{name} {directory} {hashlib.sha256(own.stdout).hexdigest()}")
    return hashlib.sha256("\n".join(parts).encode()).hexdigest(), " ".join(patches)


def passes_path(root: Path) -> Path:
    common = git(root, "rev-parse", "--path-format=absolute", "--git-common-dir").stdout.strip()
    return Path(common) / PASSES


def passed_before(root: Path, key: str) -> tuple[str, str] | None:
    """The head a lint of this key passed on, and when, if one did."""
    try:
        lines = passes_path(root).read_text().splitlines()
    except FileNotFoundError:
        return None
    for line in reversed(lines):
        fields = line.split(" ")
        if fields and fields[0] == key:
            head = fields[1] if len(fields) > 1 else ""
            when = " ".join(fields[2:]) if len(fields) > 2 else ""
            return head, when
    return None


def remember_pass(root: Path, key: str, head: str) -> None:
    path = passes_path(root)
    try:
        lines = path.read_text().splitlines()
    except FileNotFoundError:
        lines = []
    when = datetime.datetime.now(datetime.UTC).strftime("%Y-%m-%d %H:%M UTC")
    lines = [line for line in lines if not line.startswith(f"{key} ")] + [f"{key} {head} {when}"]
    temporary = path.with_name(f"{PASSES}.{os.getpid()}")
    temporary.write_text("".join(f"{line}\n" for line in lines[-PASSES_KEPT:]))
    temporary.replace(path)


def formatting_run(root: Path, push: Push) -> list[str]:
    """The repository's own format check over the changed files (tools/ci/check-changed-rustfmt.sh).

    It checks exactly the hand-written Rust the push adds, the way CI does,
    rather than every file of the packages it touches.
    """
    comparisons = push.comparisons
    if not comparisons:
        return []
    base, head = comparisons[0]
    return ["bash", str(TOOLS / "ci/check-changed-rustfmt.sh"), base, head]


def unpushed_files(root: Path) -> list[str]:
    """The files on disk that are not the pushed commit's and a lint could read.

    Every changed tracked file, and every untracked file but one at the top of the checkout or
    under `.claude/`: no package reads a stray file there (a shell's mangled redirect, an agent's
    notes), so such a file needs no checkout of the pushed commit to lint around it.
    """
    status = git(root, "status", "--porcelain", "-z", "--no-renames",
                 "--untracked-files=all").stdout
    files = []
    for entry in filter(None, status.split("\0")):
        path = entry[3:]
        if entry.startswith("?? ") and ("/" not in path or path.startswith(".claude/")):
            continue
        files.append(path)
    return files


# What a pass's key reads from the files on disk rather than from git: the manifests cargo names
# the packages and their directories from, and the files that say which checks lint them and how.
KEY_INPUT_NAMES = ("Cargo.toml", "Cargo.lock")
KEY_INPUT_PATHS = (T_TOML, UNIFY_MANIFEST.as_posix(), "tools/ci/suites.toml", "rust-toolchain.toml")


def reads_key_input(files: list[str]) -> bool:
    """Whether a pass's key would read one of these files, which the push does not send."""
    return any(Path(path).name in KEY_INPUT_NAMES or path in KEY_INPUT_PATHS for path in files)


# Where the lint checks out the pushed commit when the files on disk are not its own, in the
# repository's git directory.
TREES = "tilo-pre-push-trees"


class PushedTree:
    """The pushed commit's files, which the lint reads, where the checkout's own are not them.

    A push sends commits, never the files beside them: an untracked file, an edit not committed.
    A lint of the files on disk spoke for a tree nobody pushes, so its pass could not be kept for
    a retry, and one refused on them sent people to `--no-verify`. Where the files on disk differ
    from the pushed commit, the lint reads a checkout of the commit of its own, which t uploads as
    the state it builds, and which is removed when the lint ends. Where they do not, it reads the
    checkout itself, and a push standing on a remembered pass checks nothing out at all.
    """

    def __init__(self, root: Path, tip: str, unpushed: list[str]):
        self.root, self.tip, self.unpushed = root, tip, unpushed
        self.path: Path | None = None

    def __enter__(self) -> "PushedTree":
        return self

    def __exit__(self, *_exc: object) -> None:
        if self.path is not None:
            self.remove(self.path)

    def where(self) -> Path:
        """The directory whose files are the pushed commit's: the checkout, or one made for it."""
        if not self.unpushed:
            return self.root
        if self.path is None:
            checkout = re.sub(r"[^A-Za-z0-9_.-]+", "_", self.root.resolve().name)
            common = git(self.root, "rev-parse", "--path-format=absolute",
                         "--git-common-dir").stdout.strip()
            path = Path(common) / TREES / checkout
            self.remove(path)
            path.parent.mkdir(parents=True, exist_ok=True)
            added = git(self.root, "worktree", "add", "--detach", "--quiet", str(path), self.tip)
            if added.returncode != 0:
                say(f"pre-push: cannot check out {self.tip[:12]} to lint it: "
                    f"{added.stderr.strip()}")
                raise SystemExit(1)
            shown = ", ".join(self.unpushed[:3]) + (" and more" if len(self.unpushed) > 3 else "")
            say(f"pre-push: {shown} here {'is' if len(self.unpushed) == 1 else 'are'} not in the "
                f"pushed commit, so the lint reads a checkout of {self.tip[:12]} of its own")
            self.path = path
        return self.path

    def remove(self, path: Path) -> None:
        """Remove a checkout the lint made, and one a lint that was stopped left behind."""
        if path.exists():
            git(self.root, "worktree", "remove", "--force", str(path))
            shutil.rmtree(path, ignore_errors=True)
        git(self.root, "worktree", "prune")


def earlier_pass(root: Path, push: Push, selection: Selection, runs: list[Lint],
                 options: argparse.Namespace, what: str) -> tuple[str | None, bool]:
    """The key to remember this lint under, and whether the push stands on a pass already.

    A pass is remembered by the push's own patch, the packages it lints, and the kinds of run
    it makes; a rebase that resolves nothing keeps all of them. The lint reads the pushed
    commit's files (`PushedTree`), so files on disk that are not pushed change neither.
    """
    if options.range:
        return None, False
    naming = own_key(root, push, selection, runs)
    if naming is None:
        return None, False
    key, patches = naming
    passed = passed_before(root, key)
    if passed is None:
        return key, False
    head, when = passed
    since = f" from {when}" if when else ""
    say(f"pre-push: reusing the {what} pass for patch {patches}{since}"
        + (f" (passed at {head[:12]})" if head else "")
        + ": this is the same change replayed onto newer commits. t's checks judge it with "
        "main's changes on main")
    return None, True


def surface_only(root: Path, push: Push, options: argparse.Namespace,
                 tree: PushedTree | None = None) -> int:
    """Lint a push that changes an app's published surface and nothing Rust or Cargo.

    An app's sources and surface file move its committed catalogue entry without a crate of
    the workspace changing, so the lint has no package to lint and would
    otherwise say there is nothing to do. The checks that judge the generated files are t's; without t there
    is none to run, as no other check of this lint runs without a cargo.
    """
    checks = surface_checks(push, scoped_checks(root, push))
    if options.plan:
        catalogue_plan(push)
        if checks:
            print(f"t: {' '.join(checks)}")
        return 0
    if not checks:
        say("pre-push: t names no check for what this push changes; nothing to lint")
        return 0
    say(f"pre-push: linting the app surface's generated files through t ({' '.join(checks)})")
    formatting = formatting_run(root, push) if rustfmt_available() else ["t", "test", "fmt"]
    runs = lint_runs(root, Selection([], {}), formatting, checks)
    key, reused = earlier_pass(root, push, Selection([], {}), runs, options, "surface")
    if reused:
        return 0
    lint_tree = tree.where() if tree is not None else root
    results, elapsed = run_together(root, runs, options.keep_logs,
                                    None if lint_tree == root else lint_tree)
    status = report(results, elapsed, f"the app surface's generated files ({' '.join(checks)})",
                    t_fixes(root), root, push, {})
    if status == 0 and key is not None:
        remember_pass(root, key, push.tips[0])
    return status


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("remote", nargs="?", default="origin", help="the remote being pushed to")
    parser.add_argument("--plan", action="store_true", help="print what the push would lint, and lint nothing")
    parser.add_argument("--range", metavar="BASE..HEAD",
                        help="lint what that range changes, for a run outside a push (CI)")
    parser.add_argument("--keep-logs", metavar="DIR", type=Path,
                        help="keep each run's whole output, and how long it took, in DIR")
    parser.add_argument("--only", metavar="PACKAGES",
                        help="lint only these of the packages the range reaches, comma-separated "
                             "(to relint only some of what a range reaches)")
    parser.add_argument("--stdin", type=argparse.FileType("r"), default=sys.stdin,
                        help="the ref lines Git writes for a pre-push hook")
    options = parser.parse_args(argv)
    root = Path(git(Path.cwd(), "rev-parse", "--show-toplevel").stdout.strip())
    if options.range:
        base, _, head = options.range.partition("..")
        if not base or not head:
            parser.error("--range takes BASE..HEAD")
        push = Push(diff(root, base, head), [(base, head)], [head], [])
    else:
        push = pushed(root, options.stdin, options.remote)
    if push.behind:
        say(f"pre-push: main is at {push.behind[0][:12]} now, which this push does not contain, so "
            "the remote refuses it whatever a lint says; nothing was linted. Rebase onto it "
            "and push again: tools/land.py does both, as often as main moves")
        return 1
    if push.unnamed:
        say("pre-push: refusing a push the check cannot name")
        return 1
    # The catalogues are judged before any lint and before the early answer for a
    # push that changes no Rust: a translation left behind is drift on its own.
    if not options.plan and catalogue_range(root, push) != 0:
        return 1
    if not options.plan and builder_licenses_range(root, push) != 0:
        return 1
    files = rust_changes(push.files)
    if not files:
        if surface_change(push.files) and lints_through_t():
            if tip := unchecked_tree(root, push):
                say(f"pre-push: {tip[:12]} is not the commit checked out here, and the lint reads "
                    "the files on disk: check it out and push again")
                return 1
            with PushedTree(root, push.tips[0],
                            [] if options.plan else unpushed_files(root)) as tree:
                return surface_only(root, push, options, tree)
        if options.plan:
            catalogue_plan(push)
        if push.comparisons:
            say("pre-push: no Rust or Cargo file changed; nothing to lint")
        else:
            say("pre-push: no pushed ref has a range to lint (said above why); nothing to lint")
        return 0
    if tip := unchecked_tree(root, push):
        say(f"pre-push: {tip[:12]} is not the commit checked out here, and the lint reads the files "
            "on disk: check it out and push again")
        return 1
    with PushedTree(root, push.tips[0], [] if options.plan else unpushed_files(root)) as tree:
        return lint_packages(root, push, files, options, tree)


def lint_packages(root: Path, push: Push, files: set[str], options: argparse.Namespace,
                  tree: PushedTree) -> int:
    """Lint the packages the push changes, from the pushed commit's files.

    The pass's key is computed where the files it reads are the pushed commit's: the checkout
    itself unless what is not pushed there includes a manifest or a file naming the checks, so a
    push standing on a remembered pass checks nothing out.
    """
    work = tree.where() if reads_key_input(tree.unpushed) else root
    try:
        selection = select(work, push, files)
    except (subprocess.CalledProcessError, RuntimeError) as error:
        say(f"pre-push: cargo metadata could not name the packages this push touches: {error}")
        return 1
    if selection is None:
        say("pre-push: refusing the push until the changed packages can be named")
        return 1
    if options.only is not None:
        only = set(filter(None, options.only.split(",")))
        selection = Selection(sorted(set(selection.native) & only), selection.metadata,
                              sorted(set(selection.dependants) & only))
        if not selection.native:
            say("pre-push: none of the packages asked for is one this range reaches; nothing to lint")
            return 0
    if not selection.native:
        # Running the surface checks needs no package of the workspace, and a .tl source can
        # belong to none while still moving a committed catalogue entry.
        if surface_change(push.files) and lints_through_t():
            return surface_only(root, push, options, tree)
        say("pre-push: the changed Rust files belong to no workspace package; CI builds them outside it")
        return 0
    if options.plan:
        print(f"native: {' '.join(selection.native)}")
        if selection.dependants:
            print(f"dependants: {' '.join(selection.dependants)}")
        if lints_through_t():
            members = set(package_maps(selection.metadata, work)[0])
            named = t_checks(work, selection.linted, members)[0]
            scoped = scoped_checks(work, push)
            extra = [*surface_checks(push, scoped), *catalogue_checks(push), *shell_checks(push),
                     *policy_checks(work, push, scoped)]
            named += [name for name in extra if name not in named]
            print(f"t: {' '.join(named)}")
        catalogue_plan(push)
        return 0
    checks = None
    if lints_through_t():
        try:
            members = set(package_maps(selection.metadata, work)[0])
            checks, uncovered = t_checks(work, selection.linted, members)
            scoped = scoped_checks(work, push)
            extra = [*surface_checks(push, scoped), *catalogue_checks(push), *shell_checks(push),
                     *policy_checks(work, push, scoped)]
            checks += [name for name in extra if name not in checks]
        except (OSError, ValueError, KeyError) as error:
            say(f"pre-push: cannot read which of t's checks lint these packages from {T_TOML}: {error}")
            return 1
        if uncovered:
            say(f"pre-push: no clippy family of t's lints {', '.join(uncovered)}, so neither does "
                "this check")
    elif shutil.which(cargo_command()[0]) is None:
        say(f"pre-push: there is no cargo to lint with ({cargo_command()[0]}); the lint cannot run")
        return 1
    # A machine without rustfmt of its own formats through t, whose `fmt` check runs the
    # pinned toolchain; one with a rustfmt must hold the pinned version (the run below).
    formatting = formatting_run(work, push) if rustfmt_available() else ["t", "test", "fmt"]
    runs = lint_runs(work, selection, formatting, checks)
    # A lint of files that are not the pushed commit's speaks for no change to stand on later.
    key, reused = earlier_pass(work, push, selection, runs, options, "clippy")
    if reused:
        return 0
    say(f"pre-push: linting {packages(len(selection.linted))}"
        + (f" and compiling {len(selection.dependants)} direct dependant(s) of what changed"
           if selection.dependants else "")
        + (f" through t ({' '.join(checks)})" if checks else ""))
    checked = (f"{'clippy and rustfmt' if formatting else 'clippy (no rustfmt on PATH)'} "
               f"for {packages(len(selection.linted))}"
               + (f", a compile of {packages(len(selection.dependants))} built on them"
                  if selection.dependants else ""))
    lint_tree = tree.where()
    results, elapsed = run_together(root, runs, options.keep_logs,
                                    None if lint_tree == root else lint_tree)
    status = report(results, elapsed, checked, t_fixes(work) if lints_through_t() else set(),
                    work, push, selection.metadata)
    if status == 0 and key is not None:
        remember_pass(work, key, push.tips[0])
    return status


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
