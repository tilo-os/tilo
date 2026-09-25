"""Contract tests for the native installer bootc helper producer."""

import importlib.util
import json
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".forgejo/workflows/development-installer-bootc-helper.yml"
PIN = ROOT / "os/external-assets/bootc/source.json"
PRODUCER = ROOT / "os/dev/produce-installer-bootc-helper.py"
spec = importlib.util.spec_from_file_location("producer", PRODUCER)
producer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(producer)


class InstallerBootcHelperWorkflowTests(unittest.TestCase):
    def test_pin_and_runtime_contract_are_one_exact_identity(self):
        document, patch = producer.pin()
        self.assertEqual(document["source_commit"], producer.contract.SOURCE_COMMIT)
        self.assertEqual(document["source_cargo_lock_sha256"],
                         producer.contract.SOURCE_CARGO_LOCK_SHA256)
        self.assertEqual(document["build_image"], producer.contract.BUILD_IMAGE)
        self.assertEqual(producer.digest(patch), producer.contract.PATCH_SHA256)

    def test_workflow_uses_native_runners_immutable_builder_and_exact_cache(self):
        workflow = WORKFLOW.read_text()
        pin = json.loads(PIN.read_text())
        self.assertIn("options: [aarch64, x86_64]", workflow)
        self.assertIn("runner-disk-100g-arm64", workflow)
        self.assertIn("runner-disk-100g'", workflow)
        self.assertIn("bash os/dev/assert-native-arch.sh", workflow)
        self.assertIn(f"image: {pin['build_image']}", workflow)
        self.assertIn("actions/cache/restore@5a3ec84e", workflow)
        self.assertIn("actions/cache/save@5a3ec84e", workflow)
        self.assertIn("key: ${{ steps.source.outputs.cache_key }}", workflow)
        self.assertNotIn("restore-keys:", workflow)
        self.assertNotIn("runner.temp", workflow)

    def test_workflow_builds_only_bootc_and_uploads_bounded_output(self):
        workflow = WORKFLOW.read_text()
        self.assertEqual(workflow.count("cargo build --locked --release -p bootc"), 1)
        self.assertIn("--output /tmp/installer-bootc-helper", workflow)
        self.assertIn("path: /tmp/installer-bootc-helper/", workflow)
        self.assertIn("if-no-files-found: error", workflow)
        self.assertIn("retention-days: 14", workflow)
        self.assertNotIn("docker build", workflow)
        self.assertNotIn("kubectl", workflow)


if __name__ == "__main__":
    unittest.main()
