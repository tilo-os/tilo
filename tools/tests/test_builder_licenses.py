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
            {"name": dep, "kind": kind, "path": f"/checkout/{dep}", "optional": optional}
            for dep, kind, optional in dependencies
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

    def test_build_dependencies_count_and_dev_dependencies_do_not(self):
        packages = [
            package("tilolang", "MIT", ("codegen", "build", False), ("tilo-test-env", "dev", False)),
            package("codegen", "AGPL-3.0-only"),
            package("tilo-test-env", "AGPL-3.0-only"),
        ]
        reached = builder_licenses.manifest_closure(packages, ["tilolang"])
        self.assertEqual(sorted(reached), ["codegen", "tilolang"])

    def test_a_root_outside_the_workspace_is_refused(self):
        with self.assertRaises(ValueError):
            builder_licenses.manifest_closure([package("tilolang", "MIT")], ["tilo-ui-typo"])

    def test_test_support_is_not_a_shipped_feature(self):
        shipped = builder_licenses.shipped_features(package("tilo-app", "MIT", features=("native", "test-support")))
        self.assertEqual(shipped, ["native"])


if __name__ == "__main__":
    unittest.main()
