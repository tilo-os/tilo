#!/usr/bin/env python3
"""Lint what a push can break before its refs leave, natively and for the page.

Git runs `.githooks/pre-push` with one line per ref it is updating on stdin:

    <local ref> <local oid> <remote ref> <remote oid>

Only a push to main is linted. Work in progress goes to `integration` (or a
branch of its own) at once, and is tested once when it is done:
tools/integrate.sh merges main into it, runs `t` over the result, and moves main
to it, which is the push to main this lint runs for.

A push to main lints the packages its commits own: the changed files through the
repository's own format check, and Clippy over every target of the host with the
features the workspace's unified build gives each package (so code behind a
feature that no default build enables is compiled). Those features are the same
whichever packages are selected, so one push's build stays warm for the next.

`--wide` adds what a push leaves out, and `t`'s lint-head check runs it on every
commit of main, so a commit that breaks one of them is named there: the packages
whose own source names something the commits changed (a rename, a removed item
or a field added to a struct breaks the crates that use it, not the crate that
declares it), the packages CI also checks as they ship, and Clippy for
`wasm32-unknown-unknown` on the ones that build for the page, under the page's
own Clippy configuration (std's clocks are banned there). A push used to run
them too; with main moving every minute, their minutes of rebuilding lost the
race to the next commit, again on each retry.

The Clippy runs start together, so a one-crate push costs about one build. A run
that passed is not repeated while everything it reads is unchanged
(tools/ci/lint_inputs.py), so a push retried after a rebase onto unrelated
commits does not lint again. Commits that passed are not linted again either
when they are pushed once more as they were, rebased onto a newer main, within
REBASE_WINDOW: main takes a commit every minute or two and a lint takes minutes,
so a push that relints after every rebase loses to the next commit each time and
never lands. What the commits main gained in between do to them is what `t`'s
lint-head says on main. Cargo compiles wherever `$CARGO` points when it is
set - any cargo drop-in, which may build on another machine - and otherwise the
`cargo` on PATH. Formatting is always checked by the `cargo` and `rustfmt` on
PATH: they compile nothing, so they need no other machine.

The lint reads the files on disk, so a push is refused when what it sends is not
the tree checked out here: a lint that spoke for another commit would say
nothing about this one. A push whose changed packages the check cannot name, or
whose page lint cannot run here, is refused too, rather than passed unread.

There is no switch to skip the lint: `git push --no-verify` is Git's own
escape, and CONTRIBUTING.md asks contributors never to use it.

    tools/ci/pre_push_lint.py --plan < pre-push-stdin    print what a push would lint
    tools/ci/pre_push_lint.py --range BASE..HEAD         lint what that range changes
    tools/ci/pre_push_lint.py --wide --range BASE..HEAD  also its dependents, as shipped, and the page
"""
from __future__ import annotations

import argparse
import hashlib
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib
from typing import Iterable, NamedTuple

TOOLS = Path(__file__).resolve().parents[1]
if str(TOOLS) not in sys.path:
    sys.path.append(str(TOOLS))
from ci import package_features, suites, wasm_clippy_conf
from ci.package_sets import PAGE_ENTRY, browser_crates, page_closure, wasm_crates
from ci.lint_inputs import Workspace
from ci.workspace_graph import (
    cargo_metadata, closure, file_at, lockfile_change, package_maps, reverse_dependencies,
)

