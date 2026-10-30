from __future__ import annotations

import re
import subprocess
import sys
import tomllib
import unittest
from pathlib import Path


REPO = Path(__file__).parents[2]
NEXTEST = REPO / ".config" / "nextest.toml"
FILTERS = REPO / ".config" / "nextest-filters.toml"
SHELL = REPO / "crates" / "session" / "shell" / "src"
T_NEXTEST = REPO / "tools" / "ci" / "nextest.sh"


def native_4k_tests() -> set[str]:
    names: set[str] = set()
    pattern = re.compile(r"^\s*fn\s+(native_4k_[a-zA-Z0-9_]+)\s*\(", re.MULTILINE)
    for source in SHELL.rglob("*.rs"):
        names.update(pattern.findall(source.read_text(encoding="utf-8")))
    return names


PEER_SYNC_TESTS = SHELL / "shell" / "network" / "one_host_tests"


def peer_sync_budget_tests() -> set[str]:
    """Latency budgets of an edit crossing devices, as the shell's unit-test paths."""
    names: set[str] = set()
    pattern = re.compile(r"^\s*(?:async\s+)?fn\s+([a-zA-Z0-9_]+_budget)\s*\(", re.MULTILINE)
    for source in sorted(PEER_SYNC_TESTS.rglob("*.rs")):
        module = "::".join(source.relative_to(SHELL).with_suffix("").parts)
        names.update(f"{module}::{name}" for name in pattern.findall(source.read_text(encoding="utf-8")))
    return names


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

    def test_ci_names_leaks_without_printing_ordinary_passes(self) -> None:
        ci = tomllib.loads(NEXTEST.read_text(encoding="utf-8"))["profile"]["ci"]
        self.assertEqual(ci["status-level"], "leak")

    def test_every_native_4k_test_gets_a_share_of_the_cores(self) -> None:
        # A whole 3840x2160 software frame needs many cores, but not the whole
        # pool: a share of sixteen or more, or the pool itself.
        config = tomllib.loads(NEXTEST.read_text(encoding="utf-8"))
        expected = native_4k_tests()
        self.assertTrue(expected, "the source scan must find native-4K renderer tests")

        for profile_name in ("default", "ci"):
            overrides = effective_overrides(config, profile_name)
            exclusive_filters = [
                override["filter"]
                for override in overrides
                if override.get("threads-required") == "num-cpus"
                or (
                    isinstance(override.get("threads-required"), int)
                    and override["threads-required"] >= 16
                )
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

    def test_every_peer_sync_latency_budget_gets_an_exclusive_cpu_budget(self) -> None:
        config = tomllib.loads(NEXTEST.read_text(encoding="utf-8"))
        expected = peer_sync_budget_tests()
        self.assertTrue(expected, "the source scan must find peer-sync latency budgets")

        for profile_name in ("default", "ci"):
            exclusive_filters = [
                override["filter"]
                for override in effective_overrides(config, profile_name)
                if override.get("threads-required") == "num-cpus"
            ]
            covered = {
                test_name
                for test_name in expected
                if any(f"test(={test_name})" in expression for expression in exclusive_filters)
            }
            self.assertEqual(
                covered,
                expected,
                f"{profile_name} measures peer-sync latency under parallel-suite contention",
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
        filters["shared"] = tomllib.loads(FILTERS.read_text(encoding="utf-8"))["golden"]
        normalised = {name: " ".join(value.split()) for name, value in filters.items()}
        self.assertEqual(len(set(normalised.values())), 1, normalised)

    def test_checks_read_filters_from_the_tested_tree(self) -> None:
        self.assertIn("import nextest_filters", T_NEXTEST.read_text(encoding="utf-8"))
        output = subprocess.run([sys.executable, str(REPO / "tools/ci/nextest_filters.py")],
                                check=True, capture_output=True, text=True).stdout
        names = [line.split("=", 1)[0] for line in output.splitlines()]
        self.assertEqual(names, ["GOLDEN_FILTER", "PUSH_ONLY_FILTER", "MICRO_VM_FILTER"])

    def test_every_composed_filter_is_defined_by_the_shared_file(self) -> None:
        # A removed key must not leave a `$..._FILTER` reference behind:
        # nextest parses the composed expression, and an empty `not ()` is a
        # missing expression (exit 94) before any test is selected.
        defined = {
            f"{key.upper()}_FILTER"
            for key in tomllib.loads(FILTERS.read_text(encoding="utf-8"))
        }
        consumers = [
            REPO / ".forgejo" / "workflows" / "check.yml",
            REPO / ".forgejo" / "workflows" / "ci-nightly.yml",
            REPO / "tools" / "ci" / "local_affected.py",
        ]
        referenced: set[str] = set()
        for path in consumers:
            text = path.read_text(encoding="utf-8")
            # A consumer may also define a filter for one step of its own.
            defined.update(re.findall(r"(?m)^\s*([A-Z][A-Z0-9_]*_FILTER)\s*[:=]", text))
            referenced.update(re.findall(r"\b([A-Z][A-Z0-9_]*_FILTER)\b", text))
        self.assertEqual(referenced - defined, set())
        # An empty term is what nextest rejects, so no consumer may compose
        # one even if every name it mentions is defined.
        for path in consumers:
            with self.subTest(consumer=path.name):
                self.assertNotRegex(path.read_text(encoding="utf-8"), r"not\s*\(\s*\)")

if __name__ == "__main__":
    unittest.main()
