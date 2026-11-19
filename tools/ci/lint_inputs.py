#!/usr/bin/env python3
"""What a lint of some workspace packages reads, as one key.

The push lint keys each of its runs on the inputs it can read: the directories
of the linted packages and of every path crate they reach in Cargo.lock (a
crate's dev-dependencies count only for the linted crates themselves), the
resolved versions and sources of the external crates they reach, not the whole
lockfile, which changes whenever any package anywhere in the workspace gains a
dependency - the settings of the workspace manifest that
their builds read (not its member list, which changes whenever any app joins
the workspace), the files their sources pull in at compile time from outside
those directories, the toolchain and lint configuration, the command, and the
Cargo and Rust variables of the environment. A run that passed is not repeated
while its key is unchanged, so a push retried after a rebase onto unrelated
commits relints only what those commits reach.

The compile-time files are found in the sources: the paths named by
`include!`, `include_str!`, `include_bytes!` and `#[path]`, and in build
scripts every literal that looks like a path, resolved against the package and against the
repository directories the script climbs to. A path that cannot be resolved
falls back to the nearest directory that holds it, up to the whole tree.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import posixpath
import re
import subprocess
import tomllib
from bisect import bisect_left
from typing import Iterable

from ci.workspace_graph import _lock_graph

# Files that steer how every package builds or is linted, whatever package is
# linted. The workspace manifest is not one of them: it changes whenever any app
# joins the workspace, and a member a lint does not build changes nothing it
# builds, so only the settings its packages read are keyed (build_settings). Nor
# is the lockfile: each external crate's resolved version and source is (key).
CONFIGURATION = ("rust-toolchain.toml", "rust-toolchain", "clippy.toml", ".clippy.toml",
                 "rustfmt.toml", ".rustfmt.toml", ".cargo/config.toml", ".cargo/config")
# The workspace manifest, whose build settings are keyed in pieces.
WORKSPACE_MANIFEST = "Cargo.toml"
# The manifest tables a dependency can be declared in.
DEPENDENCY_TABLES = ("dependencies", "build-dependencies", "dev-dependencies")
# This file, the hook that runs the lint, and the tools a run goes through: a
# change to any of them can change what passing means. A run that passed is not
# reused across a push that changed one, nor across one that changed a
# dependency of the linted packages (their directories and lock records are in
# the key too), which a rebase brings in.
LINT_CODE = (Path(__file__), Path(__file__).with_name("pre_push_lint.py"),
             Path(__file__).with_name("package_features.py"),
             Path(__file__).with_name("package_sets.py"),
             Path(__file__).with_name("wasm_clippy_conf.py"),
             Path(__file__).with_name("check-changed-rustfmt.sh"),
             Path(__file__).with_name("comparison_range.py"),
             Path(__file__).with_name("workspace_graph.py"),
             Path(__file__).with_name("suites.py"),
             Path(__file__).with_name("suites.toml"))
ENVIRONMENT_PREFIXES = ("CARGO", "RUST")
INCLUDE = re.compile(r"\binclude(?:_str|_bytes)?!\s*\(")
PATH_ATTRIBUTE = re.compile(r'#\s*\[\s*path\s*=\s*"([^"\n]*)"')
LITERAL = re.compile(r'"((?:[^"\\\n]|\\.)*)"')
GENERATED = ("OUT_DIR",)
MANIFEST_DIR = "CARGO_MANIFEST_DIR"


def git(root: Path, *args: str) -> str:
    return subprocess.run(["git", *args], cwd=root, capture_output=True, text=True,
                          check=True).stdout


class Tree:
    """The committed files and directories of HEAD, by repository-relative path."""

    def __init__(self, root: Path):
        self.listing = sorted(set(git(root, "ls-tree", "-r", "-z", "--name-only", "HEAD").split("\0")) - {""})
        self.files = set(self.listing)
        self.directories = {""}
        for path in self.files:
            parent = posixpath.dirname(path)
            while parent not in self.directories:
                self.directories.add(parent)
                parent = posixpath.dirname(parent)

    def exists(self, path: str) -> bool:
        return path in self.files or path in self.directories

    def under(self, directory: str) -> list[str]:
        """The committed files below `directory`, in order."""
        if not directory:
            return self.listing
        prefix = directory + "/"
        start = bisect_left(self.listing, prefix)
        end = bisect_left(self.listing, directory + "0")  # "0" sorts right after "/"
        return self.listing[start:end]

    def holder(self, path: str) -> str:
        """The nearest existing path at or above `path`: what reading it can depend on."""
        while not self.exists(path):
            path = posixpath.dirname(path)
        return path


def resolve(base: str, literal: str) -> str | None:
    """A literal path relative to `base`, repository-relative; None when it leaves the repository."""
    path = posixpath.normpath(posixpath.join(base, literal.lstrip("/")))
    if path == ".":
        return ""
    return None if path == ".." or path.startswith("../") else path


def call_arguments(text: str, start: int) -> str:
    """The text inside the parentheses opened just before `start`."""
    depth, index, quoted = 1, start, False
    while index < len(text) and depth:
        character = text[index]
        if quoted:
            if character == "\\":
                index += 1
            elif character == '"':
                quoted = False
        elif character == '"':
            quoted = True
        elif character == "(":
            depth += 1
        elif character == ")":
            depth -= 1
        index += 1
    return text[start:index - 1]


def within(path: str, directory: str) -> bool:
    return directory == "" or path == directory or path.startswith(directory + "/")


class Package:
    """A path crate: its directory and the files a build of it reads from outside it."""

    def __init__(self, tree: Tree, root: Path, directory: str):
        self.directory = directory
        self.reads: set[str] = set()
        self.build_literals: list[str] = []
        self.scanned: set[str] = set()
        for path in tree.under(directory):
            if path.endswith(".rs"):
                self.scan(tree, root, path, self.is_build(path))
        bases = {directory}
        for literal in self.build_literals:
            if literal and set(literal.split("/")) <= {"..", ""}:
                base = resolve(directory, literal)
                if base is not None:
                    bases.add(base)
        # A build script's path names a directory or a file: a bare word ("os", "linux") is not one.
        for literal in (text for text in self.build_literals
                        if ("/" in text or "." in text) and not any(c.isspace() for c in text)):
            for base in bases:
                self.read(tree, base, literal, exact=True)

    def is_build(self, path: str) -> bool:
        relative = path[len(self.directory) + 1:] if self.directory else path
        return relative == "build.rs" or relative.startswith("build/")

    def scan(self, tree: Tree, root: Path, path: str, build: bool) -> None:
        if path in self.scanned:
            return
        self.scanned.add(path)
        try:
            text = (root / path).read_text(errors="replace")
        except OSError:
            return
        here = posixpath.dirname(path)
        if build:
            self.build_literals += LITERAL.findall(text)
        for match in INCLUDE.finditer(text):
            arguments = call_arguments(text, match.end())
            if any(name in arguments for name in GENERATED):
                continue
            pieces = [piece for piece in LITERAL.findall(arguments) if piece != MANIFEST_DIR]
            base = self.directory if MANIFEST_DIR in arguments else here
            target = self.read(tree, base, "".join(pieces), exact=False)
            if target and target.endswith(".rs") and match.group(0).startswith("include!"):
                self.scan(tree, root, target, build)
        for literal in PATH_ATTRIBUTE.findall(text):
            self.read(tree, here, literal, exact=False)

    def read(self, tree: Tree, base: str, literal: str, exact: bool) -> str | None:
        """Record what reading `literal` from `base` depends on.

        `exact` paths come from build scripts, whose literals are often not
        paths at all: only one that names a committed path, or a path pattern
        whose directory exists, counts. A compile-time include always reads
        something, so an unresolved one depends on the nearest directory above it.
        """
        if "{" in literal:
            literal = literal[:literal.index("{")]
            if "/" not in literal:
                return None
            literal = posixpath.dirname(literal)
            if exact:
                path = resolve(base, literal)
                if path is None or path not in tree.directories:
                    return None
        path = resolve(base, literal)
        if path is None or (exact and (not literal or not tree.exists(path))):
            return None
        path = tree.holder(path)
        if exact and set(literal.split("/")) <= {"..", ""}:
            return None
        if not within(path, self.directory):
            self.reads.add(path)
        return path


class Workspace:
    """The path crates of the workspace and the lock graph between all crates."""

    def __init__(self, root: Path, metadata: dict):
        self.root = root.resolve()
        self.tree = Tree(root)
        self.members = {package["name"]: package for package in metadata["packages"]}
        self.directories = {name: self.relative(Path(package["manifest_path"]).parent)
                            for name, package in self.members.items()}
        self.add_path_crates()
        _, self.lock = _lock_graph((root / "Cargo.lock").read_bytes())
        self.packages: dict[str, Package] = {}

    def relative(self, path: Path) -> str:
        relative = path.resolve().relative_to(self.root).as_posix()
        return "" if relative == "." else relative

    def add_path_crates(self) -> None:
        """Name the path crates outside the workspace: patches and path dependencies."""
        manifest = tomllib.loads((self.root / "Cargo.toml").read_text())
        pending = [self.root / spec["path"] for table in manifest.get("patch", {}).values()
                   for spec in table.values() if isinstance(spec, dict) and "path" in spec]
        pending += [self.root / spec["path"] for spec in
                    manifest.get("workspace", {}).get("dependencies", {}).values()
                    if isinstance(spec, dict) and "path" in spec]
        pending += [Path(dependency["path"]) if Path(dependency["path"]).is_absolute()
                    else self.root / dependency["path"]
                    for package in self.members.values()
                    for dependency in package.get("dependencies", []) if dependency.get("path")]
        known = set(self.directories.values())
        while pending:
            directory = pending.pop()
            try:
                relative = self.relative(directory)
                if relative in known:
                    continue
                known.add(relative)
                crate = tomllib.loads((directory / "Cargo.toml").read_text())
            except (OSError, ValueError):
                continue
            if "package" not in crate:
                continue
            self.directories.setdefault(crate["package"]["name"], relative)
            for table in ("dependencies", "build-dependencies", "dev-dependencies"):
                for spec in crate.get(table, {}).values():
                    if isinstance(spec, dict) and "path" in spec:
                        pending.append(directory / spec["path"])

    def reached(self, roots: Iterable[str], dependencies: bool) -> tuple[set[str], set[tuple]]:
        """The path crates and the external lock records a lint of `roots` can read."""
        roots = set(roots)
        if not dependencies:
            return roots, set()
        start = [package_id for package_id in self.lock if package_id[0] in roots and not package_id[2]]
        seen = set(start)
        while start:
            package_id = start.pop()
            name = package_id[0]
            member = self.members.get(name) if not package_id[2] else None
            built = None
            if member is not None and name not in roots:
                built = {dependency["name"] for dependency in member.get("dependencies", [])
                         if dependency.get("kind") != "dev"}
            for dependency in self.lock[package_id][1]:
                if dependency not in seen and (built is None or dependency[0] in built):
                    seen.add(dependency)
                    start.append(dependency)
        local = {package_id[0] for package_id in seen if not package_id[2]} | roots
        external = {(package_id, self.lock[package_id][0]) for package_id in seen if package_id[2]}
        return local, external

    def package(self, name: str) -> Package:
        if name not in self.packages:
            self.packages[name] = Package(self.tree, self.root, self.directories[name])
        return self.packages[name]

    def inherited(self, local: Iterable[str]) -> set[str] | None:
        """The workspace dependencies these packages name through `workspace = true`."""
        names: set[str] = set()
        for name in local:
            try:
                document = tomllib.loads(
                    (self.root / self.directories[name] / WORKSPACE_MANIFEST).read_text())
            except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError):
                return None
            tables = [document.get(table, {}) for table in DEPENDENCY_TABLES]
            for target in document.get("target", {}).values():
                if isinstance(target, dict):
                    tables += [target.get(table, {}) for table in DEPENDENCY_TABLES]
            for table in tables:
                if not isinstance(table, dict):
                    continue
                for key, spec in table.items():
                    if isinstance(spec, dict) and spec.get("workspace") is True:
                        names.add(key)
        return names

    @staticmethod
    def patches(tables: dict, names: set[str]) -> dict:
        """The patches of crates these packages build: a patch replaces what they compile."""
        found = {}
        for source, table in tables.items():
            if not isinstance(table, dict):
                continue
            for name, spec in table.items():
                patched = spec.get("package") if isinstance(spec, dict) else None
                if name in names or patched in names:
                    found[f"{source}\0{name}"] = spec
        return found

    @staticmethod
    def profiles(tables: dict, names: set[str]) -> dict:
        """The profile settings that apply to these packages and the crates they build."""
        found = {}
        for profile, settings in tables.items():
            if not isinstance(settings, dict):
                continue
            packages = settings.get("package", {})
            found[profile] = {
                "settings": {key: value for key, value in settings.items() if key != "package"},
                "packages": ({name: value for name, value in packages.items() if name in names}
                             if isinstance(packages, dict) else {}),
            }
        return found

    def build_settings(self, local: set[str], external: set[tuple]) -> bytes | None:
        """The workspace manifest's decisions these packages' builds read, as one blob.

        Cargo resolves every member against the workspace table: the resolver
        mode, the fields members inherit, the shared dependencies they name, the
        patches that replace a crate they build, and the profile settings that
        apply to the packages and crates they compile. The member list is not
        one of them: another app joining the workspace, or one leaving it, does
        not change how an existing package compiles.
        """
        try:
            document = tomllib.loads((self.root / WORKSPACE_MANIFEST).read_text())
        except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError):
            return None
        workspace = document.get("workspace", {})
        if not isinstance(workspace, dict):
            return None
        shared = workspace.get("dependencies", {})
        if not isinstance(shared, dict):
            return None
        used = self.inherited(local)
        if used is None:
            return None
        names = set(local) | {package_id[0] for package_id in external}
        settings = {
            "resolver": workspace.get("resolver"),
            "package": workspace.get("package", {}),
            "lints": workspace.get("lints", {}),
            "dependencies": {name: value for name, value in shared.items() if name in used},
            "patch": self.patches(document.get("patch", {}), names),
            "profile": self.profiles(document.get("profile", {}), names),
        }
        return json.dumps(settings, sort_keys=True, default=repr).encode()

    def key(self, roots: Iterable[str], dependencies: bool, command: list[str]) -> str | None:
        """The key of linting `roots` with `command`, or None when an input has no known place."""
        local, external = self.reached(roots, dependencies)
        if not local <= self.directories.keys():
            return None
        paths = set(CONFIGURATION)
        for name in local:
            package = self.package(name)
            paths.add(package.directory)
            paths |= package.reads
        settings = self.build_settings(local, external)
        if settings is None:
            return None
        digest = hashlib.sha256()
        digest.update(json.dumps(command).encode())
        for variable in sorted(os.environ):
            if variable.startswith(ENVIRONMENT_PREFIXES):
                digest.update(f"\0{variable}={os.environ[variable]}".encode())
        for code in LINT_CODE:
            digest.update(hashlib.sha256(code.read_bytes()).digest())
        digest.update(b"\0" + settings)
        for path, oid in sorted(objects(self.root, paths).items()):
            digest.update(f"\0{path} {oid}".encode())
        for record in sorted(external, key=repr):
            digest.update(f"\0{record!r}".encode())
        return digest.hexdigest()


def objects(root: Path, paths: set[str]) -> dict[str, str]:
    """The object id of each path at HEAD, '-' for one that is not there."""
    found = {path: "-" for path in paths}
    if "" in paths:
        found[""] = git(root, "rev-parse", "HEAD^{tree}").strip()
    named = sorted(path for path in paths if path)
    if named:
        listing = git(root, "ls-tree", "--full-tree", "-z", "HEAD", "--", *named)
        for entry in listing.split("\0"):
            if "\t" in entry:
                info, path = entry.split("\t", 1)
                if path in found:
                    found[path] = info.split()[2]
    return found
