"""tools/ci/nested-sandboxes.toml names workspace packages, its reader filters by name, and
t.toml's test families give every package it names a nested job."""
from __future__ import annotations

from pathlib import Path
import subprocess
import sys
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
READER = ROOT / "tools/ci/nested_sandboxes.py"
T_CONFIG = ROOT / "t.toml"


def workspace_packages() -> set[str]:
    members = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["members"]
    names = set()
    for pattern in members:
        for manifest in ROOT.glob(f"{pattern}/Cargo.toml"):
            package = tomllib.loads(manifest.read_text(encoding="utf-8")).get("package", {})
            if "name" in package:
                names.add(package["name"])
    return names


def read(*args: str) -> list[str]:
    return subprocess.run([sys.executable, str(READER), *args], check=True,
                          capture_output=True, text=True).stdout.split()


def tested(family: dict, members: set[str]) -> set[str] | None:
    """The packages a `[[cargo]]` family of t.toml tests: the ones it names, or every member but
    the ones it leaves out. `None` for a family whose packages are a nextest filter's to say
    (`-E`), which is not something this test reads."""
    if not family.get("tool") == "nextest" and not any(
        argument in ("--tests", "--lib") for argument in family.get("args", [])
    ):
        # A build with no tests: an `--examples` family runs examples, and a clippy one checks
        # every target without running it. Neither starts a sandbox of its own.
        return None
    if any(argument in ("-E", "--filterset", "--filter-expr") for argument in family.get("args", [])):
        return None
    if "packages" in family:
        return set(family["packages"])
    return members - set(family.get("exclude", []))


class NestedSandboxes(unittest.TestCase):
    def test_every_listed_package_is_a_workspace_member(self) -> None:
        listed = read()
        self.assertTrue(listed)
        self.assertEqual(sorted(set(listed) - workspace_packages()), [])

    def test_names_filter_the_list(self) -> None:
        self.assertEqual(read("-p", "thumbnails", "-p", "collab-types", "ush"), ["thumbnails", "ush"])
        self.assertEqual(read("-p", "collab-types"), [])

    def test_every_test_family_nests_the_packages_it_tests(self) -> None:
        """A package whose tests start a sandbox runs them in a nested job, in every family that
        tests it: the same job sandbox refuses the namespaces bubblewrap asks for, and the tests
        that need one fail there for the sandbox rather than for anything they test."""
        members = workspace_packages()
        nested_packages = set(read())
        families = tomllib.loads(T_CONFIG.read_text(encoding="utf-8")).get("cargo", [])
        self.assertTrue(families, "t.toml declares no cargo families")
        for family in families:
            packages = tested(family, members)
            if packages is None:
                continue
            isolation = family.get("isolation")
            nested = set(isolation.get("nested", [])) if isinstance(isolation, dict) else set()
            self.assertEqual(
                sorted(packages & nested_packages - nested),
                [],
                f"t.toml's {family.get('name')} family tests these packages, whose tests start "
                "sandboxes of their own, without a nested job",
            )


if __name__ == "__main__":
    unittest.main()
