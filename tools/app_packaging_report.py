#!/usr/bin/env python3
"""Report affected qpkg apps and conservative artifact identity candidates.

Read-only: never builds, publishes, dispatches, or authorizes artifact reuse.
Requires a clean checkout, including untracked source files. Keys cover committed workspace inputs and
explicit target/builder/runtime identities; ABI and signature validation remain
mandatory before a future consumer reuses artifacts.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys

import app_release
import ci_select

ROOT = Path(__file__).resolve().parents[1]


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def git(root, *args):
    return subprocess.check_output(["git", *args], cwd=root)


def tree_inputs(root):
    if git(root, "status", "--porcelain", "--untracked-files=all").strip():
        raise ValueError("checkout must be clean, including untracked files")
    result = {}
    for record in git(root, "ls-tree", "-r", "-z", "HEAD").split(b"\0"):
        if not record:
            continue
        descriptor, path = record.split(b"\t", 1)
        mode, kind, oid = descriptor.decode().split()
        if kind != "blob":
            raise ValueError("submodules are not supported")
        result[path.decode()] = [mode, oid]
    return result


def report(metadata, catalog, cfg, paths, blobs, root, target, builder, runtime):
    for label, value in (("builder", builder), ("runtime", runtime)):
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", value):
            raise ValueError(f"{label} must be an immutable sha256 digest")
    if target not in {"x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"}:
        raise ValueError("qpkg report currently supports the two Linux GNU targets only")
    packages, owners = ci_select.package_maps(metadata, root)
    for required in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml"):
        if required not in blobs:
            raise ValueError(f"missing committed build input: {required}")
    manifests = {str(Path(pkg["manifest_path"]).resolve()) for pkg in packages.values()}
    for pkg in packages.values():
        manifest = Path(pkg["manifest_path"]).resolve().relative_to(root.resolve()).as_posix()
        if manifest not in blobs:
            raise ValueError(f"Cargo metadata references uncommitted manifest: {manifest}")
        for dep in pkg.get("dependencies", []):
            if dep.get("path") and str((Path(dep["path"]) / "Cargo.toml").resolve()) not in manifests:
                raise ValueError("path dependency absent from workspace metadata; input closure incomplete")
    def owner(path):
        for parent in Path(path).parents:
            if parent in owners:
                return owners[parent]
        return None
    input_owners = {path: owner(path) for path in blobs}
    reverse = ci_select.reverse_dependencies(packages, root)
    forward = {name: set() for name in packages}
    for provider, consumers in reverse.items():
        for consumer in consumers:
            forward[consumer].add(provider)
    # Every manifest participates in the unified thin-app feature graph. Changes
    # there invalidate all keys even if a selected app's source is unchanged.
    common = {p: oid for p, oid in blobs.items()
              if p.endswith("Cargo.toml") or p in {"Cargo.lock", "rust-toolchain.toml"}
              or (input_owners[p] is None and not p.startswith("docs/") and not p.endswith(".md"))
              or p.startswith((".cargo/", ".config/", "vendor/", "dist/apps/", "release/"))
              or p in {"tools/app_release.py", "tools/ci_select.py",
                       "tools/rustc-thin-app-wrapper.sh", "tools/app_packaging_report.py",
                       "os/scripts/build-thin-apps.sh", "os/scripts/bake-runtime-bundle.sh",
                       "os/scripts/verify-elf-hardening.sh"}}
    unknown = [p for p in paths if ci_select.owner_for(p, owners) is None
               and p not in common and not p.startswith("docs/") and not p.endswith(".md")]
    selected = app_release.apps_from_changes(paths, catalog, metadata, root)
    if unknown or any(p in common for p in paths):
        selected = sorted(set(catalog) - app_release.OS_INTEGRATED_APPS)
    rows = []
    features = cfg.get("features", {})
    for name, app in sorted(catalog.items()):
        if name in app_release.OS_INTEGRATED_APPS:
            continue
        if app["package"] not in packages:
            raise ValueError(f"app package absent from Cargo metadata: {name}")
        closure = ci_select.closure({app["package"]}, forward)
        inputs = {p: oid for p, oid in blobs.items() if input_owners[p] in closure}
        identity = dict(schema=1, target=target, builder=builder, runtime=runtime,
                        profile="thin-app", collection_features=features,
                        common=common, package_inputs=inputs)
        rows.append(dict(app=name, affected=name in selected,
                         workspace_dependencies=sorted(closure),
                         identity_candidate="sha256:" + digest(identity)))
    return dict(schema=1, mode="report-only", reuse_authorized=False,
                target=target, builder=builder, runtime=runtime,
                affected_apps=selected, unknown_paths=unknown, apps=rows,
                compilation="Current package-apps.sh still compiles the default collection graph.",
                limitation="Conservative workspace graph includes optional/dev/other-target edges. Ignored/generated files, ambient environment and documentation included by build scripts are not modeled; keys are not cache authorization, ABI or provenance proof.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--base", required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--builder", required=True)
    parser.add_argument("--runtime", required=True)
    args = parser.parse_args()
    try:
        blobs = tree_inputs(args.root)
        result = report(ci_select.cargo_metadata(args.root), app_release.app_catalog(args.root),
                        app_release.config(args.root / "release/app-collections.toml"),
                        ci_select.changed_files(args.root, args.base, "HEAD"), blobs,
                        args.root, args.target, args.builder, args.runtime)
        result["source"] = git(args.root, "rev-parse", "HEAD").decode().strip()
        print(json.dumps(result, indent=2))
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as error:
        print(f"app packaging report: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
