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
DUMP = "import json, os; print(json.dumps(dict(os.environ)))"
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


CAPABILITIES = REPO / ".config" / "nextest-capabilities.toml"
sys.path.insert(0, str(REPO / "tools" / "policy"))
import rust_source  # noqa: E402

# Where an item, or the `scenario!` that compiles to one test, opens.
ITEM = re.compile(
    r"(?<!\w)#\s*\["
    r"|(?<![\w:])mod\s+[A-Za-z_]\w*\s*[;{]"
    r"|(?<![\w:])(?:tilo_test_env\s*::\s*)?scenario\s*!\s*\{"
)
ITEM_HEAD = re.compile(
    r"\s*(?:pub(?:\s*\([^)]*\))?\s+)?(?:(?:unsafe|async|default|const|extern)\s+)*"
    r"(fn|mod)\s+([A-Za-z_]\w*)"
)
PATH_VALUE = re.compile(r'path\s*=\s*"([^"]*)"')
SCENARIO = re.compile(r"(?:tilo_test_env\s*::\s*)?scenario\s*!\s*\{")


def package_dirs() -> dict[str, Path]:
    """Each workspace package's directory, by package name."""
    directories: dict[str, Path] = {}
    for manifest_path in REPO.rglob("Cargo.toml"):
        relative = manifest_path.relative_to(REPO)
        if any(part in {"target", "vendor", "node_modules"} for part in relative.parts):
            continue
        try:
            manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
        except tomllib.TOMLDecodeError:
            continue
        name = manifest.get("package", {}).get("name")
        if name:
            directories.setdefault(name, manifest_path.parent)
    return directories


def test_target_roots(directory: Path) -> list[Path]:
    """The root file of each test target the package builds: its library, its binaries and its
    integration tests, as Cargo lays them out or the manifest declares them."""
    manifest = tomllib.loads((directory / "Cargo.toml").read_text(encoding="utf-8"))
    package = manifest.get("package", {})
    roots = [directory / manifest.get("lib", {}).get("path", "src/lib.rs")]
    declared = manifest.get("bin", [])
    roots += [directory / binary.get("path", f"src/bin/{binary['name']}.rs") for binary in declared]
    if not any(binary.get("path") == "src/main.rs" for binary in declared):
        roots.append(directory / "src" / "main.rs")
    roots += sorted((directory / "src" / "bin").glob("*.rs"))
    if package.get("autotests", True) is not False:
        roots += sorted((directory / "tests").glob("*.rs"))
    roots += [directory / target.get("path", f"tests/{target['name']}.rs")
              for target in manifest.get("test", [])]
    return [root for root in roots if root.is_file()]


def test_names(directory: Path) -> set[str]:
    """Every test name the package's test targets report, its module path included."""
    names: set[str] = set()
    seen: set[Path] = set()
    for root in test_target_roots(directory):
        walk_file(root, (), names, seen)
    return names


def walk_file(path: Path, prefix: tuple[str, ...], names: set[str], seen: set[Path]) -> None:
    path = path.resolve()
    if path in seen or not path.is_file():
        return
    seen.add(path)
    text = path.read_text(encoding="utf-8", errors="replace")
    walk_module(text, rust_source.code_only(text), 0, len(text), prefix, path, names, seen)


def walk_module(
    text: str, code: str, start: int, end: int, prefix: tuple[str, ...], path: Path,
    names: set[str], seen: set[Path],
) -> None:
    """Collect one module's tests: an inline module's under its name, a declared module's from
    its file under its name, a `fn` with a test attribute, and a `scenario!` test."""
    at = start
    while found := ITEM.search(code, at, end):
        open_at = found.start()
        if code[open_at] == "#":
            attributes, item_start = attributes_at(text, code, open_at)
        else:
            attributes, item_start = [], open_at
        item_end = rust_source.item_extent(code, open_at)[1]
        scenario = SCENARIO.match(code, item_start)
        head = ITEM_HEAD.match(code, item_start)
        if scenario:
            # A `scenario!` test: the `fn` it opens with, after any attribute on it, is the
            # name it reports.
            name = scenario_fn(code, scenario.end(), end)
            if name:
                names.add("::".join((*prefix, name)))
        elif head and head.group(1) == "fn":
            if any(is_test_attribute(body) for body in attributes):
                names.add("::".join((*prefix, head.group(2))))
        elif head and head.group(1) == "mod":
            brace = code.find("{", head.end(), item_end)
            if brace < 0 or code.endswith(";", 0, brace):
                declared_module(text, path, head.group(2), attributes, (*prefix, head.group(2)),
                                names, seen)
            else:
                walk_module(text, code, brace + 1, rust_source.matching(code, brace) - 1,
                            (*prefix, head.group(2)), path, names, seen)
        at = max(item_end, found.end())


