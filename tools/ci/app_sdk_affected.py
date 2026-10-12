#!/usr/bin/env python3
"""Whether a change can affect what the app SDK lanes build.

The lanes build tilo-app's CLIs, the browser runtime and desktop bundle, and
every web-capable app under apps/. Linux also qualifies Calculator and the runtime.
A change matters when it touches one of
those packages or a workspace package they depend on, the workflow itself,
or a repository-wide build input. Changes only to packages outside that
closure (the shell, OS services, tools) skip the lanes.

Prints `affected=true|false` and, with --github-output, appends it there.
--lane linux audits unrelated app Rust source before omitting that native lane.
"""

from __future__ import annotations

import argparse
import hashlib
import re
from pathlib import Path
import sys

TOOLS = Path(__file__).resolve().parents[1]
if str(TOOLS) not in sys.path:
    sys.path.append(str(TOOLS))
from ci.affected_packages import resolve_lockfile
from ci.workspace_graph import (
    cargo_metadata, changed_files, closure, comparison_merge_base, file_at, owner_for,
    package_maps, reverse_dependencies,
)

# The CLI builds browser compute, runtime and desktop targets through
# subprocesses; these build inputs need not have Cargo edges back to any app.
SDK_PACKAGES = {
    "tilo-app-cli", "tilo-app-package-cli", "tilo-caps", "tilo-browser-compute",
    "tilo-browser-runtime", "tilo-web-desktop", "runtime-dylib",
}

# Actual Linux commands: native CLI tests/tools, Calculator and its shared
# runtime, plus a generated starter's SDK graph. Web-only packages still keep
# the lane when they change; they do not make every app a native source input.
LINUX_SDK_PACKAGES = {"tilo-app-cli", "tilo-app-package-cli", "tilo-caps", "runtime-dylib"}

# Paths outside any package that change what the lanes build or how they run.
GLOBAL_INPUTS = (
    ".cargo/",
    ".forgejo/workflows/app-sdk.yml",
    ".forgejo/actions/",
    "Cargo.toml",
    "rust-toolchain.toml",
    "tools/ci/app_sdk_affected.py",
    "tools/ci/workspace_graph.py",
    "tools/policy/app_capability",
    "tools/policy/app_portability",
    "tools/policy/check_app_",
    "tools/sdk/",
    "tools/web/",
    "vendor/",
)

# This module is compiled only by tilo-ui's native unit-test target, which the
# affected-package lane retains. Its build script reads three fixed hex
# fixtures, not Rust sources. Bind that audit to its exact bytes: any build
# script change restores the SDK lanes until its inputs have been re-audited.
AUDITED_UNIT_MODULES = {
    "crates/sdk/ui-core/src/app/bound_component_tests.rs": (
        "crates/sdk/ui-core/src/app/host.rs",
        "crates/sdk/ui-core/build.rs",
        "86a75673abed2b2ccb0d13b04af1248219614e292957c14055c39aae2fd3601f",
    ),
}


def audited_unit_module_only(root: Path, path: str, packages: dict, by_dir: dict) -> bool:
    """Omit SDK consumers only for an observed, isolated cfg(test) module."""
    audit = AUDITED_UNIT_MODULES.get(path)
    owner = owner_for(path, by_dir)
    if audit is None or owner is None or owner in SDK_PACKAGES:
        return False
    declaration, script, digest = audit
    changed = root / path
    parent = root / declaration
    build = root / script
    try:
        if any(item.is_symlink() for item in (changed, parent, build, *changed.parents)):
            return False
        changed.read_text()
        if hashlib.sha256(build.read_bytes()).hexdigest() != digest:
            return False
        text = parent.read_text()
        module = (r'^#\[cfg\(test\)\]\s*#\[path\s*=\s*"'
                  + re.escape(changed.name) + r'"\]\s*mod\s+\w+\s*;')
        if text.count(changed.name) != 1 or not re.search(module, text, re.MULTILINE):
            return False
        for package in packages.values():
            directory = Path(package["manifest_path"]).parent
            if any(target.get("kind") != ["test"] and
                   Path(target["src_path"]).resolve() == changed.resolve()
                   for target in package.get("targets", [])):
                return False
            roots = [directory / "src", directory / "examples"]
            if package["name"] in SDK_PACKAGES:
                roots.extend([directory / "tests", directory / "templates"])
            sources = set()
            for source_root in roots:
                if source_root.is_symlink() or any(
                        item.is_symlink() for item in source_root.rglob("*")):
                    return False
                pattern = "*" if source_root == directory / "templates" else "*.rs"
                sources.update(item for item in source_root.rglob(pattern) if item.is_file())
            sources.update(Path(target["src_path"]) for target in package.get("targets", [])
                           if target.get("kind") != ["test"])
            for source in sources:
                if source.is_symlink():
                    return False
                if source == changed or source == parent:
                    continue
                if source.resolve() == changed.resolve() or changed.name in source.read_text():
                    return False
    except (OSError, UnicodeError):
        return False
    return True



