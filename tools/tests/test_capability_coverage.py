"""Every test group of .config/nextest-capabilities.toml runs somewhere its probe passes, on main's
heads or on stable's.

tools/ci/capability_filter.py leaves a group out of a run whose job lacks the group's capability
and reports its tests as not run; the run still passes. A group whose package is tested only in
jobs that lack the capability therefore runs nowhere, and `t main` has nothing to show for it.
So each group is run by a t.toml cargo family whose isolation for its package gives the probe
what it needs, or names in `checks` the t.toml checks that run it, and those checks run on main
or stable with an isolation that gives the probe what it needs.
"""
from __future__ import annotations

from fnmatch import fnmatchcase
import importlib.util
from pathlib import Path
import re
import tomllib
import unittest


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


TOOLS = Path(__file__).parents[1]
capability = load("capability_for_coverage", TOOLS / "ci/capability_filter.py")
families = load("nested_sandboxes_for_coverage", TOOLS / "tests/test_nested_sandboxes.py")

# The probes (tilo_test_env::PROBES) each of t's isolation levels passes, as t's README describes
# them and the probe reports them there:
#   strict: the job sandbox's root without capabilities, refused every new namespace and mount;
#   nested: uid 1000 that may create user, mount and network namespaces of its own, with no
#           /dev/net/tun and no capability in the namespace it starts in, so neither a bare
#           `unshare --mount` nor a tunnel;
#   vm:     a micro-VM guest's own root, with its capabilities and a kernel of its own.
PASSES = {
    "strict": set(),
    "nested": {"user-namespace", "bubblewrap", "bubblewrap-with-proc", "bubblewrap-overlay", "non-root-identity"},
    "vm": {
        "user-namespace", "mount-namespace", "bubblewrap", "bubblewrap-with-proc",
        "bubblewrap-overlay", "network-tunnel", "root-identity",
    },
}


def on_main(entry: dict) -> bool:
    """Whether t runs a check or family by itself at every head of main or of stable: a default
    one (no `on`), one that says `default = true`, or one whose `on` names main or stable, the
    branch a release is stabilized on before it ships. An `idle` one alone runs only when t has
    room, which leaves most heads without its verdict and no release waiting for one."""
    if "on" not in entry:
        return entry.get("default", True)
    return entry.get("default", False) or "main" in entry["on"] or "stable" in entry["on"]


def family_packages(family: dict, members: set[str]) -> set[str]:
    """The packages a family runs tests of. A filter that only leaves tests out (`not (...)`)
    leaves its packages tested; one that selects tests is read as testing none of them here."""
    args = family.get("args", [])
    for flag in ("-E", "--filterset", "--filter-expr"):
        if flag in args:
            expression = args[args.index(flag) + 1].strip()
            if not expression.startswith("not "):
                return set()
            family = {**family, "args": [a for a in args if a not in (flag, args[args.index(flag) + 1])]}
    return families.tested(family, members) or set()


def isolation_of(family: dict, package: str) -> str:
    isolation = family.get("isolation", "strict")
    if isinstance(isolation, str):
        return isolation
    for level, packages in isolation.items():
        if any(fnmatchcase(package, pattern) for pattern in packages):
            return level
    return "strict"


def names(text: str, package: str) -> bool:
    return re.search(rf"(?<![\w-]){re.escape(package)}(?![\w-])", text) is not None


class Coverage:
    def __init__(self, manifest: dict, config: dict, members: set[str]):
        self.manifest = manifest
        self.families = config.get("cargo", [])
        self.checks = {check["name"]: check for check in config.get("check", [])}
        self.members = members

    def runs(self, check: dict) -> str:
        """What a check runs and what the checks it needs run: a VM step's tests are often
        built by a check it needs."""
        return "\n".join(
            [check.get("run", "")] + [self.checks[need].get("run", "") for need in check.get("needs", []) if need in self.checks]
        )

    def passes(self, check: dict) -> set[str]:
        """The probes a check's tests pass: its isolation's, and an ordinary account's where a
        micro-VM step's root switches its tests to one (`setpriv --reuid`)."""
        isolation = check.get("isolation", "strict")
        switches = isolation == "vm" and "setpriv --reuid=" in check.get("run", "")
        return PASSES[isolation] | ({"non-root-identity"} if switches else set())

    def problems(self) -> list[str]:
        found = []
        for name, entry in self.manifest.items():
            for group in entry.get("tests", []):
                probe = group["probe"]
                for check_name in group.get("checks", []):
                    check = self.checks.get(check_name)
                    if check is None:
                        found.append(f"{name} ({probe}) names check {check_name!r}, which t.toml does not have")
                        continue
                    isolation = check.get("isolation", "strict")
                    if probe not in self.passes(check):
                        found.append(f"{name} ({probe}) names check {check_name!r}, whose {isolation} isolation fails the probe")
                    if not on_main(check):
                        found.append(f"{name} ({probe}) names check {check_name!r}, which t runs at no head of main or stable (`on = {check['on']}`)")
                for package in sorted(set(capability.PACKAGE.findall(group["filter"]))):
                    in_family = any(
                        on_main(family)
                        and package in family_packages(family, self.members)
                        and probe in PASSES[isolation_of(family, package)]
                        for family in self.families
                    )
                    in_check = any(
                        check_name in self.checks and names(self.runs(self.checks[check_name]), package)
                        for check_name in group.get("checks", [])
                    )
                    if not (in_family or in_check):
                        found.append(
                            f"{name} ({probe}): no family or check runs {package}'s tests where the probe passes"
                        )
        return found


