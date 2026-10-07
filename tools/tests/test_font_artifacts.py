import importlib.util
import io
import json
import os
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("font_catalog", ROOT / "tools/font-catalog.py")
fc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fc)
SOURCE = ROOT / "crates/sdk/ui-core/assets/fonts"
REVIEWED = fc.prepared_root(SOURCE, "fixtures")


class FontArtifactsTest(unittest.TestCase):
    def metadata_source(self, destination):
        shutil.copytree(SOURCE / "LICENSES", destination / "LICENSES")
        for name in ("manifest.toml", "PROVENANCE.md", "artifacts.json"):
            shutil.copyfile(SOURCE / name, destination / name)

    def warm_cache(self, cache, rows):
        for row in rows.values():
            target = cache / row["sha256"] / row["file"]
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(REVIEWED / row["file"], target)

    def test_full_offline_catalog_without_source_fonts_preserves_licenses_and_scopes(self):
        _, rows = fc.artifact_entries(SOURCE)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            metadata = root / "metadata"
            metadata.mkdir()
            self.metadata_source(metadata)
            cache = root / "cache"
            self.warm_cache(cache, rows)
            with patch.object(fc.urllib.request, "urlopen", side_effect=AssertionError("offline network")):
                for scope in fc.SCOPES:
                    dest = root / scope
                    fc.prepare(metadata, dest, scope, cache, True)
                    expected = fc.SCOPES[scope]
                    self.assertEqual(fc.verify(dest, scope), (expected[1], fc.manifest(SOURCE)[expected[2]]))
                    for entry in fc.manifest(SOURCE)["license"]:
                        self.assertEqual((dest / entry["file"]).read_bytes(), (SOURCE / entry["file"]).read_bytes())
                    self.assertEqual((dest / "PROVENANCE.md").read_bytes(), (SOURCE / "PROVENANCE.md").read_bytes())
                    self.assertEqual((dest / "NewCM10-Regular.otf").exists(), scope != "product")

    def test_parallel_builds_publish_one_verified_catalog_and_reject_corruption(self):
        _, rows = fc.artifact_entries(SOURCE)
        with tempfile.TemporaryDirectory() as temporary:
            cache = Path(temporary)
            self.warm_cache(cache, rows)
            with patch.object(fc.urllib.request, "urlopen", side_effect=AssertionError("offline network")):
                with ThreadPoolExecutor(max_workers=2) as pool:
                    roots = list(pool.map(lambda _: fc.prepared_root(SOURCE, "fixtures", cache, True), range(2)))
                self.assertEqual(roots[0], roots[1])
                self.assertEqual(fc.verify(roots[0], "fixtures"), (37, 55771564))
                notice = roots[0] / "PROVENANCE.md"
                notice.write_bytes(notice.read_bytes() + b"changed")
                with self.assertRaisesRegex(ValueError, "provenance"):
                    fc.prepared_root(SOURCE, "fixtures", cache, True)

    def test_self_contained_offline_catalog_is_verified_without_a_digest_cache(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            supplied = root / "fonts"
            fc.stage(REVIEWED, supplied, "fixtures")
            with patch.dict(os.environ, {"TILO_FONT_CATALOG_ROOT": str(supplied)}):
                with patch.object(fc.urllib.request, "urlopen", side_effect=AssertionError("offline network")):
                    self.assertEqual(fc.prepared_root(SOURCE, "fixtures", root / "absent", True), supplied)
                    fc.prepare(SOURCE, root / "product", "product", root / "absent", True)
                    self.assertEqual(fc.verify(root / "product", "product"), (35, 54174180))
                    target = supplied / fc.manifest(SOURCE)["font"][0]["file"]
                    data = bytearray(target.read_bytes())
                    data[-1] ^= 1
                    target.write_bytes(data)
                    with self.assertRaisesRegex(ValueError, "SHA-256"):
                        fc.prepared_root(SOURCE, "fixtures", root / "absent", True)

    def test_corrupt_or_missing_cache_fails_without_replacement_or_partial_catalog(self):
        _, rows = fc.artifact_entries(SOURCE)
        row = next(iter(rows.values()))
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            cache = root / "cache"
            with patch.object(fc.urllib.request, "urlopen", side_effect=AssertionError("unexpected network")):
                with self.assertRaisesRegex(ValueError, "offline"):
                    fc.cached_font(row, cache, True)
                self.warm_cache(cache, {row["file"]: row})
                target = cache / row["sha256"] / row["file"]
                data = bytearray(target.read_bytes())
                data[-1] ^= 1
                target.write_bytes(data)
                with self.assertRaisesRegex(ValueError, "SHA-256"):
                    fc.cached_font(row, cache, False)
                with self.assertRaises(ValueError):
                    fc.prepare(SOURCE, root / "catalog", "fixtures", cache, True)
                self.assertFalse((root / "catalog").exists())
                self.assertFalse(list(root.glob(".tilo-fonts-*")))

    def test_changed_license_never_produces_an_installable_catalog(self):
        _, rows = fc.artifact_entries(SOURCE)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            metadata = root / "metadata"
            metadata.mkdir()
            self.metadata_source(metadata)
            cache = root / "cache"
            self.warm_cache(cache, rows)
            notice = metadata / fc.manifest(SOURCE)["license"][0]["file"]
            notice.write_bytes(notice.read_bytes() + b"changed")
            with patch.object(fc.urllib.request, "urlopen", side_effect=AssertionError("offline network")):
                with self.assertRaisesRegex(ValueError, "SHA-256"):
                    fc.prepare(metadata, root / "catalog", "product", cache, True)
            self.assertFalse((root / "catalog").exists())

    def test_invalid_inventory_cannot_fetch_an_unreviewed_destination(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.metadata_source(root)
            original = json.loads((SOURCE / "artifacts.json").read_text())
            variants = []
            for field, value in (("file", "../font.ttf"), ("bytes", True),
                                 ("sha256", "0" * 64), ("url", "https://example.com/font.ttf")):
                candidate = json.loads(json.dumps(original))
                candidate["assets"][0][field] = value
                variants.append(candidate)
            variants.append({**original, "assets": original["assets"][:-1]})
            variants.append({**original, "assets": original["assets"] + [original["assets"][0]]})
            with patch.object(fc.urllib.request, "urlopen", side_effect=AssertionError("unreviewed network")):
                for candidate in variants:
                    with self.subTest(candidate=candidate):
                        (root / "artifacts.json").write_text(json.dumps(candidate))
                        with self.assertRaises(ValueError):
                            fc.artifact_entries(root)

    def test_truncated_excessive_and_changed_downloads_are_not_published(self):
        _, rows = fc.artifact_entries(SOURCE)
        row = min(rows.values(), key=lambda item: item["bytes"])
        original = (REVIEWED / row["file"]).read_bytes()
        for data in (original[:-1], original + b"x", bytes([original[0] ^ 1]) + original[1:]):
            with self.subTest(length=len(data)), tempfile.TemporaryDirectory() as temporary:
                response = io.BytesIO(data)
                response.geturl = lambda: row["url"]
                with patch.object(fc.urllib.request, "urlopen", return_value=response):
                    with self.assertRaises(ValueError):
                        fc.cached_font(row, Path(temporary), False)
                self.assertFalse(any(path.is_file() for path in Path(temporary).rglob("*")))

    def test_verified_download_is_reused_offline_and_existing_destination_is_preserved(self):
        _, rows = fc.artifact_entries(SOURCE)
        row = min(rows.values(), key=lambda item: item["bytes"])
        data = (REVIEWED / row["file"]).read_bytes()
        with tempfile.TemporaryDirectory() as temporary:
            cache = Path(temporary)
            response = io.BytesIO(data)
            response.geturl = lambda: row["url"]
            with patch.object(fc.urllib.request, "urlopen", return_value=response) as request:
                path = fc.cached_font(row, cache, False)
                request.assert_called_once_with(row["url"], timeout=60)
            with patch.object(fc.urllib.request, "urlopen", side_effect=AssertionError("offline network")):
                self.assertEqual(fc.cached_font(row, cache, True).read_bytes(), data)
                with self.assertRaisesRegex(ValueError, "already exists"):
                    fc.prepare(SOURCE, cache, "product", cache, True)
                self.assertEqual(path.read_bytes(), data)


if __name__ == "__main__":
    unittest.main()
