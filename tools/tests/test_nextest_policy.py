from __future__ import annotations

import re
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path


REPO = Path(__file__).parents[2]
NEXTEST = REPO / ".config" / "nextest.toml"
FILTERS = REPO / ".config" / "nextest-filters.toml"
SHELL = REPO / "crates" / "session" / "shell" / "src"
WORKFLOWS = REPO / ".forgejo" / "workflows"


def native_4k_tests() -> set[str]:
    names: set[str] = set()
    pattern = re.compile(r"^\s*fn\s+(native_4k_[a-zA-Z0-9_]+)\s*\(", re.MULTILINE)
    for source in SHELL.rglob("*.rs"):
        names.update(pattern.findall(source.read_text(encoding="utf-8")))
    return names


def peer_sync_budget_tests() -> set[str]:
    source = (SHELL.parent / "tests" / "peer_sync_test.rs").read_text(encoding="utf-8")
    pattern = re.compile(r"^\s*async\s+fn\s+([a-zA-Z0-9_]+_budget)\s*\(", re.MULTILINE)
    return {f"peer_sync_test::{name}" for name in pattern.findall(source)}


def effective_overrides(config: dict, profile_name: str) -> list[dict]:
    """CI inherits default overrides. A CI override is added on top, not a second copy."""
    default = config["profile"]["default"].get("overrides", [])
    if profile_name == "default":
        return default
    return [*default, *config["profile"].get(profile_name, {}).get("overrides", [])]


class NextestPolicyTests(unittest.TestCase):
    def test_cleanup_keeps_executable_shared_libraries_for_later_cargo_steps(self) -> None:
        workflow = (WORKFLOWS / "check.yml").read_text(encoding="utf-8")
        cleanup = workflow.split("- name: Free the test executables", 1)[1].split("- name:", 1)[0]
        command = "\n".join(line.removeprefix("          ")
                            for line in cleanup.split("run: |\n", 1)[1].splitlines())
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            deps = root / "target/debug/deps"
            deps.mkdir(parents=True)
            names = {"app-test-123": 0o755, "libderive-123.so": 0o755,
                     "libnative.so.1": 0o755, "libapp-123.rlib": 0o644}
            for name, mode in names.items():
                path = deps / name
                path.write_bytes(b"fixture")
                path.chmod(mode)
            output = root / "outputs"
            subprocess.run(["bash", "-c", command], cwd=root, check=True,
                           env={"PATH": "/usr/bin:/bin", "GITHUB_OUTPUT": str(output)})
            self.assertEqual(output.read_text(), "shared-libraries=2\n")
            self.assertFalse((deps / "app-test-123").exists())
            for name in ("libderive-123.so", "libnative.so.1", "libapp-123.rlib"):
                self.assertTrue((deps / name).exists(), name)

    def test_ci_does_not_retry(self) -> None:
        ci = tomllib.loads(NEXTEST.read_text(encoding="utf-8"))["profile"]["ci"]
        self.assertEqual(ci.get("retries", 0), 0)
        self.assertNotIn("overrides", ci)

    def test_ci_names_leaks_without_printing_ordinary_passes(self) -> None:
        ci = tomllib.loads(NEXTEST.read_text(encoding="utf-8"))["profile"]["ci"]
        self.assertEqual(ci["status-level"], "leak")

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

    def test_workflows_read_filters_from_the_tested_tree(self) -> None:
        # A pull request is tested on top of main with its own workflow file;
        # a filterset copied into that file names packages main may have
        # renamed, and nextest rejects a package() that matches nothing.
        for name in ("check.yml", "ci-nightly.yml", "dev-visual-baselines.yml"):
            workflow = (WORKFLOWS / name).read_text(encoding="utf-8")
            self.assertNotRegex(workflow, r"(?m)^\s*(GOLDEN|PUSH_ONLY|QUARANTINE)_FILTER:", name)
            self.assertIn("python3 tools/ci/nextest_filters.py >> \"$GITHUB_ENV\"", workflow, name)
        output = subprocess.run([sys.executable, str(REPO / "tools/ci/nextest_filters.py")],
                                check=True, capture_output=True, text=True).stdout
        names = [line.split("=", 1)[0] for line in output.splitlines()]
        self.assertEqual(names, ["GOLDEN_FILTER", "PUSH_ONLY_FILTER", "QUARANTINE_FILTER"])

if __name__ == "__main__":
    unittest.main()

    def test_required_shards_skip_quarantined_tests_on_every_event(self) -> None:
        # A quarantine names its tracking issue and is skipped on pull
        # requests and on pushes to main alike, never only on one of them.
        source = FILTERS.read_text(encoding="utf-8")
        quarantine = tomllib.loads(source)["quarantine"]
        self.assertTrue(quarantine.strip())
        comment = source.split("quarantine =", 1)[0].rsplit("\n\n", 1)[-1]
        self.assertRegex(comment, r"#\d+", "a quarantine names its tracking issue")
        workflow = (WORKFLOWS / "check.yml").read_text(encoding="utf-8")
        step = workflow.split("- name: Test shard ${{ matrix.shard }}", 1)[1].split("- name:", 1)[0]
        self.assertIn('-E "not ($GOLDEN_FILTER) and not ($QUARANTINE_FILTER)$', step)
