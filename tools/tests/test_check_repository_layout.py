#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
from pathlib import Path
import tempfile
import unittest


MODULE_PATH = Path(__file__).parents[1] / "check_repository_layout.py"
SPEC = importlib.util.spec_from_file_location("check_repository_layout", MODULE_PATH)
assert SPEC and SPEC.loader
checker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(checker)


class RepositoryLayoutTests(unittest.TestCase):
    def test_source_and_curated_shot_directories_are_allowed(self) -> None:
        self.assertEqual(
            checker.generated_artifact_violations(
                [
                    "apps/calculator/src/lib.rs",
                    "apps/calculator/shots/default.png",
                    "dist/target-contract.sh",
                ]
            ),
            [],
        )

    def test_nested_generated_directories_are_rejected(self) -> None:
        self.assertEqual(
            checker.generated_artifact_violations(
                [
                    "tools/__pycache__/check.cpython-314.pyc",
                    "apps/calculator/target/shots/default.png",
                    "web/node_modules/package/index.js",
                    "tests/.pytest_cache/v/cache/nodeids",
                ]
            ),
            [
                "tracked file is inside generated `.pytest_cache` directory: tests/.pytest_cache/v/cache/nodeids",
                "tracked file is inside generated `__pycache__` directory: tools/__pycache__/check.cpython-314.pyc",
                "tracked file is inside generated `node_modules` directory: web/node_modules/package/index.js",
                "tracked file is inside generated `target` directory: apps/calculator/target/shots/default.png",
            ],
        )

    def test_only_directory_components_match(self) -> None:
        self.assertEqual(
            checker.generated_artifact_violations(
                ["docs/target/README.md", "dist/target-contract.sh", "src/node_modules.rs"]
            ),
            ["tracked file is inside generated `target` directory: docs/target/README.md"],
        )

    def test_retired_source_roots_are_rejected_with_their_replacements(self) -> None:
        self.assertEqual(
            checker.retired_source_root_violations(
                [
                    "crates/system/windows-vm/src/lib.rs",
                    "docs/architecture/overview.md",
                    "fs/src/lib.rs",
                    "plan/security.md",
                ]
            ),
            [
                "tracked file uses retired source root `crates/system/windows-vm/`; move it under `crates/windows/windows-vm/`: crates/system/windows-vm/src/lib.rs",
                "tracked file uses retired source root `fs/`; move it under `crates/system/fs/`: fs/src/lib.rs",
                "tracked file uses retired source root `plan/`; move it under `docs/architecture/`: plan/security.md",
            ],
        )

    def test_repository_layout_combines_generated_and_retired_root_checks(self) -> None:
        self.assertEqual(
            checker.repository_layout_violations(
                ["fs/src/lib.rs", "tools/__pycache__/check.cpython-314.pyc"]
            ),
            [
                "tracked file is inside generated `__pycache__` directory: tools/__pycache__/check.cpython-314.pyc",
                "tracked file uses retired source root `fs/`; move it under `crates/system/fs/`: fs/src/lib.rs",
            ],
        )

    def test_identical_tracked_fixtures_must_share_one_canonical_file(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            first = root / "one/fixtures/sample.bin"
            second = root / "two/tests/fixtures/copy.bin"
            outside = root / "three/sample.bin"
            for path in (first, second, outside):
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"same fixture bytes")

            self.assertEqual(
                checker.duplicate_fixture_violations(
                    root,
                    [
                        "one/fixtures/sample.bin",
                        "two/tests/fixtures/copy.bin",
                        "three/sample.bin",
                    ],
                ),
                [
                    "duplicate tracked fixture content; use one canonical file: one/fixtures/sample.bin, two/tests/fixtures/copy.bin"
                ],
            )


if __name__ == "__main__":
    unittest.main()
