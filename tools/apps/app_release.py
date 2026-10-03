#!/usr/bin/env python3
"""Plan independently buildable tapp app releases and named collections."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib

TOOLS = Path(__file__).resolve().parents[1]
ROOT = TOOLS.parent
if str(TOOLS) not in sys.path:
    sys.path.append(str(TOOLS))

from ci import workspace_graph as ci_select


CONFIG = ROOT / "dist/apps/app-collections.toml"
BINARY_INPUTS = {
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "dist/apps/app-collections.toml",
    "tools/apps/app_package_isa.py",
    "tools/ci/rustc-thin-app-wrapper.sh",
    "os/scripts/build-thin-apps.sh",
    "os/Containerfile.builder",
    "os/external-assets/github-assets.tsv",
    "os/dev/cache-github-assets.sh",
    "tools/ci/check-build-env.sh",
    "tools/ci/resolve_builder.py",
    "tools/ci/python.py",
    "tools/ci/sccache.sh",
    ".forgejo/actions/restore-cargo-downloads/action.yml",
}
PACKAGING_INPUTS = {
    "dist/apps/package-apps.sh",
    "dist/apps/assemble-collection.sh",
    "tools/apps/assemble_app_collection.py",
    "os/scripts/bake-runtime-bundle.sh",
    "tools/assemble_verified_apps.py",
    "tools/app_packaging_plan.py",
}
VERIFICATION_INPUTS = {
    "tools/ci/app_release_history.py",
    "tools/apps/app_release.py",
    "tools/ci/__init__.py",
    "tools/ci/workspace_graph.py",
    "tools/apps/app_channel_manifest.py",
    "tools/apps/canonical_app_verifier.py",
    "tools/apps/create_app_receipt.py",
    "tools/apps/final_app_receipt.py",
    "os/scripts/verify-elf-hardening.sh",
    "tools/canonical_verifier_bundle.py",
    "tools/app_packaging_qualification.py",
    ".forgejo/workflows/release-apps.yml",
}
GLOBAL_INPUTS = BINARY_INPUTS | PACKAGING_INPUTS | VERIFICATION_INPUTS
GLOBAL_PREFIXES = (".cargo/", "vendor/")
# Font sources under vendor/fonts/ build with their own toolchain; no app reads them.
NON_CARGO_VENDOR = ("vendor/fonts/",)
FULL_REBUILD_PREFIXES = (
    ".forgejo/actions/restore-cargo-downloads/",
    "dist/apps/",
    "tools/apps/",
)
# These are explicitly built by package-apps.sh, not ordinary app dependencies.
# A change reaching their graph affects the shared runtime or every emitted tapp.
FLEET_BUILD_PACKAGES = {"runtime-dylib", "tilo-app-package-cli", "runtime-bundle"}
OS_INTEGRATED_APPS = {"browser"}


def architecture_matrix(
    ref: str,
    event: str,
    requested: str = "",
    main_architectures: str = "",
) -> dict:
    if ref.startswith("refs/tags/"):
        raw = "x86_64 aarch64"
    elif ref == "refs/heads/main" and event == "push":
        raw = main_architectures or "x86_64 aarch64"
    else:
        raw = requested or "x86_64 aarch64"
    architectures = sorted(set(filter(None, re.split(r"[ ,]+", raw))))
    invalid = set(architectures).difference({"x86_64", "aarch64"})
    if not architectures or invalid:
        detail = ", ".join(sorted(invalid)) or "empty selection"
        raise ValueError(f"architectures must contain only x86_64 and/or aarch64: {detail}")
    # Tools from the first architecture run on its small runner; compiling the
    # whole collection is CPU-bound and takes the large one.
    return {
        "include": [
            {
                "arch": architecture,
                "runner": "runner" if architecture == "x86_64" else "runner-arm64",
                "build_runner": (
                    "runner-large" if architecture == "x86_64" else "runner-large-arm64"
                ),
            }
            for architecture in architectures
        ]
    }


def app_catalog(root: Path = ROOT) -> dict[str, dict[str, str]]:
    result: dict[str, dict[str, str]] = {}
    for manifest in sorted((root / "apps").glob("*/tilo-app.toml")):
        directory = manifest.parent
        cargo = tomllib.loads((directory / "Cargo.toml").read_text())
        app = tomllib.loads(manifest.read_text())
        name = directory.name
        binary = Path(app["entrypoint"]["bin"]).name
        result[name] = {
            "name": name,
            "package": cargo["package"]["name"],
            "binary": binary,
            "id": app["package"]["id"],
            "version": app["package"]["version"],
            "runtime": app["package"]["runtime"],
            "manifest": str(manifest.relative_to(root)),
        }
    if not result:
        raise ValueError("no apps/*/tilo-app.toml manifests found")
    return result


def config(path: Path = CONFIG) -> dict:
    return tomllib.loads(path.read_text())


def resolve_collection(name: str, catalog: dict[str, dict], cfg: dict) -> list[str]:
    collections = cfg.get("collections", {})
    visiting: set[str] = set()

    def visit(current: str) -> set[str]:
        if current in visiting:
            raise ValueError(f"collection cycle at {current}")
        if current not in collections:
            raise ValueError(f"unknown app collection: {current}")
        visiting.add(current)
        spec = collections[current]
        selected: set[str] = set()
        for parent in spec.get("extends", []):
            selected.update(visit(parent))
        includes = spec.get("include", [])
        if "*" in includes:
            selected.update(catalog)
        selected.update(item for item in includes if item != "*")
        selected.difference_update(spec.get("exclude", []))
        visiting.remove(current)
        return selected

    selected = visit(name)
    unknown = selected.difference(catalog)
    if unknown:
        raise ValueError(f"collection {name} names unknown apps: {', '.join(sorted(unknown))}")
    return sorted(selected)


def changed_paths(base: str, root: Path = ROOT) -> list[str]:
    # Share the NUL-delimited, rename-as-delete/add reader used by check CI.
    return ci_select.changed_files(root, base, "HEAD")


def lockfile_affected_packages(
    base: str,
    metadata: dict,
    root: Path = ROOT,
) -> set[str] | None:
    """Return workspace packages reached by the changed lockfile records.

    Cargo.lock contains the complete external dependency graph, while the cheap
    ``cargo metadata --no-deps`` result contains each workspace package's direct
    dependencies. Joining the two by Cargo package name lets a lockfile-only
    update select the workspace reverse closure instead of every app. Duplicate
    external versions collapse to one name in the join, which can over-select
    but cannot omit another version's consumers.

    ``None`` means the comparison could not be proved. Callers retain the old
    full-fleet behavior in that case.
    """
    merge_base = ci_select.comparison_merge_base(root, base, "HEAD")
    before = subprocess.run(
        ["git", "show", f"{merge_base}:Cargo.lock"],
        cwd=root,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    if before.returncode:
        return None
    try:
        after = (root / "Cargo.lock").read_bytes()
    except OSError:
        return None
    packages, _ = ci_select.package_maps(metadata, root)
    change = ci_select.lockfile_change(before.stdout, after, packages)
    if change is None:
        return None
    affected_names = change.reached | change.seeds
    if not affected_names:
        return set()
    workspace_seeds = {
        name
        for name, package in packages.items()
        if name in affected_names
        or any(
            dependency["name"] in affected_names
            for dependency in package.get("dependencies", [])
        )
    }
    return ci_select.closure(
        workspace_seeds,
        ci_select.reverse_dependencies(packages, root),
    )


def input_classification(paths: list[str], metadata: dict | None = None,
                         root: Path = ROOT) -> dict[str, list[str]]:
    """Artifact invalidation versus metadata revalidation, not compile commands.

    The current executor rebuilds for all three classes. Keeping them distinct
    allows a later trusted replay worker to reuse binaries for receipt-only work.
    """
    classified = {
        "binary": sorted({p for p in paths if p in BINARY_INPUTS or (p.startswith(GLOBAL_PREFIXES) and not p.startswith(NON_CARGO_VENDOR))}),
        "packaging": sorted(set(paths) & PACKAGING_INPUTS),
        "verification": sorted(set(paths) & VERIFICATION_INPUTS),
    }
    if metadata is not None:
        packages, by_dir = ci_select.package_maps(metadata, root)
        reverse = ci_select.reverse_dependencies(packages, root)
        app_packages = {name for directory, name in by_dir.items()
                        if directory.parts and directory.parts[0] == "apps"}
        for path in paths:
            owner = ci_select.owner_for(path, by_dir)
            if owner is not None:
                affected = ci_select.closure({owner}, reverse)
                packaging = bool(affected & {"tilo-app-package-cli", "runtime-bundle"})
                if packaging:
                    classified["packaging"].append(path)
                if not packaging or affected & (app_packages | {"runtime-dylib"}):
                    classified["binary"].append(path)
            elif path.startswith(("apps/", "crates/", "fs/", "shell/")):
                classified["binary"].append(path)
    return {kind: sorted(set(inputs)) for kind, inputs in classified.items()}


def apps_from_changes(
    paths: list[str],
    catalog: dict[str, dict],
    metadata: dict | None = None,
    root: Path = ROOT,
    lock_affected_packages: set[str] | None = None,
) -> list[str]:
    if any(
        (
            path in GLOBAL_INPUTS
            and not (path == "Cargo.lock" and lock_affected_packages is not None)
        )
        or (path.startswith(GLOBAL_PREFIXES + FULL_REBUILD_PREFIXES)
            and not path.startswith(NON_CARGO_VENDOR))
        for path in paths
    ):
        return sorted(set(catalog).difference(OS_INTEGRATED_APPS))
    selected = {
        parts[1]
        for path in paths
        if len(parts := Path(path).parts) >= 2
        and parts[0] == "apps"
        and parts[1] in catalog
    }
    if metadata is not None:
        packages, by_dir = ci_select.package_maps(metadata, root)
        if any(path.startswith(("apps/", "crates/", "fs/", "shell/"))
               and ci_select.owner_for(path, by_dir) is None for path in paths):
            return sorted(set(catalog).difference(OS_INTEGRATED_APPS))
        seeds = {
            owner
            for path in paths
            if (owner := ci_select.owner_for(path, by_dir)) is not None
        }
        seeds.update(lock_affected_packages or ())
        affected = ci_select.closure(seeds, ci_select.reverse_dependencies(packages, root))
        if affected & FLEET_BUILD_PACKAGES:
            return sorted(set(catalog).difference(OS_INTEGRATED_APPS))
        selected.update(
            name for name, app in catalog.items() if app["package"] in affected
        )
    return sorted(selected.difference(OS_INTEGRATED_APPS))


def prior_collection_base(source: str, collection_builder: str, builder: str) -> str | None:
    """The commit a main push may select a partial rebuild relative to, or None.

    A partial rebuild reuses the published collection's other tapps and needs
    its shared runtime to be byte-identical to the one built now. That runtime
    is compiled from source, which the commit range selects on, and carries
    system libraries from the builder image, which no source diff shows. So a
    collection built with another builder image, or with an unrecorded one,
    must be rebuilt in full; publication would otherwise refuse the changed
    runtime after every app was built.
    """
    if not re.fullmatch(r"[0-9a-f]{40}", source):
        return None
    if not builder or collection_builder != builder:
        return None
    return source


def app_from_tag(tag: str, catalog: dict[str, dict]) -> str | None:
    match = re.fullmatch(r"app-(.+)-v[0-9].*", tag)
    if not match:
        return None
    name = match.group(1)
    if name not in catalog:
        raise ValueError(f"tag selects unknown app: {name}")
    return name


def plan(args: argparse.Namespace) -> dict:
    catalog = app_catalog(args.root)
    cfg = config(args.config)
    tag_app = app_from_tag(args.tag, catalog) if args.tag else None
    classification = {"binary": [], "packaging": [], "verification": []}
    if tag_app:
        if tag_app in OS_INTEGRATED_APPS:
            raise ValueError(f"{tag_app} is released by the OS-native integration pipeline")
        tag_version = args.tag.rsplit("-v", 1)[1]
        if catalog[tag_app]["version"] != tag_version:
            raise ValueError(
                f"tag version {tag_version} does not match {tag_app} manifest "
                f"version {catalog[tag_app]['version']}"
            )
        selected = [tag_app]
        reason = f"component tag selected {tag_app}"
    elif args.apps:
        selected = sorted(set(filter(None, re.split(r"[ ,]+", args.apps))))
        reason = "explicit app selection"
    elif args.base:
        changes = changed_paths(args.base, args.root)
        metadata = ci_select.cargo_metadata(args.root)
        lock_affected = (
            lockfile_affected_packages(args.base, metadata, args.root)
            if "Cargo.lock" in changes
            else None
        )
        classification = input_classification(changes, metadata, args.root)
        selected = apps_from_changes(
            changes,
            catalog,
            metadata,
            args.root,
            lock_affected,
        )
        reason = "apps affected by the commit range"
        kinds = [kind for kind, inputs in classification.items() if inputs]
        if kinds:
            reason = "release inputs changed: " + ", ".join(kinds)
    else:
        selected = resolve_collection(args.collection, catalog, cfg)
        reason = f"{args.collection} collection"
    unknown = set(selected).difference(catalog)
    if unknown:
        raise ValueError(f"unknown apps: {', '.join(sorted(unknown))}")
    integrated = set(selected).intersection(OS_INTEGRATED_APPS)
    if integrated:
        raise ValueError(
            "OS-integrated apps cannot use the generic tapp builder: "
            + ", ".join(sorted(integrated))
        )
    features = cfg.get("features", {})
    for name in selected:
        cargo_manifest = args.root / "apps" / name / "Cargo.toml"
        if not features.get(name) or not cargo_manifest.is_file():
            continue
        declared = tomllib.loads(cargo_manifest.read_text()).get("features", {})
        undeclared = [feature for feature in features[name] if feature not in declared]
        if undeclared:
            raise ValueError(
                f"{args.config.name} selects features {name} does not declare: "
                + ", ".join(undeclared)
            )
    cargo_features = [
        f"{catalog[name]['package']}/{feature}"
        for name in selected
        for feature in features.get(name, [])
    ]
    return {
        "reason": reason,
        "input_classification": classification,
        "count": len(selected),
        "apps": selected,
        "packages": [catalog[name]["package"] for name in selected],
        "features": cargo_features,
        "catalog": [catalog[name] for name in selected],
    }


def emit_outputs(result: dict, path: Path) -> None:
    values = {
        "count": str(result["count"]),
        "apps": " ".join(result["apps"]),
        "packages": " ".join(f"-p {name}" for name in result["packages"]),
        "features": ",".join(result["features"]),
    }
    with path.open("a", encoding="utf-8") as output:
        for key, value in values.items():
            output.write(f"{key}={value}\n")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--config", type=Path, default=CONFIG)
    parser.add_argument("--collection", default="default")
    parser.add_argument("--apps", default="")
    parser.add_argument("--base")
    parser.add_argument("--tag", default="")
    parser.add_argument("--github-output", type=Path)
    parser.add_argument("--architecture-matrix", action="store_true")
    parser.add_argument("--ref", default="")
    parser.add_argument("--event", default="")
    parser.add_argument("--architectures", default="")
    parser.add_argument("--main-architectures", default="")
    parser.add_argument("--prior-collection-base", action="store_true",
                        help="print the partial-rebuild base, or nothing for a full rebuild")
    parser.add_argument("--collection-source", default="")
    parser.add_argument("--collection-builder", default="")
    parser.add_argument("--builder", default="")
    args = parser.parse_args()
    try:
        if args.prior_collection_base:
            print(prior_collection_base(
                args.collection_source, args.collection_builder, args.builder) or "")
            return 0
        if args.architecture_matrix:
            print(
                json.dumps(
                    architecture_matrix(
                        args.ref, args.event, args.architectures, args.main_architectures
                    ),
                    separators=(",", ":"),
                )
            )
            return 0
        result = plan(args)
    except (KeyError, OSError, subprocess.CalledProcessError, tomllib.TOMLDecodeError, ValueError) as error:
        print(f"app release plan: {error}", file=sys.stderr)
        return 2
    print(json.dumps(result, indent=2))
    if args.github_output:
        emit_outputs(result, args.github_output)
    return 0


if __name__ == "__main__":
    sys.exit(main())
