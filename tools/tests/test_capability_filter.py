import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib
import unittest
from unittest import mock


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


TOOLS = Path(__file__).parents[1]
capability = load("capability", TOOLS / "ci/capability_filter.py")
# What a t.toml family tests, as the nested-sandbox check reads it.
families = load("nested_sandboxes_test", TOOLS / "tests/test_nested_sandboxes.py")

FAILED = "bwrap exited with exit status: 1"


def report(**verdicts: str) -> str:
    """The probe report tilo-test-env prints, the probes `verdicts` names failing."""
    capabilities = capability.manifest()
    lines = [
        f"tilo-capability-variable {name} {entry['variable']}"
        for name, entry in capabilities.items()
    ]
    for probe in sorted({group["probe"] for group in capability.groups(capabilities)}):
        lines.append(f"tilo-capability-probe {probe} {verdicts.get(probe, 'available')}")
    return "\n".join(lines) + "\n"


def filter_of(package: str) -> str:
    """The filter of the group whose tests are in `package`."""
    (group,) = capability.groups(capability.manifest(), {package})
    return group["filter"]


class PackageTests(unittest.TestCase):
    def test_a_group_is_found_by_any_package_its_filter_names(self):
        # A group's filter says which packages its tests are in, and a run of
        # one of them is the run that must leave them out.
        found = capability.groups(capability.manifest(), {"ush"})
        self.assertTrue(found)
        self.assertTrue(all("package(ush)" in group["filter"] for group in found))

    def test_a_run_of_no_package_the_manifest_names_selects_nothing(self):
        # The ordinary job probes nothing: a probe it cannot use would cost a
        # build of the probe binary per job.
        self.assertEqual(capability.groups(capability.manifest(), {"no-such-package"}), [])

    def test_without_packages_every_group_is_there(self):
        found = capability.groups(capability.manifest())
        self.assertGreaterEqual(len(found), 1)
        self.assertTrue(any("package(ush)" in group["filter"] for group in found))


class ExclusionTests(unittest.TestCase):
    def test_the_group_whose_probe_failed_is_left_out_and_named(self):
        left_out, notes = capability.exclusions(
            report(**{"bubblewrap-overlay": FAILED}), {"env-tools"}
        )
        self.assertEqual(left_out, f"not (({filter_of('env-tools')}))")
        self.assertEqual(len(notes), 1, notes)
        self.assertIn("UserNamespace", notes[0])
        self.assertIn(FAILED, notes[0])

    def test_a_group_of_another_package_is_not_left_out(self):
        # Only what the run tests is left out of it: a group of a package this
        # run does not test has no say in its filter.
        left_out, notes = capability.exclusions(report(**{"bubblewrap-overlay": FAILED}), {"ush"})
        self.assertNotIn("package(env-tools)", left_out)
        self.assertEqual(notes, [])

    def test_nothing_left_out_is_an_empty_filter(self):
        # An empty filter term would be a filterset nextest rejects.
        left_out, notes = capability.exclusions(report(), {"env-tools"})
        self.assertEqual(left_out, "")
        self.assertEqual(notes, [])


class ProbeTests(unittest.TestCase):
    def test_a_report_names_each_probe_and_its_verdict(self):
        probes, variables = capability.probe_report(report(**{"bubblewrap-overlay": FAILED}))
        self.assertIsNone(probes["bubblewrap"])
        self.assertEqual(probes["bubblewrap-overlay"], FAILED)
        self.assertEqual(variables["UserNamespace"], "TILO_REQUIRE_USERNS")

    def test_a_handed_probe_runs_without_cargo(self):
        # A shard is handed the family's build of the probe (t.toml's `starts`): it
        # compiles nothing to run it.
        with tempfile.TemporaryDirectory() as work:
            probe = Path(work) / capability.PROBE
            probe.write_text("#!/bin/sh\n")
            probe.chmod(0o755)
            with (
                mock.patch.dict(os.environ, {capability.HANDED: str(probe)}),
                mock.patch.object(capability.subprocess, "run", side_effect=AssertionError("cargo ran")),
            ):
                self.assertEqual(capability.probe_program(), str(probe))


