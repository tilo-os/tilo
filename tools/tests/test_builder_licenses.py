import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("builder_licenses", ROOT / "tools/sdk/check_builder_licenses.py")
builder_licenses = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(builder_licenses)


def package(name, license, *dependencies, features=()):
    return {
        "name": name,
        "source": None,
        "license": license,
        "features": {feature: [] for feature in features},
        "dependencies": [
            {"name": dep, "kind": kind, "path": f"/checkout/{dep}", "optional": optional,
             "req": "^0.1.0" if versioned else "*"}
            for dep, kind, optional, *rest in dependencies
            for versioned in [bool(rest and rest[0])]
        ],
    }


class BuilderManifestGraph(unittest.TestCase):
    def test_an_optional_dependency_on_an_agpl_crate_fails(self):
        # crates.io needs every dependency a manifest names, so no feature selection hides it.
        packages = [
            package("tilolang", "MIT", ("tilo-reference", None, False), ("tilo-shell", None, True)),
            package("tilo-reference", "MIT"),
            package("tilo-shell", "AGPL-3.0-only"),
        ]
        reached = builder_licenses.manifest_closure(packages, ["tilolang"])
        self.assertEqual(sorted(reached), ["tilo-reference", "tilo-shell", "tilolang"])
        findings = builder_licenses.manifest_findings(reached, {"MIT"})
        self.assertEqual(len(findings), 1)
        self.assertIn("tilo-shell", findings[0])

    def test_build_dependencies_count_and_dev_dependencies_count_when_published(self):
        # Cargo keeps a dev-dependency that carries a version in the published manifest, so
        # crates.io needs it; one named by path alone is dropped.
        packages = [
            package("tilo-ui", "MIT", ("codegen", "build", False), ("tilo-test-env", "dev", False),
                    ("tilo-theme", "dev", False, True)),
            package("codegen", "AGPL-3.0-only"),
            package("tilo-test-env", "AGPL-3.0-only"),
            package("tilo-theme", "AGPL-3.0-only"),
        ]
        reached = builder_licenses.manifest_closure(packages, ["tilo-ui"])
        self.assertEqual(sorted(reached), ["codegen", "tilo-theme", "tilo-ui"])

    def test_what_a_builder_crate_reaches_must_be_publishable_with_it(self):
        packages = [
            package("tilolang", "MIT", ("tilo-terminal", None, False, True)),
            # A path dependency without a version; a dev-dependency by path alone is dropped
            # from the published manifest and asks for nothing.
            package("tilo-terminal", "MIT", ("environment", None, False),
                    ("tilo-test-env", "dev", False)),
            package("environment", "MIT"),
            package("tilo-test-env", "AGPL-3.0-only"),
        ]
        packages[1]["publish"] = []
        reached = builder_licenses.manifest_closure(packages, ["tilolang"])
        findings = builder_licenses.publishable_findings(reached)
        self.assertEqual(len(findings), 3, findings)
        self.assertIn("environment: a builder crate reaches it, and its name has no", findings[0])
        self.assertIn("tilo-terminal: a builder crate reaches it, and it has publish = false",
                      findings[1])
        self.assertIn("path dependency environment has no version", findings[2])
        packages[1]["publish"] = None
        packages[1]["dependencies"][0]["req"] = "^0.1.0"
        packages[2]["name"] = packages[1]["dependencies"][0]["name"] = "tilo-environment"
        reached = builder_licenses.manifest_closure(packages, ["tilolang"])
        self.assertEqual(builder_licenses.publishable_findings(reached), [])

    def test_a_root_outside_the_workspace_is_refused(self):
        with self.assertRaises(ValueError):
            builder_licenses.manifest_closure([package("tilolang", "MIT")], ["tilo-ui-typo"])

    def test_test_support_is_not_a_shipped_feature(self):
        shipped = builder_licenses.shipped_features(package("tilo-app", "MIT", features=("native", "test-support")))
        self.assertEqual(shipped, ["native"])


class ToolkitReach(unittest.TestCase):
    PACKAGES = [
        package("tilo-ui", "MIT", ("tilo-ui-paint", None, False, True),
                ("tilo-collaboration", None, True, True), ("tilo-test-env", "dev", False)),
        package("tilo-ui-paint", "MIT"),
        package("tilo-collaboration", "MIT", ("tilo-resource-store", None, False, True)),
        package("tilo-resource-store", "MIT"),
        package("tilo-test-env", "MIT"),
    ]

    def findings(self, reaches, to_cut=()):
        return builder_licenses.toolkit_findings(self.PACKAGES, "tilo-ui", reaches, to_cut)

    def test_what_the_lists_hold_passes(self):
        self.assertEqual(self.findings(["tilo-ui-paint", "tilo-collaboration"],
                                       ["tilo-resource-store"]), [])

    def test_a_crate_reached_through_another_fails_naming_the_dependency(self):
        # An optional dependency counts: crates.io needs it, whatever features are on.
        findings = self.findings(["tilo-ui-paint", "tilo-collaboration"])
        self.assertEqual(len(findings), 1, findings)
        self.assertTrue(findings[0].startswith("tilo-collaboration -> tilo-resource-store: "),
                        findings[0])

    def test_a_dev_dependency_without_a_version_is_not_reached(self):
        self.assertNotIn("tilo-test-env", "".join(self.findings(["tilo-ui-paint"])))

    def test_a_cut_edge_must_leave_the_list(self):
        packages = [dict(self.PACKAGES[0], dependencies=self.PACKAGES[0]["dependencies"][:1]),
                    *self.PACKAGES[1:]]
        findings = builder_licenses.toolkit_findings(
            packages, "tilo-ui", ["tilo-ui-paint", "tilo-collaboration"], ["tilo-resource-store"])
        self.assertEqual(findings, [
            "tilo-collaboration: tilo-ui no longer reaches it; delete its line from "
            "toolkit_reaches in tools/sdk/license-policy.toml",
            "tilo-resource-store: tilo-ui no longer reaches it; delete its line from "
            "toolkit_reaches_to_cut in tools/sdk/license-policy.toml",
        ])

    def test_a_crate_in_both_lists_fails(self):
        findings = self.findings(["tilo-ui-paint", "tilo-collaboration", "tilo-resource-store"],
                                 ["tilo-resource-store"])
        self.assertEqual(findings, [
            "tilo-resource-store: listed in both toolkit_reaches and toolkit_reaches_to_cut"])


if __name__ == "__main__":
    unittest.main()
