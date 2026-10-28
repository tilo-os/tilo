import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("sdk_licenses", ROOT / "tools/sdk/check_licenses.py")
sdk_licenses = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(sdk_licenses)


class AppLicenseGraph(unittest.TestCase):
    def test_accepts_approved_expressions_and_cargo_duplicate_markers(self):
        self.assertEqual(sdk_licenses.findings(
            "tilo-app v0.1.0 (/checkout/crates/sdk/tilo-app)\tMIT\n"
            "dependency v1.0.0\tMIT OR Apache-2.0 (*)\n",
            {"MIT", "MIT OR Apache-2.0"},
        ), [])

    def test_rejects_agpl_and_missing_license(self):
        errors = sdk_licenses.findings(
            "bad v1.0.0\tAGPL-3.0-only\nunknown v1.0.0\t\n", {"MIT"})
        self.assertEqual(len(errors), 2)
        self.assertTrue(any("AGPL-3.0-only" in error for error in errors))
        self.assertTrue(any("unknown" in error for error in errors))

    def test_host_implementation_is_rejected_even_if_manifest_is_mislicensed(self):
        errors = sdk_licenses.findings("runtime-identity v0.1.0\tMIT\n", {"MIT"})
        self.assertEqual(errors, ["runtime-identity: host implementation in the app dependency graph"])

    def test_does_not_accept_an_unreviewed_expression_containing_mit(self):
        self.assertTrue(sdk_licenses.findings("bad v1.0.0\tMIT AND AGPL-3.0-only", {"MIT"}))

    def test_empty_and_malformed_observations_fail(self):
        for output in ("", "dependency without a license column"):
            with self.subTest(output=output), self.assertRaises(ValueError):
                sdk_licenses.findings(output, {"MIT"})


if __name__ == "__main__":
    unittest.main()
