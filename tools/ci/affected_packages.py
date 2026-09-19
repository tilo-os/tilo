#!/usr/bin/env python3
"""Select and balance the workspace packages affected by a git change.

The selector works from Cargo metadata rather than a hand-maintained package
list.  A package is selected when its own files change, or when it transitively
depends on a changed workspace package.  Repository-wide build inputs fall back
to the complete workspace. Ordinary feedback checks the closure on x86 and tests
changed packages plus contract suites; --full qualifies both native architectures.
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
from typing import Iterable


# Preserve the existing wire/document/package contract suites, including
# pkg/tests/{app_manifests,verify_attest} and qdoc/tests/{convergence,office_roundtrip}.
# These catch repository contracts outside a changed crate's dependency edges.
ALWAYS_PACKAGES = {"collab-types", "pkg", "qdoc"}
EXTERNAL_ASSET_MANIFEST = "os/external-assets/github-assets.tsv"
# Audited developer/CI prose, not compiled documentation or runtime contracts.
# Keep this explicit: crates/sdk/ui-core/CHARTS.md is included by Rust, docs/app-development.md
# is inspected by tilo-dev, and docs/packaging.md is a packaging contract input.
# Security, layout and workflow policy checks remain mandatory outside Rust.
NON_COMPILING_DOCS = {
    "AGENTS.md",
    "DEVELOPMENT.md",
    "docs/ci-performance.md",
    "docs/ci-emergency-relief.md",
}
# These orchestration/validation helpers are exercised by the mandatory quality
# job's tools/policy/run.sh unittest discovery. They are not Rust build
# inputs. Keep exact source/test pairs: unknown tools, deleted files, workflows,
# compiler wrappers and packaging implementation changes retain native coverage.
QUALITY_ONLY_HELPERS = {
    "tools/ci/check-changed-rustfmt.sh": "tools/tests/test_changed_rust_comparison.py",
    "tools/ci/check_changed_rust_allows.py": "tools/tests/test_changed_rust_comparison.py",
    "tools/ci/affected_packages.py": "tools/tests/test_ci_select.py",
    "tools/ci/queue_coalesce.py": "tools/tests/test_ci_queue_coalesce.py",
    "tools/ci/timing_report.py": "tools/tests/test_ci_timing_report.py",
    "tools/ci/merge_pr.py": "tools/tests/test_ci_merge_pr.py",
    "tools/ci/builder_workflow_plan.py": "tools/tests/test_builder_workflow_plan.py",
    "tools/ci/resolve_promoted_builder.py": "tools/tests/test_resolve_promoted_builder.py",
    "tools/ci/native-mold-linker.sh": "tools/tests/test_ci_native_mold.py",
    "tools/ci/verify-native-mold.sh": "tools/tests/test_ci_native_mold.py",
    "tools/ci/dispatch_merge.py": "tools/tests/test_ci_merge_dispatch.py",
    "tools/ci/workflow_applicability.py": "tools/tests/test_workflow_applicability.py",
    "tools/policy/run.sh": "tools/tests/test_ci_select.py",
    "tools/policy/check_workspace_members.py": "tools/tests/test_check_workspace_members.py",
    "tools/policy/check_repository_layout.py": "tools/tests/test_check_repository_layout.py",
    "tools/policy/check_workflow_actions.py": "tools/tests/test_workflow_actions.py",
    "tools/policy/check_workflow_package_paths.py": "tools/tests/test_workflow_package_paths.py",
    "tools/policy/check_workflow_timeouts.py": "tools/tests/test_workflow_timeouts.py",
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


def run(*args: str, cwd: Path) -> str:
    return subprocess.check_output(args, cwd=cwd, text=True).strip()


def cargo_metadata(root: Path) -> dict:
    return json.loads(run("cargo", "metadata", "--locked", "--no-deps", "--format-version", "1", cwd=root))


def package_maps(metadata: dict, root: Path) -> tuple[dict[str, dict], dict[Path, str]]:
    packages = {p["name"]: p for p in metadata["packages"]}
    by_dir = {
        Path(p["manifest_path"]).resolve().parent.relative_to(root.resolve()): p["name"]
        for p in metadata["packages"]
    }
    return packages, by_dir


def comparison_merge_base(root: Path, base: str | None, head: str) -> str | None:
    if not base:
        base = run("git", "rev-parse", f"{head}^", cwd=root)
    try:
        return run("git", "merge-base", base, head, cwd=root)
    except subprocess.CalledProcessError:
        return None


def changed_files(root: Path, base: str | None, head: str) -> list[str]:
    if not base:
        base = run("git", "rev-parse", f"{head}^", cwd=root)
    merge_base = comparison_merge_base(root, base, head)
    if merge_base is None:
        # A depth-limited checkout may have the event's exact base tree without
        # enough ancestry to compute a merge base. Comparing those two trees can
        # over-select files for a pull request whose base moved, but it cannot
        # omit a file that differs between the advertised base and head.
        diff_range = f"{base}..{head}"
    else:
        diff_range = f"{merge_base}...{head}"
    # A move affects both the old owner and the new owner. Rename detection's
    # name-only output reports only the destination, losing the old package's
    # reverse dependants. NUL delimiters also avoid Git quoting Unicode paths
    # or splitting valid filenames containing newlines/leading whitespace.
    output = subprocess.check_output(
        ["git", "diff", "--no-renames", "--name-only", "-z",
         "--diff-filter=ACDMRTUXB", diff_range, "--"], cwd=root,
    )
    return [path.decode("utf-8", errors="surrogateescape")
            for path in output.split(b"\0") if path]


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
    if merge_base is None:
        return False
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


def owner_for(path: str, by_dir: dict[Path, str]) -> str | None:
    candidate = Path(path)
    owners = ((len(directory.parts), name) for directory, name in by_dir.items() if candidate.is_relative_to(directory))
    return max(owners, default=(0, None))[1]


def reverse_dependencies(packages: dict[str, dict], root: Path) -> dict[str, set[str]]:
    manifest_to_name = {
        str(Path(pkg["manifest_path"]).resolve()): name for name, pkg in packages.items()
    }
    reverse = {name: set() for name in packages}
    for consumer, pkg in packages.items():
        for dependency in pkg.get("dependencies", []):
            dep_path = dependency.get("path")
            if not dep_path:
                continue
            manifest = str((Path(dep_path).resolve() / "Cargo.toml"))
            provider = manifest_to_name.get(manifest)
            if provider:
                reverse[provider].add(consumer)
    return reverse


def closure(seeds: Iterable[str], reverse: dict[str, set[str]]) -> set[str]:
    selected = set(seeds)
    pending = list(selected)
    while pending:
        package = pending.pop()
        for dependent in reverse.get(package, ()):
            if dependent not in selected:
                selected.add(dependent)
                pending.append(dependent)
    return selected


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


def make_plan(
    metadata: dict,
    root: Path,
    changes: list[str],
    shard_count: int,
    *,
    force_full: bool = False,
    builder_only_external_assets: bool = False,
) -> dict:
    packages, by_dir = package_maps(metadata, root)
    seeds = {owner for path in changes if (owner := owner_for(path, by_dir))}
    projected_quality_paths = (
        {EXTERNAL_ASSET_MANIFEST} if builder_only_external_assets else set()
    )
    global_change = any(
        (path in GLOBAL_FILES and path not in projected_quality_paths)
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

    quality_paths = {
        path for source, test in QUALITY_ONLY_HELPERS.items()
        if (root / source).is_file() and (root / test).is_file()
        for path in (source, test)
    }
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
        selected = closure(seeds, reverse_dependencies(packages, root))
        selected.update(ALWAYS_PACKAGES & packages.keys())
        reason = f"{len(seeds)} changed packages, their reverse dependencies, and contract packages"
        full = False
    # Ordinary feedback type-checks the entire reverse-dependency closure once
    # on x86, but links tests only for changed crates and the existing contract
    # suites. Global/unowned workspace inputs retain complete test coverage.
    tested = (selected if full else (seeds | ALWAYS_PACKAGES) & selected)
    effective_shards = useful_shard_count(selected, packages, shard_count if force_full else 1)
    runners = NATIVE_RUNNERS if force_full else {"x86_64": NATIVE_RUNNERS["x86_64"]}
    matrix = []
    for arch, runner in runners.items():
        for shard in balance(selected, packages, effective_shards):
            shard_names = set(shard["packages"].split()[1::2])
            test_names = sorted(tested & shard_names)
            matrix.append({**shard, "arch": arch, "runner": runner,
                           "test_packages": " ".join(f"-p {name}" for name in test_names)})
    return {
        "full": full,
        "rust_required": bool(selected),
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
    }


def emit_github(plan: dict, output: Path) -> None:
    compact_matrix = json.dumps(plan["matrix"], separators=(",", ":"))
    with output.open("a", encoding="utf-8") as handle:
        handle.write(f"matrix={compact_matrix}\n")
        handle.write(f"full={'true' if plan['full'] else 'false'}\n")
        handle.write(f"rust_required={'true' if plan['rust_required'] else 'false'}\n")
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
    plan = make_plan(
        metadata,
        root,
        changes,
        args.shards,
        force_full=args.full,
        builder_only_external_assets=builder_only_external_assets,
    )
    print(json.dumps(plan, indent=2))
    if args.github_output:
        emit_github(plan, args.github_output)
    return 0


if __name__ == "__main__":
    sys.exit(main())
