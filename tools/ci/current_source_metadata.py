#!/usr/bin/env python3
"""Seal CI Cargo metadata and validate it for a clean exact-source worktree.

This program never runs Cargo. The explicit CI workflow produces the complete
--locked --no-deps export; validation binds it to Git source and manifest bytes.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
from urllib.parse import quote

ROOT = "__TILO_SOURCE_ROOT__"


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args])


def identity(root):
    if git(root, "status", "--porcelain", "--untracked-files=all"):
        raise ValueError("metadata requires a clean worktree; commit source changes first")
    paths = git(root, "ls-files", "-z").decode().split("\0")
    manifests = {p: hashlib.sha256((root / p).read_bytes()).hexdigest()
                 for p in paths if p and (Path(p).name in ("Cargo.toml", "Cargo.lock")
                                         or p.startswith(".cargo/"))}
    return {"commit": git(root, "rev-parse", "HEAD").decode().strip(),
            "tree": git(root, "rev-parse", "HEAD^{tree}").decode().strip(),
            "manifests": manifests}


def map_strings(value, old, new):
    if isinstance(value, str):
        return value.replace(quote(old, safe="/"), quote(new, safe="/")).replace(old, new)
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


def seal(root, metadata):
    normalized = map_strings(metadata, str(root), ROOT)
    complete(normalized)
    source = identity(root)
    for package in normalized["packages"]:
        if package["manifest_path"][len(ROOT) + 1:] not in source["manifests"]:
            raise ValueError("workspace package manifest is not tracked")
    return {"schema": 1, "source": source, "metadata": normalized}


def validate(root, bundle):
    if bundle["schema"] != 1 or bundle["source"] != identity(root):
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
    parser.add_argument("mode", choices=("seal", "validate"))
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root = args.root.resolve()
    operation = seal if args.mode == "seal" else validate
    result = operation(root, json.loads(args.input.read_text()))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result) + "\n")
    print(json.dumps({"mode": args.mode, "source_sha": git(root, "rev-parse", "HEAD").decode().strip(),
                      "workspace_packages": len((result["metadata"] if args.mode == "seal" else result)["packages"])}))


if __name__ == "__main__":
    main()
