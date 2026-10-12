import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("test_assets", ROOT / "tools/test-assets.py")
assets = importlib.util.module_from_spec(spec)
spec.loader.exec_module(assets)


class TestAssets(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.cache = self.root / "cache"
        self.payload = b"immutable fixture bytes\n"
        digest = hashlib.sha256(self.payload).hexdigest()
        self.row = {"file": "case.png", "sha256": digest, "bytes": len(self.payload),
                    "url": f"https://code.tilo.so/api/packages/tilo/generic/source-evidence/{digest}/case.png"}
        self.path = self.root / "assets.json"
        self.write_manifest([self.row])

    def write_manifest(self, rows):
        self.path.write_text(json.dumps({"schema": 1, "assets": rows}))

    def fetch(self, url, timeout):
        self.assertEqual(url, self.row["url"])
        self.assertEqual(timeout, 60)
        return io.BytesIO(self.payload)

    def no_network(self, *args, **kwargs):
        self.fail("a cached/offline run must never open the network")

    def test_prewarmed_offline_run_uses_the_same_verified_bytes(self):
        warmed = assets.resolve(self.path, self.cache, opener=self.fetch)
        self.assertEqual(warmed[0].read_bytes(), self.payload)
        self.assertEqual(assets.resolve(self.path, self.cache, True, opener=self.no_network), warmed)
        with self.assertRaisesRegex(ValueError, "uncached"):
            assets.resolve(self.path, self.root / "empty", True, opener=self.no_network)

    def test_corrupt_cache_fails_instead_of_substituting_a_download(self):
        path = assets.resolve(self.path, self.cache, opener=self.fetch)[0]
        path.write_bytes(self.payload[:-2] + b"x\n")
        with self.assertRaisesRegex(ValueError, "SHA-256"):
            assets.resolve(self.path, self.cache, opener=self.no_network)
        path.write_bytes(b"short")
        with self.assertRaisesRegex(ValueError, "length"):
            assets.resolve(self.path, self.cache, True, opener=self.no_network)

    def test_bad_download_never_publishes_partial_or_excess_bytes(self):
        for payload in (b"short", self.payload[:-2] + b"x\n", self.payload + b"excess"):
            with self.subTest(payload=payload):
                with self.assertRaises(ValueError):
                    assets.resolve(self.path, self.cache, opener=lambda *a, **kw: io.BytesIO(payload))
                self.assertEqual(list(self.cache.rglob("*.*")), [])
                self.assertFalse((self.cache / self.row["sha256"] / "case.png").exists())

    def test_manifest_refuses_traversal_unpinned_urls_duplicates_and_bad_lengths(self):
        for field, value in (("file", "../case.png"), ("url", "https://example.com/case.png"),
                             ("sha256", "invalid"), ("bytes", True), ("bytes", assets.MAX_BYTES + 1)):
            with self.subTest(field=field):
                self.write_manifest([dict(self.row, **{field: value})])
                with self.assertRaises(ValueError):
                    assets.resolve(self.path, self.cache, opener=self.no_network)
        self.write_manifest([self.row, self.row])
        with self.assertRaisesRegex(ValueError, "duplicate"):
            assets.manifest(self.path)
        self.write_manifest([self.row])
        with self.assertRaisesRegex(ValueError, "not declared"):
            assets.resolve(self.path, self.cache, filename="unknown.png", opener=self.no_network)

    def test_review_collection_omits_identical_bytes_and_preserves_changed_or_new_work(self):
        candidates = self.root / "candidates"
        candidates.mkdir()
        path = candidates / "case.png"
        path.write_bytes(self.payload)
        assets.prune_identical_candidates(self.path, candidates)
        self.assertFalse(path.exists())
        path.write_bytes(b"changed")
        new = candidates / "new.png"
        new.write_bytes(b"new")
        assets.prune_identical_candidates(self.path, candidates)
        self.assertEqual(path.read_bytes(), b"changed")
        self.assertEqual(new.read_bytes(), b"new")

    def test_document_inputs_keep_pinned_names_and_reject_unknown_extensions(self):
        for extension in ("pdf", "epub", "docx", "odt"):
            name = f"case.{extension}"
            row = dict(self.row, file=name, url=self.row["url"].replace("case.png", name))
            self.write_manifest([row])
            resolved = assets.resolve(self.path, self.cache, opener=lambda *a, **kw: io.BytesIO(self.payload))
            self.assertEqual(resolved[0].read_bytes(), self.payload)
            self.assertEqual(assets.resolve(self.path, self.cache, True, opener=self.no_network), resolved)
        name = "case.exe"
        self.write_manifest([dict(self.row, file=name, url=self.row["url"].replace("case.png", name))])
        with self.assertRaisesRegex(ValueError, "filename"):
            assets.resolve(self.path, self.cache, opener=self.no_network)

    def test_all_six_real_cases_remain_pinned(self):
        rows = assets.manifest(ROOT / "crates/sdk/ui-core/tests/fixtures/typography/artifacts.json")
        self.assertEqual(len(rows), 6)
        self.assertEqual(sum(row["bytes"] for row in rows), 3718171)
        self.assertEqual({row["file"] for row in rows}, {
            f"{theme}-display-{display}-text-{text}.png"
            for theme in ("dark", "light")
            for display, text in (("1", "1"), ("1_25", "1"), ("1_5", "1_25"))})

    def test_shell_decision_states_and_wheel_remain_pinned(self):
        rows = assets.manifest(ROOT / "crates/session/shell/tests/snapshots/artifacts.json")
        self.assertEqual(len(rows), 3)
        self.assertEqual(sum(row["bytes"] for row in rows), 68243)
        self.assertEqual({row["file"] for row in rows}, {
            "e2e-tab-drag-reorder-settled.png",
            "e2e-maximized-tile-controls.png", "three-spokes.png"})

    def test_reader_linux_and_windows_baselines_remain_pinned(self):
        rows = assets.manifest(ROOT / "apps/ebook/tests/fixtures/typography/artifacts.json")
        self.assertEqual(len(rows), 4)
        self.assertEqual(sum(row["bytes"] for row in rows), 801087)
        self.assertEqual({row["file"] for row in rows}, {
            "serif-reader.png", "serif-reader-windows.png",
            "math-reader.png", "math-reader-windows.png"})


if __name__ == "__main__":
    unittest.main()