class WrapperTests(unittest.TestCase):
    """tools/ci/nextest.sh asks the tool only for a run that could have a group to leave out."""

    def run_of(self, packages: str) -> tuple[list[str], list[str]]:
        """A shard of PACKAGES as t starts one, through the production wrapper: the command
        cargo was given, and every Python the wrapper started."""
        with tempfile.TemporaryDirectory() as work:
            tree, programs = Path(work) / "tree", Path(work) / "bin"
            (tree / "tools/ci").mkdir(parents=True)
            (tree / ".config").mkdir()
            programs.mkdir()
            shutil.copy(TOOLS / "ci/nextest.sh", tree / "tools/ci/nextest.sh")
            shutil.copy(capability.MANIFEST, tree / ".config/nextest-capabilities.toml")
            given, asked = Path(work) / "cargo", Path(work) / "python"
            asked.touch()
            (programs / "cargo").write_text(f'#!/bin/sh\nprintf "%s\\n" "$@" >"{given}"\n')
            # The tool's answer for a machine that lacks nothing: no filter.
            (programs / "python3").write_text(f'#!/bin/sh\necho "$*" >>"{asked}"\n')
            for program in programs.iterdir():
                program.chmod(0o755)
            env = {"PATH": f"{programs}{os.pathsep}{os.environ.get('PATH', '/usr/bin:/bin')}",
                   "T_NEXTEST_PACKAGES": packages}
            # A filter and a test of its own: the wrapper adds neither the goldens' filter
            # nor the timings' recorder, which are other questions and other programs.
            command = ["cargo", "nextest", "run", "--locked", "--profile", "ci",
                       "-E", "all()", "--test", "it"]
            result = subprocess.run(["bash", str(tree / "tools/ci/nextest.sh"), *command],
                                    env=env, capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(given.read_text().splitlines()[:len(command) - 1], command[1:])
            return given.read_text().splitlines(), asked.read_text().splitlines()

    def test_an_ordinary_run_starts_no_interpreter_to_hear_it_probes_nothing(self):
        # Every job of a suite goes through the wrapper, and all but a handful test a
        # package the manifest does not name: the manifest's text answers those here.
        for packages in ("no-such-package", "tilo-reference collab-types"):
            with self.subTest(packages=packages):
                _, asked = self.run_of(packages)
                self.assertEqual(asked, [])

    def test_a_run_of_a_package_the_manifest_names_asks_the_tool(self):
        probed = sorted({
            package
            for group in capability.groups(capability.manifest())
            for package in capability.PACKAGE.findall(group["filter"])
        })
        self.assertTrue(probed)
        for package in probed:
            with self.subTest(package=package):
                _, asked = self.run_of(f"collab-types {package}")
                self.assertEqual(len(asked), 1, asked)
                self.assertTrue(asked[0].endswith(
                    f"tools/ci/capability_filter.py --packages collab-types {package}"), asked)
        # A name that only begins like one of them is another package.
        _, asked = self.run_of(f"{probed[0]}-extra")
        self.assertEqual(asked, [])

    def test_a_run_that_names_no_package_asks_the_tool(self):
        # It tests every package, the manifest's among them.
        _, asked = self.run_of("")
        self.assertEqual(len(asked), 1, asked)
        self.assertTrue(asked[0].endswith("tools/ci/capability_filter.py --packages"), asked)


class StartsTests(unittest.TestCase):
    def test_every_family_hands_the_probe_to_the_packages_it_probes_for(self):
        """A run of a package the manifest names runs the probe first; t hands every shard of it
        the family's build of the probe, or each shard would compile it from its ~765 units."""
        probed = {
            package
            for group in capability.groups(capability.manifest())
            for package in capability.PACKAGE.findall(group["filter"])
        }
        self.assertTrue(probed)
        members = families.workspace_packages()
        config = tomllib.loads(families.T_CONFIG.read_text(encoding="utf-8"))
        wanted = f"tilo-test-env:{capability.PROBE}"
        for family in config.get("cargo", []):
            packages = families.tested(family, members)
            if packages is None:
                continue
            starts = family.get("starts", {})
            missing = sorted(p for p in packages & probed if wanted not in starts.get(p, []))
            self.assertEqual(
                missing,
                [],
                f"t.toml's {family.get('name')} family tests these packages, which "
                f".config/nextest-capabilities.toml names, without {wanted!r} in their `starts`",
            )


if __name__ == "__main__":
    unittest.main()
