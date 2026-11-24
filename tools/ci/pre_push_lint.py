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
family names is also linted for wasm32, a target the host lint cannot see.
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

A lint that passed stands for the same change after a rebase that leaves it alone: a pass is
remembered by the push's own patch (`git patch-id`) and the trees of the linted packages' own
directories at its head. A push retried after main moved, whose rebase resolved nothing and whose
packages' directories main's new commits did not touch, has both the same and is sent without
linting again, however long the lint took and however much main changed elsewhere, dependencies
of those packages included. What only the combination breaks is t's clippy families' to find on
main, and the next commit's to fix. The passes are kept in the repository's git directory.

The lint reads the files on disk, so a push is refused when what it sends is not
the tree checked out here: a lint that spoke for another commit would say
nothing about this one. A push whose changed packages the check cannot name is
refused too, rather than passed unread.

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
from typing import Iterable, NamedTuple

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


def surface_checks(root: Path, push: Push) -> list[str]:
    """t's checks over what a surface or an action or record type writes, when the push reaches them.

    The app-fleet catalogue and the SDK's generated types are written from the apps' surfaces
    and from the action and record types of every crate, so the list of what moves them is
    t's own: `t scope` names the checks a commit's change touches. A push that reaches one
    and does not regenerate refuses here, not on main.
    """
    wanted = ("sdk-policy", "sdk-types")
    if push.tips:
        try:
            scoped = subprocess.run(["t", "scope", "--rev", push.tips[0], "--names"],
                                    cwd=root, capture_output=True, text=True, check=False)
        except OSError:
            scoped = None
        if scoped is not None and scoped.returncode == 0:
            named = set(scoped.stdout.split())
            return [name for name in wanted if name in named]
    # t could not tell: an app's surface alone still moves the generated catalogue and types.
    if any(path.startswith(("apps/", "crates/desktop/app-fleet/")) for path in push.files):
        return list(wanted)
    return []


