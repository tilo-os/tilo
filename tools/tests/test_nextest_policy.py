from __future__ import annotations

import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path


REPO = Path(__file__).parents[2]
NEXTEST = REPO / ".config" / "nextest.toml"
FILTERS = REPO / ".config" / "nextest-filters.toml"
T_NEXTEST = REPO / "tools" / "ci" / "nextest.sh"
T_TOML = REPO / "t.toml"
sys.path.insert(0, str(REPO / "tools" / "ci"))
import package_features  # noqa: E402


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


# What a `[[cargo]]` family tests, as the nested-sandbox check reads it.
families = load("nested_sandboxes_test", Path(__file__).with_name("test_nested_sandboxes.py"))

COST_TABLE = REPO / "apps" / "tilolang" / "tests" / "hosted" / "cost.rs"
# The test binary and module path the file is compiled into: the crate's tests are modules of one
# `tests/it.rs`, so a test's name carries every module from there down.
COST_TABLE_MODULE = "hosted::cost::"


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
        self.assertIn(f"test(={COST_TABLE_MODULE}{test.group(1)})", printing[0])

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

    def test_a_family_gets_its_test_features_from_unify_or_names_them_in_its_args(self) -> None:
        # t resolves a cargo family's unit graph from the family's own command, and a `--features`
        # there is a demand workspace-unify's manifest does not make: a family with `unify` gets
        # its packages' test features through that manifest alone. A family without one is built
        # whole, so its args carry its packages' test features or the tests build without them.
        families = tomllib.loads(T_TOML.read_text(encoding="utf-8"))["cargo"]
        unified = 0
        for family in families:
            if "tools/ci/nextest.sh" not in family.get("wrapper", []):
                continue
            args = family["args"]
            given = [args[i + 1] for i, arg in enumerate(args) if arg in ("--features", "-F")]
            if "unify" in family:
                unified += 1
                with self.subTest(family=family["name"]):
                    self.assertEqual(given, [], "a unified family's features come through the manifest")
                continue
            with self.subTest(family=family["name"]):
                self.assertIn("packages", family, "a family without `unify` names its packages")
                expected = package_features.arguments(family["packages"])
                self.assertEqual(sorted(",".join(given).split(",")) if given else [],
                                 sorted(expected[1].split(",")) if expected else [])
        self.assertGreater(unified, 0, "the unified families this holds are read")


PACKAGE = re.compile(r"package\(\s*([^)\s]+)\s*\)")


class SetupHandedProgramTests(unittest.TestCase):
    """A plain program a nextest setup builds is handed to the shards that need it.

    A setup builds another package's program with tools/build-sibling-binaries.sh, which builds
    only what the run was not handed: t's family build builds and names what t.toml's `starts` give
    the tested package. So a `package:bin` a setup names belongs in the `starts` of every package
    whose tests run that setup, or each shard of them compiles the program again.
    """

    # What `starts` cannot name yet, so every shard that needs it still builds it: a program its
    # package only builds with features (`starts` names `package:bin`). Naming the list keeps a
    # plain program from joining it by accident, and makes leaving one here a decision.
    BUILT_WITH_FEATURES = {"tilo-relay:tilo-relay=tilo-relay/agents"}

    def config(self) -> dict:
        return tomllib.loads(NEXTEST.read_text(encoding="utf-8"))

    def setups(self) -> dict[str, list[str]]:
        """setup name -> the PACKAGE:BIN entries it builds, for the setups that build programs."""
        builds = {}
        for name, script in self.config().get("scripts", {}).get("setup", {}).items():
            command = script["command"]
            if len(command) > 2 and command[1].endswith("tools/build-sibling-binaries.sh"):
                builds[name] = command[2:]
        return builds

    def packages_running(self, setup: str) -> set[str]:
        """The packages whose tests run `setup`, as the rules that name it say."""
        packages = set()
        for profile in ("default", "ci"):
            for rule in self.config().get("profile", {}).get(profile, {}).get("scripts", []):
                if rule.get("setup") == setup:
                    packages |= set(PACKAGE.findall(rule["filter"]))
        return packages

    def families_testing(self, package: str) -> list[dict]:
        members = families.workspace_packages()
        found = []
        for family in tomllib.loads(T_TOML.read_text(encoding="utf-8")).get("cargo", []):
            tested = families.tested(family, members)
            if tested and package in tested:
                found.append(family)
        return found

    def test_a_plain_program_a_setup_builds_is_in_the_starts_of_the_packages_that_run_it(self):
        setups = self.setups()
        self.assertIn("session-siblings", setups, "the setups that build programs are read")
        checked = 0
        for name, entries in setups.items():
            for entry in entries:
                if "=" in entry:
                    continue
                for tested in sorted(self.packages_running(name)):
                    with self.subTest(setup=name, program=entry, tested=tested):
                        testing = self.families_testing(tested)
                        self.assertTrue(testing, f"{tested} is a package a family tests")
                        for family in testing:
                            self.assertIn(
                                entry,
                                family.get("starts", {}).get(tested, []),
                                f"t.toml's {family['name']} hands {tested} no {entry}, so every "
                                "shard of it builds the program again",
                            )
                        checked += 1
        self.assertGreater(checked, 0, "a plain program a setup builds is checked")

    def test_only_a_program_built_with_features_is_left_to_the_shards(self):
        built_with_features = {
            entry for entries in self.setups().values() for entry in entries if "=" in entry
        }
        self.assertEqual(built_with_features, self.BUILT_WITH_FEATURES)