def scenario_fn(code: str, start: int, end: int) -> str | None:
    """The name of the `fn` item a `scenario!` opens with, past the attributes on it."""
    at = start
    while at < end and code[at].isspace():
        at += 1
    while attribute := rust_source.OUTER_ATTRIBUTE.match(code, at, end):
        at = rust_source.matching(code, attribute.end() - 1)
        while at < end and code[at].isspace():
            at += 1
    found = re.match(r"fn\s+([A-Za-z_]\w*)\s*;", code[at:end])
    return found.group(1) if found else None


def is_test_attribute(body: str) -> bool:
    """Whether an attribute body is a test: `test`, `tokio::test(flavor = …)`, or a `cfg` that
    holds only where tests run."""
    stripped = re.sub(r"\s*\(.*\)\s*$", "", body.strip(), flags=re.DOTALL)
    if rust_source.TEST_ITEM_ATTRIBUTE.fullmatch(stripped):
        return True
    return rust_source.attribute_is_test(body)


def declared_module(
    text: str, path: Path, name: str, attributes: list[str], prefix: tuple[str, ...],
    names: set[str], seen: set[Path],
) -> None:
    """Walk the file a `mod name;` declaration names: its `#[path]`, or Cargo's layout."""
    for attribute in attributes:
        value = PATH_VALUE.search(attribute)
        if value:
            walk_file(path.parent / value.group(1), prefix, names, seen)
            return
    children = rust_source.module_children(path)
    for candidate in (children / f"{name}.rs", children / name / "mod.rs"):
        if candidate.is_file():
            walk_file(candidate, prefix, names, seen)
            return


def attributes_at(text: str, code: str, position: int) -> tuple[list[str], int]:
    """The attributes at `position` (their bodies read from `text`, so a `#[path]`'s literal
    survives), and where the item they guard starts."""
    bodies: list[str] = []
    at = position
    while attribute := rust_source.OUTER_ATTRIBUTE.match(code, at):
        close = rust_source.matching(code, attribute.end() - 1)
        bodies.append(text[attribute.end() : close - 1])
        at = close
        while at < len(code) and code[at].isspace():
            at += 1
    return bodies, at


def top_level(expression: str, keyword: str) -> list[str]:
    """`expression` split where `keyword` stands outside every parenthesis."""
    parts, depth, start, index = [], 0, 0, 0
    while index < len(expression):
        char = expression[index]
        if char in "([":
            depth += 1
        elif char in ")]":
            depth -= 1
        elif (depth == 0 and expression.startswith(keyword, index)
              and not (expression[index - 1].isalnum() or expression[index - 1] == "_")
              and (index + len(keyword) >= len(expression)
                   or not (expression[index + len(keyword)].isalnum()
                           or expression[index + len(keyword)] == "_"))):
            parts.append(expression[start:index])
            start = index + len(keyword)
            index += len(keyword) - 1
        index += 1
    parts.append(expression[start:])
    return [part.strip() for part in parts if part.strip()]


