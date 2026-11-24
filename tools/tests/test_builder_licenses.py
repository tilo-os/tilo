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


if __name__ == "__main__":
    unittest.main()
