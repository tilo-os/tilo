import importlib.util
import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).with_name("release-catalog.py")
GITHUB_RELEASES = {"TILO_RELEASE_REPOSITORY": "tilo-os/releases", "TILO_RELEASE_SERVER": "https://github.com"}
SPEC = importlib.util.spec_from_file_location("release_catalog", SCRIPT)
catalog = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(catalog)


class CatalogSelectionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def write(self, version, schema=None, directory=None):
        path = (directory or self.root) / f"tilo-installer-catalog-v{version}.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps({"schema": version if schema is None else schema,
                                    "tag": "unreleased", "releases": []}))
        return path

    def test_selection_and_embedding_accept_exactly_one_supported_catalog(self):
        with self.subTest(phase="real native embedding accepts both supported versions"):
            for version in (1, 2):
                with self.subTest(version=version):
                    source = self.write(version)
                    destination = self.root / "release-catalog.json"
                    result = subprocess.run([sys.executable, str(SCRIPT), str(self.root),
                                             "--copy-to", str(destination)], capture_output=True)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(destination.read_bytes(), source.read_bytes())
                    source.unlink()

        self.setUp()
        with self.subTest(phase="empty selection is rejected"):
            with self.assertRaisesRegex(ValueError, "exactly one"):
                catalog.select(self.root)

        self.setUp()
        with self.subTest(phase="two versions and duplicate nested catalogs are rejected"):
            first = self.write(1)
            second = self.write(2)
            with self.assertRaisesRegex(ValueError, "exactly one"):
                catalog.select(self.root)
            second.unlink()
            self.write(1, directory=self.root / "duplicate")
            with self.assertRaisesRegex(ValueError, "exactly one"):
                catalog.select(self.root)
            self.assertTrue(first.is_file())

        self.setUp()
        with self.subTest(phase="filename must match supported integer schema"):
            for version, schema in ((1, 2), (2, 1), (3, 3), (1, True), (1, "1")):
                with self.subTest(version=version, schema=schema):
                    path = self.write(version, schema)
                    with self.assertRaises(ValueError):
                        catalog.select(self.root)
                    path.unlink()

        self.setUp()
        with self.subTest(phase="failed embedding preserves existing destination"):
            self.write(1, 2)
            destination = self.root / "release-catalog.json"
            destination.write_bytes(b"existing authenticated build input")
            result = subprocess.run([sys.executable, str(SCRIPT), str(self.root),
                                     "--copy-to", str(destination)], capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(destination.read_bytes(), b"existing authenticated build input")

    def media_fixture(self):
        entries = []
        for arch, profile in (("x86_64", "generic"), ("aarch64", "generic"), ("aarch64", "lenovo-83ed")):
            suffix = "" if profile == "generic" else f"-{profile}"
            name = f"tilo-installer-linux-{arch}{suffix}.img.zst"
            path = self.root / name
            path.write_bytes(name.encode())
            entry = {"arch": arch, "hardware_profile": profile,
                     "url": f"https://github.com/tilo-os/releases/releases/download/imager-v1.2.3/{name}",
                     "archive_size": path.stat().st_size, "image_size": 1024,
                     "archive_sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                     "image_sha256": "b" * 64}
            if profile != "generic":
                entry["provenance"] = {"source_image_id": "sha256:" + "a" * 64,
                                       "input_qcow2_sha256": "c" * 64,
                                       "hardware_profile_sha256": "d" * 64,
                                       "target_update_ref": "code.tilo.so/tilo/tilo:lenovo-fixture"}
            entries.append(entry)
        return {"schema": 2, "tag": "imager-v1.2.3", "releases": entries}

    def test_media_entries_are_bound_to_the_configured_release_origin(self):
        with self.subTest(phase="media rejects unexpected origin, tag, credentials, port and path"), patch.dict(os.environ, GITHUB_RELEASES):
            data = self.media_fixture()
            self.assertEqual(len(catalog.media(data, self.root)), 3)
            original = data["releases"][-1]["url"]
            bad_urls = [original.replace("github.com", "example.invalid"),
                        original.replace("github.com", "user@github.com"),
                        original.replace("github.com", "github.com:444"),
                        original.replace("imager-v1.2.3", "imager-v9.9.9"),
                        original.replace("/tilo-os/releases/", "/other/releases/"),
                        original + "?download=1", original + "#fragment",
                        original.replace("lenovo-83ed", "other-board")]
            for url in bad_urls:
                with self.subTest(url=url):
                    data["releases"][-1]["url"] = url
                    with self.assertRaisesRegex(ValueError, "configured release origin"):
                        catalog.media(data, self.root)

        self.setUp()
        with self.subTest(phase="board provenance is mandatory and uses the OS origin contract"), patch.dict(os.environ, GITHUB_RELEASES):
            original = self.media_fixture()
            for field in ("source_image_id", "input_qcow2_sha256", "hardware_profile_sha256", "target_update_ref"):
                data = copy.deepcopy(original)
                del data["releases"][-1]["provenance"][field]
                with self.subTest(missing=field), self.assertRaises(ValueError):
                    catalog.media(data, self.root)
            for field in ("hardware_profile", "provenance"):
                data = copy.deepcopy(original)
                del data["releases"][-1][field]
                with self.subTest(missing=field), self.assertRaises(ValueError):
                    catalog.media(data, self.root)
            data = copy.deepcopy(original)
            data["releases"][-1]["provenance"]["target_update_ref"] = "example.invalid/tilo:latest"
            with self.assertRaisesRegex(ValueError, "signature-required"):
                catalog.media(data, self.root)

        self.setUp()
        with self.subTest(phase="internal bundle entries are checked but are not media"), patch.dict(os.environ, GITHUB_RELEASES):
            data = self.media_fixture()
            entry = {"purpose": "tilo-internal-installer", "layout": "internal-bundle-v2",
                     "release_id": "imager-v1.2.3", "arch": "x86_64",
                     "manifest_url": "https://code.tilo.so/api/packages/tilo/generic/tilo-internal-installer/"
                                     "imager-v1.2.3/tilo-internal-installer-x86_64.json",
                     "manifest_sha256": "e" * 64, "manifest_size": 4096}
            data["releases"].append(entry)
            self.assertEqual(len(catalog.media(data, self.root)), 3)
            for field, bad in (("layout", "internal-bundle-v1"), ("release_id", "imager-v9.9.9"),
                               ("manifest_url", entry["manifest_url"].replace("code.tilo.so", "github.com")),
                               ("manifest_sha256", "E" * 64), ("manifest_size", 65537), ("hardware_profile", "generic")):
                with self.subTest(field=field):
                    changed = copy.deepcopy(data)
                    changed["releases"][-1][field] = bad
                    with self.assertRaises(ValueError):
                        catalog.media(changed, self.root)

        self.setUp()
        with self.subTest(phase="explicit public release origin configuration is respected"):
            data = self.media_fixture()
            for entry in data["releases"]:
                entry["url"] = entry["url"].replace("https://github.com/tilo-os/releases/",
                                                   "https://downloads.example.invalid/team/public/")
            with patch.dict(os.environ, {"TILO_RELEASE_REPOSITORY": "team/public",
                                         "TILO_RELEASE_SERVER": "https://downloads.example.invalid"}):
                self.assertEqual(len(catalog.media(data, self.root)), 3)
    def test_target_policy_entries_are_checked_and_are_not_media(self):
        with patch.dict(os.environ, GITHUB_RELEASES):
            data = self.media_fixture()
            entry = {"purpose": "tilo-target-policy", "layout": "target-policy-bundle-v1",
                     "release_id": "imager-v1.2.3", "arch": "x86_64", "generation": 11,
                     "target_uki_sha256": "e" * 64,
                     "url": "https://code.tilo.so/api/packages/tilo/generic/tilo-target-policy/"
                            "imager-v1.2.3/tilo-target-policy-x86_64.tar",
                     "archive_sha256": "f" * 64, "archive_size": 4096}
            data["releases"].append(entry)
            self.assertEqual(len(catalog.media(data, self.root)), 3)
            for field, bad in (("layout", "target-policy-bundle-v0"),
                               ("release_id", "imager-v9.9.9"),
                               ("generation", 0),
                               ("target_uki_sha256", "E" * 64),
                               ("archive_sha256", "not-a-digest"),
                               ("archive_size", 0),
                               ("url", entry["url"].replace("code.tilo.so", "github.com")),
                               ("hardware_profile", "generic")):
                with self.subTest(field=field):
                    changed = copy.deepcopy(data)
                    changed["releases"][-1][field] = bad
                    with self.assertRaises(ValueError):
                        catalog.media(changed, self.root)

if __name__ == "__main__":
    unittest.main()
