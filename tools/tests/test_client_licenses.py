import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("client_licenses", ROOT / "tools/sdk/check_client_licenses.py")
client_licenses = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(client_licenses)

WORKSPACE = {"tilo-actions", "tilo-session-host"}


class ClientLicenseGraph(unittest.TestCase):
    def test_accepts_approved_expressions_and_cargo_duplicate_markers(self):
        dependencies = client_licenses.parse(
            "tilo-actions v0.1.0 (/checkout/crates/collab/actions)\tMIT\n"
            "dependency v1.0.0\tMIT OR Apache-2.0 (*)\n")
        self.assertEqual(client_licenses.findings(dependencies, {"MIT", "MIT OR Apache-2.0"}), [])

    def test_rejects_agpl_and_missing_license(self):
        dependencies = client_licenses.parse("bad v1.0.0\tAGPL-3.0-only\nunknown v1.0.0\t\n")
        errors = client_licenses.findings(dependencies, {"MIT"})
        self.assertEqual(len(errors), 2)
        self.assertTrue(any("AGPL-3.0-only" in error for error in errors))
        self.assertTrue(any("unknown" in error for error in errors))

    def test_does_not_accept_an_unreviewed_expression_containing_mit(self):
        dependencies = client_licenses.parse("bad v1.0.0\tMIT AND AGPL-3.0-only\n")
        self.assertTrue(client_licenses.findings(dependencies, {"MIT"}))

    def test_empty_and_malformed_observations_fail(self):
        for output in ("", "dependency without a license column"):
            with self.subTest(output=output), self.assertRaises(ValueError):
                client_licenses.parse(output)

    def test_a_tilo_crate_joining_the_client_side_is_reported(self):
        errors = client_licenses.boundary({"tilo-actions", "tilo-session-host"}, {"tilo-actions"}, WORKSPACE)
        self.assertEqual(len(errors), 1)
        self.assertIn("tilo-session-host: a Tilo crate the client side now reaches", errors[0])
        self.assertIn("license-policy.toml", errors[0])

    def test_a_crate_that_left_the_client_side_is_reported(self):
        errors = client_licenses.boundary({"tilo-actions"}, {"tilo-actions", "tilo-session-host"}, WORKSPACE)
        self.assertEqual(errors, ["tilo-session-host: named as the client side but no longer reached"])

    def test_third_party_crates_are_not_part_of_the_boundary(self):
        self.assertEqual(client_licenses.boundary({"tilo-actions", "serde"}, {"tilo-actions"}, WORKSPACE), [])


if __name__ == "__main__":
    unittest.main()