class WrapperHandedProgramTests(unittest.TestCase):
    """tools/ci/nextest.sh builds only a program the run was not handed.

    t builds the programs a package's tests start once in the family's build (t.toml's `starts`)
    and names each to every shard as NEXTEST_BIN_EXE_<bin>. A shard that built them again placed
    the program's ~1,000 compiled units for itself; a run handed none (a developer's) builds it.
    """

    # The run's cargo argv, as the fake cargo records it: everything after `cargo`.
    RUN = "nextest run --locked --profile ci -E all() --test it --no-fail-fast"
    # Package whose shards run the wrapper -> the program they need and the build a run handed
    # none makes.
    PROGRAMS = {
        "tilo-agent-participant": (
            "tilo-video-render",
            "build --locked -p tilo-video-render --bin tilo-video-render",
        ),
        "app-host": ("tilo-install", "build --locked -p install --bin tilo-install"),
        "app-cli": ("tilo-install", "build --locked -p install --bin tilo-install"),
    }

    def run_wrapper(self, packages: str, handed: tuple[str, ...] = (), broken: str = ""):
        """A shard of PACKAGES as t starts one, through the production wrapper: the result, the
        cargo invocations it made, the TILO_VIDEO_RENDER each run was given, and where the handed
        programs are."""
        with tempfile.TemporaryDirectory() as work:
            tree, programs = Path(work) / "tree", Path(work) / "bin"
            (tree / "tools" / "ci").mkdir(parents=True)
            programs.mkdir()
            shutil.copy(T_NEXTEST, tree / "tools" / "ci" / "nextest.sh")
            argv, video = Path(work) / "argv", Path(work) / "video"
            (programs / "cargo").write_text(
                '#!/bin/sh\nprintf "%s\\n" "$*" >>"$GIVEN"\n'
                'printf "%s\\n" "${TILO_VIDEO_RENDER:-}" >>"$VIDEO"\n'
            )
            (programs / "cargo").chmod(0o755)
            env = {"PATH": f"{programs}{os.pathsep}{os.environ.get('PATH', '/usr/bin:/bin')}",
                   "T_NEXTEST_PACKAGES": packages, "GIVEN": str(argv), "VIDEO": str(video)}
            paths = {}
            for name in handed:
                path = Path(work) / name
                path.write_text("#!/bin/sh\n")
                path.chmod(0o755)
                paths[name] = str(path)
                env[f"NEXTEST_BIN_EXE_{name}"] = str(path)
            if broken:
                path = Path(work) / "not-executable"
                path.write_text("")
                env[f"NEXTEST_BIN_EXE_{broken}"] = str(path)
            command = ["cargo", "nextest", "run", "--locked", "--profile", "ci",
                       "-E", "all()", "--test", "it"]
            result = subprocess.run(["bash", str(tree / "tools" / "ci" / "nextest.sh"), *command],
                                    env=env, capture_output=True, text=True, check=False)
            ran = argv.read_text().splitlines() if argv.exists() else []
            named = video.read_text().splitlines() if video.exists() else []
            return result, ran, named, paths

    def test_a_handed_program_is_not_built(self):
        for package, (program, _) in self.PROGRAMS.items():
            with self.subTest(package=package, program=program):
                result, ran, _, _ = self.run_wrapper(package, handed=(program,))
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(ran, [self.RUN], "the run, and no build")

    def test_a_handed_render_helper_is_named_to_the_run(self):
        result, _, named, paths = self.run_wrapper("tilo-agent-participant",
                                                  handed=("tilo-video-render",))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(named, [paths["tilo-video-render"]])

    def test_a_run_not_handed_builds_the_program(self):
        # A developer's run has no NEXTEST_BIN_EXE_<bin>: it builds the program, as it always did.
        for package, (_, build) in self.PROGRAMS.items():
            with self.subTest(package=package):
                result, ran, _, _ = self.run_wrapper(package)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(ran, [build, self.RUN])

    def test_a_handed_program_that_cannot_run_fails_instead_of_building(self):
        # Building here would hide a broken handoff and compile the program in every shard again.
        result, ran, _, _ = self.run_wrapper("app-host", broken="tilo-install")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("NEXTEST_BIN_EXE_tilo-install", result.stderr)
        self.assertEqual(ran, [])


if __name__ == "__main__":
    unittest.main()
