from __future__ import annotations

import tempfile
import unittest
from fnmatch import fnmatchcase
from pathlib import Path

from tools.policy.check_workflow_package_paths import (
    audit,
    dependency_directories,
    pattern_covers,
    workflow_path_blocks,
)


ROOT = Path(__file__).resolve().parents[2]


def metadata(root: Path) -> dict:
    def package(name: str, directory: str) -> dict:
        return {
            "id": name,
            "name": name,
            "manifest_path": str(root / directory / "Cargo.toml"),
        }

    return {
        "packages": [
            package("app", "apps/app"),
            package("shared", "crates/shared"),
            {
                "id": "external",
                "name": "external",
                "manifest_path": "/registry/external/Cargo.toml",
            },
        ],
        "resolve": {
            "nodes": [
                {"id": "app", "dependencies": ["shared", "external"]},
                {"id": "shared", "dependencies": []},
                {"id": "external", "dependencies": []},
            ]
        },
    }


class WorkflowPackagePathTests(unittest.TestCase):
    def test_extracts_each_paths_block_and_ignores_other_lists(self) -> None:
        text = """on:
  push:
    paths:
      - "apps/app/**"
      - crates/shared/** # local dependency
  pull_request:
    paths:
      - 'apps/app/**'
jobs:
  check:
    steps:
      - run: cargo test
"""
        self.assertEqual(
            workflow_path_blocks(text),
            [
                {"apps/app/**", "crates/shared/**"},
                {"apps/app/**"},
            ],
        )

    def test_dependency_closure_excludes_registry_packages(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.assertEqual(
                dependency_directories(metadata(root), root, "app"),
                {"apps/app", "crates/shared"},
            )

    def test_directory_and_broad_globs_cover_a_package(self) -> None:
        self.assertTrue(pattern_covers("crates/shared", "crates/shared/**"))
        self.assertTrue(pattern_covers("crates/shared", "crates/**"))
        self.assertFalse(pattern_covers("crates/shared", "apps/**"))

    def test_every_trigger_block_must_cover_the_closure(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            violations = audit(
                metadata(root),
                root,
                "app",
                """on:
  push:
    paths:
      - apps/app/**
      - crates/shared/**
  pull_request:
    paths:
      - apps/app/**
""",
            )
        self.assertEqual(
            [str(item) for item in violations],
            ['paths block 2 misses - "crates/shared/**"'],
        )

    def test_repository_targeted_workflows_follow_package_closures(self) -> None:
        from tools.policy.check_workflow_package_paths import load_metadata

        repository_metadata = load_metadata(ROOT)
        relay_text = (
            ROOT / ".forgejo/workflows/tilo-relay.yaml"
        ).read_text(encoding="utf-8")
        relay_inputs = {
            ".cargo/**",
            "Cargo.toml",
            "Cargo.lock",
            "keys/cosign.pub",
            "os/base-snapshot.env",
            "os/rpms-snapshot.env",
            "os/dev/verify-rpm-snapshot-package-set.sh",
        }
        for index, block in enumerate(workflow_path_blocks(relay_text), start=1):
            self.assertTrue(
                relay_inputs <= block,
                f"relay paths block {index} misses direct build/signing inputs: "
                f"{sorted(relay_inputs - block)}",
            )
        pairs = (
            ("tilo-relay", ".forgejo/workflows/tilo-relay.yaml"),
            ("tilo-crypto", ".forgejo/workflows/build-kernel.yml"),
        )
        violations: list[str] = []
        for package, relative in pairs:
            workflow = (ROOT / relative).read_text(encoding="utf-8")
            violations.extend(
                f"{relative}: {item}"
                for item in audit(repository_metadata, ROOT, package, workflow)
            )
        self.assertEqual(violations, [])

    def test_relay_catalog_only_changes_do_not_build_an_unsigned_image(self) -> None:
        source = (ROOT / ".forgejo/workflows/tilo-relay.yaml").read_text(
            encoding="utf-8"
        )
        blocks = workflow_path_blocks(source)
        self.assertEqual(len(blocks), 2)
        for block in blocks:
            # PR72 added only PDFium entries here; relay's catalog consumer is
            # Cosign installation, which runs only for tags/manual publication.
            self.assertFalse(any(
                fnmatchcase("os/external-assets/github-assets.tsv", pattern)
                for pattern in block
            ))
            for path in (
                "crates/hosting/tilo-relay/src/main.rs",
                "crates/system/fs/src/lib.rs",
                ".forgejo/actions/install-cosign/action.yml",
            ):
                self.assertTrue(any(fnmatchcase(path, pattern) for pattern in block), path)
        self.assertIn('    tags: ["v*"]', source)
        self.assertIn("\n  workflow_dispatch:\n", source)


if __name__ == "__main__":
    unittest.main()
