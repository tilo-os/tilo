#!/usr/bin/env python3
"""Select and balance the workspace packages affected by a git change.

The selector works from Cargo metadata rather than a hand-maintained package
list.  A package is selected when its own files change, or when it transitively
depends on a changed workspace package through normal or build dependencies, or
dev-depends on one of those (a dev-dependency reaches only the dependent's own
test, example and bench targets, not the library its dependents link).  Root
Cargo.toml and Cargo.lock changes are attributed to the packages they reach
when the difference can be proved, lockfile edges by the same kinds;
other repository-wide build inputs fall back to the complete workspace. A
test-run configuration change (.config/nextest*.toml) is attributed to the
packages its filters name, and .config/hakari.toml to workspace-hack, whose
features every member builds with.
Ordinary feedback checks the closure on x86 and tests changed packages, their
direct dependants (within one shard's cost) and contract suites; --full
qualifies both native architectures.
The hermetic suites outside Cargo declared in tools/ci/suites.toml are selected
by the paths they read, as are the packages it declares to read repository files
outside their own directory. A workspace path that no Cargo package owns is
attributed by those reader declarations, so a change to sources shipped without
a package of their own (the tilolang apps under apps/tl) selects the packages
that read them; only a path no package reads keeps the complete workspace.
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
from typing import Callable, NamedTuple

# Direct CLI execution and importlib-based policy tests both load the same graph.
TOOLS = Path(__file__).resolve().parents[1]
if str(TOOLS) not in sys.path:
    sys.path.append(str(TOOLS))
from ci.workspace_graph import (
    LockId, _lock_graph, cargo_metadata, changed_files, closure, comparison_merge_base, file_at,
    lockfile_change, owner_for, package_maps, reverse_dependencies, run,
)
from ci import runner_labels, qualified_native
from ci import suites as path_suites


# Preserve the existing wire/document/package contract suites, including
# tilo-app-package/tests/{app_manifests,verify_attest} and tilo-doc-codecs/tests/{convergence,office_roundtrip}.
# These catch repository contracts outside a changed crate's dependency edges.
ALWAYS_PACKAGES = {"collab-types", "tilo-app-package", "tilo-doc", "tilo-doc-codecs"}
# The desk, and the crates the session composes it from. A change to any of them
# reaches tilo-session, so the desktop gate keys on the session: whatever the
# change, if it can alter the desk and the session's whole test suite is not
# scheduled anyway, the desk's own integration tests run instead — a window
# becomes a tab of the strip, an app opens where the desk puts it, the owner's
# door answers. When the suite is scheduled they come with it and the gate adds
# nothing.
DESK_PACKAGES = (
    "crates/session/desktop",
    "crates/session/desktop-model",
    "crates/session/desktop-policy",
    "crates/session/desktop-surface",
    "crates/session/core",
)
DESKTOP_GATE_PACKAGE = "tilo-session"
DESKTOP_GATE_MANIFEST = "crates/session/session/Cargo.toml"
# The desk's integration tests, by the file each is written in. They are modules
# of the package's one test binary (`tests/it.rs` includes each file), so the
# gate names that binary once and picks the modules out by the name each test
# gained from it.
DESKTOP_GATE_TESTS = ("hosted_windows", "launch", "owner_door")
DESKTOP_GATE_BINARY = "it"


def desktop_gate(root: Path) -> dict[tuple[str, str], path_suites.ReaderScope]:
    """The desk's integration tests, refused when the tree lacks what they name.

    The gate keys on a package name, so a renamed or deleted crate would be in
    no plan's selection: it would select nothing and run nothing, silently. A
    test target that went away fails a job only once the gate fires, and a desk
    crate the session no longer depends on takes the gate's reason with it. All
    three fail here instead, where every plan is computed.
    """
    session = root / DESKTOP_GATE_MANIFEST
    manifest = tomllib.loads(session.read_text(encoding="utf-8")) if session.is_file() else {}
    if manifest.get("package", {}).get("name") != DESKTOP_GATE_PACKAGE:
        raise ValueError(f"the desktop gate names {DESKTOP_GATE_PACKAGE!r}, "
                         f"which {DESKTOP_GATE_MANIFEST} does not declare")
    targets = {target.get("name") for target in manifest.get("test", [])}
    if DESKTOP_GATE_BINARY not in targets:
        raise ValueError(f"the desktop gate names test binary {DESKTOP_GATE_BINARY!r}, "
                         f"which {DESKTOP_GATE_MANIFEST} does not declare")
    missing = [test for test in DESKTOP_GATE_TESTS
               if not (session.parent / "tests" / f"{test}.rs").is_file()]
    if missing:
        raise ValueError(f"the desktop gate names test files {missing} "
                         f"that {session.parent / 'tests'} does not hold")
    linked = {(session.parent / entry["path"]).resolve()
              for entry in manifest.get("dependencies", {}).values()
              if isinstance(entry, dict) and "path" in entry}
    unreached = [desk for desk in DESK_PACKAGES if (root / desk).resolve() not in linked]
    if unreached:
        raise ValueError(f"the desktop gate names desk crates {unreached} that {DESKTOP_GATE_PACKAGE} "
                         "does not depend on: a change to one no longer reaches the desk's tests")
    # One entry: the modules share the binary, so a run of it covers them all.
    modules = " or ".join(f"test({test}::)" for test in DESKTOP_GATE_TESTS)
    return {(DESKTOP_GATE_PACKAGE, DESKTOP_GATE_BINARY): path_suites.ReaderScope(
        (), DESKTOP_GATE_BINARY, f"binary({DESKTOP_GATE_BINARY}) and ({modules})")}


DESKTOP_GATE = desktop_gate(TOOLS.parent)
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
    "docs/headless-testing.md",
    "docs/policies.md",
}
# These orchestration/validation helpers are exercised by the mandatory quality
# job's tools/policy/run.sh unittest discovery. They are not Rust build
# inputs. Keep exact source/test pairs: unknown tools, deleted files, workflows,
# compiler wrappers and packaging implementation changes retain native coverage.
QUALITY_ONLY_HELPERS = {
    ".githooks/pre-commit": "tools/tests/test_pre_commit_contract.py",
    "scripts/ci-run.sh": "tools/tests/test_ci_run.py",
    "tools/ci/check-changed-rustfmt.sh": "tools/tests/test_changed_rust_comparison.py",
    "tools/ci/comparison_range.py": "tools/tests/test_changed_rust_comparison.py",
    "tools/ci/check_lock_update.py": "tools/tests/test_lock_update.py",
    "tools/ci/measure.sh": "tools/tests/test_ci_measure.py",
    "tools/ci/compiler_cache_metrics.py": "tools/tests/test_ci_compiler_cache_metrics.py",
    "tools/ci/affected_packages.py": "tools/tests/test_ci_select.py",
    "tools/ci/current_source_metadata.py": "tools/tests/test_current_source_metadata.py",
    "tools/ci/local_affected.py": "tools/tests/test_local_affected.py",
    "tools/ci/local-affected.sh": "tools/tests/test_local_affected.py",
    "tools/ci/test-changed.sh": "tools/tests/test_local_affected.py",
    "tools/ci/lint-affected.sh": "tools/tests/test_local_affected.py",
    "tools/ci/macos_apps.py": "tools/tests/test_dev_macos_workflows.py",
    "tools/ci/workspace_graph.py": "tools/tests/test_ci_select.py",
    "tools/ci/recorded_test_time.json": "tools/tests/test_ci_select.py",
    "tools/ci/recorded_build_bytes.json": "tools/tests/test_ci_select.py",
    "tools/rust_feedback.py": "tools/tests/test_rust_feedback.py",
    "tools/ci/run_scoped_tests.py": "tools/tests/test_ci_scoped_tests.py",
    "tools/ci/native-mold-linker.sh": "tools/tests/test_ci_native_mold.py",
    "tools/ci/verify-native-mold.sh": "tools/tests/test_ci_native_mold.py",
    "tools/ci/suites.py": "tools/tests/test_suites.py",
    "tools/ci/suites.toml": "tools/tests/test_suites.py",
    "tools/policy/run.sh": "tools/tests/test_ci_select.py",
    "tools/policy/steps.toml": "tools/tests/test_policy_steps.py",
    "tools/policy/steps.py": "tools/tests/test_policy_steps.py",
    "tools/policy/added_line_policy.py": "tools/tests/test_added_line_policy.py",
    "tools/policy/check_commit_policy.py": "tools/tests/test_commit_hooks.py",
    "tools/policy/check_allowance_markers.py": "tools/tests/test_allowance_markers.py",
    "scripts/scan-secrets.sh": "tools/tests/test_added_line_policy.py",
    "tools/policy/check_workspace_members.py": "tools/tests/test_check_workspace_members.py",
    "tools/policy/check_repository_layout.py": "tools/tests/test_check_repository_layout.py",
    "tools/policy/check_workflow_actions.py": "tools/tests/test_workflow_actions.py",
    "tools/policy/check_workflow_package_paths.py": "tools/tests/test_workflow_package_paths.py",
    "tools/policy/check_workflow_timeouts.py": "tools/tests/test_workflow_timeouts.py",
    "tools/policy/check_ui_clock_contract.py": "tools/tests/test_ui_clock_contract.py",
    "tools/policy/check_clock_contract.py": "tools/tests/test_clock_contract.py",
    "tools/policy/judged.py": "tools/tests/test_staged_judgement.py",
    "tools/policy/check_motion_library.py": "tools/tests/test_motion_library.py",
    "tools/policy/check_formats.py": "tools/tests/test_formats_contract.py",
    "tools/policy/check_settings_writes.py": "tools/tests/test_settings_writes.py",
    "tools/policy/check_app_content_store.py": "tools/tests/test_app_content_store.py",
    "tools/policy/check_document_engine_boundary.py": "tools/tests/test_document_engine_boundary.py",
    "tools/policy/check_crate_layers.py": "tools/tests/test_check_crate_layers.py",
    "tools/policy/check_media_stack.py": "tools/tests/test_media_stack_policy.py",
}
# Control-plane files are validated by tests that read or execute the production
# file in the mandatory repository-policy suite. A file missing from this exact
# map retains native Rust checks.
QUALITY_ONLY_CONTROL_INPUTS = {
    # Read only by the crate-layers policy check, which run.sh executes.
    "tools/policy/crate_layers.toml": (
        "tools/policy/check_crate_layers.py",
        "tools/tests/test_check_crate_layers.py",
    ),
    "tools/policy/crate_layers_baseline.toml": (
        "tools/policy/check_crate_layers.py",
        "tools/tests/test_check_crate_layers.py",
    ),
    "os/dev/ci-native-podman-store.sh": (
        "tools/tests/test_ci_native_podman_store.py",
        "os/tests/disk_image_root_podman_store.sh",
    ),
    # These wrappers select/request the existing image workflow; they change
    # no compiler, app source or packaging inputs. Mandatory policy tests
    # observe their request gate, paths, architecture selection and delegation.
    "os/dev/rpm-snapshot-fingerprint.py": (
        "os/tests/test_rpm_snapshot_fingerprint.py",
    ),
    "os/dev/resolve-rpm-snapshot.sh": (
        "os/tests/test_rpm_snapshot_fingerprint.py",
    ),
    ".forgejo/workflows/build-ci-builder.yml": (
        "os/tests/test_builder_candidate_workflow.py",
        "os/tests/workflow_dependency_invariants.sh",
        "tools/tests/test_ci_native_mold.py",
    ),
    ".forgejo/workflows/check.yml": (
        "tools/tests/test_ci_receipt.sh",
        "tools/tests/test_ci_test_shard_environment.py",
        "tools/tests/test_resolve_ci_builder.py",
    ),
}
NATIVE_RUNNERS = runner_labels.NATIVE
LARGE_RUNNERS = runner_labels.LARGE
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
    "vendor/",
)
# Test-run configuration: which tests run, in which environment, under which
# limits. A change here is attributed to the packages its filters name; a file
# in CONFIG_DIRECTORY that is not listed keeps the complete workspace.
CONFIG_DIRECTORY = ".config/"
FILTER_CONFIG_FILES = (
    ".config/nextest.toml",
    ".config/nextest-filters.toml",
    ".config/nextest-capabilities.toml",
)
# The feature selection of crates/workspace-hack, which every member depends on.
FEATURE_CONFIG_FILE = ".config/hakari.toml"
# Vendored crates that carry Tilo patches, with the paths their own tests read.
# The workspace builds them only as path dependencies, so Cargo selection never
# runs their tests; check.yml's `vendored` job does
# (tools/ci/test-vendored-crates.sh) when one of these paths or the runner
# changes, and ci-nightly.yml runs every one.
VENDORED_TEST_CRATES = {
    "vendor/automerge": ("vendor/automerge/", "vendor/automerge-test/"),
}
VENDORED_TEST_RUNNER = "tools/ci/test-vendored-crates.sh"
HEAVY_PACKAGES = {
    "browser",
    "ebook",
    "tilo-media",
    "tilo-doc-pdf",
    "tilo-session",
    "slides",
    "tube",
    "typst-editor",
    "tilo-ui",
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
# tilo-actions, tilo-app, runtime-dylib or tilo-doc would add 20-60% of the whole
# workspace to every such change; they stay type-checked here and are
# tested nightly on both architectures.
REVERSE_TEST_COST_BUDGET = SHARD_COST_BUDGET
# Recorded test execution seconds per package (tools/ci/recorded_test_time.py). A
# shard's test job links its tests and then runs them on about four threads;
# one recorded test second costs roughly as much shard wall time as two units
# of structural cost. Only shard balancing uses these numbers.
TEST_SECONDS = Path(__file__).with_name("recorded_test_time.json")
TEST_SECOND_WEIGHT = 2
# Bytes a test job's build writes per package (tools/ci/recorded_build_bytes.py).
# Like the test seconds above, this record only moves packages between shards:
# it never changes which packages are selected or tested.
TEST_BYTES = Path(__file__).with_name("recorded_build_bytes.json")
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
# ci-arm-1 keeps one arm64 slot, and it answers both the native and the large
# arm64 label, so the shards of one arm64 run execute one after another: a
# fourth job there only adds a queue behind one runner. Arm test jobs are
# therefore fewer and, balanced over the same packages, larger; the slot's
# 40 GiB carries them.
MAX_TEST_SHARDS_ARM = 2
# A tested package heavier than this (structural cost plus weighted recorded
# test seconds) gets a test job of its own, so it builds and tests beside the
# balanced shards instead of lengthening one of them. Arm test jobs are
# balanced as a whole instead (see MAX_TEST_SHARDS_ARM).
DEDICATED_TEST_WEIGHT = 1_000
# A test job links every binary of the packages it carries, several at a time
# under CARGO_BUILD_JOBS, and the native class's 16 GiB job limit is sized for
# the structural cost one shard should hold (SHARD_COST_BUDGET). A shard of a
# whole-workspace run carries about twice that and was killed by the kernel
# mid-build (exit 137) on every shard above 3,400 cost; those shards ask for
# the large class (40 GiB) instead. The session's test binaries are the tree's
# largest link, so a shard that carries them asks for the large class at any
# size, whatever its recorded build bytes say.
LARGE_TEST_SHARD_COST = 2 * SHARD_COST_BUDGET
LARGE_TEST_PACKAGES = frozenset({"tilo-session"})
# What one test job may write and use, measured on the large class (36 GiB
# cgroup limit, about 105 GiB of free store): a whole-workspace shard of 113
# packages reached memory.peak = memory.max (213k max events) and filled its
# store until a mold link failed with "Disk full?" after 732 s, while the
# smaller shards beside it passed. A shard is therefore bounded by the bytes
# its build writes and by the memory its concurrent jobs need, not by how many
# packages it names. 70 GiB leaves the store room for the image, the app
# binaries certification links and the goldens; 28 GiB leaves the class's
# 36 GiB room for the page cache and the suites themselves.
TEST_SHARD_STORE_BYTES = 70 * 1024**3
TEST_SHARD_MEMORY_BYTES = 28 * 1024**3
# The native class gives a test job a 50 GiB store and 16 GiB of memory. A
# shard whose measured build stays inside this keeps the native class; past it
# the shard asks for the large one.
NATIVE_TEST_SHARD_BYTES = 30 * 1024**3
# Peak memory one concurrent compile/link job needs, from the same run: 36 GiB
# reached with eight jobs on x86_64, and the arm64 slot runs four inside the
# same ceiling.
TEST_JOB_BYTES = {"x86_64": 4_608 * 1024**2, "aarch64": 9 * 1024**3}
# Jobs each class runs when memory allows; the planner reduces them to stay
# inside TEST_SHARD_MEMORY_BYTES. These are the counts the classes were tuned
# to before memory was bounded per shard.
TEST_CLASS_JOBS = {
    "runner": {"x86_64": 4},
    "runner-arm64": {"aarch64": 1},
    "runner-large": {"x86_64": 8},
    "runner-large-arm64": {"aarch64": 4},
}
# Suites whose tests fsync heavily: fs's durability, checked-restore and
# release-matrix tests over real FUSE (with fs-native), and the
# host suites that commit a durable Store, owner journal or relay journal on
# every step. On one runner their fsyncs queue behind each other until the host
# suites' handshake bounds expire, so test shards keep them apart whenever
# there is more than one shard.
TEST_SHARD_APART = (
    frozenset({"tilo-fs", "fs-native"}),
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


class BuildFootprint(NamedTuple):
    """What a test job's build writes, from tools/ci/recorded_build_bytes.json.

    ``baseline`` is what every test job writes before workspace code: the
    external dependency tree, build-script output and fingerprints.
    ``packages`` maps a package to the bytes of its own library and of its test
    binaries: a shard writes the libraries of everything it depends on and the
    test binaries of the packages it tests. ``None`` sizes mean no measurement.
    """
    baseline: int | None = None
    unrecorded: tuple[int, int] = (0, 0)
    packages: dict[str, tuple[int, int]] = {}

    def of(self, name: str) -> tuple[int, int]:
        """(library bytes, test bytes) for one package."""
        return self.packages.get(name, self.unrecorded)

    def shard(self, tested: set[str], closure: set[str]) -> int | None:
        """The bytes a shard of `tested` packages writes, or None unmeasured.

        `closure` holds the workspace packages the shard's build links, which
        is what its libraries cost to build. A package in `tested` also writes
        its test binaries.
        """
        if self.baseline is None:
            return None
        return (self.baseline
                + sum(self.of(name)[0] for name in closure)
                + sum(self.of(name)[1] for name in tested))


def recorded_build_bytes(path: Path | None = None) -> BuildFootprint:
    """Measured what a test job's build writes, per package and before them.

    Absent data yields no readings (``None`` sizes): the planner then balances
    on time alone, as a checkout without the record always could.
    """
    path = path or TEST_BYTES
    if not path.is_file():
        return BuildFootprint()
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict) or set(data) != {"baseline", "unrecorded", "packages"}:
        raise ValueError(f"{path} must have exactly baseline, unrecorded and packages")

    def sizes(value: object) -> tuple[int, int]:
        if (not isinstance(value, dict) or set(value) != {"libraries", "tests"}
                or not all(type(part) is int and part >= 0 for part in value.values())):
            raise ValueError(f"{path} must measure libraries and tests in whole bytes")
        return value["libraries"], value["tests"]

    packages = data["packages"]
    if isinstance(packages, dict):
        packages = {name: sizes(value) for name, value in packages.items()}
    else:
        raise ValueError(f"{path} must map package names to their sizes")
    if type(data["baseline"]) is not int or data["baseline"] <= 0:
        raise ValueError(f"{path}: baseline must be a positive number of bytes")
    unrecorded = sizes(data["unrecorded"])
    if min(unrecorded) <= 0:
        # A package the record does not know must never count as free weight.
        raise ValueError(f"{path}: unrecorded must measure positive bytes")
    return BuildFootprint(data["baseline"], unrecorded, packages)


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
        (name not in before or builder_mold_asset(name, before[name]))
        and (name not in after or builder_mold_asset(name, after[name]))
        for name in changed_names
    )


class ResolvedInput(NamedTuple):
    """A root manifest/lockfile change attributed to specific workspace packages.

    ``seeds`` are checked with their reverse dependencies and tested; ``checked``
    are additionally type-checked. ``dev_only`` are checked and tested but not
    expanded: the change enters only their own test, example and bench targets
    (a changed third-party dev-dependency), as `check_closure` treats a dev edge.
    """
    seeds: frozenset[str]
    checked: frozenset[str]
    detail: str
    dev_only: frozenset[str] = frozenset()


# Workspace tables whose semantic difference maps to specific packages. Every
# other root manifest key (lints, workspace.package, exclude, default-members,
# resolver, metadata, profile, patch, replace, a root package, ...) keeps the
# complete workspace.
ATTRIBUTABLE_WORKSPACE_KEYS = {"members", "dependencies"}
# The cargo-hakari package (.config/hakari.toml). Every member depends on it so
# that each shared third-party crate is built with one feature set; it is never
# named in code. Like a change to its own manifest (it has no path dependants),
# a locked crate it re-exports to nobody does not reach the members through it.
HAKARI_PACKAGE = "workspace-hack"
# Sources whose lockfile record (name, version, checksum, dependencies) is the
# whole locked input. A git or out-of-workspace path record is not attributed
# by dependency kind.
REGISTRY_SOURCES = ("registry+", "sparse+")


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
    by_kind = lockfile_change_by_kind(before, after, metadata)
    if by_kind is not None:
        return by_kind
    return ResolvedInput(
        seeds=change.seeds,
        checked=frozenset(change.reached & packages),
        detail=f"Cargo.lock reaches {len(change.seeds)} packages directly",
    )


def _compatible(version: str) -> str:
    """Cargo's semver-compatibility key: 1.4.2 -> 1, 0.9.3 -> 0.9, 0.0.4 -> 0.0.4."""
    parts = re.split(r"[-+]", version, maxsplit=1)[0].split(".")
    for index, part in enumerate(parts):
        if part != "0":
            return ".".join(parts[:index + 1])
    return ".".join(parts)