def exact_names_of(expression: str, packages: tuple[str, ...] = ()) -> list[tuple[str, str]]:
    """Each `package(name)` and `test(=NAME)` a filter pairs, as `(package, test name)`."""
    disjoined = top_level(expression, "or")
    if len(disjoined) > 1:
        return [pair for part in disjoined for pair in exact_names_of(part, packages)]
    conjoint = top_level(expression, "and")
    if len(conjoint) > 1:
        found: list[tuple[str, str]] = []
        carried = packages
        for part in conjoint:
            here = tuple(PACKAGE.findall(part))
            if here:
                carried = here
            found += exact_names_of(part, carried)
        return found
    body = expression.strip()
    if body.startswith("(") and rust_source.matching(body, 0) == len(body):
        return exact_names_of(body[1:-1], packages)
    here = tuple(PACKAGE.findall(expression)) or packages
    return [(package, name) for package in here for name in TEST_EXACT.findall(expression)]


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
        self.assertEqual(names, ["GOLDEN_FILTER", "PUSH_ONLY_FILTER", "FLOWS_FILTER"])

    def test_a_run_leaves_out_the_tests_whose_capability_this_machine_lacks(self) -> None:
        # The ci profile's setup promises every mapped capability, so a test of
        # one must be left out where the probe says this machine has it not.
        # The wrapper asks, and the run has its answer wherever it is started:
        # a workflow's shard and a job t shards alike.
        text = T_NEXTEST.read_text(encoding="utf-8")
        self.assertIn("tools/ci/capability_filter.py", text)
        # Into the filter the run was given, not beside it: nextest takes one.
        self.assertIn('(${arguments[index + 1]}) & ($left_out)', text)

    def test_every_capability_names_a_test_that_exists(self) -> None:
        # nextest reads `test(=NAME)` as the whole test name, the module path
        # from the test target's root down included: an abbreviated name
        # matches nothing, and the ci profile's promise of the capability makes
        # each such test fail where the capability is missing rather than be
        # left out. So every name is resolved against the package's sources.
        # A test its target's features hide is out of a default listing too;
        # the sources hold it either way.
        manifest = tomllib.loads(CAPABILITIES.read_text(encoding="utf-8"))["capabilities"]
        directories = package_dirs()
        known: dict[str, set[str]] = {}
        named = 0
        for capability, groups in manifest.items():
            for group in groups["tests"]:
                for package, name in exact_names_of(group["filter"]):
                    named += 1
                    with self.subTest(capability=capability, package=package, name=name):
                        self.assertIn(package, directories, "a package the tree has")
                        if package not in known:
                            known[package] = test_names(directories[package])
                        self.assertIn(name, known[package],
                                      f"{package} has no test named that")
        self.assertGreater(named, 0, "the manifest's exact names are read")

    def test_every_composed_filter_is_defined_by_the_shared_file(self) -> None:
        # A removed key must not leave a `$..._FILTER` reference behind:
        # nextest parses the composed expression, and an empty `not ()` is a
        # missing expression (exit 94) before any test is selected.
        defined = {
            f"{key.upper()}_FILTER"
            for key in tomllib.loads(FILTERS.read_text(encoding="utf-8"))
        }
        consumers = [
            REPO / "tools" / "ci" / "local_affected.py",
            T_NEXTEST,
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
# A filter's test terms: the part of a test's name `test(term)` asks for, and the whole name
# `test(=name)` asks for.
TEST_TERM = re.compile(r"\btest\(\s*([^=)][^)]*)\)")
TEST_EXACT = re.compile(r"\btest\(\s*=\s*([^)\s]+)\s*\)")


def started(entry: str) -> tuple[str, str, tuple[str, ...]]:
    """One artifact, as `(package, target, features)`.

    tools/build-sibling-binaries.sh names one `package:bin`, or `package:bin=package/feature`;
    t.toml's `starts` names it as t reads it, `package:name`, `package:kind:name`, either with
    `@profile` (the cargo profile it is built with, in an invocation of its own) and either with
    `=feature,feature`. One artifact, whichever way a line spells it: the profile says how it is
    built, and the tests are handed it under the same name.
    """
    target, _, features = entry.partition("=")
    fields = target.partition("@")[0].split(":")
    named = {feature.partition("/")[2] or feature for feature in features.split(",") if feature}
    return fields[0], fields[-1], tuple(sorted(named))


class SetupHandedProgramTests(unittest.TestCase):
    """A program a nextest setup builds is handed to the shards that need it.

    A setup builds another package's program with tools/build-sibling-binaries.sh, which builds
    only what the run was not handed: t's family build builds and names what t.toml's `starts` give
    the tested package. So every artifact a setup names belongs in the `starts` of every package
    whose tests run that setup, or each shard of them compiles the program again.

    Nextest runs a setup only for a test its rule's filter enables it for, and only where the run
    holds such a test. A rule that names a test module is owed by the families that run a test of
    it, not by every family that tests the package.
    """

    HANDED = "tools/build-sibling-binaries.sh"
    # What a setup builds that `starts` cannot name, so every shard that needs it still makes it:
    # the browser runtime's wasm page bundle, made by a tool of tilo's and handed as
    # TILO_PAGE_BUNDLE. Empty would say every program a setup builds is a cargo artifact `starts`
    # hands; naming one makes leaving another here a decision.
    NOT_CARGO_ARTIFACTS = {"page-bundle": "tools/build-page-bundle.sh"}

    def config(self) -> dict:
        return tomllib.loads(NEXTEST.read_text(encoding="utf-8"))

    def setups(self) -> dict[str, list[str]]:
        """setup name -> the PACKAGE:BIN entries it builds, for the setups that build programs."""
        builds = {}
        for name, script in self.config().get("scripts", {}).get("setup", {}).items():
            command = script["command"]
            if len(command) > 2 and command[1].endswith(self.HANDED):
                builds[name] = command[2:]
        return builds

    def rules_running(self, setup: str) -> list[tuple[str, list[str] | None]]:
        """(package, test terms) for every rule that runs `setup`.

        `terms` is None where a rule names its tests in a way this check does not read (a regex),
        which leaves every test of the package running the setup.
        """
        rules = []
        for profile in ("default", "ci"):
            for rule in self.config().get("profile", {}).get(profile, {}).get("scripts", []):
                if rule.get("setup") != setup:
                    continue
                terms = TEST_TERM.findall(rule["filter"])
                if any(term.startswith("/") for term in terms):
                    terms = None
                for package in PACKAGE.findall(rule["filter"]):
                    rules.append((package, terms))
        return rules

    @staticmethod
    def exact_names(family: dict) -> list[str] | None:
        """The tests a family's run picks by exact name, or None where its selection is not one
        this check reads.

        A `-E` that asks for whole names (`test(=name)`) and says no `not` runs those heard
        alone. A
        selection by substring, by regex, under `not`, or none at all (the package's every test)
        leaves open which tests the run holds.
        """
        arguments = family.get("args", [])
        expression = next((arguments[index + 1] for index, argument in enumerate(arguments)
                           if argument in ("-E", "--filter-expr", "--filterset")), None)
        if expression is None:
            return None
        if re.search(r"\bnot\b", expression) or re.search(r"\btest\(\s*[^=]", expression):
            return None
        return TEST_EXACT.findall(expression) or None

    @classmethod
    def runs_any(cls, family: dict, terms: list[str] | None) -> bool:
        """Whether `family`'s run holds a test a rule naming `terms` enables the setup for."""
        if not terms:
            return True
        names = cls.exact_names(family)
        if names is None:
            return True
        return any(term in name for name in names for term in terms)

    @staticmethod
    def packages_tested(family: dict, members: set[str]) -> set[str] | None:
        """The packages `family` tests, as this test reads a family.

        As the nested-sandbox check reads one, and beside that the packages a family names
        although its arguments hold an `-E`: it tests them, whatever part of their tests the
        `-E` leaves to another family (the per-arch family leaves tilolang-tilo's budget
        checks to test-bench), so it owes them the same programs. An `-E` over a family that
        names no packages says itself which they are, and that is still not read: `None`.
        """
        tested = families.tested(family, members)
        if tested is None and family.get("tool") == "nextest" and "packages" in family:
            tested = set(family["packages"])
        return tested

    def families_testing(self, package: str) -> list[dict]:
        members = families.workspace_packages()
        found = []
        for family in tomllib.loads(T_TOML.read_text(encoding="utf-8")).get("cargo", []):
            tested = self.packages_tested(family, members)
            if tested and package in tested:
                found.append(family)
        return found

    def test_every_program_a_setup_builds_is_in_the_starts_of_the_packages_that_run_it(self):
        setups = self.setups()
        self.assertIn("session-siblings", setups, "the setups that build programs are read")
        checked = 0
        for name, entries in setups.items():
            for tested, terms in self.rules_running(name):
                testing = self.families_testing(tested)
                self.assertTrue(testing, f"{tested} is a package a family tests")
                for family in testing:
                    # Only the tests the rule asks for enable the setup, and only where the run
                    # holds one: a family that picks another test of the package owes nothing.
                    if not self.runs_any(family, terms):
                        continue
                    handed = {
                        started(entry) for entry in family.get("starts", {}).get(tested, [])
                    }
                    for entry in entries:
                        # The package's own plain bin is nextest's to build for its tests
                        # (NEXTEST_BIN_EXE_<bin>), and t refuses it as a `starts` entry: the
                        # family's own build already made it.
                        package, _, features = started(entry)
                        if package == tested and not features:
                            continue
                        with self.subTest(setup=name, program=entry, tested=tested,
                                          family=family["name"]):
                            self.assertIn(
                                started(entry),
                                handed,
                                f"t.toml's {family['name']} hands {tested} no {entry}, so every "
                                "shard of it builds the program again",
                            )
                            checked += 1
        self.assertGreater(checked, 0, "a program a setup builds is checked")

    def test_a_family_that_picks_its_tests_by_name_owes_the_setups_of_their_modules(self):
        # The throughput family's shape: one test by whole name, whose module a setup of another
        # package's tests does not serve.
        family = {"name": "throughput", "tool": "nextest", "packages": ["first"],
                  "args": ["--tests", "--run-ignored", "only", "-E",
                           "test(=workspace_actions::throughput::a_workspace_is_measured)"]}
        self.assertEqual(self.exact_names(family),
                         ["workspace_actions::throughput::a_workspace_is_measured"])
        self.assertFalse(self.runs_any(family, ["hosted_coding_agents::"]))
        self.assertTrue(self.runs_any(family, ["workspace_actions::throughput::"]))

    def test_a_family_whose_selection_is_not_read_owes_every_setup(self):
        # A substring term, a regex or a `not` leaves open which tests run.
        for expression in ("package(first) & test(workspace_actions::)",
                           "not (package(first) & test(=one))",
                           "test(/^throughput::/)"):
            family = {"name": "some", "tool": "nextest", "args": ["-E", expression]}
            with self.subTest(expression=expression):
                self.assertIsNone(self.exact_names(family))
                self.assertTrue(self.runs_any(family, ["hosted_coding_agents::"]))
        self.assertTrue(self.runs_any({"name": "whole"}, ["hosted_coding_agents::"]),
                        "a family running a package whole owes every module's setup")

    def test_a_family_that_names_its_packages_is_read_whatever_its_filter_leaves_out(self):
        # The per-arch family's shape: its packages by name, and an `-E` that leaves some of
        # one package's tests to another family.
        members = {"first", "second", "third"}
        family = {"name": "test-{arch}", "tool": "nextest", "packages": ["first", "second"],
                  "args": ["--locked", "--tests", "-E", "not (package(second) & binary(budgets))"]}
        self.assertIsNone(families.tested(family, members),
                          "the nested-sandbox check reads no family with an -E")
        self.assertEqual(self.packages_tested(family, members), {"first", "second"})

    def test_a_family_whose_filter_alone_says_its_packages_answers_nothing(self):
        members = {"first", "second", "third"}
        family = {"name": "test-some", "tool": "nextest",
                  "args": ["--locked", "--tests", "-E", "package(first)"]}
        self.assertIsNone(self.packages_tested(family, members))

    def test_a_family_declares_one_build_of_each_target(self):
        # tools/build-sibling-binaries.sh reads a bin's hand as NEXTEST_BIN_EXE_<bin>, which names
        # a file and not the features it was built with. t hands one declaration per target per
        # family (it refuses two), so the tree must keep it: a plain `tilo-server` declared beside
        # the featured one would be two builds of one file's name, and the hand could be either.
        for family in tomllib.loads(T_TOML.read_text(encoding="utf-8")).get("cargo", []):
            declared: dict[tuple[str, str], set[tuple[str, ...]]] = {}
            for entries in family.get("starts", {}).values():
                for entry in entries:
                    package, target, features = started(entry)
                    declared.setdefault((package, target), set()).add(features)
            for target, feature_sets in declared.items():
                with self.subTest(family=family["name"], target=target):
                    self.assertLessEqual(len(feature_sets), 1, feature_sets)

    def test_the_only_program_a_setup_builds_that_starts_cannot_name_is_the_page_bundle(self):
        # Every setup's command runs one tool of three: this one (cargo artifacts `starts`
        # hands), tools/test-env-setup.sh (the environment, no program), and
        # tools/build-page-bundle.sh (the wasm bundle, no cargo artifact). A setup that runs
        # another builds something nothing hands, which must be a decision taken here.
        building = {}
        for name, script in self.config().get("scripts", {}).get("setup", {}).items():
            tool = next((word for word in script["command"][1:] if word.endswith(".sh")), "")
            if tool.endswith(self.HANDED) or tool.endswith("tools/test-env-setup.sh"):
                continue
            building[name] = tool
        self.assertEqual(building, self.NOT_CARGO_ARTIFACTS)


class WrapperHandedProgramTests(unittest.TestCase):
    """tools/ci/nextest.sh builds only what the run was not handed.

    t builds a program a package's tests need once in the family's build (t.toml's `starts`) and
    names each to every shard as NEXTEST_BIN_EXE_<bin>. A shard that built them again placed the
    artifact's ~1,000 compiled units for itself; a run handed none (a developer's) builds it.
    The values a package's tests need as variables (a render helper's path, the PAM module, the
    PDF sandbox, the TPM) are the ci profile's setup steps instead, so the job's environment
    carries none of them into a build (tools/tests/test_test_environment.py).
    """

    # The run's cargo argv, as the fake cargo records it: everything after `cargo`.
    RUN = "nextest run --locked --profile ci -E all() --test it --no-fail-fast"
    # Each program a package's tests need, and the build a shard that was handed none makes.
    BUILDS = {
        "tilo-install": "build --locked -p install --bin tilo-install",
        "tilolang-process": "build --locked -p tilolang-process --bin tilolang-process",
    }
    # Package whose shards go through the wrapper -> the programs they need, built in this order.
    PROGRAMS = {
        "tilo-agent-session": ("tilolang-process",),
        "app-cli": ("tilo-install",),
    }

    def run_wrapper(self, packages: str, handed: tuple[str, ...] = (), broken: str = "",
                    command: tuple[str, ...] = ()):
        """A shard of PACKAGES as t starts one, through the production wrapper: the result, the
        cargo invocations it made, and the paths of the artifacts handed to it. COMMAND is the
        wrapped command, the shard's nextest run by default. The tree's tools/test-env.sh records
        that it ran, as a line `test-env` among the invocations."""
        with tempfile.TemporaryDirectory() as work:
            tree, programs = Path(work) / "tree", Path(work) / "bin"
            (tree / "tools" / "ci").mkdir(parents=True)
            programs.mkdir()
            shutil.copy(T_NEXTEST, tree / "tools" / "ci" / "nextest.sh")
            shutil.copy(T_NEXTEST.parent / "handed.sh", tree / "tools" / "ci" / "handed.sh")
            (tree / "tools" / "test-env.sh").write_text(
                '#!/bin/sh\nprintf "test-env\\n" >>"$GIVEN"\nexec "$@"\n'
            )
            argv = Path(work) / "argv"
            (programs / "cargo").write_text(
                '#!/bin/sh\nprintf "%s\\n" "$*" >>"$GIVEN"\n'
            )
            (programs / "cargo").chmod(0o755)
            env = {"PATH": f"{programs}{os.pathsep}{os.environ.get('PATH', '/usr/bin:/bin')}",
                   "T_NEXTEST_PACKAGES": packages, "GIVEN": str(argv)}
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
            command = list(command) or ["cargo", "nextest", "run", "--locked", "--profile", "ci",
                                        "-E", "all()", "--test", "it"]
            result = subprocess.run(["bash", str(tree / "tools" / "ci" / "nextest.sh"), *command],
                                    env=env, capture_output=True, text=True, check=False)
            ran = argv.read_text().splitlines() if argv.exists() else []
            return result, ran, paths

    def test_a_handed_program_is_not_built(self):
        for package, programs in self.PROGRAMS.items():
            with self.subTest(package=package):
                result, ran, _ = self.run_wrapper(package, handed=programs)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(ran, [self.RUN], "the run, and no build")

    def test_a_run_not_handed_builds_the_program(self):
        # A developer's run has no NEXTEST_BIN_EXE_<bin>: it builds the program, as it always did.
        for package, programs in self.PROGRAMS.items():
            with self.subTest(package=package):
                result, ran, _ = self.run_wrapper(package)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(ran, [*(self.BUILDS[program] for program in programs), self.RUN])

    def test_a_family_build_through_the_wrapper_starts_no_test(self):
        # t's build of a family lists its test binaries and then builds the programs their tests
        # start, both through this wrapper. Neither starts a test: the listing builds no program
        # for one, and the programs' build runs as given, outside the test environment, whose
        # steps (the native NTFS images among them) it waited minutes for.
        listing = ("cargo", "nextest", "list", "--list-type", "full", "-p", "tilo-session",
                   "-E", "all()")
        started = ("cargo", "build", "--message-format", "json", "--locked",
                   "-p", "tilolang-process", "--bin", "tilolang-process")
        for command in (listing, started):
            for package in self.PROGRAMS:
                with self.subTest(command=command[1], package=package):
                    result, ran, _ = self.run_wrapper(package, command=command)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(ran, [" ".join(command[1:])], "the command alone")

    def test_a_cargo_test_runs_in_the_test_environment(self):
        # A command that starts tests itself, not through nextest, still gets the environment.
        command = ("cargo", "test", "--locked", "-p", "tilo-agent-session")
        result, ran, _ = self.run_wrapper("tilo-agent-session", handed=("tilolang-process",),
                                          command=command)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(ran, ["test-env", " ".join(command[1:])])

    def test_a_handed_program_that_cannot_run_fails_instead_of_building(self):
        # Building here would hide a broken handoff and compile the program in every shard again.
        result, ran, _ = self.run_wrapper("app-cli", broken="tilo-install")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("NEXTEST_BIN_EXE_tilo-install", result.stderr)
        self.assertEqual(ran, [])

    def test_the_jobs_environment_carries_no_per_package_value(self):
        """What only a package's tests need is the ci profile's setup steps, not the job's
        environment: there, each value keys every build script's and proc macro's cache entry by
        it, so jobs testing different packages would share no build.
        """
        with tempfile.TemporaryDirectory() as work:
            tree, programs = Path(work) / "tree", Path(work) / "bin"
            (tree / "tools" / "ci").mkdir(parents=True)
            programs.mkdir()
            shutil.copy(T_NEXTEST, tree / "tools" / "ci" / "nextest.sh")
            shutil.copy(T_NEXTEST.parent / "handed.sh", tree / "tools" / "ci" / "handed.sh")
            dumped = Path(work) / "env"
            (programs / "cargo").write_text(f'#!/bin/sh\npython3 -c \'{DUMP}\' >"{dumped}"\n')
            (programs / "cargo").chmod(0o755)
            excluded = ("TILO_TEST_NTFS_FIXTURES", "TILO_REQUIRE_SWTPM",
                        "TILO_REQUIRE_PDF_SANDBOX", "TILO_PAM_TILO_MODULE", "TILO_VIDEO_RENDER",
                        # The capabilities every test is promised, and the deep family's count:
                        # the ci profile's setup sets them for the tests, and no family's `env`
                        # (t.toml) may carry one into a build.
                        "TILO_REQUIRE_MEDIA", "TILO_REQUIRE_AUDIO", "TILO_REQUIRE_GPU",
                        "PROPTEST_CASES")
            # Every package whose tests needed one of them.
            for package in ("tilo-ntfs-helper", "tilo-pdf", "tilo-thumbnail",
                            "tilo-agent-session", "pam-tilo", "tilo-auth", "tilo-rollback"):
                with self.subTest(package=package):
                    env = {"PATH": f"{programs}{os.pathsep}{os.environ.get('PATH', '/usr/bin:/bin')}",
                           "T_NEXTEST_PACKAGES": package}
                    result = subprocess.run(
                        ["bash", str(tree / "tools" / "ci" / "nextest.sh"), "cargo", "nextest",
                         "run", "--locked", "--profile", "ci", "-E", "all()", "--test", "it"],
                        env=env, capture_output=True, text=True, check=False)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    cargo_env = json.loads(dumped.read_text())
                    for name in excluded:
                        self.assertNotIn(name, cargo_env)


class SiblingBinarySetupTests(unittest.TestCase):
    """tools/build-sibling-binaries.sh builds only what the run was not handed.

    The setups that start other packages' programs run it before their tests. t hands the programs
    a family's `starts` names (NEXTEST_BIN_EXE_<bin>); a run handed one builds nothing, and a run
    handed none (a developer's) builds it, with the features its entry names.
    """

    # The workspace host, which only `agents` gives `tilo-server` the subcommand the Lounge tests
    # start: the one program a setup builds with features.
    ENTRY = "tilo-server:tilo-server=tilo-server/agents"
    BUILD = ("build --locked --message-format=json-render-diagnostics --package tilo-server "
             "--bin tilo-server --features tilo-server/agents")

    def run_setup(self, hand: str = ""):
        """The Lounge setup as a shard runs it: the result, the cargo invocations it made, the
        variables it handed the run (NEXTEST_ENV's lines), and where the program it builds would
        be. `hand` is whether the run was handed the build (`featured`) or not (empty)."""
        with tempfile.TemporaryDirectory() as work:
            tree, programs = Path(work) / "tree", Path(work) / "bin"
            (tree / "tools").mkdir(parents=True)
            programs.mkdir()
            shutil.copy(REPO / "tools" / "build-sibling-binaries.sh",
                        tree / "tools" / "build-sibling-binaries.sh")
            argv, env_file = Path(work) / "argv", Path(work) / "nextest-env"
            env_file.write_text("")
            built = programs / "built" / "tilo-server"
            built.parent.mkdir()
            built.write_text("#!/bin/sh\nexit 0\n")
            built.chmod(0o755)
            (programs / "cargo").write_text(
                '#!/bin/sh\nprintf "%s\\n" "$*" >>"$GIVEN"\n'
                'printf \'{"reason":"compiler-artifact","executable":"%s"}\\n\' "$BUILT"\n'
            )
            (programs / "cargo").chmod(0o755)
            env = {"PATH": f"{programs}{os.pathsep}{os.environ.get('PATH', '/usr/bin:/bin')}",
                   "GIVEN": str(argv), "BUILT": str(built), "NEXTEST_ENV": str(env_file)}
            if hand:
                path = programs / "handed" / "tilo-server"
                path.parent.mkdir()
                path.write_text("#!/bin/sh\nexit 0\n")
                path.chmod(0o755)
                env["NEXTEST_BIN_EXE_tilo-server"] = str(path)
            result = subprocess.run(
                ["bash", str(tree / "tools" / "build-sibling-binaries.sh"), self.ENTRY],
                env=env, capture_output=True, text=True, check=False)
            ran = argv.read_text().splitlines() if argv.exists() else []
            return result, ran, env_file.read_text().splitlines(), str(built)

    def test_a_handed_featured_program_is_not_built(self):
        result, ran, handed, _ = self.run_setup("featured")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(ran, [], "the setup, and no build")
        self.assertEqual(len(handed), 1, handed)
        self.assertTrue(handed[0].startswith("TILO_SIBLING_BIN_tilo_server="), handed)
        self.assertIn(os.sep + "handed" + os.sep + "tilo-server", handed[0])

    def test_a_run_not_handed_builds_the_program_with_its_features(self):
        result, ran, handed, built = self.run_setup()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(ran, [self.BUILD])
        self.assertEqual(handed, [f"TILO_SIBLING_BIN_tilo_server={built}"])


if __name__ == "__main__":
    unittest.main()
