#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
from pathlib import Path
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


if __name__ == "__main__":
    unittest.main()
