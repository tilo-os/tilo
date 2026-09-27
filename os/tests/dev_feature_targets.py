#!/usr/bin/env python3
"""Every `pkg/feature` the dev image asks for reaches the package itself.

os/components.toml names dev features as `pkg/feature`, and the dev image
build passes them to cargo as `--features pkg/feature`. When `pkg` also has a
dependency named `pkg` (a self dev-dependency that turns on a test feature, for
example), cargo reads `pkg/feature` as that dependency's feature, and the
package builds without it: a dev image whose shell lacked dev-overrides failed
only in the image's final check.
"""

from __future__ import annotations

import sys
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


def main() -> int:
    components = tomllib.loads((REPO / "os/components.toml").read_text())
    packages = manifests()
    requested = sorted({
        feature
        for component in components.get("component", [])
        for feature in component.get("dev_features", [])
    })
    if not requested:
        print("FAIL: os/components.toml requests no dev features; the check sees nothing", file=sys.stderr)
        return 1
    failures = []
    for feature in requested:
        package, _, name = feature.partition("/")
        if package not in packages:
            failures.append(f"{feature}: no workspace package named {package}")
            continue
        path, data = packages[package]
        if name not in data.get("features", {}):
            failures.append(f"{feature}: {path.relative_to(REPO)} declares no feature {name}")
        if package in dependency_names(data):
            failures.append(
                f"{feature}: {path.relative_to(REPO)} has a dependency named {package}, so "
                f"`--features {feature}` enables it on that dependency, not on the package; rename the dependency"
            )
    for failure in failures:
        print(f"FAIL: {failure}", file=sys.stderr)
    if failures:
        return 1
    print(f"ok: {len(requested)} dev feature(s) name their package unambiguously: {', '.join(requested)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
