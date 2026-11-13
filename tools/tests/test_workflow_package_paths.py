from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path

from tools.policy.check_workflow_package_paths import (
    GENERATED_BEGIN,
    GENERATED_END,
    audit,
    dependency_directories,
    pattern_covers,
    regenerate,
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
                marked = lambda *entries: (
                    "on:\n  push:\n    paths:\n      - Cargo.toml\n"
                    f"      {GENERATED_BEGIN}\n"
                    + "".join(f'      - "{entry}/**"\n' for entry in entries)
                    + f"      {GENERATED_END}\n"
                )
                violations = audit(
                    metadata(root),
                    root,
                    "app",
                    marked("apps/app", "crates/shared").replace(
                        f"      {GENERATED_END}\n",
                        f"      {GENERATED_END}\n  pull_request:\n    paths:\n"
                        f"      - Cargo.toml\n      {GENERATED_BEGIN}\n"
                        '      - "apps/app/**"\n'
                        f"      {GENERATED_END}\n",
                    ),
                )
            self.assertIn(
                'paths block 2 misses - "crates/shared/**"',
                [str(item) for item in violations],
            )
        with self.subTest(phase='the generator fills the region between the markers'):
            text = (
                "on:\n  push:\n    paths:\n      - Cargo.toml\n"
                f"      {GENERATED_BEGIN}\n      {GENERATED_END}\n"
            )
            regenerated, violations = regenerate(
                text, {"apps/app", "crates/shared"}, report_drift=False
            )
            self.assertEqual(violations, [])
            self.assertIn(
                '      - "apps/app/**"\n      - "crates/shared/**"\n', regenerated
            )
            self.assertEqual(
                regenerate(regenerated, {"apps/app", "crates/shared"}),
                (regenerated, []),
                "regenerating a current region changes nothing",
            )
        with self.subTest(phase='a region that does not hold the closure is reported'):
            current, _ = regenerate(text, {"apps/app"})
            _, violations = regenerate(current, {"apps/app", "crates/shared"})
            self.assertEqual(
                [str(item) for item in violations],
                [
                    "paths block 1 generated closure is not current: run "
                    "tools/policy/check_workflow_package_paths.py --edges normal --write "
                    "<package> <workflow>"
                ],
            )
        with self.subTest(phase='a directory glob kept by hand outside the region is reported'):
            hand = text.replace(
                "      - Cargo.toml\n", "      - Cargo.toml\n      - apps/app/**\n"
            )
            _, violations = regenerate(hand, {"apps/app"}, report_drift=False)
            self.assertEqual(
                [str(item) for item in violations],
                [
                    "paths block 1 keeps '- apps/app/**' by hand outside the "
                    "generated region"
                ],
            )
        with self.subTest(phase='a block that cannot be generated is reported'):
            _, violations = regenerate(
                "on:\n  push:\n    paths:\n      - Cargo.toml\n", {"apps/app"}
            )
            self.assertEqual(len(violations), 1)
            self.assertIn("needs one", str(violations[0]))
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