WASM_TARGET = "wasm32-unknown-unknown"
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
UNIFY_MANIFEST = Path("crates/dev/workspace-unify/Cargo.toml")
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
# The branch everyone lands on, and the only one a push is linted for: work in progress goes to
# `integration` or a branch of its own unlinted, and is tested once when it is done
# (tools/integrate.sh). A push to main that is not a fast-forward is a race already lost:
# nobody force-pushes main, so the remote refuses it after any lint.
MAIN_REF = "refs/heads/main"
# Where work in progress lands. It is pushed unlinted, but never rewritten: a push that drops
# commits from it drops someone else's work, so the one check on it is that it fast-forwards.
INTEGRATION_REF = "refs/heads/integration"
# How many passing lint keys a clone keeps.
KEPT_PASSES = 256
# How long commits that passed the lint are pushed again without it after a rebase that left them
# as they were. Long enough for a push to be retried until it wins the race for main's head, short
# enough that main has not become another tree under them.
REBASE_WINDOW = 20 * 60
# Identifiers of the language itself, and words every crate uses, say nothing
# about which crate names what a change did: a dependent is picked by the names
# a change removed or declared, never by these.
KEYWORDS = frozenset("""as async await box break const continue crate dyn else enum extern false
fn for if impl in let loop macro_rules match mod move mut pub raw ref return self Self static
struct super trait true try type union unsafe use where while""".split())
EVERYDAY = frozenset("""allow cfg cfg_attr default derive deny doc expect allow warn Option Result
Some None Ok Err String Vec Box Arc Rc HashMap HashSet BTreeMap default new from into try_from
clone eq ne fmt debug display hash iter map filter collect extend push pop insert remove get set
len is_empty as_ref as_mut as_str as_deref to_string to_owned iter_mut into_iter collect
Default Clone Copy Debug Display Send Sync Sized Drop From Iterator Into PartialEq Eq Hash
Self self serde derive macro use super crate""".split())
# A line that declares a name another crate can spell: an item, or a field of a
# public struct, which a dependent's own struct literal has to name.
DECLARED_ITEM = re.compile(
    r"^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?(?:default[ \t]+)?(?:const[ \t]+)?(?:async[ \t]+)?"
    r"(?:unsafe[ \t]+)?(?:extern[ \t]+\"[^\"]*\"[ \t]+)?"
    r"(?:struct|enum|trait|fn|type|union|static|const|mod)[ \t]+([A-Za-z_][A-Za-z0-9_]*)",
    re.MULTILINE)
DECLARED_MACRO = re.compile(r"^[ \t]*macro_rules![ \t]*([A-Za-z_][A-Za-z0-9_]*)", re.MULTILINE)
DECLARED_FIELD = re.compile(r"^[ \t]*pub[ \t]+([A-Za-z_][A-Za-z0-9_]*)[ \t]*:", re.MULTILINE)
# Names a change names: the dependents are searched for these in one pass. Above
# this many the change reaches too much of the workspace for a word match to
# narrow, so every dependent is linted instead; the number is about how long a
# command line may be, not how many crates a change may touch.
NAMED_LIMIT = 2000


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
    """One lint run of the push check: what it is called, the command that reruns it, and what it reads.

    `packages` are linted; with `dependencies` the run also reads every crate they build on.
    """
    label: str
    command: list[str]
    packages: list[str]
    dependencies: bool
    env: dict[str, str] | None = None
    # A run too cheap to be worth keying: formatting a handful of files. It is
    # always made, because what it reads is the changed files rather than the
    # packages, and a key of the packages would speak for them.
    reusable: bool = True


class Push(NamedTuple):
    """What the refs being pushed change, and what the check could not name about them."""
    files: set[str]
    comparisons: list[tuple[str, str]]
    tips: list[str]
    unnamed: list[str]
    # Pushes to main that main's own head, as the remote reported it when the push began, is not
    # part of: the remote refuses them whatever the lint says.
    behind: list[str] = []
    # Pushes that would rewrite integration: its head on the remote is not part of what is sent.
    rewrites: list[str] = []
    # The heads this push sends to main, and to integration.
    main_heads: list[str] = []
    integration_heads: list[str] = []


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


