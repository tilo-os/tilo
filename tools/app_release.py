#!/usr/bin/env python3
"""Plan independently buildable qpkg app releases and named collections."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib

import ci_select


ROOT = Path(__file__).resolve().parents[1]
CONFIG = ROOT / "release/app-collections.toml"
GLOBAL_INPUTS = {
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "release/app-collections.toml",
    "tools/app_release.py",
    "dist/apps/package-apps.sh",
    "dist/apps/assemble-collection.sh",
    "tools/assemble_app_collection.py",
    "os/scripts/build-thin-apps.sh",
    ".forgejo/workflows/release-apps.yml",
}
GLOBAL_PREFIXES = (".cargo/",)
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
        raw = main_architectures or "x86_64"
    else:
        raw = requested or "x86_64 aarch64"
    architectures = sorted(set(filter(None, re.split(r"[ ,]+", raw))))
    invalid = set(architectures).difference({"x86_64", "aarch64"})
    if not architectures or invalid:
        detail = ", ".join(sorted(invalid)) or "empty selection"
        raise ValueError(f"architectures must contain only x86_64 and/or aarch64: {detail}")
    return {
        "include": [
            {
                "arch": architecture,
                "runner": "runner" if architecture == "x86_64" else "runner-arm64",
            }
            for architecture in architectures
        ]
    }


def app_catalog(root: Path = ROOT) -> dict[str, dict[str, str]]:
    result: dict[str, dict[str, str]] = {}
    for manifest in sorted((root / "apps").glob("*/qos-app.toml")):
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
        raise ValueError("no apps/*/qos-app.toml manifests found")
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
    merge_base = subprocess.check_output(
        ["git", "merge-base", base, "HEAD"], cwd=root, text=True
    ).strip()
    output = subprocess.check_output(
        ["git", "diff", "--name-only", "--diff-filter=ACDMRTUXB", f"{merge_base}...HEAD"],
        cwd=root,
        text=True,
    )
    return output.splitlines()


def apps_from_changes(
    paths: list[str],
    catalog: dict[str, dict],
    metadata: dict | None = None,
    root: Path = ROOT,
) -> list[str]:
    if any(path in GLOBAL_INPUTS or path.startswith(GLOBAL_PREFIXES) for path in paths):
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
        seeds = {
            owner
            for path in paths
            if (owner := ci_select.owner_for(path, by_dir)) is not None
        }
        affected = ci_select.closure(seeds, ci_select.reverse_dependencies(packages, root))
        selected.update(
            name for name, app in catalog.items() if app["package"] in affected
        )
    return sorted(selected.difference(OS_INTEGRATED_APPS))


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
        selected = apps_from_changes(
            changed_paths(args.base, args.root),
            catalog,
            ci_select.cargo_metadata(args.root),
            args.root,
        )
        reason = "apps affected by the commit range"
    else:
        selected = resolve_collection(args.collection, catalog, cfg)
        reason = f"{args.collection} collection"
    unknown = set(selected).difference(catalog)
    if unknown:
        raise ValueError(f"unknown apps: {', '.join(sorted(unknown))}")
    integrated = set(selected).intersection(OS_INTEGRATED_APPS)
    if integrated:
        raise ValueError(
            "OS-integrated apps cannot use the generic qpkg builder: "
            + ", ".join(sorted(integrated))
        )
    features = cfg.get("features", {})
    cargo_features = [
        f"{catalog[name]['package']}/{feature}"
        for name in selected
        for feature in features.get(name, [])
    ]
    return {
        "reason": reason,
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
    args = parser.parse_args()
    try:
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
