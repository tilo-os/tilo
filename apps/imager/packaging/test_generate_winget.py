import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("winget", Path(__file__).with_name("generate-winget.py"))
winget = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(winget)


class ManifestTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.url = "https://code.podesta.ai/qos/qos/releases/download/imager-v1.2.3"
        for arch in ("x86_64", "aarch64"):
            directory = self.root / arch
            directory.mkdir()
            installer = directory / f"qos-imager-setup-windows-{arch}.exe"
            installer.write_bytes(arch.encode())
            digest = hashlib.sha256(arch.encode()).hexdigest()
            installer.with_suffix(".exe.sha256").write_text(f"{digest}  {installer.name}\n")

    def test_manifest_contains_both_exact_installer_digests(self):
        documents = winget.manifests(self.root, "1.2.3", self.url)
        self.assertEqual(len(documents), 3)
        installer = documents["qOS.Imager.installer.yaml"]
        for arch, name in (("x86_64", "x64"), ("aarch64", "arm64")):
            self.assertIn(f"Architecture: {name}\n", installer)
            self.assertIn(hashlib.sha256(arch.encode()).hexdigest().upper(), installer)
            self.assertIn(f"{self.url}/qos-imager-setup-windows-{arch}.exe", installer)

    def test_missing_arm_installer_fails(self):
        (self.root / "aarch64/qos-imager-setup-windows-aarch64.exe").unlink()
        with self.assertRaises(ValueError):
            winget.manifests(self.root, "1.2.3", self.url)

    def test_tampered_installer_fails(self):
        (self.root / "aarch64/qos-imager-setup-windows-aarch64.exe").write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "invalid aarch64 setup digest"):
            winget.manifests(self.root, "1.2.3", self.url)

    def test_duplicate_installer_fails(self):
        (self.root / "qos-imager-setup-windows-aarch64.exe").write_bytes(b"duplicate")
        with self.assertRaises(ValueError):
            winget.manifests(self.root, "1.2.3", self.url)

    def test_wrong_release_or_origin_fails(self):
        for url in (self.url.replace("1.2.3", "1.2.4"), self.url.replace("code.podesta.ai", "example.org"), self.url + "?token=bad"):
            with self.subTest(url=url), self.assertRaises(ValueError):
                winget.manifests(self.root, "1.2.3", url)


if __name__ == "__main__":
    unittest.main()
