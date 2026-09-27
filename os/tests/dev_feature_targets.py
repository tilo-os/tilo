#!/usr/bin/env python3
"""Every dev feature reaches the package the dev image intends to build.

Cargo reads `pkg/feature` as a dependency feature when a package named `pkg`
also has a dependency of that name. The shell has a self dev-dependency for
its integration tests, so its dev image must request bare `dev-overrides`.
Bare features are safe here only when one selected dev-image package declares
them; both the image's combined build and a single-component swap then target
the same package.
"""

from __future__ import annotations

import sys
import subprocess
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
TABLES = ("dependencies", "dev-dependencies", "build-dependencies")


def manifests() -> dict[str, tuple[Path, dict]]:
    found = {}
    for path in [*REPO.glob("crates/**/Cargo.toml"), *REPO.glob("apps/**/Cargo.toml")]:
        if "target" in path.parts:
            continue
        data = tomllib.loads(path.read_text())
        name = data.get("package", {}).get("name")
        if name:
            found[name] = (path, data)
    return found


def dependency_names(data: dict) -> set[str]:
    names = set()
    for table in TABLES:
        names.update(data.get(table, {}))
    for target in data.get("target", {}).values():
        for table in TABLES:
            names.update(target.get(table, {}))
    return names


def declares(packages: dict[str, tuple[Path, dict]], package: str, feature: str) -> bool:
    entry = packages.get(package)
    return entry is not None and feature in entry[1].get("features", {})


def main() -> int:
    components = tomllib.loads((REPO / "os/components.toml").read_text())
    packages = manifests()
    rows = components.get("component", [])
    requested = sorted({feature for component in rows for feature in component.get("dev_features", [])})
    if not requested:
        print("FAIL: os/components.toml requests no dev features; the check sees nothing", file=sys.stderr)
        return 1
    dev_packages = {
        package
        for component in rows
        if component.get("dev_features") or any(
            entry.get("layer", component.get("layer", "")).startswith("dev-")
            for entry in component.get("files", [])
        )
        for package in component.get("packages", [])
    }
    failures = []
    for component in rows:
        for feature in component.get("dev_features", []):
            if "/" not in feature:
                owners = [p for p in component.get("packages", []) if declares(packages, p, feature)]
                dev_owners = [p for p in dev_packages if declares(packages, p, feature)]
                if len(component.get("packages", [])) != 1 or len(owners) != 1 or dev_owners != owners:
                    failures.append(
                        f"{component['name']}: bare {feature!r} must belong to its only package "
                        f"and to no other dev-image package (component owners: {owners}; dev-image owners: {dev_owners})"
                    )
                continue
            package, name = feature.split("/", 1)
            if package not in packages:
                failures.append(f"{feature}: no workspace package named {package}")
                continue
            path, data = packages[package]
            if name not in data.get("features", {}):
                failures.append(f"{feature}: {path.relative_to(REPO)} declares no feature {name}")
            if package in dependency_names(data):
                failures.append(
                    f"{feature}: {path.relative_to(REPO)} has a dependency named {package}, so "
                    f"`--features {feature}` enables it on that dependency, not on the package; "
                    "use a bare feature in a single-package component"
                )
    for failure in failures:
        print(f"FAIL: {failure}", file=sys.stderr)
    if failures:
        return 1
    command = subprocess.check_output(
        [sys.executable, str(REPO / "os/scripts/install-components.py"), "--dev", "--dry-run", "--format", "cargo"],
        text=True,
    ).split()
    selected = command[command.index("--features") + 1].split(",") if "--features" in command else []
    for feature in requested:
        if feature not in selected:
            print(f"FAIL: dev image cargo plan omits {feature!r}", file=sys.stderr)
            return 1
    print(f"ok: {len(requested)} dev feature(s) target their package unambiguously: {', '.join(requested)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
