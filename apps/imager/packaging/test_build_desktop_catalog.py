"""Release-generator contract tests; fixture metadata is deliberately test-only."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("build-desktop-catalog.py")


class DesktopCatalogGeneratorTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.artifact = self.root / "package.rpm"
        self.artifact.write_bytes(b"test artifact bytes")
        self.digest = hashlib.sha256(self.artifact.read_bytes()).hexdigest()
        self.report = self.root / "native-report.md"
        self.report.write_text("TEST FIXTURE ONLY: explicit native evidence would be supplied here.\n")
        self.evidence = {"schema": 1, "reference": str(self.report), "package_sha256": self.digest,
                         "graphical_login": True, "return_to_prior_session": True,
                         "identity_setup": True, "selected_folder_import": True, "source_intact": True}
        self.release = {"id": "test-fedora", "version": "fixture-1", "distro_id": "fedora", "distro_version": "44",
                        "architecture": "aarch64", "package_format": "rpm", "url": "https://code.tilo.so/test/package.rpm",
                        "artifact": str(self.artifact), "verified_runtime": self.evidence}

    def generate(self, releases=None, tag="fixture-1"):
        manifest = self.root / "manifest.json"
        manifest.write_text(json.dumps({"schema": 1, "tag": tag, "releases": [self.release] if releases is None else releases}))
        output = self.root / "catalog.json"
        result = subprocess.run([sys.executable, str(SCRIPT), str(manifest), "--output", str(output)], capture_output=True, text=True)
        return result, output

    def test_generates_catalog_from_actual_artifact_or_explicit_empty_release(self):
        with self.subTest(phase="derives size and digest from actual artifact"):
            result, output = self.generate()
            self.assertEqual(result.returncode, 0, result.stderr)
            release = json.loads(output.read_text())["releases"][0]
            self.assertEqual(release["byte_size"], len(self.artifact.read_bytes()))
            self.assertEqual(release["sha256"], self.digest)
            self.assertNotIn("artifact", release)
            self.assertEqual(release["verified_runtime"], self.evidence)

        self.setUp()
        with self.subTest(phase="explicit unreleased empty catalog"):
            result, output = self.generate([], "unreleased")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(output.read_text()), {"schema": 1, "tag": "unreleased", "releases": []})
            self.assertNotEqual(self.generate([], "fixture-1")[0].returncode, 0)

    def test_refuses_incomplete_evidence_invented_digests_untrusted_urls_and_duplicates(self):
        with self.subTest(phase="requires actual matching complete runtime evidence"):
            for field, value in [("package_sha256", "0" * 64), ("graphical_login", False),
                                 ("identity_setup", False), ("selected_folder_import", False),
                                 ("source_intact", False), ("return_to_prior_session", False),
                                 ("reference", str(self.root / "missing.md")), ("schema", 2), ("extra", True)]:
                with self.subTest(field=field):
                    original = self.release["verified_runtime"]
                    self.release["verified_runtime"] = {**self.evidence, field: value}
                    result, output = self.generate()
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse(output.exists())
                    self.release["verified_runtime"] = original
            self.release["verified_runtime"] = None
            self.assertNotEqual(self.generate()[0].returncode, 0)

        self.setUp()
        with self.subTest(phase="refuses invented digest, empty artifact and untrusted urls"):
            self.release["sha256"] = self.digest
            self.assertNotEqual(self.generate()[0].returncode, 0)
            del self.release["sha256"]
            for url in ["http://code.tilo.so/p.rpm", "https://example.org/p.rpm", "https://code.tilo.so:444/p.rpm", "https://user@code.tilo.so/p.rpm"]:
                self.release["url"] = url
                self.assertNotEqual(self.generate()[0].returncode, 0)
            self.release["url"] = "https://code.tilo.so/p.rpm"
            self.artifact.write_bytes(b"")
            self.assertNotEqual(self.generate()[0].returncode, 0)

        self.setUp()
        with self.subTest(phase="refuses duplicate selectors and unsupported formats"):
            duplicate = {**self.release, "id": "different"}
            self.assertNotEqual(self.generate([self.release, duplicate])[0].returncode, 0)
            self.release["package_format"] = "exe"
            self.assertNotEqual(self.generate()[0].returncode, 0)

if __name__ == "__main__":
    unittest.main()
