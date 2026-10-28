from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path

from tools.policy.check_workflow_package_paths import (
    audit,
    dependency_directories,
    pattern_covers,
    unmatched_patterns,
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
    def test_workflow_path_filters_cover_every_package_closure(self) -> None:
        with self.subTest(phase='extracts each paths block and ignores other lists'):
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
        with self.subTest(phase='dependency closure excludes registry packages'):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                self.assertEqual(
                    dependency_directories(metadata(root), root, "app"),
                    {"apps/app", "crates/shared"},
                )
        with self.subTest(phase='directory and broad globs cover a package'):
            self.assertTrue(pattern_covers("crates/shared", "crates/shared/**"))
            self.assertTrue(pattern_covers("crates/shared", "crates/**"))
            self.assertFalse(pattern_covers("crates/shared", "apps/**"))
        with self.subTest(phase='every trigger block must cover the closure'):
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
        with self.subTest(phase='a pattern matching no tracked file is reported'):
            text = "on:\n  push:\n    paths:\n      - apps/app/**\n      - apps/gone.rs\n      - '!apps/x'\n"
            self.assertEqual(unmatched_patterns(text, ["apps/app/src/lib.rs"]), ["apps/gone.rs"])
        with self.subTest(phase='every repository workflow trigger names a tracked file'):
            files = subprocess.run(["git", "ls-files"], cwd=ROOT, text=True,
                                   capture_output=True, check=True).stdout.split()
            stale = [
                f"{workflow.name}: {pattern}"
                for workflow in sorted((ROOT / ".forgejo/workflows").glob("*.yml"))
                for pattern in unmatched_patterns(workflow.read_text(encoding="utf-8"), files)
            ]
            self.assertEqual(stale, [])


if __name__ == "__main__":
    unittest.main()
