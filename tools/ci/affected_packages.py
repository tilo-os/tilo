#!/usr/bin/env python3
"""Select and balance the workspace packages affected by a git change.

The selector works from Cargo metadata rather than a hand-maintained package
list.  A package is selected when its own files change, or when it transitively
depends on a changed workspace package.  Root Cargo.toml and Cargo.lock changes
are attributed to the packages they reach when the difference can be proved;
other repository-wide build inputs fall back to the complete workspace.
Ordinary feedback checks the closure on x86 and tests changed packages, their
direct dependants (within one shard's cost) and contract suites; --full
qualifies both native architectures.
The hermetic suites outside Cargo declared in tools/ci/suites.toml are selected
by the paths they read, as are the packages it declares to read repository files
outside their own directory.
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
from ci import runner_labels, qualified_native
from ci import suites as path_suites


# Preserve the existing wire/document/package contract suites, including
# pkg/tests/{app_manifests,verify_attest} and qdoc-codecs/tests/{convergence,office_roundtrip}.
# These catch repository contracts outside a changed crate's dependency edges.
ALWAYS_PACKAGES = {"collab-types", "pkg", "qdoc", "qdoc-codecs"}
EXTERNAL_ASSET_MANIFEST = "os/external-assets/github-assets.tsv"
# Snapshot pins CI publishes and signs (kernel only; RPM snapshots resolve by
# fingerprint). A pull request that changes only these changes no code:
# check.yml's `pins` job proves each changed pin is the signed snapshot
# published for this source (os/dev/verify-snapshot-pins.sh) instead of the
# Rust and security suites, which main runs after the merge.
PIN_FILES = ("os/kernel-snapshot.env",)
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
    "tools/ci/check_lock_update.py": "tools/tests/test_lock_update.py",
    "tools/ci/measure.sh": "tools/tests/test_ci_measure.py",
    "tools/ci/affected_packages.py": "tools/tests/test_ci_select.py",
    "tools/ci/app_sdk_affected.py": "tools/tests/test_app_sdk_affected.py",
    "tools/ci/recheck_base.py": "tools/tests/test_recheck_base.py",
    "tools/ci/workspace_graph.py": "tools/tests/test_ci_select.py",
    "tools/ci/recorded_test_time.json": "tools/tests/test_ci_select.py",
    "tools/ci/recorded_test_time.py": "tools/tests/test_ci_recorded_test_time.py",
    "tools/rust_feedback.py": "tools/tests/test_rust_feedback.py",
    "tools/ci/timing_report.py": "tools/tests/test_ci_timing_report.py",
    "tools/ci/nextest_report.py": "tools/tests/test_nextest_report.py",
    "tools/ci/run_scoped_tests.py": "tools/tests/test_ci_scoped_tests.py",
    "tools/ci/builder_workflow_plan.py": "tools/tests/test_builder_workflow_plan.py",
    "tools/ci/resolve_promoted_builder.py": "tools/tests/test_resolve_promoted_builder.py",
    "tools/ci/native-mold-linker.sh": "tools/tests/test_ci_native_mold.py",
    "tools/ci/verify-native-mold.sh": "tools/tests/test_ci_native_mold.py",
    "tools/ci/suites.py": "tools/tests/test_suites.py",
    "tools/ci/suites.toml": "tools/tests/test_suites.py",
    "tools/policy/run.sh": "tools/tests/test_ci_select.py",
    "tools/policy/check_workspace_members.py": "tools/tests/test_check_workspace_members.py",
    "tools/policy/check_repository_layout.py": "tools/tests/test_check_repository_layout.py",
    "tools/policy/check_workflow_actions.py": "tools/tests/test_workflow_actions.py",
    "tools/policy/check_workflow_package_paths.py": "tools/tests/test_workflow_package_paths.py",
    "tools/policy/check_workflow_timeouts.py": "tools/tests/test_workflow_timeouts.py",
    "tools/policy/check_ui_clock_contract.py": "tools/tests/test_ui_clock_contract.py",
    "tools/policy/check_settings_writes.py": "tools/tests/test_settings_writes.py",
    "tools/policy/check_app_content_store.py": "tools/tests/test_app_content_store.py",
    "tools/policy/check_document_engine_boundary.py": "tools/tests/test_document_engine_boundary.py",
}
# Control-plane files are validated by tests that read or execute the production
# file in the mandatory repository-policy suite. A file missing from this exact
# map retains native Rust checks.
QUALITY_ONLY_CONTROL_INPUTS = {
    "os/dev/ci-native-podman-store.sh": (
        "tools/tests/test_ci_native_podman_store.py",
        "os/tests/disk_image_root_podman_store.sh",
    ),
    # These wrappers select/request the existing image workflow; they change
    # no compiler, app source or packaging inputs. Mandatory policy tests
    # observe their request gate, paths, architecture selection and delegation.
    ".forgejo/workflows/os-dev.yml": (
        "tools/tests/test_os_dev_arches.py",
        "tools/tests/test_os_release_matrix.py",
    ),
    ".forgejo/workflows/os-dev-x86.yml": (
        "tools/tests/test_os_dev_arches.py",
    ),
    ".forgejo/workflows/os-dev-arm.yml": (
        "tools/tests/test_os_dev_arches.py",
    ),
    "os/dev/rpm-snapshot-fingerprint.py": (
        "os/tests/test_rpm_snapshot_fingerprint.py",
        "tools/tests/test_rpm_pin_contract.py",
    ),
    "os/dev/resolve-rpm-snapshot.sh": (
        "os/tests/test_rpm_snapshot_fingerprint.py",
        "tools/tests/test_rpm_pin_contract.py",
    ),
    ".forgejo/workflows/build-ci-builder.yml": (
        "os/tests/test_builder_candidate_workflow.py",
        "os/tests/release_pipeline_invariants.sh",
        "os/tests/workflow_dependency_invariants.sh",
        "tools/tests/test_cargo_download_cache_action.py",
        "tools/tests/test_ci_native_mold.py",
        "tools/tests/test_cosign_install_action.py",
    ),
    ".forgejo/workflows/build-kernel.yml": (
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
        "tools/tests/test_resolve_ci_builder.py",
    ),
    ".forgejo/workflows/ci-legacy-apps.yml": (
        "tools/tests/test_ci_queue_relief.py",
        "os/tests/test_builder_candidate_workflow.py",
    ),
    ".forgejo/workflows/release-relay.yml": (
        "tools/tests/test_workflow_package_paths.py",
        "os/tests/workflow_dependency_invariants.sh",
    ),
}
NATIVE_RUNNERS = runner_labels.NATIVE
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
    "tilo-media",
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
# Ordinary feedback also tests the direct dependants of changed packages, as
# long as together they cost no more than one shard. That covers the direct
# consumers of almost every package (138 of 149 at the time of writing,
# including tilo-ui's six). The dependants of foundation crates such as
# tilo-actions, tilo-app, runtime-dylib or qdoc would add 20-60% of the whole
# workspace to every such change; they stay type-checked here and are
# tested nightly on both architectures.
REVERSE_TEST_COST_BUDGET = SHARD_COST_BUDGET
# Recorded test execution seconds per package (tools/ci/recorded_test_time.py). A
# shard's test job links its tests and then runs them on about four threads;
# one recorded test second costs roughly as much shard wall time as two units
# of structural cost. Only shard balancing uses these numbers.
TEST_SECONDS = Path(__file__).with_name("recorded_test_time.json")
TEST_SECOND_WEIGHT = 2
# A package without a recorded time (new since the last refresh) is assumed to
# test for about as long as the upper quartile of recorded packages, so a new
# service suite does not ride along as free weight on an already full shard.
UNRECORDED_TEST_SECONDS = 20
# Test jobs are sharded on their own, by recorded test time: one test job per
# this many recorded seconds, so a large tested set runs its suites on several
# runners instead of queueing them on one. Each test job links only its own
# packages' tests; the type-check shards above are unaffected.
TEST_SHARD_SECONDS = 500
MAX_TEST_SHARDS = 4
# A tested package heavier than this (structural cost plus weighted recorded
# test seconds) gets a test job of its own, so it builds and tests beside the
# balanced shards instead of lengthening one of them.
DEDICATED_TEST_WEIGHT = 1_000
# Suites whose tests fsync heavily: fs's durability, checked-restore and
# release-matrix tests over real FUSE (with fs-native and private-fs), and the
# host suites that commit a durable Store, owner journal or relay journal on
# every step. On one runner their fsyncs queue behind each other until the host
# suites' handshake bounds expire, so test shards keep them apart whenever
# there is more than one shard.
TEST_SHARD_APART = (
    frozenset({"tilo-fs", "fs-native", "private-fs"}),
    frozenset({
        "tilo-action-broker",
        "tilo-peer-state",
        "tilo-relay",
    }),
)


def recorded_test_seconds(path: Path = TEST_SECONDS) -> dict[str, int]:
    """Per-package test seconds; absent data balances on structure alone."""
    if not path.is_file():
        return {}
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict) or not all(
            isinstance(name, str) and type(value) is int and value >= 0
            for name, value in data.items()):
        raise ValueError(f"{path} must map package names to whole seconds")
    return data


def expected_test_seconds(name: str, test_seconds: dict[str, int] | None) -> int:
    """Recorded seconds, the unrecorded default when timings exist, else zero."""
    if not test_seconds:
        return 0
    return test_seconds.get(name, UNRECORDED_TEST_SECONDS)


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


def image_tool_asset(name: str, entry: tuple[str, str]) -> bool:
    """chunkah's release source and vendor tree: only os/Containerfile.rechunk reads them (TIL-747)."""
    match = re.fullmatch(r"chunkah-(\d+\.\d+\.\d+)(?:-vendor)?\.tar\.gz", name)
    if not match:
        return False
    return entry[1] == (
        f"https://github.com/coreos/chunkah/releases/download/v{match.group(1)}/{name}"
    )