def pushed(root: Path, lines: Iterable[str]) -> Push:
    """Every file a push to main changes, and its comparison; pushes to other branches are not linted."""
    files: set[str] = set()
    comparisons: list[tuple[str, str]] = []
    tips: list[str] = []
    unnamed: list[str] = []
    behind: list[str] = []
    rewrites: list[str] = []
    main_heads: list[str] = []
    integration_heads: list[str] = []
    for line in lines:
        parts = line.split()
        if len(parts) != 4:
            continue
        ref, head, remote_ref, remote_oid = parts
        if set(head) != ZERO_OID:
            if remote_ref == MAIN_REF:
                main_heads.append(head)
            elif remote_ref == INTEGRATION_REF:
                integration_heads.append(head)
        if (remote_ref == INTEGRATION_REF and set(remote_oid) != ZERO_OID
                and not contains(root, head, remote_oid)):
            rewrites.append(remote_oid)
            continue
        if remote_ref != MAIN_REF or set(head) == ZERO_OID:
            continue  # work in progress, or a deleted ref, which changes nothing that is built
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
    return Push(files, comparisons, tips, unnamed, behind, rewrites, main_heads,
                integration_heads)


# Set in the environment of a coding agent's shell. A push to main from one is held to the rule
# that main only fast-forwards to what integration holds; a person pushing by hand is not.
AGENT_MARKERS = ("AI_AGENT", "CLAUDECODE")


def unintegrated(root: Path, remote: str, push: Push) -> str | None:
    """A head this push sends to main that integration does not hold, if any.

    main moves only to a commit integration already has: tools/integrate.sh pushes the tested
    commit to both in one atomic push, so it is in this push's own integration head. A push
    that sends main alone is checked against the remote's integration as it stands now.
    """
    if not push.main_heads:
        return None
    integrations = list(push.integration_heads)
    if not integrations:
        listed = git(root, "ls-remote", remote, INTEGRATION_REF)
        integrations = [line.split()[0] for line in listed.stdout.splitlines() if line.strip()]
    for head in push.main_heads:
        if not any(contains(root, integration, head) for integration in integrations):
            return head
    return None


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


def declarations(text: str) -> set[str]:
    """The names a source text declares: an item, a macro, or a public field."""
    return (set(DECLARED_ITEM.findall(text)) | set(DECLARED_MACRO.findall(text))
            | set(DECLARED_FIELD.findall(text)))


def named_by_change(names: set[str]) -> set[str]:
    """The words of a change worth matching a dependent against."""
    return {name for name in names
            if len(name) >= 3 and name not in KEYWORDS and name not in EVERYDAY}


def changed_names(root: Path, comparisons: list[tuple[str, str]], files: set[str]) -> set[str]:
    """The names the pushed commits stop offering, and the ones they start declaring.

    What a dependent can break on is a declared name: an item or a public field
    it names goes away, or a public field it has to spell in a struct literal
    appears. A value on a changed line is nobody's to depend on, and an import
    line names what its own crate borrows, not what another crate offers.
    """
    paths = sorted(path for path in files if path.endswith(RUST_SUFFIXES))
    if not paths:
        return set()
    names: set[str] = set()
    for base, head in comparisons:
        result = subprocess.run(
            ["git", "diff", "-U0", "--no-renames", f"{base}..{head}", "--", *paths],
            cwd=root, capture_output=True, check=False,
        )
        for line in result.stdout.decode("utf-8", "surrogateescape").splitlines():
            if line.startswith(("---", "+++")):
                continue
            if line.startswith(("-", "+")):
                names |= declarations(line[1:])
    return named_by_change(names)


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