def lockfile_change_by_kind(before: bytes, after: bytes, metadata: dict) -> ResolvedInput | None:
    """Attribute a Cargo.lock difference using each member's dependency kinds.

    The lockfile records no dependency kinds, but only a member's own edges
    can be dev edges: Cargo locks the dev-dependencies of workspace members
    and of no other package, so every third-party edge is a normal or build
    edge and carries a change into its consumer's library. A member's edge
    takes its kind from the member's manifest entries for that package
    (``cargo metadata --no-deps`` records them). The library of every locked
    package that a changed record reaches through normal/build edges changes:
    a member among them is seeded and expanded like a changed package. A
    member that reaches that set only through dev edges is ``dev_only``.

    Edges between members are left to the path-dependency closure, except
    patched members without a ``path`` entry, which are followed here; edges
    into HAKARI_PACKAGE do not propagate. A member whose own record changed in
    a way the changed records do not explain (a version, or an edge gained or
    lost to an unchanged or member package) is seeded, as is a member edge
    whose package has no manifest entry.

    ``None`` keeps the kind-blind lockfile attribution, which never narrows
    past the lockfile graph: when a record that is not a registry release or
    a member is added or removed (a git revision, a move to or from a git or
    patched path source), when a changed third-party crate gains or drops an
    edge to an unchanged package (it may change that package's features for
    other members, which no lockfile records), or when the graph is unreadable.
    """
    try:
        old_meta, old = _lock_graph(before)
        new_meta, new = _lock_graph(after)
    except (KeyError, TypeError, ValueError, UnicodeDecodeError, tomllib.TOMLDecodeError):
        return None
    if old_meta != new_meta:
        return None
    manifests = {package["name"]: package.get("dependencies", []) for package in metadata["packages"]}
    every = old.keys() | new.keys()
    members = {package_id for package_id in every if not package_id[2] and package_id[0] in manifests}
    changed = {package_id for package_id in every if old.get(package_id) != new.get(package_id)}
    external_changes = changed - members
    # A git revision, or a crate moving to or from a git or patched path
    # source, adds or removes a record that is not a registry release.
    if any(not source.startswith(REGISTRY_SOURCES) for _, _, source in (old.keys() ^ new.keys()) - members):
        return None
    # A third-party crate that gains or drops an edge to an unchanged package
    # may enable or disable features on it for every other consumer: the
    # kind-blind graph attribution, which never narrows past the lockfile,
    # decides then. Records are compared within one semver-compatible line,
    # so a new crate or a major move with any such edge falls back.
    def unchanged_edges(graph: dict, line: tuple[str, str, str]) -> set[LockId]:
        return {dependency for package_id, (_, deps) in graph.items()
                if (package_id[0], package_id[2], _compatible(package_id[1])) == line
                for dependency in deps if dependency not in changed}

    for line in {(name, source, _compatible(version)) for name, version, source in external_changes}:
        if unchanged_edges(old, line) != unchanged_edges(new, line):
            return None

    def dependencies(package_id: LockId) -> frozenset[LockId]:
        return frozenset().union(*(graph[package_id][1] for graph in (old, new) if package_id in graph))

    def edge(consumer: str, dependency: LockId) -> str | None:
        """The edge's kind: linked, dev, or None when the path closure (or nothing) carries it."""
        entries = [entry for entry in manifests[consumer] if entry.get("name") == dependency[0]]
        if dependency in members:
            if dependency[0] == HAKARI_PACKAGE:
                return None
            if entries and all(entry.get("path") for entry in entries):
                return None
            entries = [entry for entry in entries if not entry.get("path")]
        if entries and all(entry.get("kind") == "dev" for entry in entries):
            return "dev"
        return "linked"

    start = set(external_changes)
    unexplained = set()
    for name in {package_id[0] for package_id in changed & members}:
        old_ids = [package_id for package_id in old if package_id in members and package_id[0] == name]
        new_ids = [package_id for package_id in new if package_id in members and package_id[0] == name]
        if (len(old_ids) != 1 or len(new_ids) != 1 or old[old_ids[0]][0] != new[new_ids[0]][0]
                or not old[old_ids[0]][1] ^ new[new_ids[0]][1] <= external_changes):
            unexplained.add(new_ids[0] if len(new_ids) == 1 else old_ids[0])
    start |= unexplained

    consumers: dict[LockId, set[LockId]] = {}
    for graph in (old, new):
        for consumer, (_, deps) in graph.items():
            for dependency in deps:
                consumers.setdefault(dependency, set()).add(consumer)
    affected = set(start)
    pending = list(start)
    while pending:
        dependency = pending.pop()
        for consumer in consumers.get(dependency, ()):
            if consumer not in affected and (
                    consumer not in members or edge(consumer[0], dependency) == "linked"):
                affected.add(consumer)
                pending.append(consumer)
    seeds = {package_id[0] for package_id in unexplained} | {
        package_id[0] for package_id in members & affected
        if any(dependency in affected and dependency not in members
               and edge(package_id[0], dependency) == "linked"
               for dependency in dependencies(package_id))
    }
    checked = {package_id[0] for package_id in members & affected} - seeds
    dev_only = {
        package_id[0] for package_id in members - affected
        if any(dependency in affected and edge(package_id[0], dependency) == "dev"
               for dependency in dependencies(package_id))
    } - seeds - checked
    return ResolvedInput(
        seeds=frozenset(seeds), checked=frozenset(checked), dev_only=frozenset(dev_only),
        detail=(f"Cargo.lock reaches {len(seeds)} packages directly and "
                f"{len(dev_only)} only through dev-dependencies"),
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


FILTER_PACKAGE = re.compile(r"package\(\s*=?\s*([A-Za-z0-9_.-]+)\s*\)")


def filter_packages(expression: str) -> set[str] | None:
    """The packages a nextest filter expression names, or None when it names none.

    A filter that names no package (`all()`, a bare test or binary filter)
    selects across every package, so a change to it is a repository-wide input.
    """
    names = set(FILTER_PACKAGE.findall(expression))
    return names or None


def config_units(path: str, document: dict) -> dict[tuple, tuple[str, ...] | None] | None:
    """Each value a test run reads, with the filter expressions it decides.

    A unit's key carries both what it is and what it says, so two documents
    that mean the same thing have equal units. Its value is the filters whose
    tests it decides: an entry's own filter, the filters of the entries that
    name a setup, a capability group's tests. An empty tuple decides no test
    run (a setup nothing names), and None decides every test's (`[profile.ci]`
    timeouts, `all()`, an unknown table). None is also the answer when the
    document is not this file's shape.
    """
    units: dict[tuple, tuple[str, ...] | None] = {}
    if path == ".config/nextest-filters.toml":
        for key, value in document.items():
            if not isinstance(value, str):
                return None
            units[("filters", key, value)] = (value,)
        return units
    if path == ".config/nextest-capabilities.toml":
        capabilities = document.get("capabilities")
        if not isinstance(capabilities, dict):
            return None
        for group, table in capabilities.items():
            if not isinstance(table, dict):
                return None
            tests = table.get("tests", [])
            if not isinstance(tests, list) or not all(
                    isinstance(entry, dict) and isinstance(entry.get("filter"), str) for entry in tests):
                return None
            # A group that maps no test decides every run that reads its variable.
            filters = tuple(entry["filter"] for entry in tests) or None
            for key, value in table.items():
                if key != "tests":
                    units[("capability", group, key, json.dumps(value, sort_keys=True))] = filters
            for entry in tests:
                units[("capability-test", group, json.dumps(entry, sort_keys=True))] = (entry["filter"],)
        return units
    # .config/nextest.toml: profiles, the setups they register and their overrides.
    profile = document.get("profile")
    scripts = document.get("scripts", {})
    if not isinstance(profile, dict) or not isinstance(scripts, dict):
        return None
    setups = scripts.get("setup", {})
    if not isinstance(setups, dict):
        return None
    for key, value in scripts.items():
        if key != "setup":
            units[("scripts", key, json.dumps(value, sort_keys=True))] = None
    # A setup's own change reaches the entries that name it.
    name_setups: dict[str, list[tuple[str, ...]]] = {}
    for profile_name, table in profile.items():
        if not isinstance(table, dict):
            return None
        for key, value in table.items():
            if key not in ("scripts", "overrides"):
                units[("profile", profile_name, key, json.dumps(value, sort_keys=True))] = None
                continue
            if not isinstance(value, list) or not all(
                    isinstance(entry, dict) and isinstance(entry.get("filter"), str) for entry in value):
                return None
            for entry in value:
                units[("entry", profile_name, key, json.dumps(entry, sort_keys=True))] = (entry["filter"],)
                named = entry.get("setup")
                for setup in named if isinstance(named, list) else [named]:
                    if isinstance(setup, str):
                        name_setups.setdefault(setup, []).append((entry["filter"],))
    for name, value in setups.items():
        units[("setup", name, json.dumps(value, sort_keys=True))] = tuple(
            filters for entry in name_setups.get(name, ()) for filters in entry)
    for key, value in document.items():
        if key not in ("profile", "scripts"):
            units[("document", key, json.dumps(value, sort_keys=True))] = None
    return units


def resolve_config_file(
    path: str, before: bytes | None, after: bytes | None, metadata: dict
) -> ResolvedInput | None:
    """Attribute a test-run or feature configuration change to its packages.

    The configuration changes no code: it changes which of a package's tests
    run and in what environment. Those packages are checked and tested without
    their dependants, as a changed dev-dependency reaches only its own test,
    example and bench targets. A unit that decides every test's run (`all()`,
    a profile's timeouts, a setup command, an unknown table) or a file that was
    added or removed keeps the complete workspace.
    """
    if before is None or after is None:
        return None
    try:
        old = tomllib.loads(before.decode("utf-8"))
        new = tomllib.loads(after.decode("utf-8"))
    except (UnicodeDecodeError, tomllib.TOMLDecodeError):
        return None
    if not isinstance(old, dict) or not isinstance(new, dict):
        return None
    member_names = {package["name"] for package in metadata["packages"]}
    if path == FEATURE_CONFIG_FILE:
        if old == new:
            return ResolvedInput(frozenset(), frozenset(), f"{path} says the same thing")
        if HAKARI_PACKAGE not in member_names:
            return None
        return ResolvedInput(frozenset({HAKARI_PACKAGE}), frozenset(),
                             f"{path} selects the features every member builds with")
    old_units, new_units = config_units(path, old), config_units(path, new)
    if old_units is None or new_units is None:
        return None
    absent = object()
    changed = ([filters for key, filters in old_units.items() if new_units.get(key, absent) != filters]
               + [filters for key, filters in new_units.items() if old_units.get(key, absent) != filters])
    if not changed:
        return ResolvedInput(frozenset(), frozenset(), f"{path} changes comments only")
    names: set[str] = set()
    for filters in changed:
        if filters is None:
            return None
        for expression in filters:
            found = filter_packages(expression)
            if found is None:
                return None
            names |= found
    if not names <= member_names:
        return None
    return ResolvedInput(frozenset(), frozenset(),
                         f"{path} changes the test run of {len(names)} packages",
                         frozenset(names))


def resolve_manifest_inputs(
    root: Path, base: str | None, head: str, metadata: dict, changes: list[str]
) -> dict[str, ResolvedInput]:
    """Return the build inputs that can be attributed to specific packages."""
    resolvers = {"Cargo.lock": resolve_lockfile,
                 "Cargo.toml": lambda old, new, meta: resolve_root_manifest(old, new, meta, root)}
    for path in (*FILTER_CONFIG_FILES, FEATURE_CONFIG_FILE):
        resolvers[path] = lambda old, new, meta, path=path: resolve_config_file(path, old, new, meta)
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


class DependencyEdges(NamedTuple):
    """Provider -> consumers, split by what the edge carries into the consumer.

    ``linked``: normal and build dependencies (and any kind Cargo may add
    later). They change the consumer's library, so they propagate further.
    ``dev``: dev-dependencies. They reach only the consumer's own test,
    example and bench targets, which no other package compiles.
    """
    linked: dict[str, set[str]]
    dev: dict[str, set[str]]


def dependency_edges(packages: dict[str, dict]) -> DependencyEdges:
    """Workspace path-dependency edges by kind; see `DependencyEdges`.

    Resolves providers exactly like workspace_graph.reverse_dependencies (by
    manifest path), so the union of both maps is that function's result. It
    lives here rather than there because release selection fingerprints
    workspace_graph.py.
    """
    manifest_to_name = {
        str(Path(pkg["manifest_path"]).resolve()): name for name, pkg in packages.items()
    }
    linked: dict[str, set[str]] = {name: set() for name in packages}
    dev: dict[str, set[str]] = {name: set() for name in packages}
    for consumer, pkg in packages.items():
        for dependency in pkg.get("dependencies", []):
            dep_path = dependency.get("path")
            if not dep_path:
                continue
            provider = manifest_to_name.get(str(Path(dep_path).resolve() / "Cargo.toml"))
            if provider:
                (dev if dependency.get("kind") == "dev" else linked)[provider].add(consumer)
    return DependencyEdges(linked, dev)


def check_closure(seeds: set[str], edges: DependencyEdges) -> set[str]:
    """Every package with a compiled target (lib, bin, test, example, bench)
    that a change to `seeds` reaches.

    A change to A reaches B's library when B depends on A as a normal or build
    dependency, and so on transitively. When B dev-depends on A, A enters only
    B's own test/example/bench targets: B is selected, and nothing that links
    B is, because B's library is unchanged. Each package of the linked closure
    therefore adds its dev dependants, and those are not expanded further.
    """
    linked = closure(seeds, edges.linked)
    return linked | {consumer for package in linked for consumer in edges.dev.get(package, ())}


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
            apart: tuple[frozenset[str], ...] = (),
            written: Callable[[set[str]], int | None] | None = None,
            capacity: int = TEST_SHARD_STORE_BYTES) -> list[dict]:
    """Spread packages over shards by structural cost plus recorded test time.

    Packages of different `apart` sets never share a shard while another
    shard can take them. When `written` measures what a shard's build writes,
    no shard takes a package that would push it past `capacity`: the balancer
    opens further shards for the rest, so the partition follows the build's
    bytes rather than how many packages it names. A package that alone exceeds
    the capacity keeps a shard of its own; a package cannot be split.
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

    def fits(shard: dict, name: str) -> bool:
        if written is None:
            return True
        size = written(set(shard["names"]) | {name})
        return size is None or size <= capacity

    shards = [{"names": [], "cost": 0, "weight": 0}
              for _ in range(max(1, min(shard_count, len(names))))]
    for name in sorted(names, key=lambda n: (-weight(n), n)):
        candidates = [shard for shard in shards if not clashes(name, shard)] or shards
        roomy = [shard for shard in candidates if fits(shard, name)]
        if roomy:
            candidates = roomy
        else:
            # Nothing has room. A package that alone exceeds the capacity
            # takes an empty shard and stays whole: it cannot be split.
            candidates = [shard for shard in candidates if not shard["names"]]
            if not candidates:
                candidates = [{"names": [], "cost": 0, "weight": 0}]
                shards.append(candidates[0])
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


def useful_test_shard_count(names: set[str], test_seconds: dict[str, int],
                            maximum: int | None = None) -> int:
    total = sum(expected_test_seconds(name, test_seconds) for name in names)
    time_limited = max(1, (total + TEST_SHARD_SECONDS - 1) // TEST_SHARD_SECONDS)
    return min(MAX_TEST_SHARDS if maximum is None else maximum,
               max(1, len(names)), time_limited)


def test_shard_runner(shard: dict, arch: str,
                      written: Callable[[set[str]], int | None] | None = None) -> str:
    """The class a test shard asks for: large when it has outgrown the native one.

    The native class gives a test job 16 GiB and a 50 GiB store; a shard whose
    measured build writes more than that, or whose links exceed the native
    class, asks for the large one.
    """
    names = set(shard["packages"].split()[1::2])
    size = written(names) if written is not None else None
    if (shard["estimated_cost"] > LARGE_TEST_SHARD_COST or names & LARGE_TEST_PACKAGES
            or (size is not None and size > NATIVE_TEST_SHARD_BYTES)):
        return LARGE_RUNNERS[arch]
    return NATIVE_RUNNERS[arch]


def test_build_jobs(runner: str, arch: str) -> int:
    """Compile/link jobs a test job may run without reaching its memory limit.

    Each concurrent job peaks at TEST_JOB_BYTES, so the class's job count is
    reduced to what TEST_SHARD_MEMORY_BYTES holds. A class always keeps one.
    """
    class_jobs = TEST_CLASS_JOBS.get(runner, {}).get(arch, 1)
    return max(1, min(class_jobs, TEST_SHARD_MEMORY_BYTES // TEST_JOB_BYTES[arch]))


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


def vendored_test_crates(changes: list[str], run_all: bool = False) -> list[str]:
    """The VENDORED_TEST_CRATES whose tests `changes` reach, or all of them."""
    if run_all or VENDORED_TEST_RUNNER in changes:
        return sorted(VENDORED_TEST_CRATES)
    return sorted(crate for crate, inputs in VENDORED_TEST_CRATES.items()
                  if any(path.startswith(inputs) for path in changes))


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
    build_bytes: BuildFootprint | None = None,
    only: dict | None = None,
) -> dict:
    """Plan the check jobs for `changes`.

    `only` narrows a re-check: {"packages": ..., "suites": ...} that the base
    branch changed since the last green run of the same pull request. Every
    other package and suite gave the same result on that run.
    """
    packages, by_dir = package_maps(metadata, root)
    build_bytes = build_bytes if build_bytes is not None else recorded_build_bytes()
    resolved_inputs = {path: resolved for path, resolved in (resolved_inputs or {}).items()
                       if path in changes}
    reverse = reverse_dependencies(packages, root)
    seeds = {owner for path in changes if (owner := owner_for(path, by_dir))}
    # Packages whose tests or build read a changed file from outside their own
    # directory. Their code is unchanged, so their dependants are not re-checked.
    reader_scopes = {name: scope for name, scope in (
        path_suites.select_package_scopes(suite_manifest, changes).items()
        if suite_manifest is not None else ()) if name in packages}
    # A verification recipe change must run its subject, not just contract tests.
    # No client reverse closure: these inputs change CI features, not app code.
    if "tools/ci/package_features.py" in changes:
        for subject in ("tilo-model-gateway", "imager", "imager-backend"):
            if subject in packages:
                reader_scopes[subject] = None
    readers = set(reader_scopes)
    # Readers reached only through scoped inputs run just the tests that read them.
    whole_readers = {name for name, scope in reader_scopes.items() if scope is None}
    checked: set[str] = set()
    # Changed in their own test/example/bench targets only: checked and tested, not expanded.
    dev_only: set[str] = set()
    for resolved in resolved_inputs.values():
        seeds.update(resolved.seeds & packages.keys())
        checked.update(resolved.checked & packages.keys())
        dev_only.update(resolved.dev_only & packages.keys())
    projected_quality_paths = (
        {EXTERNAL_ASSET_MANIFEST} if builder_only_external_assets else set()
    )
    global_change = any(
        (path in GLOBAL_FILES and path not in projected_quality_paths
         and path not in resolved_inputs)
        or path.startswith(GLOBAL_PREFIXES)
        or (path.startswith(CONFIG_DIRECTORY) and path not in resolved_inputs)
        for path in changes
    )
    workspace_source_roots = ("apps/", "crates/")
    # A workspace path no Cargo package owns is still accounted for when the
    # suite manifest declares a package that reads it from outside its own
    # directory: those are the readers selected below, and their tests or build
    # compile the file. Only a path no package reads keeps the complete
    # workspace.
    declared = (path_suites.read_paths(suite_manifest, changes)
                if suite_manifest is not None else set())
    unowned = sorted(
        path for path in changes
        if path.startswith(workspace_source_roots) and owner_for(path, by_dir) is None
        and path not in declared)
    unmapped_workspace_change = bool(unowned)
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
                  "repository-wide build input changed" if global_change else
                  "workspace path has no package owner and no declared reader: "
                  + ", ".join(unowned[:3]) + (f" and {len(unowned) - 3} more" if len(unowned) > 3 else ""))
        full = True
    else:
        # Normal/build edges propagate; a dev edge adds one dependant whose
        # own tests change and stops (check_closure). Test selection below
        # keeps every direct dependant, dev ones included, via `reverse`.
        selected = check_closure(seeds | checked, dependency_edges(packages)) | dev_only
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
    # the existing contract suites. Global or undeclared workspace inputs retain
    # complete test coverage.
    deferred_dependants: list[str] = []
    if full:
        tested = selected
    else:
        tested = (seeds | dev_only | whole_readers | ALWAYS_PACKAGES) & selected
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
    # Scoped tests by (package, test target): a package's reading scope and the
    # desk's gate can name different targets of the same package, and each target
    # is its own run.
    scoped_tests = {(name, scope.test): scope for name, scope in reader_scopes.items()
                    if scope is not None and name in selected and name not in tested}
    for key, gate in DESKTOP_GATE.items():
        if key[0] not in selected or key[0] in tested:
            continue
        reading = scoped_tests.get(key)
        if reading is None:
            scoped_tests[key] = gate
        else:
            scoped_tests[key] = path_suites.ReaderScope(
                reading.inputs, reading.test, f"({reading.filter}) or ({gate.filter})")
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
    # recorded test time and bounded by what each job's build writes. Packages
    # too heavy to share a job get their own shards, which start first; the
    # rest balance around them. Arm shards are balanced as one set
    # (MAX_TEST_SHARDS_ARM).
    test_units = (tested | {package for package, _ in scoped_tests}) & selected
    build_edges = dependency_edges(packages)

    def shard_bytes(names: set[str]) -> int | None:
        """What a test job carrying `names` writes: its links plus its own tests."""
        return build_bytes.shard(names, closure(names, build_edges.linked))

    def test_shards_for(arch: str) -> list[dict]:
        if arch == "aarch64":
            shards = balance(test_units, packages,
                             useful_test_shard_count(test_units, test_seconds or {},
                                                     MAX_TEST_SHARDS_ARM),
                             test_seconds, TEST_SHARD_APART, shard_bytes)
        else:
            dedicated = dedicated_test_shards(tested & selected, packages, test_seconds)
            shared_units = test_units - {shard["packages"].split()[1] for shard in dedicated}
            shards = dedicated + balance(
                shared_units, packages, useful_test_shard_count(shared_units, test_seconds or {}),
                test_seconds, TEST_SHARD_APART, shard_bytes)
        for index, shard in enumerate(shards, 1):
            shard["shard"] = index
        if selected and not shards:
            # Keep the aggregate's test result applicable: one row whose steps skip.
            shards = [{"shard": 1, "packages": "", "package_count": 0,
                       "estimated_cost": 0, "recorded_test_seconds": 0}]
        return shards

    test_shards = {arch: test_shards_for(arch) for arch in runners}

    def test_row(shard: dict, arch: str) -> dict:
        shard_names = set(shard["packages"].split()[1::2])
        test_names = sorted(tested & shard_names)
        scoped = [{"package": package, "test": scope.test, "filter": scope.filter}
                  for (package, _), scope in sorted(scoped_tests.items()) if package in shard_names]
        # check_runner: a pull request runs its own check.yml against this
        # planner from the merged tree; older branches' check.yml reads it.
        # Older PR workflows still evaluate optional qualification fields.
        # Empty strings retain their general fallback without missing lookups.
        placement = {"qualified_runner": "", "qualified_builders": "",
                     **qualified_native.placement(root, shard_names, arch)}
        certify = any(Path(packages[name]["manifest_path"]).resolve().parent.parent == root.resolve() / "apps"
                      and name != "agent" for name in test_names)
        # The shard's own class: a shard whose build outgrows the native
        # class's store, or that carries the tree's largest test link, needs
        # the large one (test_shard_runner). Its jobs are then sized so the
        # peak of its concurrent links stays inside the class's memory.
        runner = test_shard_runner(shard, arch, shard_bytes)
        return {**shard, "arch": arch, "runner": runner, "check_runner": runner, **placement,
                "certify": certify,
                "estimated_build_bytes": shard_bytes(shard_names),
                "build_jobs": test_build_jobs(runner, arch),
                "test_packages": " ".join(f"-p {name}" for name in test_names),
                "scoped_tests": json.dumps(scoped, separators=(",", ":")) if scoped else ""}

    matrix = [test_row(shard, arch)
              for arch, shards in test_shards.items() for shard in shards]
    # Hermetic suites outside Cargo follow their declared inputs; explicit full
    # runs and pushes to main (all_suites) run every one of them.
    suites = (path_suites.select(suite_manifest, changes, run_all=force_full or all_suites)
              if suite_manifest is not None and not pin_only else [])
    if only is not None:
        suites = [suite for suite in suites if suite in set(only["suites"])]
    # Vendored crates' own tests follow their files alone, not pushes to main:
    # explicit full runs and the nightly run all of them.
    vendored = vendored_test_crates(changes, run_all=force_full) if not pin_only else []
    if only is not None:
        vendored = [crate for crate in vendored if crate in set(only.get("vendored", ()))]
    android =sorted(android_apps(packages) & selected)
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
        # Vendored crates whose own tests check.yml's `vendored` job runs.
        "vendored_required": bool(vendored),
        "vendored": vendored,
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
        # One edge from the change: the changed packages, the packages that read the changed
        # files, every direct dependant of a changed package, and what is tested. A change being
        # edited is linted over these (tools/ci/test-changed.sh); a batch over `selected`.
        "near": sorted(selected if full else
                       (seeds | checked | dev_only | readers | tested
                        | {name for seed in seeds for name in reverse.get(seed, ())}) & selected),
        # Packages that only read a changed file through a scoped input, or that
        # the desk's gate reads, and the target of each the test job runs.
        "scoped_tests": {f"{package}:{test}": f"--test {scope.test} -E '{scope.filter}'"
                         for (package, test), scope in sorted(scoped_tests.items())},
        # Changed packages whose direct dependants are only type-checked here
        # (and tested nightly) because testing them exceeds the budget.
        "dependants_tested_nightly": deferred_dependants,
        "qualification": "full-native" if force_full else "developer-feedback",
        "estimated_cost": sum(package_cost(packages[name]) for name in selected),
        "requested_shards": shard_count,
        "shard_count": effective_shards,
        # The most test jobs any one architecture runs (arm runs fewer).
        "test_shard_count": max((len(shards) for shards in test_shards.values()), default=0),
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
        handle.write(f"vendored_required={'true' if plan['vendored_required'] else 'false'}\n")
        handle.write(f"vendored_crates={' '.join(plan['vendored'])}\n")
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
    return {"packages": set(delta["reached"]), "suites": set(delta["suites"]),
            "vendored": set(delta["vendored"])}


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
        parser.error('local planning requires verified metadata: pass --metadata as tools/ci/local-affected.sh does '
                     '(cargo metadata sealed to the clean source and validated by tools/ci/current_source_metadata.py)')

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