def coverage() -> Coverage:
    config = tomllib.loads(families.T_CONFIG.read_text(encoding="utf-8"))
    return Coverage(capability.manifest(), config, families.workspace_packages())


class CapabilityCoverage(unittest.TestCase):
    def test_every_group_runs_on_main_where_its_probe_passes(self) -> None:
        self.assertEqual(coverage().problems(), [])

    def test_every_probe_a_group_names_has_a_level_that_passes_it(self) -> None:
        probes = {group["probe"] for group in capability.groups(capability.manifest())}
        self.assertEqual(sorted(probes - set().union(*PASSES.values())), [])

    def test_a_group_tested_only_where_its_probe_fails_is_named(self) -> None:
        manifest = {"UserNamespace": {"tests": [{"probe": "user-namespace", "filter": "package(alpha) and test(/x::/)"}]}}
        config = {"cargo": [{"name": "test", "tool": "nextest", "args": ["--tests"]}]}
        self.assertEqual(
            Coverage(manifest, config, {"alpha"}).problems(),
            ["UserNamespace (user-namespace): no family or check runs alpha's tests where the probe passes"],
        )

    def test_a_nested_family_runs_a_user_namespace_group(self) -> None:
        manifest = {"UserNamespace": {"tests": [{"probe": "user-namespace", "filter": "package(alpha)"}]}}
        config = {"cargo": [{"name": "test", "tool": "nextest", "args": ["--tests", "-E", "not (package(beta))"],
                             "packages": ["alpha"], "isolation": {"nested": ["alpha"]}}]}
        self.assertEqual(Coverage(manifest, config, {"alpha"}).problems(), [])

    def test_a_vm_check_only_named_runs_nowhere_on_main(self) -> None:
        manifest = {"Root": {"tests": [{"probe": "root-identity", "filter": "package(alpha)", "checks": ["alpha-root"]}]}}
        config = {
            "cargo": [{"name": "test", "tool": "nextest", "args": ["--tests"]}],
            "check": [
                {"name": "alpha-root-build", "run": "cargo test -p alpha --no-run"},
                {"name": "alpha-root", "isolation": "vm", "needs": ["alpha-root-build"], "on": ["dispatch"], "run": "x"},
            ],
        }
        self.assertEqual(
            Coverage(manifest, config, {"alpha"}).problems(),
            ["Root (root-identity) names check 'alpha-root', which t runs at no head of main or stable (`on = ['dispatch']`)"],
        )
        config["check"][1]["on"] = ["dispatch", "idle"]
        self.assertEqual(
            Coverage(manifest, config, {"alpha"}).problems(),
            ["Root (root-identity) names check 'alpha-root', which t runs at no head of main or stable (`on = ['dispatch', 'idle']`)"],
        )
        config["check"][1]["on"] = ["idle", "stable"]
        self.assertEqual(Coverage(manifest, config, {"alpha"}).problems(), [])
        del config["check"][1]["on"]
        self.assertEqual(Coverage(manifest, config, {"alpha"}).problems(), [])

    def test_a_check_that_fails_the_probe_does_not_run_the_group(self) -> None:
        manifest = {"Root": {"tests": [{"probe": "root-identity", "filter": "package(alpha)", "checks": ["alpha-root"]}]}}
        config = {"check": [{"name": "alpha-root", "isolation": "nested", "run": "cargo test -p alpha"}]}
        self.assertEqual(
            Coverage(manifest, config, {"alpha"}).problems(),
            ["Root (root-identity) names check 'alpha-root', whose nested isolation fails the probe"],
        )


if __name__ == "__main__":
    unittest.main()