def naming(root: Path, by_dir: dict[Path, str], candidates: set[str], names: set[str]) -> list[str]:
    """The candidates whose own source names what the change did.

    One pass of Git's own search, over the candidates' sources only: a search
    of the whole tree reads the vendored crates and shipped assets that no
    member compiles from source, which costs more than everything else here.
    """
    if not names or not candidates:
        return []
    patterns = [item for name in sorted(names) for item in ("-e", name)]
    by_directory = {directory.as_posix(): name for directory, name in by_dir.items()}
    directories = {directory for directory, name in by_directory.items() if name in candidates}
    sources = [pattern for directory in sorted(directories)
               for pattern in (f"{directory}/*.rs", f"{directory}/*.tl")]
    result = subprocess.run(["git", "grep", "-l", "-z", "-F", "-w", *patterns, "--", *sources],
                            cwd=root, capture_output=True, check=False)
    found = {owner_of(path, by_directory) for path in
             result.stdout.decode("utf-8", "surrogateescape").split("\0") if path}
    return sorted(found & candidates)


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
        # A change outside the workspace table - the lint table, a profile, a
        # patch - decides how every member is built.
        if {key: value for key, value in old.items() if key != "workspace"} != {
                key: value for key, value in new.items() if key != "workspace"}:
            return None
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
    """What a push is linted as: the packages, and the dependents that made them that many."""
    native: list[str]
    web: list[str]
    dependents: list[str]
    metadata: dict


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


def select(root: Path, push: Push, files: set[str], wide: bool = True) -> Selection | None:
    """The packages to lint for the host and the page, or None when the push cannot be named.

    Those are the packages the commits own or read the changed files through,
    plus every package whose own source names something they changed: a rename,
    a removed item or a field added to a struct breaks the crates that use it.
    The page lint covers the same crates that build for it, and the page's entry
    when the change reaches anything the page compiles. Without `wide`, only the
    packages the commits own are linted, for the host alone.
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
                "reaches; that reaches the whole workspace and only CI builds all of it")
            return None
        seeds |= reached
    if WORKSPACE_MANIFEST in files:
        reached = manifest_seeds(root, push, metadata)
        if reached is None:
            say(f"pre-push: {WORKSPACE_MANIFEST}'s change decides how every member is built, which "
                "only a workspace-wide lint speaks for (`t cargo clippy --workspace`)")
            return None
        seeds |= reached
    if not wide:
        return Selection(sorted(seeds), [], [], metadata)
    names = changed_names(root, push.comparisons, files)
    dependents = closure(seeds, reverse_dependencies(packages, root)) - seeds
    if len(names) > NAMED_LIMIT:
        say(f"pre-push: the change names {len(names)} items, too many to say which of the "
            f"{len(dependents)} packages that build on these use one: they are all linted")
        named = sorted(dependents)
    else:
        named = naming(root, by_dir, dependents, names)
    native = sorted(seeds | set(named))
    web = (set(native) & wasm_crates(packages))
    if set(native) & (page_closure(packages) | browser_crates(packages)):
        web |= {PAGE_ENTRY}
    web &= members
    return Selection(native, sorted(web), named, metadata)


def unified_features(root: Path, selected: Iterable[str]) -> list[str]:
    """The features the workspace's whole build gives the selected packages.

    crates/dev/workspace-unify unifies every family's build for the members, in
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


def page_clippy_config(root: Path) -> dict[str, str]:
    """The environment of the page's lint: the workspace's Clippy configuration, with std's clocks banned.

    The same configuration tools/ci/check-wasm.sh lints with. A configuration
    that could not be written is said, not silently replaced by the root one:
    the clocks are why the run exists.
    """
    directory = Path(os.environ.get("CARGO_TARGET_DIR", str(root / "target"))) / "wasm-clippy"
    result = subprocess.run([sys.executable, "-I", str(TOOLS / "ci/wasm_clippy_conf.py"), str(directory)],
                            cwd=root, capture_output=True, text=True, check=False)
    if result.returncode != 0 or not (directory / "clippy.toml").is_file():
        say("pre-push: the page's Clippy configuration could not be written, so the page lint runs "
            f"without it: {(result.stderr or result.stdout).strip()}")
        return {}
    return {"CLIPPY_CONF_DIR": str(directory)}


