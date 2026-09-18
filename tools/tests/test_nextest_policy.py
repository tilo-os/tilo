from __future__ import annotations

import re
import tomllib
import unittest
from pathlib import Path


REPO = Path(__file__).parents[2]
NEXTEST = REPO / ".config" / "nextest.toml"
SHELL = REPO / "crates" / "session" / "shell" / "src"


def native_4k_tests() -> set[str]:
    names: set[str] = set()
    pattern = re.compile(r"^\s*fn\s+(native_4k_[a-zA-Z0-9_]+)\s*\(", re.MULTILINE)
    for source in SHELL.rglob("*.rs"):
        names.update(pattern.findall(source.read_text(encoding="utf-8")))
    return names


class NextestPolicyTests(unittest.TestCase):
    def test_every_native_4k_test_gets_an_exclusive_cpu_budget(self) -> None:
        config = tomllib.loads(NEXTEST.read_text(encoding="utf-8"))
        expected = native_4k_tests()
        self.assertTrue(expected, "the source scan must find native-4K renderer tests")

        for profile_name in ("default", "ci"):
            overrides = config["profile"][profile_name]["overrides"]
            exclusive_filters = [
                override["filter"]
                for override in overrides
                if override.get("threads-required") == "num-cpus"
            ]
            covered = {
                test_name
                for test_name in expected
                if any(test_name in filter_expression for filter_expression in exclusive_filters)
            }
            self.assertEqual(
                covered,
                expected,
                f"{profile_name} leaves native-4K tests competing with the parallel suite",
            )


if __name__ == "__main__":
    unittest.main()
