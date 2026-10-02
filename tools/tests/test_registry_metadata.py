#!/usr/bin/env python3
"""Contract tests for publisher-authored Tilo registry presentation data."""

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
                self.assertEqual(len(entries), 57)
                self.assertEqual(len(list(media.glob("*"))), 81)
                for path in entries:
                    entry = json.loads(path.read_text(encoding="utf-8"))
                    with self.subTest(app=path.stem):
                        for field in ("name", "description", "tagline"):
                            self.assertTrue(entry[field]["en"].strip())
                        self.assertEqual(entry["supported_languages"], sorted(entry["supported_languages"]))
                        self.assertIn("en", entry["supported_languages"])
                        self.assertTrue(entry["images"]["en"])
                        self.assertEqual(entry["source_url"], "https://code.tilo.so/tilo/tilo")
                        for image in entry["images"]["en"]:
                            self.assertTrue(image["url"].startswith("https://apps.tilo.so/"))
                            self.assertTrue(image["alt"].strip())
                            self.assertTrue((media / image["url"].rsplit("/", 1)[1]).is_file())
        with self.subTest(phase='audit reports the intended generic fleet'):
            result = self.run_tool("audit")
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads(result.stdout)
            self.assertEqual(report["count"], 57)
            self.assertNotIn("browser", report["apps"])


if __name__ == "__main__":
    unittest.main()