def lint_runs(root: Path, selection: Selection, web: list[str], formatting: list[str],
              own_build_dirs: bool = False, wide: bool = True) -> list[Lint]:
    """The runs the check makes: formatting, the host lint, and the page lint where it can run.

    `web` is what is actually linted for the page, so a toolchain without the
    target leaves it empty. Each command is also the line a failure prints to
    rerun it.

    With `own_build_dirs`, the lints as the packages ship and for the page build in build
    directories of their own. Cargo locks a build directory for the whole of a build, so the
    three otherwise waited for one another instead of starting together; a build directory
    of their own costs a first build each.
    """
    cargo = cargo_command()

    def build_dir(name: str) -> list[str]:
        return ["--config", f'build.build-dir="target/{name}"'] if own_build_dirs else []
    native = selection.native
    selected = [item for name in native for item in ("-p", name)]
    runs = []
    if formatting:
        runs.append(Lint("rustfmt", formatting, native, False, None, False))
    runs.append(Lint("clippy (native)",
                     [*cargo, "clippy", "--locked", *selected, *unified_features(root, native),
                      "--all-targets", "--keep-going", "--", "-D", "warnings"], native, True))
    # The packages CI lints as they ship too (tools/ci/package_features.py): a
    # lint that only sees them with extra features never builds that shape.
    shipped = package_features.default_builds(native) if wide else []
    if shipped:
        runs.append(Lint("clippy (native, as it ships)", [
            *cargo, "clippy", "--locked", *build_dir("lint-as-shipped"),
            *(item for name in shipped for item in ("-p", name)),
            "--all-targets", "--keep-going", "--", "-D", "warnings",
        ], shipped, True))
    if web:
        page = [item for name in web for item in ("-p", name)]
        runs.append(Lint("clippy (wasm32)", [
            *cargo, "clippy", "--locked", *build_dir("lint-page"), "--target", WASM_TARGET, *page,
            "--", "-D", "warnings",
        ], web, True, page_clippy_config(root)))
    return runs


def rustfmt_available() -> bool:
    """Whether the repository's format check can run here.

    It calls rustfmt on the changed files itself, so it needs that binary and
    nothing from the cargo `$CARGO` may point at.
    """
    return shutil.which("rustfmt") is not None


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
                                   env={**os.environ, **(run.env or {})})
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


class Passes:
    """The keys of lint runs that passed, kept in the clone's Git directory for all its worktrees."""

    def __init__(self, root: Path):
        common = Path(git(root, "rev-parse", "--git-common-dir").stdout.strip())
        self.directory = (root / common) / "pre-push-lint"

    def passed(self, key: str | None) -> bool:
        return key is not None and (self.directory / key).is_file()

    def passed_since(self, key: str | None, seconds: float) -> float | None:
        """How long ago the run of `key` passed, when that is within `seconds`."""
        if key is None:
            return None
        try:
            age = time.time() - (self.directory / key).stat().st_mtime
        except FileNotFoundError:
            return None
        return age if 0 <= age <= seconds else None

    def keep(self, key: str | None, label: str) -> None:
        if key is None:
            return
        self.directory.mkdir(parents=True, exist_ok=True)
        (self.directory / key).write_text(label + "\n")
        # Every worktree of the clone records here, and pushes run at the same time: an entry
        # another of them pruned between the listing and its stat is not one to keep or to fail on.
        entries = []
        for entry in self.directory.iterdir():
            try:
                entries.append((entry.stat().st_mtime, entry))
            except FileNotFoundError:
                continue
        entries.sort(key=lambda modified: modified[0])
        for _modified, stale in entries[:-KEPT_PASSES]:
            stale.unlink(missing_ok=True)


