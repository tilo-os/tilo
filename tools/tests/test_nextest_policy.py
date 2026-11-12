from __future__ import annotations

import json
import re
import subprocess
import sys
import tomllib
import unittest
from pathlib import Path


REPO = Path(__file__).parents[2]
NEXTEST = REPO / ".config" / "nextest.toml"
FILTERS = REPO / ".config" / "nextest-filters.toml"
T_NEXTEST = REPO / "tools" / "ci" / "nextest.sh"
COST_TABLE = REPO / "apps" / "tilolang" / "tests" / "hosted" / "cost.rs"


def effective_overrides(config: dict, profile_name: str) -> list[dict]:
    """CI inherits default overrides. A CI override is added on top, not a second copy."""
    default = config["profile"]["default"].get("overrides", [])
    if profile_name == "default":
        return default
    return [*default, *config["profile"].get(profile_name, {}).get("overrides", [])]


def effective_scripts(config: dict, profile_name: str) -> list[dict]:
    """The setup scripts a profile runs, default's included for `ci`."""
    default = config["profile"]["default"].get("scripts", [])
    if profile_name == "default":
        return default
    return [*default, *config["profile"].get(profile_name, {}).get("scripts", [])]


def package_targets() -> dict[str, set[str]]:
    """Every package's test-binary names, by package name."""
    targets: dict[str, set[str]] = {}
    for manifest_path in REPO.rglob("Cargo.toml"):
        relative = manifest_path.relative_to(REPO)
        if any(part in {"target", "vendor", "node_modules"} for part in relative.parts):
            continue
        try:
            manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
        except tomllib.TOMLDecodeError:
            continue
        name = manifest.get("package", {}).get("name")
        if not name:
            continue
        names = targets.setdefault(name, set())
        # The lib's and each binary's own test target, then the declared ones.
        names.add(name.replace("-", "_"))
        for kind in ("bin", "test", "bench"):
            for target in manifest.get(kind, []):
                names.add(target["name"])
                if "path" in target:
                    names.add(Path(target["path"]).stem)
        for directory in ("src/bin", "benches"):
            for source in (manifest_path.parent / directory).glob("*.rs"):
                names.add(source.stem)
    return targets


