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
        errors = sdk_licenses.findings("tilo-identity v0.1.0\tMIT\n", {"MIT"})
        self.assertEqual(errors, ["tilo-identity: host implementation in the app dependency graph"])

    def test_does_not_accept_an_unreviewed_expression_containing_mit(self):
        self.assertTrue(sdk_licenses.findings("bad v1.0.0\tMIT AND AGPL-3.0-only", {"MIT"}))

    def test_empty_and_malformed_observations_fail(self):
        for output in ("", "dependency without a license column"):
            with self.subTest(output=output), self.assertRaises(ValueError):
                sdk_licenses.findings(output, {"MIT"})

    def test_the_all_set_is_every_shipping_feature_and_not_test_support(self):
        manifest = {"features": {
            "default": ["native"],
            "native": ["dep:tilo-runner-native"],
            "mpris": [],
            "test-support": ["ui-core/test-support", "dep:tilo-broker-core"],
        }}
        self.assertEqual(
            sdk_licenses.audited_features(manifest), ["default", "mpris", "native"])
        self.assertEqual(sdk_licenses.audited_features({}), [])

    def test_a_crate_a_person_runs_cannot_enable_the_test_feature(self):
        def tilo_app(features, kind=None):
            return {"name": "tilo-app", "kind": kind, "features": features}

        metadata = {"packages": [
            # Published, so it ships to everyone who builds on it.
            {"name": "published", "publish": None, "features": {}, "targets": [],
             "dependencies": [tilo_app(["test-support"])]},
            # A binary a person runs, though unpublished.
            {"name": "program", "publish": [], "features": {},
             "targets": [{"kind": ["bin"], "required-features": []}],
             "dependencies": [tilo_app(["test-support"]),
                              {"name": "reached", "kind": None, "features": []}]},
            # A library only a shipped program reaches ships with it.
            {"name": "reached", "publish": [], "features": {},
             "targets": [{"kind": ["lib"]}],
             "dependencies": [tilo_app(["test-support"])]},
            # The test host: unpublished, no binary, reached only from a dev-dependency.
            {"name": "test-host", "publish": [], "features": {},
             "targets": [{"kind": ["lib"]}],
             "dependencies": [tilo_app(["test-support"])]},
            # A shipped app dev-depends on the host and enables the feature for its tests only.
            {"name": "app", "publish": [], "features": {},
             "targets": [{"kind": ["bin"], "required-features": []}],
             "dependencies": [tilo_app([]),
                              {"name": "test-host", "kind": "dev", "features": []},
                              tilo_app(["test-support"], kind="dev")]},
            # A generated crate's stub binary requires a feature nothing enables: it builds
            # nothing, so it ships nothing.
            {"name": "unify", "publish": [], "features": {"never": []},
             "targets": [{"kind": ["bin"], "required-features": ["never"]}],
             "dependencies": [tilo_app(["test-support"])]},
            # Anything else enabling it is not about tilo-app.
            {"name": "unrelated", "publish": [], "features": {}, "targets": [],
             "dependencies": [{"name": "other", "kind": None, "features": ["test-support"]}]},
        ]}
        self.assertEqual(
            sdk_licenses.test_support_from_normal_dependencies(metadata),
            [
                "program: enables tilo-app's test-support from a normal dependency",
                "published: enables tilo-app's test-support from a normal dependency",
                "reached: enables tilo-app's test-support from a normal dependency",
            ],
        )


if __name__ == "__main__":
    unittest.main()
