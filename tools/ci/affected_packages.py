#!/usr/bin/env python3
"""Select and balance the workspace packages affected by a git change.

The selector works from Cargo metadata rather than a hand-maintained package
list.  A package is selected when its own files change, or when it transitively
depends on a changed workspace package.  Root Cargo.toml and Cargo.lock changes
are attributed to the packages they reach when the difference can be proved;
other repository-wide build inputs fall back to the complete workspace.
Ordinary feedback checks the closure on x86 and tests changed packages plus
contract suites; --full qualifies both native architectures.
The hermetic suites outside Cargo declared in tools/ci/suites.toml are selected
by the paths they read.
The emitted matrix is consumed directly by Forgejo Actions.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib
from typing import NamedTuple

# Direct CLI execution and importlib-based policy tests both load the same graph.
TOOLS = Path(__file__).resolve().parents[1]
if str(TOOLS) not in sys.path:
    sys.path.append(str(TOOLS))
from ci.workspace_graph import (
    cargo_metadata, changed_files, closure, comparison_merge_base, file_at,
    lockfile_change, owner_for, package_maps, reverse_dependencies, run,
)
from ci import suites as path_suites


# Preserve the existing wire/document/package contract suites, including
# pkg/tests/{app_manifests,verify_attest} and qdoc/tests/{convergence,office_roundtrip}.
# These catch repository contracts outside a changed crate's dependency edges.
ALWAYS_PACKAGES = {"collab-types", "pkg", "qdoc"}
EXTERNAL_ASSET_MANIFEST = "os/external-assets/github-assets.tsv"
# Audited developer/CI prose, not compiled documentation or runtime contracts.
# Keep this explicit: crates/sdk/ui-core/CHARTS.md is included by Rust, docs/app-development.md
# is inspected by tilo-app, and docs/packaging.md is a packaging contract input.
# Security, layout and workflow policy checks remain mandatory outside Rust.
NON_COMPILING_DOCS = {
    "AGENTS.md",
    "DEVELOPMENT.md",
    "docs/ci-performance.md",
    "docs/ci-emergency-relief.md",
    "docs/headless-testing.md",
}
# These orchestration/validation helpers are exercised by the mandatory quality
# job's tools/policy/run.sh unittest discovery. They are not Rust build
# inputs. Keep exact source/test pairs: unknown tools, deleted files, workflows,
# compiler wrappers and packaging implementation changes retain native coverage.
QUALITY_ONLY_HELPERS = {
    ".githooks/pre-commit": "tools/tests/test_pre_commit_contract.py",
    "scripts/ci-run.sh": "tools/tests/test_ci_run.py",
    "tools/ci/check-changed-rustfmt.sh": "tools/tests/test_changed_rust_comparison.py",
    "tools/ci/check_changed_rust_allows.py": "tools/tests/test_changed_rust_comparison.py",
    "tools/ci/measure.sh": "tools/tests/test_ci_measure.py",
    "tools/ci/affected_packages.py": "tools/tests/test_ci_select.py",
    "tools/ci/workspace_graph.py": "tools/tests/test_ci_select.py",
    "tools/ci/queue_coalesce.py": "tools/tests/test_ci_queue_coalesce.py",
    "tools/ci/timing_report.py": "tools/tests/test_ci_timing_report.py",
    "tools/ci/merge_pr.py": "tools/tests/test_ci_merge_pr.py",
    "tools/ci/builder_workflow_plan.py": "tools/tests/test_builder_workflow_plan.py",
    "tools/ci/resolve_promoted_builder.py": "tools/tests/test_resolve_promoted_builder.py",
    "tools/ci/native-mold-linker.sh": "tools/tests/test_ci_native_mold.py",
    "tools/ci/verify-native-mold.sh": "tools/tests/test_ci_native_mold.py",
    "tools/ci/dispatch_merge.py": "tools/tests/test_ci_merge_dispatch.py",
    "tools/ci/workflow_applicability.py": "tools/tests/test_workflow_applicability.py",
    "tools/ci/suites.py": "tools/tests/test_suites.py",
    "tools/ci/suites.toml": "tools/tests/test_suites.py",
    "tools/policy/run.sh": "tools/tests/test_ci_select.py",
    "tools/policy/check_workspace_members.py": "tools/tests/test_check_workspace_members.py",
    "tools/policy/check_repository_layout.py": "tools/tests/test_check_repository_layout.py",
    "tools/policy/check_workflow_actions.py": "tools/tests/test_workflow_actions.py",
    "tools/policy/check_workflow_package_paths.py": "tools/tests/test_workflow_package_paths.py",
    "tools/policy/check_workflow_timeouts.py": "tools/tests/test_workflow_timeouts.py",
    "tools/policy/check_ui_clock_contract.py": "tools/tests/test_ui_clock_contract.py",
}
# Control-plane files are validated by tests that read or execute the production
# file in the mandatory repository-policy suite. A file missing from this exact
# map retains native Rust checks.
QUALITY_ONLY_CONTROL_INPUTS = {
    ".forgejo/workflows/build-builder-image.yml": (
        "os/tests/test_builder_candidate_workflow.py",
        "os/tests/release_pipeline_invariants.sh",
        "os/tests/workflow_dependency_invariants.sh",
        "tools/tests/test_cargo_download_cache_action.py",
        "tools/tests/test_ci_native_mold.py",
        "tools/tests/test_cosign_install_action.py",
    ),
    ".forgejo/workflows/build-kernel.yml": (
        "tools/tests/test_workflow_applicability.py",
        "os/tests/workflow_dependency_invariants.sh",
    ),
    ".forgejo/workflows/dev-disk.yml": (
        "tools/tests/test_dev_disk_workflow.py",
        "os/tests/workflow_dependency_invariants.sh",
    ),
    ".forgejo/workflows/check.yml": (
        "tools/tests/test_ci_check_aggregate.py",
        "tools/tests/test_ci_queue_relief.py",
        "tools/tests/test_ci_receipt.sh",
    ),
    ".forgejo/workflows/legacy-app-e2e.yml": (
        "tools/tests/test_ci_queue_relief.py",
        "os/tests/test_builder_candidate_workflow.py",
    ),
    ".forgejo/workflows/tilo-relay.yaml": (
        "tools/tests/test_workflow_package_paths.py",
        "tools/tests/test_workflow_applicability.py",
        "os/tests/workflow_dependency_invariants.sh",
    ),
}
NATIVE_RUNNERS = {"x86_64": "runner", "aarch64": "runner-arm64"}
# A tilo-ui lib-test link has repeatedly exceeded 5 GiB RSS by itself. Its
# affected shard also links shell and other test targets, so the general 10 GiB
# envelope can OOM even when the source is valid. ARM has no large label and is
# already serialized by the workflow.
LARGE_X86_TEST_PACKAGES = {"tilo-ui"}
GLOBAL_FILES = {
    "Cargo.toml",
    "Cargo.lock",
    "clippy.toml",
    "rust-toolchain.toml",
    "deny.toml",
    "os/Containerfile.builder",
    EXTERNAL_ASSET_MANIFEST,
}
GLOBAL_PREFIXES = (
    ".cargo/",
    ".config/",
    "vendor/",
)
HEAVY_PACKAGES = {
    "browser",
    "ebook",
    "media",
    "qdoc-pdf",
    "shell",
    "slides",
    "tube",
    "typst-editor",
    "ui",
    "video-editor",
    "word-processor",
}

# Below this structural cost, a second clean target directory spends more CPU
# rebuilding shared dependencies than it saves in wall time. Shard timing and
# CPU figures are emitted by tools/ci/measure.sh so this can be tuned from runs.
SHARD_COST_BUDGET = 1_000
# Ordinary global-input checks use at most two existing x86 runners (four
# Cargo workers each). Keep small/reverse-closure changes on one warm target;
# explicit full qualification retains its native architecture matrix.
MAX_DEVELOPER_SHARDS = 2


def manifest_at(root: Path, revision: str) -> dict[str, tuple[str, str]] | None:
    result = subprocess.run(
        ["git", "show", f"{revision}:{EXTERNAL_ASSET_MANIFEST}"],
        cwd=root, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    if result.returncode:
        return None
    try:
        text = result.stdout.decode("utf-8")
    except UnicodeDecodeError:
        return None
    entries: dict[str, tuple[str, str]] = {}
    for line in text.splitlines():
        if not line or line.startswith("#"):
            continue
        fields = line.split("\t")
        if len(fields) != 3 or not all(fields):
            return None
        checksum, name, url = fields
        if not re.fullmatch(r"[0-9a-f]{64}", checksum) or name in entries:
            return None
        entries[name] = (checksum, url)
    return entries


def builder_mold_asset(name: str, entry: tuple[str, str]) -> bool:
    match = re.fullmatch(
        r"mold-(\d+\.\d+\.\d+)-(x86_64|aarch64)-linux\.tar\.gz", name
    )
    if not match:
        return False
    version = match.group(1)
    return entry[1] == (
        f"https://github.com/rui314/mold/releases/download/v{version}/{name}"
    )


def builder_only_asset_manifest_change(root: Path, base: str | None, head: str) -> bool:
    merge_base = comparison_merge_base(root, base, head)
    before = manifest_at(root, merge_base)
    after = manifest_at(root, head)
    if before is None or after is None:
        return False
    changed_names = {
        name for name in before.keys() | after.keys()
        if before.get(name) != after.get(name)
    }
    return bool(changed_names) and all(
        (name not in before or builder_mold_asset(name, before[name]))
        and (name not in after or builder_mold_asset(name, after[name]))
        for name in changed_names
    )


class ResolvedInput(NamedTuple):
    """A root manifest/lockfile change attributed to specific workspace packages.

    ``seeds`` are checked with their reverse dependencies and tested; ``checked``
    are additionally type-checked. ``native`` reports whether the change reaches
    the vendored native dependency contract.
    """
    seeds: frozenset[str]
    checked: frozenset[str]
    native: bool
    detail: str


# Workspace tables whose semantic difference maps to specific packages. Every
# other root manifest key (lints, workspace.package, exclude, default-members,
# resolver, metadata, profile, patch, replace, a root package, ...) keeps the
# complete workspace.
ATTRIBUTABLE_WORKSPACE_KEYS = {"members", "dependencies"}
NATIVE_CONTRACT_PACKAGE = "openh264-sys2"


def resolve_lockfile(before: bytes | None, after: bytes | None, metadata: dict) -> ResolvedInput | None:
    packages = {package["name"] for package in metadata["packages"]}
    change = lockfile_change(before, after, packages)
    if change is None:
        return None
    try:
        locked_local = {
            entry["name"] for entry in tomllib.loads(after.decode("utf-8")).get("package", [])
            if "source" not in entry
        }
    except (KeyError, TypeError, UnicodeDecodeError, tomllib.TOMLDecodeError):
        return None
    # Attribution relies on every workspace member being a lockfile node.
    if not packages <= locked_local:
        return None
    return ResolvedInput(
        seeds=change.seeds,
        checked=frozenset(change.reached & packages),
        native=NATIVE_CONTRACT_PACKAGE in change.reached,
        detail=f"Cargo.lock reaches {len(change.seeds)} packages directly",
    )


def _dependency_names(entry: object, key: str) -> set[str]:
    names = {key}
    if isinstance(entry, dict) and isinstance(entry.get("package"), str):
        names.add(entry["package"])
    return names


def resolve_root_manifest(
    before: bytes | None, after: bytes | None, metadata: dict, root: Path
) -> ResolvedInput | None:
    """Attribute workspace member and shared dependency edits to their users."""
    if before is None or after is None:
        return None
    try:
        old = tomllib.loads(before.decode("utf-8"))
        new = tomllib.loads(after.decode("utf-8"))
    except (UnicodeDecodeError, tomllib.TOMLDecodeError):
        return None
    if {key: value for key, value in old.items() if key != "workspace"} != {
        key: value for key, value in new.items() if key != "workspace"
    }:
        return None
    old_ws, new_ws = old.get("workspace"), new.get("workspace")
    if not isinstance(old_ws, dict) or not isinstance(new_ws, dict):
        return None
    changed_keys = {key for key in old_ws.keys() | new_ws.keys() if old_ws.get(key) != new_ws.get(key)}
    if not changed_keys <= ATTRIBUTABLE_WORKSPACE_KEYS:
        return None

    packages, by_dir = package_maps(metadata, root)
    seeds: set[str] = set()
    details = []
    if "members" in changed_keys:
        old_members, new_members = old_ws.get("members", []), new_ws.get("members", [])
        if not all(
            isinstance(members, list)
            and all(isinstance(member, str) and not re.search(r"[*?\[]", member) for member in members)
            for members in (old_members, new_members)
        ):
            return None
        added = set(new_members) - set(old_members)
        removed = set(old_members) - set(new_members)
        for member in added:
            owner = by_dir.get(Path(os.path.normpath(member)))
            if owner is None:
                return None
            seeds.add(owner)
        removed_dirs = {(root / member).resolve() for member in removed}
        seeds.update(
            name for name, package in packages.items()
            if any(dependency.get("path") and Path(dependency["path"]).resolve() in removed_dirs
                   for dependency in package.get("dependencies", []))
        )
        details.append(f"{len(added)} added and {len(removed)} removed members")
    if "dependencies" in changed_keys:
        old_deps, new_deps = old_ws.get("dependencies", {}), new_ws.get("dependencies", {})
        if not isinstance(old_deps, dict) or not isinstance(new_deps, dict):
            return None
        names: set[str] = set()
        changed_deps = {key for key in old_deps.keys() | new_deps.keys() if old_deps.get(key) != new_deps.get(key)}
        for key in changed_deps:
            names |= _dependency_names(old_deps.get(key), key) | _dependency_names(new_deps.get(key), key)
        # Match both the dependency package and its local rename. Direct,
        # non-inherited uses of the same crate can over-select, never omit.
        seeds.update(
            name for name, package in packages.items()
            if any(dependency.get("name") in names or dependency.get("rename") in names
                   for dependency in package.get("dependencies", []))
        )
        details.append(f"{len(changed_deps)} shared dependencies")
    return ResolvedInput(frozenset(seeds), frozenset(), False,
                         "Cargo.toml changes " + " and ".join(details))


def resolve_manifest_inputs(
    root: Path, base: str | None, head: str, metadata: dict, changes: list[str]
) -> dict[str, ResolvedInput]:
    """Return the root manifest/lockfile changes that can be attributed exactly."""
    resolvers = {"Cargo.lock": resolve_lockfile,
                 "Cargo.toml": lambda old, new, meta: resolve_root_manifest(old, new, meta, root)}
    wanted = [path for path in resolvers if path in changes]
    if not wanted:
        return {}
    merge_base = comparison_merge_base(root, base, head)
    resolved = {}
    for path in wanted:
        result = resolvers[path](file_at(root, merge_base, path), file_at(root, head, path), metadata)
        if result is not None:
            resolved[path] = result
    return resolved


def package_cost(pkg: dict) -> int:
    """Stable relative cost used only to balance shards.

    CI records elapsed/user/system time for every shard.  This structural score
    remains stable between runs while separating known heavy linkers and
    accounting for targets and direct dependencies.
    """
    targets = len(pkg.get("targets", []))
    local_and_external_deps = len(pkg.get("dependencies", []))
    score = 10 + targets * 3 + min(local_and_external_deps, 40)
    if pkg["name"] in HEAVY_PACKAGES:
        score *= 4
    return score


def balance(names: set[str], packages: dict[str, dict], shard_count: int) -> list[dict]:
    if not names:
        return []
    shards = [{"names": [], "cost": 0} for _ in range(max(1, min(shard_count, len(names))))]
    for name in sorted(names, key=lambda n: (-package_cost(packages[n]), n)):
        shard = min(shards, key=lambda item: (item["cost"], len(item["names"])))
        shard["names"].append(name)
        shard["cost"] += package_cost(packages[name])
    result = []
    for index, shard in enumerate(shards, 1):
        names_sorted = sorted(shard["names"])
        result.append(
            {
                "shard": index,
                "packages": " ".join(f"-p {name}" for name in names_sorted),
                "package_count": len(names_sorted),
                "estimated_cost": shard["cost"],
            }
        )
    return result


def useful_shard_count(names: set[str], packages: dict[str, dict], maximum: int) -> int:
    total_cost = sum(package_cost(packages[name]) for name in names)
    cost_limited = max(1, (total_cost + SHARD_COST_BUDGET - 1) // SHARD_COST_BUDGET)
    return min(max(1, maximum), len(names), cost_limited)


def native_dependencies_required(
    root: Path,
    changes: list[str],
    force_full: bool = False,
    resolved_inputs: dict[str, ResolvedInput] | None = None,
) -> bool:
    """Select a declared native dependency contract without hiding missing payloads.

    A root manifest or lockfile change that was attributed exactly selects the
    contract only when it reaches the vendored native crate's locked graph.
    """
    manifest = tomllib.loads((root / "Cargo.toml").read_text())
    declared = "openh264-sys2" in manifest.get("patch", {}).get("crates-io", {})
    # Introduction/deletion of native sources must be checked even if the patch
    # declaration was also removed. Never infer applicability from file presence.
    if any(path.startswith("vendor/openh264-sys2/") for path in changes):
        return True
    if not declared:
        return False
    files = {"Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "os/Containerfile.builder",
             ".forgejo/workflows/check.yml", "os/tests/test_openh264_source_order.py",
             "tools/ci/affected_packages.py", "tools/ci/workspace_graph.py", "tools/tests/test_ci_select.py",
             "tools/tests/test_ci_check_aggregate.py"}
    prefixes = (".cargo/", ".forgejo/actions/prepare-rust-build/")
    resolved_inputs = resolved_inputs or {}
    return force_full or any(
        resolved_inputs[path].native if path in resolved_inputs
        else path in files or path.startswith(prefixes)
        for path in changes
    )


def make_plan(
    metadata: dict,
    root: Path,
    changes: list[str],
    shard_count: int,
    *,
    force_full: bool = False,
    builder_only_external_assets: bool = False,
    allow_security_skip: bool = False,
    resolved_inputs: dict[str, ResolvedInput] | None = None,
    suite_manifest: path_suites.Manifest | None = None,
    all_suites: bool = False,
) -> dict:
    packages, by_dir = package_maps(metadata, root)
    resolved_inputs = {path: resolved for path, resolved in (resolved_inputs or {}).items()
                       if path in changes}
    seeds = {owner for path in changes if (owner := owner_for(path, by_dir))}
    checked: set[str] = set()
    for resolved in resolved_inputs.values():
        seeds.update(resolved.seeds & packages.keys())
        checked.update(resolved.checked & packages.keys())
    projected_quality_paths = (
        {EXTERNAL_ASSET_MANIFEST} if builder_only_external_assets else set()
    )
    global_change = any(
        (path in GLOBAL_FILES and path not in projected_quality_paths
         and path not in resolved_inputs)
        or path.startswith(GLOBAL_PREFIXES)
        for path in changes
    )
    workspace_source_roots = ("apps/", "crates/")
    unmapped_workspace_change = any(
        path.startswith(workspace_source_roots) and owner_for(path, by_dir) is None
        for path in changes
    )
    # Only audited developer/CI and design prose is non-compiling. Other docs can contain
    # generated contracts, fixtures or embedded Rust inputs and retain coverage.
    def prose(path: str) -> bool:
        return (
        path in NON_COMPILING_DOCS or (
            path.startswith("docs/designs/") and path.endswith(".md")
            and ".." not in Path(path).parts))

    quality_helper_paths = {
        path for source, test in QUALITY_ONLY_HELPERS.items()
        if (root / source).is_file() and (root / test).is_file()
        for path in (source, test)
    }
    security_prose = {
        path for path in changes if prose(path) and (root / path).is_file()
    }
    # Security may be omitted only for pull-request inputs whose observers do
    # not live in the security suite itself. Workflow and OS control inputs are
    # added to Rust's broader quality-only set below, after this decision.
    security_not_applicable = (
        allow_security_skip
        and not force_full
        and bool(changes)
        and all(path in security_prose or path in quality_helper_paths for path in changes)
    )
    quality_paths = set(quality_helper_paths)
    quality_paths.update(
        path for source, observers in QUALITY_ONLY_CONTROL_INPUTS.items()
        if (root / source).is_file()
        and all((root / observer).is_file() for observer in observers)
        for path in (source, *observers)
    )
    # Ordinary checks use the already-published builder, so compiling Rust
    # cannot validate an edited recipe. Its dedicated workflow builds and
    # qualifies the candidate. Deletion and mixed build inputs remain full.
    recipe_present = (
        "os/Containerfile.builder" in changes
        and (root / "os/Containerfile.builder").is_file()
    )
    quality_only = bool(changes) and all(
        prose(path) or path in quality_paths
        or path in projected_quality_paths
        or (recipe_present and path == "os/Containerfile.builder")
        for path in changes
    )
    if quality_only and not force_full:
        selected = set()
        reason = ("builder recipe covered by mandatory policy checks; candidate image qualification is separate"
                  if recipe_present else
                  "audited developer/CI or design prose only; Rust checks not applicable"
                  if all(prose(path) for path in changes) else
                  "audited CI helpers covered by mandatory repository-policy tests; Rust checks not applicable")
        full = False
    elif force_full or global_change or unmapped_workspace_change:
        selected = set(packages)
        reason = ("full workspace explicitly requested" if force_full else
                  "repository-wide build input changed" if global_change else "workspace path has no package owner")
        full = True
    else:
        selected = closure(seeds | checked, reverse_dependencies(packages, root))
        selected.update(ALWAYS_PACKAGES & packages.keys())
        reason = f"{len(seeds)} changed packages, their reverse dependencies, and contract packages"
        if resolved_inputs:
            reason += " (" + "; ".join(item.detail for _, item in sorted(resolved_inputs.items())) + ")"
        full = False
    # Ordinary feedback type-checks the entire reverse-dependency closure once
    # on x86, but links tests only for changed crates and the existing contract
    # suites. Global/unowned workspace inputs retain complete test coverage.
    tested = (selected if full else (seeds | ALWAYS_PACKAGES) & selected)
    shard_limit = (shard_count if force_full else
                   min(shard_count, MAX_DEVELOPER_SHARDS) if full else 1)
    effective_shards = useful_shard_count(selected, packages, shard_limit)
    runners = NATIVE_RUNNERS if force_full else {"x86_64": NATIVE_RUNNERS["x86_64"]}
    matrix = []
    for arch, runner in runners.items():
        for shard in balance(selected, packages, effective_shards):
            shard_names = set(shard["packages"].split()[1::2])
            test_names = sorted(tested & shard_names)
            selected_runner = (
                "runner-large"
                if arch == "x86_64" and LARGE_X86_TEST_PACKAGES.intersection(test_names)
                else runner
            )
            matrix.append({**shard, "arch": arch, "runner": selected_runner, "check_runner": runner,
                           "test_packages": " ".join(f"-p {name}" for name in test_names)})
    # Hermetic suites outside Cargo follow their declared inputs; explicit full
    # runs and pushes to main (all_suites) run every one of them.
    suites = (path_suites.select(suite_manifest, changes, run_all=force_full or all_suites)
              if suite_manifest is not None else [])
    return {
        "full": full,
        "rust_required": bool(selected),
        "security_required": not security_not_applicable,
        "native_dependencies_required": native_dependencies_required(
            root, changes, force_full, resolved_inputs),
        "suites_required": bool(suites),
        "suites": suites,
        "reason": reason,
        "changed_files": changes,
        "selected_count": len(selected),
        "workspace_count": len(packages),
        "selected": sorted(selected),
        "tested": sorted(tested),
        "qualification": "full-native" if force_full else "developer-feedback",
        "estimated_cost": sum(package_cost(packages[name]) for name in selected),
        "requested_shards": shard_count,
        "shard_count": effective_shards,
        "matrix": {"include": matrix},
        # Clippy's lint result is architecture-independent; lint each package
        # set once, on x86_64.
        "clippy_matrix": {"include": [row for row in matrix if row["arch"] == "x86_64"]},
    }


def emit_github(plan: dict, output: Path) -> None:
    compact_matrix = json.dumps(plan["matrix"], separators=(",", ":"))
    with output.open("a", encoding="utf-8") as handle:
        handle.write(f"matrix={compact_matrix}\n")
        compact_clippy_matrix = json.dumps(plan["clippy_matrix"], separators=(",", ":"))
        handle.write(f"clippy_matrix={compact_clippy_matrix}\n")
        handle.write(f"full={'true' if plan['full'] else 'false'}\n")
        handle.write(f"rust_required={'true' if plan['rust_required'] else 'false'}\n")
        handle.write(f"security_required={'true' if plan['security_required'] else 'false'}\n")
        handle.write(f"native_dependencies_required={str(plan['native_dependencies_required']).lower()}\n")
        handle.write(f"suites_required={'true' if plan['suites_required'] else 'false'}\n")
        handle.write(f"suites={' '.join(plan['suites'])}\n")
        handle.write(f"selected_count={plan['selected_count']}\n")
        handle.write(f"workspace_count={plan['workspace_count']}\n")
        handle.write(f"estimated_cost={plan['estimated_cost']}\n")
        handle.write(f"shard_count={plan['shard_count']}\n")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", help="base commit/ref (defaults to HEAD^)")
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--shards", type=int, default=2)
    parser.add_argument("--full", action="store_true", help="check the complete workspace on each native architecture")
    parser.add_argument(
        "--allow-security-skip",
        action="store_true",
        help="allow audited pull-request-only inputs to omit security invariants",
    )
    parser.add_argument(
        "--all-suites",
        action="store_true",
        help="run every declared hermetic suite regardless of changed paths",
    )
    parser.add_argument("--github-output", type=Path)
    parser.add_argument("--metadata", type=Path, help="read metadata JSON instead of invoking Cargo")
    parser.add_argument("--changes", type=Path, help="read changed paths instead of invoking git")
    args = parser.parse_args()

    root = Path(run("git", "rev-parse", "--show-toplevel", cwd=Path.cwd()))
    metadata = json.loads(args.metadata.read_text()) if args.metadata else cargo_metadata(root)
    changes = args.changes.read_text().splitlines() if args.changes else changed_files(root, args.base, args.head)
    builder_only_external_assets = (
        args.changes is None
        and EXTERNAL_ASSET_MANIFEST in changes
        and builder_only_asset_manifest_change(root, args.base, args.head)
    )
    resolved_inputs = (
        resolve_manifest_inputs(root, args.base, args.head, metadata, changes)
        if args.changes is None and not args.full else {}
    )
    plan = make_plan(
        metadata,
        root,
        changes,
        args.shards,
        force_full=args.full,
        builder_only_external_assets=builder_only_external_assets,
        allow_security_skip=args.allow_security_skip,
        resolved_inputs=resolved_inputs,
        suite_manifest=path_suites.load(root),
        all_suites=args.all_suites,
    )
    print(json.dumps(plan, indent=2))
    if args.github_output:
        emit_github(plan, args.github_output)
    return 0


if __name__ == "__main__":
    sys.exit(main())
