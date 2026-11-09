import importlib.util
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location(
    "capability", Path(__file__).parents[1] / "ci/capability_filter.py"
)
capability = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(capability)

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


if __name__ == "__main__":
    unittest.main()
