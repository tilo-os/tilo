#!/usr/bin/env python3
"""Contract tests for publisher-authored Tilo registry presentation data."""

import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "tools/apps/registry_metadata.py"


class RegistryMetadataTests(unittest.TestCase):
    def run_tool(self, *args: str, cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, "-I", str(SCRIPT), *args],
            cwd=cwd or ROOT,
            text=True,
            capture_output=True,
            check=False,
        )

    def test_store_metadata_is_complete_for_the_intended_generic_fleet(self):
        with self.subTest(phase='every downloadable app has complete store metadata'):
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                output, media = root / "entries", root / "media"
                result = self.run_tool(
                    "build",
                    "--media-base", "https://apps.tilo.so/artifacts/tilo/generic/tilo-app-metadata/test",
                    "--output", str(output),
                    "--media-output", str(media),
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                entries = sorted(output.glob("*.json"))
                self.assertEqual(len(entries), 62)
                # Every screenshot, including the Notes image fixture, is archived.
                # Observe the exact icon output and the original Settings image pin.
                self.assertEqual({path.name for path in media.iterdir()},
                                 {f"{path.stem}.svg" for path in entries})
                settings = json.loads((output / "settings.json").read_text(encoding="utf-8"))
                self.assertEqual(len(settings["images"]["en"]), 5)
                self.assertEqual(settings["images"]["en"][-1]["url"],
                                 "https://apps.tilo.so/artifacts/tilo/generic/tilo-app-metadata/"
                                 "bd5d23b61827b873b51daf0d84e09db582104b5a93c617c733a5c282191eb96f/settings.png")
                image_count = 0
                for path in entries:
                    entry = json.loads(path.read_text(encoding="utf-8"))
                    with self.subTest(app=path.stem):
                        for field in ("name", "description", "tagline"):
                            self.assertTrue(entry[field]["en"].strip())
                        self.assertEqual(entry["supported_languages"], sorted(entry["supported_languages"]))
                        self.assertIn("en", entry["supported_languages"])
                        self.assertTrue(entry["images"]["en"])
                        self.assertEqual(entry["source_url"], "https://code.tilo.so/tilo/tilo")
                        image_count += len(entry["images"]["en"])
                        archive_file = ROOT / "apps" / path.stem / "shots" / "archive.json"
                        archived = json.loads(archive_file.read_text()) if archive_file.exists() else []
                        archive_urls = {item["url"] for item in archived}
                        for image in entry["images"]["en"]:
                            self.assertTrue(image["url"].startswith("https://apps.tilo.so/"))
                            self.assertTrue(image["alt"].strip())
                            if "/tilo-app-metadata/test/" in image["url"]:
                                self.assertTrue((media / image["url"].rsplit("/", 1)[1]).is_file())
                            else:
                                self.assertEqual(image["kind"], "screenshot")
                                self.assertIn(image["url"], archive_urls)
                self.assertEqual(image_count, 86)
        with self.subTest(phase='audit reports the intended generic fleet'):
            result = self.run_tool("audit")
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads(result.stdout)
            self.assertEqual(report["count"], 62)
            self.assertIn("browser", report["apps"])
            self.assertIn("messages", report["apps"])
            self.assertIn("chip-design", report["apps"])

    def test_archive_rejects_unpinned_urls_and_invalid_records(self):
        spec = importlib.util.spec_from_file_location("registry_metadata", SCRIPT)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            shots = root / "apps/example/shots"
            shots.mkdir(parents=True)
            sha = "a" * 64
            row = {"name": "image.png", "sha256": sha, "bytes": 12,
                   "url": f"https://apps.tilo.so/artifacts/tilo/generic/tilo-app-metadata/{sha}/image.png"}
            archive = shots / "archive.json"
            archive.write_text(json.dumps([row]))
            self.assertEqual(module.archived_screenshots(root, "example"), [row])
            for key, value in [("url", "https://elsewhere.test/image.png"),
                               ("name", "../image.png"), ("sha256", "unverified"),
                               ("bytes", 0), ("bytes", True)]:
                with self.subTest(field=key, value=value):
                    archive.write_text(json.dumps([row | {key: value}]))
                    with self.assertRaises(ValueError):
                        module.archived_screenshots(root, "example")
            archive.write_text(json.dumps([row, row]))
            with self.assertRaises(ValueError):
                module.archived_screenshots(root, "example")


if __name__ == "__main__":
    unittest.main()
