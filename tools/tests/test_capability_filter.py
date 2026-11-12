import importlib.util
import os
from pathlib import Path
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