def integration_only(root: Path, path: str, packages: dict, by_dir: dict) -> bool:
    """Audit a separate integration target outside the SDK's tested packages.

    SDK consumers build dependency libraries and apps, not their test targets.
    Keep it.rs harnesses, nested fixtures, build scripts and source references affected.
    The ordinary native selector still selects these integration tests.
    """
    relative = Path(path)
    owner = owner_for(path, by_dir)
    if owner is None or owner in SDK_PACKAGES:
        return False
    package = packages[owner]
    directory = Path(package["manifest_path"]).parent / "tests"
    changed = root / relative
    if (changed.parent.resolve() != directory.resolve() or
            relative.suffix != ".rs" or relative.name == "it.rs" or
            changed.is_symlink() or directory.is_symlink()):
        return False
    targets = package.get("targets", [])
    if any("custom-build" in target.get("kind", []) or
           (target.get("kind") != ["test"] and
            Path(target["src_path"]).resolve() == changed.resolve())
           for target in targets):
        return False
    try:
        # A changed standalone target must be observed too; deleted or unknown
        # inputs retain the lanes. Modules need an explicit direct declaration.
        changed.read_text()
        module = r'#\[path\s*=\s*"' + re.escape(relative.name) + r'"\]\s*mod\s+\w+\s*;'
        test_roots = [Path(target["src_path"]) for target in targets
                      if target.get("kind") == ["test"] and
                      Path(target["src_path"]).parent.resolve() == directory.resolve()]
        if not any(test_root.resolve() == changed.resolve() or
                   re.search(module, test_root.read_text()) for test_root in test_roots):
            return False
        # Fail closed if a library, binary, example or build script names the
        # module. Include comments too: a conservative false positive is cheap.
        for package in packages.values():
            package_dir = Path(package["manifest_path"]).parent
            source_roots = [package_dir / "src"]
            if package["name"] in SDK_PACKAGES:
                source_roots.append(package_dir / "tests")
            sources = set()
            for source_root in source_roots:
                # rglob omits directory links and extensionless file aliases.
                # Either can make SDK code consume a dependency's test file.
                if source_root.is_symlink() or any(
                        entry.is_symlink()
                        for entry in source_root.rglob("*")):
                    return False
                sources |= set(source_root.rglob("*.rs"))
            sources |= {Path(target["src_path"]) for target in package.get("targets", [])
                        if target.get("kind") != ["test"]}
            if any(source.resolve() == changed.resolve() or
                   relative.name in source.read_text() for source in sources):
                return False
    except (OSError, UnicodeError):
        return False
    return True