def change_key(root: Path, push: Push) -> str | None:
    """What the pushed commits change, whatever they stand on, or None when that cannot be named.

    A commit's patch id is the same before and after a rebase that applied it unchanged, and
    another one as soon as a conflict was resolved or the commit amended. A merge has none, and
    neither has a worktree that is not the commit: both are linted.
    """
    if git(root, "status", "--porcelain").stdout.strip():
        return None
    digest = hashlib.sha256()
    for start, head in push.comparisons:
        commits = git(root, "rev-list", "--reverse", "--parents", f"{start}..{head}").stdout.split("\n")
        commits = [line.split() for line in commits if line.strip()]
        if not commits or any(len(parents) != 2 for parents in commits):
            return None
        patches = subprocess.run(["git", "log", "--reverse", "--patch", "--no-color", "--no-ext-diff",
                                  "--format=commit %H", f"{start}..{head}"],
                                 cwd=root, capture_output=True, text=True, check=False)
        named = subprocess.run(["git", "patch-id", "--stable"], cwd=root, input=patches.stdout,
                               capture_output=True, text=True, check=False)
        ids = [line.split()[0] for line in named.stdout.splitlines() if line.strip()]
        if patches.returncode or named.returncode or len(ids) != len(commits):
            return None  # an empty commit has no patch id: nothing says it is the one that passed
        for patch in ids:
            digest.update(patch.encode() + b"\n")
        digest.update(b"\0")
    return f"change-{digest.hexdigest()}"


def lint_keys(root: Path, metadata: dict, runs: list[Lint]) -> list[str | None]:
    """Each run's key, or None for every run when the inputs cannot be named.

    The lint reads the files on disk, so only a clean worktree is keyed by its commit.
    """
    if git(root, "status", "--porcelain").stdout.strip():
        return [None] * len(runs)
    try:
        workspace = Workspace(root, metadata)
        return [workspace.key(run.packages, run.dependencies, run.command) if run.reusable else None
                for run in runs]
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        say(f"pre-push: cannot name what the lint reads, so earlier passes are not reused: {error}")
        return [None] * len(runs)


def packages(count: int) -> str:
    return f"{count} package" if count == 1 else f"{count} packages"