def no_workspace_asset(name: str, entry: tuple[str, str]) -> bool:
    return builder_mold_asset(name, entry) or image_tool_asset(name, entry)


def builder_only_asset_manifest_change(root: Path, base: str | None, head: str) -> bool:
    """Whether every changed manifest row is an asset no workspace build reads."""
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
        (name not in before or no_workspace_asset(name, before[name]))
        and (name not in after or no_workspace_asset(name, after[name]))
        for name in changed_names
    )


class ResolvedInput(NamedTuple):
    """A root manifest/lockfile change attributed to specific workspace packages.

    ``seeds`` are checked with their reverse dependencies and tested; ``checked``
    are additionally type-checked.
    """
    seeds: frozenset[str]
    checked: frozenset[str]
    detail: str


# Workspace tables whose semantic difference maps to specific packages. Every
# other root manifest key (lints, workspace.package, exclude, default-members,
# resolver, metadata, profile, patch, replace, a root package, ...) keeps the
# complete workspace.
ATTRIBUTABLE_WORKSPACE_KEYS = {"members", "dependencies"}


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
    return ResolvedInput(frozenset(seeds), frozenset(),
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


def has_native_tests(package: dict) -> bool:
    """Conservatively retain runnable targets; whole-crate wasm gates are native-empty."""
    for target in package.get("targets", []):
        if target.get("test") is False or "custom-build" in target.get("kind", []):
            continue
        source = target.get("src_path")
        if source is None:
            return True
        try:
            with Path(source).open(encoding="utf-8") as stream:
                for _, line in zip(range(128), stream):
                    stripped = line.strip()
                    if not stripped or stripped.startswith("//"):
                        continue
                    if not re.fullmatch(r'#!\[\s*cfg\(\s*target_arch\s*=\s*"wasm32"\s*\)\s*\]', stripped):
                        return True
                    break
                else:
                    return True
        except (OSError, UnicodeError):
            return True
    return False


def direct_dependants_within_budget(
    seeds: set[str],
    reverse: dict[str, set[str]],
    packages: dict[str, dict],
    already_tested: set[str],
    budget: int | None = None,
) -> tuple[set[str], list[str]]:
    """Direct dependants of the seeds to test, cheapest seed first, within budget.

    Returns the dependants to add and the seeds whose dependants were left to
    nightly because they would exceed the budget.
    """
    if budget is None:
        budget = REVERSE_TEST_COST_BUDGET
    added: set[str] = set()
    deferred: list[str] = []
    spent = 0

    def extra(seed: str) -> set[str]:
        return reverse.get(seed, set()) - already_tested - added

    def cost(names: set[str]) -> int:
        return sum(package_cost(packages[name]) for name in names)

    for seed in sorted(seeds, key=lambda name: (cost(extra(name)), name)):
        candidates = extra(seed)
        if spent + cost(candidates) <= budget:
            added |= candidates
            spent += cost(candidates)
        elif candidates:
            deferred.append(seed)
    return added, sorted(deferred)


def balance(names: set[str], packages: dict[str, dict], shard_count: int,
            test_seconds: dict[str, int] | None = None,
            apart: tuple[frozenset[str], ...] = ()) -> list[dict]:
    """Spread packages over shards by structural cost plus recorded test time.

    Packages of different `apart` sets never share a shard while another
    shard can take them.
    """
    if not names:
        return []
    test_seconds = test_seconds or {}

    def weight(name: str) -> int:
        return (package_cost(packages[name])
                + TEST_SECOND_WEIGHT * expected_test_seconds(name, test_seconds))

    def clashes(name: str, shard: dict) -> bool:
        return any(name in group and any(other in rival for other in shard["names"])
                   for group in apart for rival in apart if rival is not group)

    shards = [{"names": [], "cost": 0, "weight": 0}
              for _ in range(max(1, min(shard_count, len(names))))]
    for name in sorted(names, key=lambda n: (-weight(n), n)):
        candidates = [shard for shard in shards if not clashes(name, shard)] or shards
        shard = min(candidates, key=lambda item: (item["weight"], len(item["names"])))
        shard["names"].append(name)
        shard["cost"] += package_cost(packages[name])
        shard["weight"] += weight(name)
    result = []
    for index, shard in enumerate(shards, 1):
        names_sorted = sorted(shard["names"])
        result.append(
            {
                "shard": index,
                "packages": " ".join(f"-p {name}" for name in names_sorted),
                "package_count": len(names_sorted),
                "estimated_cost": shard["cost"],
                "recorded_test_seconds": sum(test_seconds.get(name, 0) for name in names_sorted),
            }
        )
    return result


def dedicated_test_shards(names: set[str], packages: dict[str, dict],
                          test_seconds: dict[str, int] | None) -> list[dict]:
    """A test shard of its own for each package too heavy to share one."""
    test_seconds = test_seconds or {}
    heavy = [name for name in names
             if package_cost(packages[name])
             + TEST_SECOND_WEIGHT * expected_test_seconds(name, test_seconds)
             > DEDICATED_TEST_WEIGHT]
    return [{"packages": f"-p {name}", "package_count": 1,
             "estimated_cost": package_cost(packages[name]),
             "recorded_test_seconds": test_seconds.get(name, 0)}
            for name in sorted(heavy, key=lambda n: (-test_seconds.get(n, 0), n))]


def useful_shard_count(names: set[str], packages: dict[str, dict], maximum: int) -> int:
    total_cost = sum(package_cost(packages[name]) for name in names)
    cost_limited = max(1, (total_cost + SHARD_COST_BUDGET - 1) // SHARD_COST_BUDGET)
    return min(max(1, maximum), len(names), cost_limited)


def useful_test_shard_count(names: set[str], test_seconds: dict[str, int]) -> int:
    total = sum(expected_test_seconds(name, test_seconds) for name in names)
    time_limited = max(1, (total + TEST_SHARD_SECONDS - 1) // TEST_SHARD_SECONDS)
    return min(MAX_TEST_SHARDS, max(1, len(names)), time_limited)


def android_apps(packages: dict[str, dict]) -> set[str]:
    """Apps that opt into the Android host by declaring an `<app>-android` example."""
    return {
        name for name, package in packages.items()
        if any(target.get("name") == f"{name}-android" and "example" in target.get("kind", ())
               for target in package.get("targets", ()))
    }


def browser_crates(packages: dict[str, dict]) -> set[str]:
    """Crates that build for the browser: they depend on `wasm-bindgen`."""
    return {
        name for name, package in packages.items()
        if any(dependency.get("name") == "wasm-bindgen" and dependency.get("kind") is None
               for dependency in package.get("dependencies", ()))
    }


# Example apps; check.yml's `wasm` job builds the web-capable ones with
# `tilo-app build --platform web` (tools/ci/build-web-examples.sh).
EXAMPLE_APPS = "examples/apps/"
# What the web build of an example runs besides the example: a change to any of
# these rebuilds every web-capable example.
WEB_BUILD_INPUTS = (
    "crates/browser/tilo-browser-runtime/scripts/",
    "crates/dev/tilo-app-cli/src/web.rs",
    "crates/dev/tilo-app-cli/src/targets/",
    "crates/dev/tilo-app-cli/src/portability.rs",
    "tools/ci/build-web-examples.sh",
)


def example_apps(packages: dict[str, dict], root: Path) -> set[str]:
    """Packages under examples/apps/."""
    base = root.resolve()
    return {
        name for name, package in packages.items()
        if Path(package["manifest_path"]).resolve().parent.relative_to(base).as_posix()
        .startswith(EXAMPLE_APPS)
    }


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
    test_seconds: dict[str, int] | None = None,
    only: dict | None = None,
) -> dict:
    """Plan the check jobs for `changes`.

    `only` narrows a re-check: {"packages": ..., "suites": ...} that the base
    branch changed since the last green run of the same pull request. Every
    other package and suite gave the same result on that run.
    """
    packages, by_dir = package_maps(metadata, root)
    resolved_inputs = {path: resolved for path, resolved in (resolved_inputs or {}).items()
                       if path in changes}
    reverse = reverse_dependencies(packages, root)
    seeds = {owner for path in changes if (owner := owner_for(path, by_dir))}
    # Packages whose tests or build read a changed file from outside their own
    # directory. Their code is unchanged, so their dependants are not re-checked.
    reader_scopes = {name: scope for name, scope in (
        path_suites.select_package_scopes(suite_manifest, changes).items()
        if suite_manifest is not None else ()) if name in packages}
    readers = set(reader_scopes)
    # Readers reached only through scoped inputs run just the tests that read them.
    whole_readers = {name for name, scope in reader_scopes.items() if scope is None}
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
    pins = sorted(path for path in changes if path in PIN_FILES)
    pin_only = (
        allow_security_skip
        and not force_full
        and bool(changes)
        and all(path in PIN_FILES and (root / path).is_file() for path in changes)
    )
    security_not_applicable = (
        allow_security_skip
        and not force_full
        and bool(changes)
        and all(path in security_prose or path in quality_helper_paths for path in changes)
    ) or pin_only
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
    quality_only = bool(changes) and not readers and all(
        prose(path) or path in quality_paths
        or path in projected_quality_paths
        or (recipe_present and path == "os/Containerfile.builder")
        for path in changes
    )
    # Packages whose own inputs or dependencies the changes reach, without the
    # contract packages every plan adds.
    reached: set[str] = set()
    if pin_only:
        selected = set()
        reason = "snapshot pins only; the pins job verifies their signatures and provenance"
        full = False
    elif quality_only and not force_full:
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
        selected = closure(seeds | checked, reverse)
        reached = selected | readers
        selected.update(ALWAYS_PACKAGES & packages.keys())
        selected.update(readers)
        reason = f"{len(seeds)} changed packages, their reverse dependencies, and contract packages"
        if readers - seeds:
            reason += f", and {len(readers - seeds)} packages that read the changed files"
        if resolved_inputs:
            reason += " (" + "; ".join(item.detail for _, item in sorted(resolved_inputs.items())) + ")"
        full = False
    # Ordinary feedback type-checks the entire reverse-dependency closure once
    # on x86, but links tests only for changed crates, the crates that read the
    # changed files, their direct dependants within REVERSE_TEST_COST_BUDGET and
    # the existing contract suites. Global/unowned workspace inputs retain
    # complete test coverage.
    deferred_dependants: list[str] = []
    if full:
        tested = selected
    else:
        tested = (seeds | whole_readers | ALWAYS_PACKAGES) & selected
        dependants, deferred_dependants = direct_dependants_within_budget(
            seeds, reverse, packages, tested)
        tested |= dependants & selected
    # The native build of a whole-crate wasm-only target is an empty library.
    # Keep its all-target compile and wasm job; never schedule a zero-test run.
    tested = {name for name in tested if has_native_tests(packages[name])}
    if only is not None:
        selected &= set(only["packages"])
        tested &= selected
        reason += "; re-check limited to what the base branch changed since the last green run"
    scoped_tests = {name: scope for name, scope in reader_scopes.items()
                    if scope is not None and name in selected and name not in tested}
    shard_limit = (shard_count if force_full else
                   min(shard_count, MAX_DEVELOPER_SHARDS) if full else 1)
    effective_shards = useful_shard_count(selected, packages, shard_limit)
    runners = NATIVE_RUNNERS if force_full else {"x86_64": NATIVE_RUNNERS["x86_64"]}
    # Type-check (Clippy/check) shards cover the selected closure.
    check_rows = []
    for arch, runner in runners.items():
        for shard in balance(selected, packages, effective_shards, test_seconds):
            placement = {"qualified_runner": "", "qualified_builders": "",
                         **qualified_native.placement(root, set(shard["packages"].split()[1::2]), arch)}
            check_rows.append({**shard, "arch": arch, "runner": runner, "check_runner": runner, **placement})
    # Test shards cover the tested packages and scoped readers, split by
    # recorded test time. Packages too heavy to share a job get their own
    # shards, which start first; the rest balance around them.
    test_units = (tested | scoped_tests.keys()) & selected
    dedicated = dedicated_test_shards(tested & selected, packages, test_seconds)
    shared_units = test_units - {shard["packages"].split()[1] for shard in dedicated}
    test_shards = dedicated + balance(
        shared_units, packages, useful_test_shard_count(shared_units, test_seconds or {}),
        test_seconds, TEST_SHARD_APART)
    for index, shard in enumerate(test_shards, 1):
        shard["shard"] = index
    if selected and not test_shards:
        # Keep the aggregate's test result applicable: one row whose steps skip.
        test_shards = [{"shard": 1, "packages": "", "package_count": 0,
                        "estimated_cost": 0, "recorded_test_seconds": 0}]

    def test_row(shard: dict, arch: str, runner: str) -> dict:
        shard_names = set(shard["packages"].split()[1::2])
        test_names = sorted(tested & shard_names)
        scoped_names = sorted(scoped_tests.keys() & shard_names)
        scoped = [{"package": name, "test": scoped_tests[name].test,
                   "filter": scoped_tests[name].filter} for name in scoped_names]
        # check_runner: a pull request runs its own check.yml against this
        # planner from the merged tree; older branches' check.yml reads it.
        # Older PR workflows still evaluate optional qualification fields.
        # Empty strings retain their general fallback without missing lookups.
        placement = {"qualified_runner": "", "qualified_builders": "",
                     **qualified_native.placement(root, shard_names, arch)}
        certify = any(Path(packages[name]["manifest_path"]).resolve().parent.parent == root.resolve() / "apps"
                      and name != "agent" for name in test_names)
        return {**shard, "arch": arch, "runner": runner, "check_runner": runner, **placement,
                "certify": certify,
                "test_packages": " ".join(f"-p {name}" for name in test_names),
                "scoped_tests": json.dumps(scoped, separators=(",", ":")) if scoped else ""}

    matrix = [test_row(shard, arch, runner)
              for arch, runner in runners.items() for shard in test_shards]
    # Hermetic suites outside Cargo follow their declared inputs; explicit full
    # runs and pushes to main (all_suites) run every one of them.
    suites = (path_suites.select(suite_manifest, changes, run_all=force_full or all_suites)
              if suite_manifest is not None and not pin_only else [])
    if only is not None:
        suites = [suite for suite in suites if suite in set(only["suites"])]
    android = sorted(android_apps(packages) & selected)
    wasm = sorted(browser_crates(packages) & selected)
    examples = example_apps(packages, root)
    web_examples = sorted(
        examples if any(path.startswith(WEB_BUILD_INPUTS) for path in changes)
        else examples & selected)
    return {
        "full": full,
        "rust_required": bool(selected),
        "security_required": not security_not_applicable,
        "suites_required": bool(suites),
        "pins_required": pin_only,
        "pins": pins if pin_only else [],
        "suites": suites,
        # Selected apps with an Android host example; check.yml's `android`
        # job type-checks them for the Android targets.
        "android_required": bool(android),
        "android": android,
        # Selected browser crates; check.yml's `wasm` job type-checks them for
        # wasm32-unknown-unknown, a target no other job builds.
        # Selected examples, or all of them when their web build changed; the
        # `wasm` job builds the web-capable ones for the web.
        "wasm_required": bool(wasm) or bool(web_examples),
        "wasm": wasm,
        "web_examples": web_examples,
        "reason": reason,
        "changed_files": changes,
        "selected_count": len(selected),
        "workspace_count": len(packages),
        "selected": sorted(selected),
        "reached": sorted(reached if not full else selected),
        "tested": sorted(tested),
        # Packages that only read a changed file through a scoped input; the
        # test job runs just the tests that read it.
        "scoped_tests": {name: f"--test {scope.test} -E '{scope.filter}'"
                         for name, scope in sorted(scoped_tests.items())},
        # Changed packages whose direct dependants are only type-checked here
        # (and tested nightly) because testing them exceeds the budget.
        "dependants_tested_nightly": deferred_dependants,
        "qualification": "full-native" if force_full else "developer-feedback",
        "estimated_cost": sum(package_cost(packages[name]) for name in selected),
        "requested_shards": shard_count,
        "shard_count": effective_shards,
        "test_shard_count": len(matrix) // len(runners) if matrix else 0,
        "matrix": {"include": matrix},
        # Clippy's lint result is architecture-independent; lint each package
        # set once, on x86_64.
        "clippy_matrix": {"include": [row for row in check_rows if row["arch"] == "x86_64"]},
        # Clippy type-checks every target it lints with the same flags, so on
        # x86_64 it is the all-target check. Other architectures, which are
        # not linted, keep a standalone all-target check.
        "check_matrix": {"include": [row for row in check_rows if row["arch"] != "x86_64"]},
    }


def emit_github(plan: dict, output: Path) -> None:
    compact_matrix = json.dumps(plan["matrix"], separators=(",", ":"))
    with output.open("a", encoding="utf-8") as handle:
        handle.write(f"matrix={compact_matrix}\n")
        compact_clippy_matrix = json.dumps(plan["clippy_matrix"], separators=(",", ":"))
        handle.write(f"clippy_matrix={compact_clippy_matrix}\n")
        compact_check_matrix = json.dumps(plan["check_matrix"], separators=(",", ":"))
        handle.write(f"check_matrix={compact_check_matrix}\n")
        handle.write(f"check_required={'true' if plan['check_matrix']['include'] else 'false'}\n")
        handle.write(f"full={'true' if plan['full'] else 'false'}\n")
        handle.write(f"rust_required={'true' if plan['rust_required'] else 'false'}\n")
        handle.write(f"security_required={'true' if plan['security_required'] else 'false'}\n")
        handle.write(f"suites_required={'true' if plan['suites_required'] else 'false'}\n")
        handle.write(f"suites={' '.join(plan['suites'])}\n")
        handle.write(f"pins_required={'true' if plan['pins_required'] else 'false'}\n")
        handle.write(f"pins={' '.join(plan['pins'])}\n")
        handle.write(f"android_required={'true' if plan['android_required'] else 'false'}\n")
        handle.write(f"android_packages={' '.join(f'-p {name}' for name in plan['android'])}\n")
        handle.write(f"wasm_required={'true' if plan['wasm_required'] else 'false'}\n")
        handle.write(f"wasm_packages={' '.join(f'-p {name}' for name in plan['wasm'])}\n")
        handle.write(f"web_examples={' '.join(plan['web_examples'])}\n")
        handle.write(f"selected_count={plan['selected_count']}\n")
        handle.write(f"workspace_count={plan['workspace_count']}\n")
        handle.write(f"estimated_cost={plan['estimated_cost']}\n")
        handle.write(f"shard_count={plan['shard_count']}\n")
        handle.write(f"test_shard_count={plan['test_shard_count']}\n")


def recheck_scope(root: Path, metadata: dict, tested_base: str, base: str,
                  suite_manifest) -> dict | None:
    """Packages and suites whose inputs changed between two base commits, or None for all.

    A pull request green on `tested_base` gives the same result on `base` for
    every package and suite the base's own changes do not reach. None when the
    old base is not an ancestor, or the change reaches the whole workspace.
    """
    if subprocess.run(["git", "merge-base", "--is-ancestor", tested_base, base],
                      cwd=root, capture_output=True).returncode:
        return None
    changes = changed_files(root, tested_base, base)
    delta = make_plan(metadata, root, changes, 1, suite_manifest=suite_manifest,
                      resolved_inputs=resolve_manifest_inputs(root, tested_base, base, metadata, changes))
    if delta["full"]:
        return None
    return {"packages": set(delta["reached"]), "suites": set(delta["suites"])}


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
    parser.add_argument("--recheck-from",
                        help="re-check of a pull request that was green on this older base commit: "
                             "limit the plan to what the base changed since (--base is the new base)")
    args = parser.parse_args()

    if args.metadata is None and os.environ.get('GITHUB_ACTIONS') != 'true':
        parser.error('local planning requires verified metadata; use the stable offline_affected_plan.py entrypoint')

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
    suite_manifest = path_suites.load(root)
    only = None
    if args.recheck_from:
        only = recheck_scope(root, metadata, args.recheck_from, args.base, suite_manifest)
    plan = make_plan(
        metadata,
        root,
        changes,
        args.shards,
        force_full=args.full,
        builder_only_external_assets=builder_only_external_assets,
        allow_security_skip=args.allow_security_skip,
        resolved_inputs=resolved_inputs,
        suite_manifest=suite_manifest,
        all_suites=args.all_suites,
        test_seconds=recorded_test_seconds(),
        only=only,
    )
    print(json.dumps(plan, indent=2))
    if args.github_output:
        emit_github(plan, args.github_output)
    return 0


if __name__ == "__main__":
    sys.exit(main())