# What the component catalogue (`crates/ui/ui-graph/components.json`) is
# generated from: the toolkit's widgets and the window that builds them by name.
# The catalogue is committed, so a push that changes one of these without
# regenerating it lands it stale.
CATALOGUE_INPUTS = (
    "crates/ui/ui-core/src/",
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


def run_together(root: Path, runs: list[Lint],
                 keep: Path | None = None) -> tuple[list[tuple[Lint, int, str]], float]:
    """Start every run at once and collect them, so the wall time is the slowest one.

    With `keep`, each run's whole output is written there, headed by its command and how long
    it took: what tells a lint that compiled everything from one that compiled what changed.
    """
    started = time.monotonic()
    running = []
    for run in runs:
        # A file, not a pipe: a run whose output outgrows the pipe buffer must
        # not wait on a reader that is still waiting on another run.
        log = tempfile.TemporaryFile(mode="w+")
        process = subprocess.Popen(run.command, cwd=root, stdout=log, stderr=subprocess.STDOUT, text=True,
                                   env=landing_env(root))
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
    """Each check's result from t's output, and the errors t gave instead of results."""
    finished, errors = [], []
    for document in t_documents(output):
        if document.get("type") == "error":
            errors.append(document)
        elif isinstance(document.get("body"), dict) and isinstance(document["body"].get("finished"), dict):
            finished.append(document["body"]["finished"])
    return finished, errors


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


def say_t_failure(output: str) -> bool:
    """Each failed check of t's with its diagnostics and the command that prints its whole log;
    False when t's output names none."""
    finished, errors = t_outcomes(output)
    failed = [result for result in finished if result.get("outcome") != "pass"]
    for result in failed:
        say(f"  {result['check']}: {result.get('outcome')} - whole log: t log {result.get('log')}")
        say_lines(str(result.get("excerpt") or "").strip().splitlines())
    for error in errors:
        say(f"  t: {error.get('message')}")
        if error.get("hint"):
            say(f"  {error['hint']}")
    return bool(failed or errors)


def report(results: list[tuple[Lint, int, str]], elapsed: float, checked: str) -> int:
    for run, status, output in results:
        if run.t and status == 0:
            say_t_results(output)
    failed = [(run, output) for run, status, output in results if status != 0]
    if not failed:
        say(f"pre-push: lint passed in {elapsed:.0f}s ({checked})")
        return 0
    for run, output in failed:
        command = [part for part in run.command if part != "--json"] if run.t else run.command
        say(f"pre-push: {run.label} failed - rerun: {shlex.join(command)}")
        if run.t and say_t_failure(output):
            continue
        say_lines([line for line in output.strip().splitlines()
                   if not any(remark in line for remark in CARGO_REMARKS)
                   and not PROGRESS.match(line)])
    say("pre-push: refusing the push until the lint passes")
    return 1


def own_key(root: Path, push: Push, runs: list[Lint]) -> tuple[str, str] | None:
    """What a pass of this push's lint is remembered by, and the patches it names.

    The push's own patches and the runs it makes. A rebase changes neither: the same
    change replayed onto newer commits has the same patch-id, and its runs are the
    changed packages', which the patch decides. A rebase that resolves a conflict
    into another change has another patch-id, so it is linted. What main's commits
    since then did to a linted package's own files does not enter here: t's clippy
    families lint those interactions on main, and a red main is fixed forward.
    """
    # The format check names the commits it compares, which a rebase changes; the change it reads
    # is the patch below.
    commits = {commit for comparison in push.comparisons for commit in comparison}
    parts = [shlex.join(part for part in run.command if part not in commits) for run in runs]
    patches = []
    for base, head in push.comparisons:
        patch = subprocess.run(["git", "diff", "--no-renames", "--binary", f"{base}..{head}", "--"],
                               cwd=root, capture_output=True, check=False)
        identity = subprocess.run(["git", "patch-id", "--stable"], cwd=root, input=patch.stdout,
                                  capture_output=True, check=False).stdout.split()
        if patch.returncode != 0 or not identity:
            return None
        patches.append(identity[0].decode())
        parts.append(patches[-1])
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
            "and push again")
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
    try:
        selection = select(root, push, files)
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
        say("pre-push: the changed Rust files belong to no workspace package; CI builds them outside it")
        return 0
    if options.plan:
        print(f"native: {' '.join(selection.native)}")
        if selection.dependants:
            print(f"dependants: {' '.join(selection.dependants)}")
        if lints_through_t():
            members = set(package_maps(selection.metadata, root)[0])
            named = t_checks(root, selection.linted, members)[0]
            named += [name for name in catalogue_checks(push) if name not in named]
            print(f"t: {' '.join(named)}")
        catalogue_plan(push)
        return 0
    checks = None
    if lints_through_t():
        try:
            members = set(package_maps(selection.metadata, root)[0])
            checks, uncovered = t_checks(root, selection.linted, members)
            extra = [*surface_checks(root, push), *catalogue_checks(push)]
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
    dirty = bool(git(root, "status", "--porcelain").stdout.strip())
    if dirty:
        say("pre-push: the worktree has uncommitted changes; the lint covers the files on disk, which "
            "is not quite the pushed commit")
    # A machine without rustfmt of its own formats through t, whose `fmt` check runs the
    # pinned toolchain; one with a rustfmt must hold the pinned version (the run below).
    formatting = formatting_run(root, push) if rustfmt_available() else ["t", "test", "fmt"]
    runs = lint_runs(root, selection, formatting, checks)
    # A lint of files that are not the pushed commit's speaks for no change to stand on later.
    naming = None if options.range or dirty else own_key(root, push, runs)
    key, patches = naming if naming is not None else (None, "")
    if key is not None and (passed := passed_before(root, key)):
        head, when = passed
        since = f" from {when}" if when else ""
        say(f"pre-push: reusing the clippy pass for patch {patches}{since}"
            + (f" (passed at {head[:12]})" if head else "")
            + ": this is the same change replayed onto newer commits. t's clippy families lint "
            "it with main's changes on main")
        return 0
    say(f"pre-push: linting {packages(len(selection.linted))}"
        + (f" and compiling {len(selection.dependants)} direct dependant(s) of what changed"
           if selection.dependants else "")
        + (f" through t ({' '.join(checks)})" if checks else ""))
    checked = (f"{'clippy and rustfmt' if formatting else 'clippy (no rustfmt on PATH)'} "
               f"for {packages(len(selection.linted))}"
               + (f", a compile of {packages(len(selection.dependants))} built on them"
                  if selection.dependants else ""))
    results, elapsed = run_together(root, runs, options.keep_logs)
    status = report(results, elapsed, checked)
    if status == 0 and key is not None:
        remember_pass(root, key, push.tips[0])
    return status


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
