import hashlib
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("winget", Path(__file__).with_name("generate-winget.py"))
winget = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(winget)


class ManifestTests(unittest.TestCase):
    def setUp(self):
        environment = patch.dict(os.environ, {
            "GITHUB_SERVER_URL": "https://code.sandbox.example",
            "TILO_RELEASE_REPOSITORY": "qos/releases",
        })
        environment.start()
        self.addCleanup(environment.stop)
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.url = "https://code.sandbox.example/qos/releases/releases/download/imager-v1.2.3"
        for arch in ("x86_64", "aarch64"):
            directory = self.root / arch
            directory.mkdir()
            installer = directory / f"tilo-imager-setup-windows-{arch}.exe"
            installer.write_bytes(arch.encode())
            digest = hashlib.sha256(arch.encode()).hexdigest()
            installer.with_suffix(".exe.sha256").write_text(f"{digest}  {installer.name}\n")

    def test_manifest_contains_both_exact_installer_digests(self):
        documents = winget.manifests(self.root, "1.2.3", self.url)
        self.assertEqual(len(documents), 3)
        installer = documents["Tilo.Imager.installer.yaml"]
        for arch, name in (("x86_64", "x64"), ("aarch64", "arm64")):
            self.assertIn(f"Architecture: {name}\n", installer)
            self.assertIn(hashlib.sha256(arch.encode()).hexdigest().upper(), installer)
            self.assertIn(f"{self.url}/tilo-imager-setup-windows-{arch}.exe", installer)

    def test_missing_arm_installer_fails(self):
        (self.root / "aarch64/tilo-imager-setup-windows-aarch64.exe").unlink()
        with self.assertRaises(ValueError):
            winget.manifests(self.root, "1.2.3", self.url)

    def test_tampered_installer_fails(self):
        (self.root / "aarch64/tilo-imager-setup-windows-aarch64.exe").write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "invalid aarch64 setup digest"):
            winget.manifests(self.root, "1.2.3", self.url)

    def test_duplicate_installer_fails(self):
        (self.root / "tilo-imager-setup-windows-aarch64.exe").write_bytes(b"duplicate")
        with self.assertRaises(ValueError):
            winget.manifests(self.root, "1.2.3", self.url)

    def test_wrong_release_or_origin_fails(self):
        for url in (self.url.replace("1.2.3", "1.2.4"), self.url.replace("code.sandbox.example", "example.org"), self.url + "?token=bad"):
            with self.subTest(url=url), self.assertRaises(ValueError):
                winget.manifests(self.root, "1.2.3", url)

    def test_insecure_configured_origin_fails(self):
        with patch.dict(os.environ, {"GITHUB_SERVER_URL": "http://forgejo.internal:3000"}):
            with self.assertRaisesRegex(ValueError, "release server must be an HTTPS origin"):
                winget.manifests(self.root, "1.2.3", self.url)


if __name__ == "__main__":
    unittest.main()