class NextestPolicyTests(unittest.TestCase):
    def test_every_named_test_binary_exists(self) -> None:
        # nextest refuses a whole config whose filter names a binary no package
        # builds, so one test binary merged into another stops every run.
        metadata = subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--no-deps", "--offline", "--locked"],
            cwd=REPO, check=True, capture_output=True, text=True).stdout
        targets = {
            package["name"]: {target["name"] for target in package["targets"]}
            for package in json.loads(metadata)["packages"]
        }
        for config in (NEXTEST, FILTERS):
            text = config.read_text(encoding="utf-8")
            for package, binary in re.findall(r"package\(([\w-]+)\) and binary\((\w+)\)", text):
                with self.subTest(config=config.name, package=package, binary=binary):
                    self.assertIn(package, targets)
                    self.assertIn(binary, targets[package])

    def test_ci_does_not_retry(self) -> None:
        ci = tomllib.loads(NEXTEST.read_text(encoding="utf-8"))["profile"]["ci"]
        self.assertEqual(ci.get("retries", 0), 0)
        self.assertNotIn("overrides", ci)

    def test_ci_names_leaks_without_printing_ordinary_passes(self) -> None:
        ci = tomllib.loads(NEXTEST.read_text(encoding="utf-8"))["profile"]["ci"]
        self.assertEqual(ci["status-level"], "leak")

    def test_ci_prints_slow_tests_within_a_generous_hang_bound(self) -> None:
        ci = tomllib.loads(NEXTEST.read_text(encoding="utf-8"))["profile"]["ci"]
        timeout = ci["slow-timeout"]
        period = int(timeout["period"].removesuffix("s"))
        # SLOW lines appear within half a minute of a test starting ...
        self.assertLessEqual(period, 30)
        self.assertIn(ci["status-level"], ("slow", "leak", "pass", "skip", "all"))
        # ... while the kill stays a hang bound far above the longest test.
        self.assertGreaterEqual(period * timeout["terminate-after"], 480)

    def test_every_setup_script_names_a_test_binary_that_exists(self) -> None:
        # A filter naming a binary the package does not build matches nothing:
        # nextest refuses the run outright when a test is selected, and the
        # setup a test needs silently never runs when it is not. Tests
        # consolidated into one binary leave such a name behind.
        targets = package_targets()
        config = tomllib.loads(NEXTEST.read_text(encoding="utf-8"))
        checked = 0
        for profile in ("default", "ci"):
            for script in effective_scripts(config, profile):
                found = re.search(r"\bpackage\(\s*([A-Za-z0-9_-]+)\s*\)", script["filter"])
                binary = re.search(r"\bbinary\(\s*([A-Za-z0-9_-]+)\s*\)", script["filter"])
                if not found or not binary:
                    continue
                checked += 1
                with self.subTest(profile=profile, filter=script["filter"]):
                    self.assertIn(found.group(1), targets, "a package the tree has")
                    self.assertIn(
                        binary.group(1),
                        targets[found.group(1)],
                        f"{found.group(1)} builds no test binary of that name",
                    )
        self.assertGreater(checked, 0, "the config's setup scripts are read")

    def test_the_cost_table_prints_for_the_run_ci_makes(self) -> None:
        # A `test(=…)` names the whole test, its module included: a filter that
        # leaves the module out matches nothing, and the table the interaction
        # test prints on a pass never reaches a log. The run the CI makes is
        # the profile's own overrides on top of the default ones.
        test = re.search(
            r"(?m)^fn (what_the_interactions_a_person_feels_cost)\s*\(",
            COST_TABLE.read_text(encoding="utf-8"),
        )
        self.assertIsNotNone(test)
        config = tomllib.loads(NEXTEST.read_text(encoding="utf-8"))
        printing = [
            override["filter"]
            for override in effective_overrides(config, "ci")
            if override.get("success-output") == "final" and "cost::" in override["filter"]
        ]
        self.assertEqual(len(printing), 1, printing)
        self.assertIn(f"test(=cost::{test.group(1)})", printing[0])

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
        self.assertEqual(names, ["GOLDEN_FILTER", "PUSH_ONLY_FILTER"])

    def test_a_run_leaves_out_the_tests_whose_capability_this_machine_lacks(self) -> None:
        # The ci profile's setup promises every mapped capability, so a test of
        # one must be left out where the probe says this machine has it not.
        # The wrapper asks, and the run has its answer wherever it is started:
        # a workflow's shard and a job t shards alike.
        text = T_NEXTEST.read_text(encoding="utf-8")
        self.assertIn("tools/ci/capability_filter.py", text)
        # Into the filter the run was given, not beside it: nextest takes one.
        self.assertIn('(${arguments[index + 1]}) & ($left_out)', text)

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
            # A consumer may also define a filter for one step of its own: a shell
            # or env assignment, or one a step hands to the steps after it
            # (`echo "SOME_FILTER=$filter" >> "$GITHUB_ENV"`).
            defined.update(re.findall(r"(?m)^\s*([A-Z][A-Z0-9_]*_FILTER)\s*[:=]", text))
            defined.update(re.findall(r"\b([A-Z][A-Z0-9_]*_FILTER)=", text))
            referenced.update(re.findall(r"\b([A-Z][A-Z0-9_]*_FILTER)\b", text))
        self.assertEqual(referenced - defined, set())
        # An empty term is what nextest rejects, so no consumer may compose
        # one even if every name it mentions is defined.
        for path in consumers:
            with self.subTest(consumer=path.name):
                self.assertNotRegex(path.read_text(encoding="utf-8"), r"not\s*\(\s*\)")

if __name__ == "__main__":
    unittest.main()
