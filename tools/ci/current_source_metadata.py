#!/usr/bin/env python3
"""Seal CI Cargo metadata and validate it for a clean exact-source worktree.

This program never runs Cargo. Its caller produces the complete export with
`cargo metadata --locked --no-deps --format-version 1` (which compiles nothing:
tools/ci/local-affected.sh runs it before planning); sealing binds it to the
clean worktree's commit, tree and manifest bytes, and validation checks that
binding, completeness and every target against the worktree it is used in.

With --snapshot COMMIT the export is bound to a commit made of the worktree as it
is (tracked and untracked, non-ignored files: `snapshot` makes one without
touching the index or HEAD), so a change still being edited is planned from
exactly the sources the builders compile; the worktree must still equal that
commit when the bundle is validated.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
from urllib.parse import quote

ROOT = "__TILO_SOURCE_ROOT__"


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args])


def worktree_tree(root):
    """The tree object of the worktree as it is: tracked and untracked, non-ignored files,
    staged or not. Written through a private index, so the real index is untouched."""
    with tempfile.TemporaryDirectory() as scratch:
        env = {**os.environ, "GIT_INDEX_FILE": str(Path(scratch) / "index")}
        for args in (("read-tree", "HEAD"), ("add", "-A", "--", ".")):
            subprocess.run(["git", "-C", str(root), *args], env=env, check=True,
                           stdout=subprocess.DEVNULL)
        return subprocess.check_output(["git", "-C", str(root), "write-tree"], env=env).decode().strip()


def snapshot(root):
    """A commit (child of HEAD, on no branch) of the worktree as it is; HEAD itself when the
    worktree is clean."""
    tree = worktree_tree(root)
    if tree == git(root, "rev-parse", "HEAD^{tree}").decode().strip():
        return git(root, "rev-parse", "HEAD").decode().strip()
    env = {**os.environ, "GIT_AUTHOR_NAME": "snapshot", "GIT_AUTHOR_EMAIL": "snapshot@localhost",
           "GIT_COMMITTER_NAME": "snapshot", "GIT_COMMITTER_EMAIL": "snapshot@localhost"}
    return subprocess.check_output(
        ["git", "-C", str(root), "commit-tree", tree, "-p", "HEAD", "-m", "worktree snapshot"],
        env=env).decode().strip()


def identity(root, snapshot_commit=None):
    if snapshot_commit is None:
        if git(root, "status", "--porcelain", "--untracked-files=all"):
            raise ValueError("metadata requires a clean worktree; commit source changes first")
        revision = "HEAD"
        paths = git(root, "ls-files", "-z").decode().split("\0")
    else:
        revision = snapshot_commit
        if worktree_tree(root) != git(root, "rev-parse", f"{revision}^{{tree}}").decode().strip():
            raise ValueError("the worktree differs from its snapshot; take a new one")
        paths = git(root, "ls-tree", "-r", "-z", "--name-only", revision).decode().split("\0")
    manifests = {p: hashlib.sha256((root / p).read_bytes()).hexdigest()
                 for p in paths if p and (Path(p).name in ("Cargo.toml", "Cargo.lock")
                                         or p.startswith(".cargo/"))}
    return {"commit": git(root, "rev-parse", revision).decode().strip(),
            "tree": git(root, "rev-parse", f"{revision}^{{tree}}").decode().strip(),
            "manifests": manifests}


def map_strings(value, old, new):
    if isinstance(value, str):
        if value.startswith(("path+file://", "file://")):
            return value.replace(quote(old, safe="/"), quote(new, safe="/"))
        return value.replace(old, new)
    if isinstance(value, list):
        return [map_strings(item, old, new) for item in value]
    if isinstance(value, dict):
        return {key: map_strings(item, old, new) for key, item in value.items()}
    return value


def complete(metadata):
    members = metadata["workspace_members"]
    packages = metadata["packages"]
    ids = [p["id"] for p in packages]
    if not members or len(ids) != len(set(ids)) or set(ids) != set(members):
        raise ValueError("export must contain every workspace package exactly once")
    if metadata["workspace_root"] != ROOT or metadata.get("version") != 1:
        raise ValueError("unsupported metadata root or schema")
    for package in packages:
        manifest = package["manifest_path"]
        if not manifest.startswith(ROOT + "/") or not package["targets"]:
            raise ValueError("workspace manifest or targets missing")
        for target in package["targets"]:
            if not target["src_path"].startswith(ROOT + "/"):
                raise ValueError("target outside source worktree")
        for dep in package["dependencies"]:
            if dep.get("path") and not dep["path"].startswith(ROOT + "/"):
                raise ValueError("path dependency outside source worktree")


def seal(root, metadata, snapshot_commit=None):
    normalized = map_strings(metadata, str(root), ROOT)
    complete(normalized)
    source = identity(root, snapshot_commit)
    for package in normalized["packages"]:
        if package["manifest_path"][len(ROOT) + 1:] not in source["manifests"]:
            raise ValueError("workspace package manifest is not tracked")
    return {"schema": 1, "source": source, "metadata": normalized}


def validate(root, bundle, snapshot_commit=None):
    if bundle["schema"] != 1 or bundle["source"] != identity(root, snapshot_commit):
        raise ValueError("export source revision, tree or manifest bytes differ")
    complete(bundle["metadata"])
    for package in bundle["metadata"]["packages"]:
        for target in package["targets"]:
            path = target["src_path"][len(ROOT) + 1:]
            if not (root / path).is_file():
                raise ValueError("exported target is absent in caller worktree")
    return map_strings(bundle["metadata"], ROOT, str(root))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("seal", "validate", "snapshot"))
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--input", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--snapshot", metavar="COMMIT",
                        help="bind to this snapshot of the worktree instead of a clean HEAD")
    args = parser.parse_args()
    root = args.root.resolve()
    if args.mode == "snapshot":
        print(snapshot(root))
        return
    if args.input is None or args.output is None:
        parser.error(f"{args.mode} needs --input and --output")
    operation = seal if args.mode == "seal" else validate
    result = operation(root, json.loads(args.input.read_text()), args.snapshot)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result) + "\n")
    print(json.dumps({"mode": args.mode, "source_sha": git(root, "rev-parse", args.snapshot or "HEAD").decode().strip(),
                      "workspace_packages": len((result["metadata"] if args.mode == "seal" else result)["packages"])}))


if __name__ == "__main__":
    main()
