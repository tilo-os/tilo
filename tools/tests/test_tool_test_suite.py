from __future__ import annotations

import ast
import unittest
from pathlib import Path


TESTS = Path(__file__).resolve().parent


class ToolTestSuiteTests(unittest.TestCase):
    def test_every_python_test_module_uses_unittest_discovery(self) -> None:
        missing: list[str] = []
        for path in sorted(TESTS.glob("test_*.py")):
            tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
            has_test_case = any(
                isinstance(node, ast.ClassDef)
                and any(
                    isinstance(base, ast.Attribute) and base.attr == "TestCase"
                    or isinstance(base, ast.Name) and base.id == "TestCase"
                    for base in node.bases
                )
                for node in tree.body
            )
            has_load_tests = any(
                isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
                and node.name == "load_tests"
                for node in tree.body
            )
            if not (has_test_case or has_load_tests):
                missing.append(path.name)

        self.assertEqual(
            missing,
            [],
            "test modules must expose unittest.TestCase or load_tests so CI discovery runs them",
        )


if __name__ == "__main__":
    unittest.main()