def native_app_source_only(root: Path, path: str, packages: dict,
                           by_dir: dict, native_inputs: set[str], audits: dict[str, bool]) -> bool:
    """Audit app source absent from every Linux SDK command's Cargo graph.

    All manifests, assets, unknown/deleted inputs and cross-source references
    retain the lane. Calculator, the runtime and their dependencies also retain it.
    """
    relative = Path(path)
    owner = owner_for(path, by_dir)
    if owner is None or owner in native_inputs:
        return False
    directory = Path(packages[owner]["manifest_path"]).parent
    app = directory.relative_to(root)
    if (app.parts[:1] != ("apps",) or relative.suffix != ".rs" or
            not relative.is_relative_to(app / "src")):
        return False
    changed = root / relative
    try:
        changed.read_text()
        if any(parent.is_symlink() for parent in (changed, *changed.parents)):
            return False
    except (OSError, UnicodeError):
        return False
    if owner in audits:
        return audits[owner]

    def finish(result: bool) -> bool:
        audits[owner] = result
        return result

    try:
        for name in native_inputs:
            package = packages[name]
            package_dir = Path(package["manifest_path"]).parent
            roots = [package_dir / "src"]
            if name in SDK_PACKAGES:
                roots.append(package_dir / "tests")
            sources = set()
            for source_root in roots:
                if source_root.is_symlink() or any(entry.is_symlink()
                                                  for entry in source_root.rglob("*")):
                    return finish(False)
                sources |= set(source_root.rglob("*.rs"))
            sources |= {Path(target["src_path"]) for target in package.get("targets", [])
                        if target.get("kind") != ["test"]}
            if not sources:
                return finish(False)
            if any(source.resolve().is_relative_to(directory.resolve()) or
                   app.as_posix() in source.read_text() for source in sources):
                return finish(False)
    except (OSError, UnicodeError, ValueError):
        return finish(False)
    return finish(True)


def affected(root: Path, changed: list[str], metadata: dict,
             base: str, head: str, lane: str = "all") -> tuple[bool, str]:
    packages, by_dir = package_maps(metadata, root)
    seeds = {name for name in packages if name in SDK_PACKAGES}
    seeds |= {name for d, name in by_dir.items() if d.parts[:1] == ("apps",)}
    reverse = reverse_dependencies(packages, root)
    native_seeds = seeds & LINUX_SDK_PACKAGES
    calculator = by_dir.get(Path("apps/calculator"))
    if calculator:
        native_seeds.add(calculator)
    native_inputs = {name for name in packages
                     if closure([name], reverse) & native_seeds} if lane == "linux" else set()
    native_audits: dict[str, bool] = {}
    for path in changed:
        if audited_unit_module_only(root, path, packages, by_dir):
            continue
        if integration_only(root, path, packages, by_dir):
            continue
        if (lane == "linux" and calculator and
                native_app_source_only(root, path, packages, by_dir, native_inputs, native_audits)):
            continue
        if path == "Cargo.lock":
            # Attributed to the packages whose locked inputs changed; unattributable
            # differences count as repository-wide.
            lock = resolve_lockfile(file_at(root, base, path), file_at(root, head, path), metadata)
            if lock is None:
                return True, path
            if closure(lock.seeds, reverse) & seeds:
                return True, f"{path} ({', '.join(sorted(lock.seeds)[:3])})"
            continue
        if path.startswith(GLOBAL_INPUTS):
            return True, path
        owner = owner_for(path, by_dir)
        if owner is None:
            # Under apps/ or crates/ but in no package: be safe.
            if path.startswith(("apps/", "crates/")):
                return True, path
            continue
        if closure([owner], reverse) & seeds:
            return True, f"{path} ({owner})"
    return False, ""


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", help="base commit/ref (defaults to HEAD^)")
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--github-output", type=Path)
    parser.add_argument("--lane", choices=("all", "linux"), default="all")
    args = parser.parse_args()
    root = Path.cwd()
    base = comparison_merge_base(root, args.base, args.head)
    hit, why = affected(root, changed_files(root, args.base, args.head), cargo_metadata(root),
                        base, args.head, args.lane)
    output = "linux_affected" if args.lane == "linux" else "affected"
    print(f"{output}={'true' if hit else 'false'}" + (f"  # {why}" if why else ""))
    if args.github_output:
        with args.github_output.open("a") as out:
            out.write(f"{output}={'true' if hit else 'false'}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
