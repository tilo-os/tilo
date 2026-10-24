#!/usr/bin/env python3
"""Exercise fresh-build identity declarations on the real packaging stamp path."""
from pathlib import Path
import subprocess
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
RUNTIME = "tilo-runtime/2026.07+native." + "a" * 32


class SdkServiceStagingTests(unittest.TestCase):
    def stamp(self, source):
        temp = tempfile.TemporaryDirectory(prefix="tilo-sdk-services-")
        self.addCleanup(temp.cleanup)
        manifest = Path(temp.name) / "manifest.toml"
        manifest.write_text(source)
        result = subprocess.run(
            ["bash", str(ROOT / "os/scripts/stamp-runtime.sh"), str(manifest), RUNTIME],
            capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return manifest

    def test_the_identity_requirement_is_the_runtime_version_the_image_serves(self):
        served = tomllib.loads((ROOT / "os/usr/share/tilo/system-interfaces.toml").read_text())
        source = (ROOT / "tools/apps/stage_sdk_services.py").read_text()
        self.assertIn(
            f'IDENTITY_REQUIREMENT = "optional@{served["interfaces"]["runtime"]}"', source
        )

    def test_actual_app_staging_adds_only_negotiated_identity_requirement(self):
        original = (ROOT / "apps/file-explorer/tilo-app.toml").read_text()
        expected = tomllib.loads(original)
        expected["package"]["runtime"] = RUNTIME
        expected["sandbox"]["services"]["identity"] = "optional@8"
        manifest = self.stamp(original)
        self.assertEqual(tomllib.loads(manifest.read_text()), expected)
        # Already-staged output is idempotent and retains publisher/trust inputs.
        before = manifest.read_bytes()
        subprocess.run(
            ["python3", str(ROOT / "tools/apps/stage_sdk_services.py"), str(manifest)],
            check=True,
        )
        self.assertEqual(manifest.read_bytes(), before)

    def test_every_current_sdk_app_manifest_can_declare_presentation(self):
        manifests = sorted((ROOT / "apps").glob("*/tilo-app.toml"))
        self.assertGreaterEqual(len(manifests), 40, "the shipped app fleet is missing")
        for authored in manifests:
            with self.subTest(app=authored.parent.name):
                original = tomllib.loads(authored.read_text())
                manifest = self.stamp(authored.read_text())
                staged = tomllib.loads(manifest.read_text())
                self.assertEqual(
                    staged["sandbox"]["services"]["identity"],
                    original.get("sandbox", {}).get("services", {}).get("identity", "optional@8"),
                )

    def test_authored_legacy_identity_contract_remains_legacy(self):
        original = (ROOT / "apps/file-explorer/tilo-app.toml").read_text()
        original = original.replace("[sandbox.services]", '[sandbox.services]\nidentity = "required@1"')
        expected = tomllib.loads(original)
        expected["package"]["runtime"] = RUNTIME
        manifest = self.stamp(original)
        self.assertEqual(tomllib.loads(manifest.read_text()), expected)

    def test_missing_service_table_is_created_without_other_defaults(self):
        source = '[package]\nkind="app"\nruntime="tilo-runtime/2026.07"\n[sandbox]\nseccomp="tilo-default"\n'
        manifest = self.stamp(source)
        self.assertEqual(tomllib.loads(manifest.read_text())["sandbox"], {
            "seccomp": "tilo-default", "services": {"identity": "optional@8"},
        })

    def test_unbound_runtime_and_non_app_are_refused_without_mutation(self):
        with tempfile.TemporaryDirectory(prefix="tilo-sdk-refusal-") as directory:
            manifest = Path(directory) / "manifest.toml"
            for source in (
                '[package]\nkind="app"\nruntime="tilo-runtime/2026.07"\n',
                f'[package]\nkind="tool"\nruntime="{RUNTIME}"\n',
            ):
                manifest.write_text(source)
                result = subprocess.run(
                    ["python3", str(ROOT / "tools/apps/stage_sdk_services.py"), str(manifest)],
                    capture_output=True, text=True,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(manifest.read_text(), source)


if __name__ == "__main__":
    unittest.main()
