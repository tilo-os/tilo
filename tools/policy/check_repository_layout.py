#!/usr/bin/env python3
"""Reject generated artifacts, retired roots, and misplaced repository tooling."""

from __future__ import annotations

import argparse
import hashlib
import subprocess
import sys
from pathlib import Path, PurePosixPath


GENERATED_DIRECTORIES = frozenset(
    {
        ".pytest_cache",
        "__pycache__",
        "node_modules",
        "target",
    }
)

RETIRED_SOURCE_ROOTS = {
    "apps/imager/profile-tool/": "tools/imager-profile/",
    "crates/app/": "crates/sdk/",
    "crates/format/": "crates/doc/",
    "crates/identity/": "crates/collab/",
    "crates/packaging/hello/": "examples/packaging/hello/",
    "crates/packaging/notes-cli/": "examples/packaging/notes-cli/",
    "crates/sdk/rt/": "crates/platform/tilo-rt/",
    "crates/session/tilo-power/": "crates/system/power/",
    "crates/system/tilo-clock/": "crates/platform/tilo-clock/",
    "crates/system/windows-vm/": "crates/windows/windows-vm/",
    "fs/": "crates/system/fs/",
    "os/usr/lib/systemd/system/tilo-power.service": "crates/system/power/",
    "plan/": "docs/architecture/",
    "release/": "dist/",
    "shell/": "crates/session/shell/",
    "tools/check-windows-": "tools/windows/check-",
    "tools/cargo-shim": "tools/ci/cargo-shim",
    "tools/check-build-env.sh": "tools/ci/check-build-env.sh",
    "tools/check-changed-rustfmt.sh": "tools/ci/check-changed-rustfmt.sh",
    "tools/check-warnings.sh": "tools/ci/check-warnings.sh",
    "tools/check_changed_rust_allows.py": "tools/ci/check_changed_rust_allows.py",
    "tools/check_": "tools/policy/check_",
    "tools/ci-": "tools/ci/",
    "tools/ci_": "tools/ci/",
    "tools/clippy-": "tools/ci/clippy-",
    "tools/gstreamer-licenses/": "tools/windows/gstreamer-licenses/",
    "tools/install-windows-": "tools/windows/install-",
    "tools/Run-ImagerEphemeralRunner.ps1": "tools/ci/runners/imager-ephemeral-windows.ps1",
    "tools/run-imager-ephemeral-runner.sh": "tools/ci/runners/imager-ephemeral-linux.sh",
    "tools/test-windows-": "tools/windows/test-",
    "tools/resolve_ci_builder.py": "tools/ci/resolve_builder.py",
    "tools/app_resource_action_baseline.toml": "tools/policy/app_resource_action_baseline/",
    "tools/report_action_directory_provider_offenders.py": "tools/policy/",
    "tools/run-repository-policy.sh": "tools/policy/run.sh",
    "tools/rustc-thin-app-wrapper.sh": "tools/ci/rustc-thin-app-wrapper.sh",
    "tools/windows-": "tools/windows/",
    "ui/": "crates/sdk/ui-core/",
}

# release-apps.yml fetches these application tools by exact reviewed path and
# digest, so moving them means moving those pins with them. This is a closed
# exception, not a precedent for adding more application code to the tools root.
ROOT_APPLICATION_CONTROLLER_FILES = frozenset(
    {
        "tools/app_packaging_contract.py",
        "tools/app_packaging_plan.py",
        "tools/app_packaging_qualification.py",
        "tools/assemble_verified_apps.py",
        "tools/canonical_verifier_bundle.py",
    }
)


def generated_artifact_violations(paths: list[str]) -> list[str]:
    violations = []
    for path in paths:
        parts = PurePosixPath(path).parts
        generated = next((part for part in parts[:-1] if part in GENERATED_DIRECTORIES), None)
        if generated is not None:
            violations.append(f"tracked file is inside generated `{generated}` directory: {path}")
    return sorted(violations)


def retired_source_root_violations(paths: list[str]) -> list[str]:
    violations = []
    for path in paths:
        for retired, replacement in RETIRED_SOURCE_ROOTS.items():
            if path.startswith(retired):
                violations.append(
                    f"tracked file uses retired source root `{retired}`; move it under `{replacement}`: {path}"
                )
                break
    return sorted(violations)


def flat_application_tooling_violations(paths: list[str]) -> list[str]:
    candidates = (
        path
        for path in paths
        if path.startswith(("tools/app_", "tools/accept_app_"))
        or path == "tools/assemble_verified_apps.py"
    )
    return sorted(
        "new application tooling must live under `tools/apps/`; the root-level "
        f"controller exception is closed: {path}"
        for path in candidates
        if path not in ROOT_APPLICATION_CONTROLLER_FILES
    )


def repository_layout_violations(paths: list[str]) -> list[str]:
    return sorted(
        generated_artifact_violations(paths)
        + retired_source_root_violations(paths)
        + flat_application_tooling_violations(paths)
    )


def duplicate_fixture_violations(root: Path, paths: list[str]) -> list[str]:
    by_digest: dict[str, list[str]] = {}
    for path in paths:
        relative = PurePosixPath(path)
        if "fixtures" not in relative.parts[:-1]:
            continue
        candidate = root / path
        if not candidate.is_file():
            continue
        digest = hashlib.sha256(candidate.read_bytes()).hexdigest()
        by_digest.setdefault(digest, []).append(path)
    return sorted(
        f"duplicate tracked fixture content; use one canonical file: {', '.join(sorted(matches))}"
        for matches in by_digest.values()
        if len(matches) > 1
    )


def tracked_files(root: Path) -> list[str]:
    result = subprocess.run(
        ["git", "ls-files", "-z"],
        cwd=root,
        check=True,
        capture_output=True,
    )
    return [
        path.decode("utf-8", errors="surrogateescape")
        for path in result.stdout.split(b"\0")
        if path
    ]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    args = parser.parse_args(argv)
    root = args.root.resolve()
    try:
        paths = tracked_files(root)
        violations = repository_layout_violations(paths)
        violations.extend(duplicate_fixture_violations(root, paths))
        violations.sort()
    except (OSError, subprocess.CalledProcessError) as error:
        print(f"repository layout check failed: {error}", file=sys.stderr)
        return 1
    if violations:
        for violation in violations:
            print(f"  {violation}", file=sys.stderr)
        print("FAIL: repair the repository layout violations above", file=sys.stderr)
        return 1
    print("repository layout OK (no generated artifacts, retired roots, or misplaced tooling)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
