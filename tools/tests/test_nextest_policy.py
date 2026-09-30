from __future__ import annotations

import re
import tomllib
import unittest
from pathlib import Path


REPO = Path(__file__).parents[2]
NEXTEST = REPO / ".config" / "nextest.toml"
SHELL = REPO / "crates" / "session" / "shell" / "src"
WORKFLOWS = REPO / ".forgejo" / "workflows"


def native_4k_tests() -> set[str]:
    names: set[str] = set()
    pattern = re.compile(r"^\s*fn\s+(native_4k_[a-zA-Z0-9_]+)\s*\(", re.MULTILINE)
    for source in SHELL.rglob("*.rs"):
        names.update(pattern.findall(source.read_text(encoding="utf-8")))
    return names


def folded_values(workflow: str, key: str) -> list[str]:
    """Values of `key: >-` folded scalars, without a YAML dependency."""
    values = []
    lines = workflow.splitlines()
    for index, line in enumerate(lines):
        match = re.fullmatch(rf"(\s*){key}: >-", line)
        if match is None:
            continue
        indent = len(match.group(1))
        body = []
        for following in lines[index + 1:]:
            if following.strip() and len(following) - len(following.lstrip()) <= indent:
                break
            body.append(following.strip())
        values.append(" ".join(part for part in body if part))
    return values


def effective_overrides(config: dict, profile_name: str) -> list[dict]:
    """CI inherits default overrides. A CI override is added on top, not a second copy."""
    default = config["profile"]["default"].get("overrides", [])
    if profile_name == "default":
        return default
    return [*default, *config["profile"].get(profile_name, {}).get("overrides", [])]


class NextestPolicyTests(unittest.TestCase):
    def test_ci_does_not_retry(self) -> None:
        ci = tomllib.loads(NEXTEST.read_text(encoding="utf-8"))["profile"]["ci"]
        self.assertEqual(ci.get("retries", 0), 0)
        self.assertNotIn("overrides", ci)

    def test_every_native_4k_test_gets_an_exclusive_cpu_budget(self) -> None:
        config = tomllib.loads(NEXTEST.read_text(encoding="utf-8"))
        expected = native_4k_tests()
        self.assertTrue(expected, "the source scan must find native-4K renderer tests")

        for profile_name in ("default", "ci"):
            overrides = effective_overrides(config, profile_name)
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

    def test_ci_prints_slow_tests_within_a_generous_hang_bound(self) -> None:
        ci = tomllib.loads(NEXTEST.read_text(encoding="utf-8"))["profile"]["ci"]
        timeout = ci["slow-timeout"]
        period = int(timeout["period"].removesuffix("s"))
        # SLOW lines appear within half a minute of a test starting ...
        self.assertLessEqual(period, 30)
        self.assertIn(ci["status-level"], ("slow", "leak", "pass", "skip", "all"))
        # ... while the kill stays a hang bound far above the longest test.
        self.assertGreaterEqual(period * timeout["terminate-after"], 480)

    def test_every_golden_selection_names_the_same_tests(self) -> None:
        # PR shards exclude the goldens, nightly verifies them on an idle
        # renderer and the refresh workflow renders candidates for them.
        # Diverging copies would verify or refresh a different set.
        config = tomllib.loads(NEXTEST.read_text(encoding="utf-8"))
        filters = {
            f"nextest {profile}": override["filter"]
            for profile in ("default", "ci")
            for override in effective_overrides(config, profile)
            if override.get("test-group") == "gpu-golden"
        }
        for name in ("check.yml", "ci-nightly.yml", "dev-visual-baselines.yml"):
            values = folded_values((WORKFLOWS / name).read_text(encoding="utf-8"), "GOLDEN_FILTER")
            self.assertEqual(len(values), 1, name)
            filters[name] = values[0]
        normalised = {name: " ".join(value.split()) for name, value in filters.items()}
        self.assertEqual(len(set(normalised.values())), 1, normalised)


if __name__ == "__main__":
    unittest.main()
