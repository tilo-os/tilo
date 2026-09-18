"""Exercise real Cargo metadata and Git blobs without compiling packages."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import app_packaging_report as planner


class ReportTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        files = {"Cargo.toml": '[workspace]\nmembers=["ui","apps/one","apps/two"]\nresolver="3"\n',
                 "docs/readme.md": "docs", "rust-toolchain.toml": (planner.ROOT / "rust-toolchain.toml").read_text()}
        for name in ("ui", "apps/one", "apps/two"):
            package = name.split("/")[-1]
            deps = '\n[dependencies]\nui={path="../../ui"}\n' if name != "ui" else ""
            files[name + "/Cargo.toml"] = f'[package]\nname="{package}"\nversion="0.1.0"\nedition="2024"\n{deps}'
            files[name + "/src/lib.rs"] = "pub fn example() {}\n"
        for path, text in files.items():
            dest = self.root / path
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_text(text)
        self.command("cargo", "generate-lockfile", "--offline")
        self.metadata = json.loads(self.command("cargo", "metadata", "--locked", "--offline", "--no-deps", "--format-version", "1"))
        self.command("git", "init", "-q")
        self.command("git", "add", ".")
        self.command("git", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.test", "commit", "-qm", "fixture")
        self.blobs = planner.tree_inputs(self.root)
        self.catalog = {name: {"package": name} for name in ("one", "two")}

    def command(self, *args):
        return subprocess.check_output(args, cwd=self.root, text=True, stderr=subprocess.PIPE)

    def report(self, paths, **kwargs):
        opts = dict(target="x86_64-unknown-linux-gnu", builder="sha256:" + "1" * 64,
                    runtime="sha256:" + "2" * 64)
        opts.update(kwargs)
        return planner.report(self.metadata, self.catalog, {}, paths, self.blobs, self.root, **opts)

    def keys(self, result):
        return {row["app"]: row["identity_candidate"] for row in result["apps"]}

    def test_actual_dependency_graph(self):
        self.assertEqual(self.report(["apps/one/src/lib.rs"])["affected_apps"], ["one"])
        self.assertEqual(self.report(["ui/src/lib.rs"])["affected_apps"], ["one", "two"])
        self.assertEqual(self.report(["docs/readme.md"])["affected_apps"], [])
        self.assertEqual(self.report(["native/unknown.h"])["affected_apps"], ["one", "two"])

    def test_source_identity_is_dependency_scoped(self):
        original = self.keys(self.report([]))
        self.blobs["apps/one/src/lib.rs"] = ["100644", "changed"]
        changed = self.keys(self.report([]))
        self.assertNotEqual(original["one"], changed["one"])
        self.assertEqual(original["two"], changed["two"])
        self.blobs["ui/src/lib.rs"] = ["100644", "changed"]
        shared = self.keys(self.report([]))
        self.assertTrue(all(shared[name] != changed[name] for name in shared))

    def test_target_runtime_builder_and_feature_graph_separate_keys(self):
        original = self.keys(self.report([]))
        for change in (dict(target="aarch64-unknown-linux-gnu"), dict(builder="sha256:" + "3" * 64),
                       dict(runtime="sha256:" + "4" * 64)):
            self.assertNotEqual(original, self.keys(self.report([], **change)))
        self.blobs["apps/one/Cargo.toml"] = ["100644", "changed feature graph"]
        self.assertTrue(all(value != original[name] for name, value in self.keys(self.report([])).items()))

    def test_dirty_checkout_and_mutable_builder_fail(self):
        (self.root / "ui/src/lib.rs").write_text("changed")
        with self.assertRaises(ValueError):
            planner.tree_inputs(self.root)
        with self.assertRaises(ValueError):
            self.report([], builder="latest")
        self.assertFalse(self.report([])["reuse_authorized"])

    def test_untracked_input_rejected(self):
        (self.root / "ui/src/generated.rs").write_text("untracked")
        with self.assertRaises(ValueError):
            planner.tree_inputs(self.root)

    def test_deleted_manifest_and_unknown_native_inputs(self):
        original = self.keys(self.report([]))
        self.blobs["native/sdk/config.h"] = ["100644", "new"]
        self.assertTrue(all(value != original[name] for name, value in self.keys(self.report([])).items()))
        self.assertEqual(self.report(["removed/crate/Cargo.toml"])["affected_apps"], ["one", "two"])
        del self.blobs["apps/one/Cargo.toml"]
        with self.assertRaises(ValueError):
            self.report([])


if __name__ == "__main__":
    unittest.main()