def report(results: list[tuple[Lint, int, str]], elapsed: float, checked: str) -> int:
    failed = [(run, output) for run, status, output in results if status != 0]
    if not failed:
        say(f"pre-push: lint passed in {elapsed:.0f}s ({checked})")
        return 0
    for run, output in failed:
        say(f"pre-push: {run.label} failed - rerun: {shlex.join(run.command)}")
        lines = [line for line in output.strip().splitlines()
                 if not any(remark in line for remark in CARGO_REMARKS) and not PROGRESS.match(line)]
        for line in lines[:SHOWN_DIAGNOSTIC_LINES]:
            say(f"  {line}")
        if len(lines) > SHOWN_DIAGNOSTIC_LINES:
            say(f"  ... {len(lines) - SHOWN_DIAGNOSTIC_LINES} more line(s); the rerun above prints them")
    say("pre-push: refusing the push until the lint passes")
    return 1


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
                        help="lint what that range changes, for a run outside a push (CI, t)")
    parser.add_argument("--wide", action="store_true",
                        help="also lint the dependents that name what changed, the packages as they "
                             "ship, and the page (what t's lint-head runs on every commit of main)")
    parser.add_argument("--own-build-dirs", action="store_true",
                        help="build the lints as packages ship and for the page in build directories "
                             "of their own, so all three start together")
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
        push = pushed(root, options.stdin)
    if push.rewrites:
        say(f"pre-push: integration is at {push.rewrites[0][:12]}, which this push does not contain, "
            "so it would drop the commits after it, which are other people's work. Fetch, rebase "
            "or merge onto origin/integration, and push again")
        return 1
    if any(os.environ.get(marker) for marker in AGENT_MARKERS) and (
            head := unintegrated(root, options.remote, push)):
        say(f"pre-push: {head[:12]} is not on integration, and main moves only to what "
            "integration holds. Push your work to origin/integration; tools/integrate.sh tests it "
            "once and moves main. For a fix to a red main, push the one fix to integration and "
            "run tools/integrate.sh: it moves main as soon as that fix passes")
        return 1
    if push.behind:
        say(f"pre-push: main is at {push.behind[0][:12]} now, which this push does not contain, so "
            "the remote refuses it whatever a lint says; nothing was linted. Rebase onto it "
            "and push again")
        return 1
    files = rust_changes(push.files)
    if not files:
        say("pre-push: no Rust or Cargo file changed; nothing to lint")
        return 0
    if push.unnamed:
        say("pre-push: refusing a push the check cannot name")
        return 1
    if tip := unchecked_tree(root, push):
        say(f"pre-push: {tip[:12]} is not the commit checked out here, and the lint reads the files "
            "on disk: check it out and push again")
        return 1
    # Only a push asks this: `--range` and `--wide` are how main itself is linted, and they read
    # every commit on the tree it landed on.
    passes = Passes(root)
    change = None
    if not (options.range or options.wide or options.only or options.plan):
        change = change_key(root, push)
        age = passes.passed_since(change, REBASE_WINDOW)
        if age is not None:
            say(f"pre-push: these commits passed the lint {age / 60:.0f} min ago and are pushed as "
                "they were, on a newer main; what main's commits since do to them is linted on main")
            return 0
    try:
        selection = select(root, push, files, options.wide)
    except (subprocess.CalledProcessError, RuntimeError) as error:
        say(f"pre-push: cargo metadata could not name the packages this push touches: {error}")
        return 1
    if selection is None:
        say("pre-push: refusing the push until the changed packages can be named")
        return 1
    if options.only is not None:
        only = set(filter(None, options.only.split(",")))
        selection = Selection(sorted(set(selection.native) & only), sorted(set(selection.web) & only),
                              sorted(set(selection.dependents) & only), selection.metadata)
        if not selection.native:
            say("pre-push: none of the packages asked for is one this range reaches; nothing to lint")
            return 0
    if not selection.native:
        say("pre-push: the changed Rust files belong to no workspace package; CI builds them outside it")
        return 0
    if options.plan:
        print(f"native: {' '.join(selection.native)}")
        print(f"wasm32: {' '.join(selection.web)}")
        if selection.dependents:
            print(f"dependents: {' '.join(selection.dependents)}")
        return 0
    web = selection.web
    if shutil.which(cargo_command()[0]) is None:
        say(f"pre-push: there is no cargo to lint with ({cargo_command()[0]}); the lint cannot run")
        return 1
    if web and not (os.environ.get("CARGO", "").strip() or wasm_target_installed()):
        say(f"pre-push: {WASM_TARGET} is not installed here, so the page build cannot be linted, and "
            f"this push changes crates the page compiles (rustup target add {WASM_TARGET})")
        return 1
    if git(root, "status", "--porcelain").stdout.strip():
        say("pre-push: the worktree has uncommitted changes; the lint covers the files on disk, which "
            "is not quite the pushed commit")
    formatting = formatting_run(root, push) if rustfmt_available() else []
    if not formatting:
        say("pre-push: rustfmt is not installed, so formatting is not checked here")
    page = f", {len(web)} of them for the page" if web else ""
    dependents = (f" and {packages(len(selection.dependents))} built on them"
                  if selection.dependents else "")
    say(f"pre-push: linting {packages(len(selection.native))}{dependents}{page}")
    runs = lint_runs(root, selection, web, formatting,
                     options.own_build_dirs, options.wide)
    checked = f"{'clippy and rustfmt' if formatting else 'clippy'} for {packages(len(selection.native))}{page}"
    pending = []
    for run, key in zip(runs, lint_keys(root, selection.metadata, runs)):
        if passes.passed(key):
            say(f"pre-push: {run.label} passed before on the same inputs")
        else:
            pending.append((run, key))
    results, elapsed = run_together(root, [run for run, _key in pending], options.keep_logs)
    for (_run, key), (run, status, _output) in zip(pending, results):
        if status == 0:
            passes.keep(key, " ".join(run.command))
    status = report(results, elapsed, checked)
    if status == 0:
        passes.keep(change, "the pushed commits")
    return status


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
