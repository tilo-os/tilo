import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "ci/current_source_metadata.py"
spec = importlib.util.spec_from_file_location("current_source_metadata", SCRIPT)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class CurrentSourceMetadataTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "source"
        (self.root / "src").mkdir(parents=True)
        (self.root / "Cargo.toml").write_text('[package]\nname="fixture"\nversion="0.1.0"\nedition="2024"\n')
        (self.root / "Cargo.lock").write_text("version = 4\n")
        (self.root / "src/lib.rs").write_text("pub fn fixture() {}\n")
        for args in (("init", "-q"), ("add", "."),
                     ("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false", "commit", "-qm", "fixture")):
            subprocess.run(["git", "-C", str(self.root), *args], check=True)
        package_id = "path+file://" + str(self.root) + "#fixture@0.1.0"
        self.metadata = {"version": 1, "workspace_root": str(self.root),
                         "workspace_members": [package_id], "packages": [{"id": package_id,
                         "manifest_path": str(self.root / "Cargo.toml"),
                         "dependencies": [{"name": "optional", "optional": True, "kind": "build", "target": "cfg(unix)"}],
                         "targets": [{"src_path": str(self.root / "src/lib.rs")}]}]}

    def test_complete_round_trip_preserves_ids_dependency_and_target_details(self):
        bundle = module.seal(self.root, self.metadata)
        self.assertNotIn(str(self.root), json.dumps(bundle))
        self.assertEqual(module.validate(self.root, bundle), self.metadata)
        bundle["metadata"]["packages"] = []
        with self.assertRaisesRegex(ValueError, "every workspace package"):
            module.validate(self.root, bundle)

    def test_source_manifest_and_untracked_changes_fail_closed(self):
        bundle = module.seal(self.root, self.metadata)
        for path in ("Cargo.toml", "src/lib.rs"):
            original = (self.root / path).read_bytes()
            (self.root / path).write_bytes(original + b"\n")
            with self.assertRaisesRegex(ValueError, "clean worktree"):
                module.validate(self.root, bundle)
            (self.root / path).write_bytes(original)
        (self.root / "src/untracked.rs").write_text("// uncommitted target\n")
        with self.assertRaisesRegex(ValueError, "clean worktree"):
            module.validate(self.root, bundle)

    def test_forged_identity_and_external_targets_fail_closed(self):
        bundle = module.seal(self.root, self.metadata)
        bundle["source"]["manifests"]["Cargo.lock"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "manifest bytes differ"):
            module.validate(self.root, bundle)
        self.metadata["packages"][0]["targets"][0]["src_path"] = "/outside/lib.rs"
        with self.assertRaisesRegex(ValueError, "outside source"):
            module.seal(self.root, self.metadata)


if __name__ == "__main__":
    unittest.main()
