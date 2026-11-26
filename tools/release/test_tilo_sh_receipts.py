import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("tilo_sh_receipts.py")
COMMIT = "a" * 40
REF = "tag:v1.2.3"


class TiloShReceipts(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name)
        self.rows = []
        self.receipts = {}
        for i, (name, directory) in enumerate((("lounge", "lounge/release"), ("web-entry", "web-entry"), ("api", "api"))):
            repository = f"registry.tilo.so/tilo/{name}"
            digest = "sha256:" + str(i) * 64
            image = f"{repository}@{digest}"
            self.rows.append(f"{name} {image}")
            path = self.root / directory / "receipt.json"
            path.parent.mkdir(parents=True)
            receipt = dict(repository=repository, digest=digest, image=image, sourceRevision=COMMIT, reference=REF)
            path.write_text(json.dumps(receipt))
            self.receipts[name] = (path, receipt)

    def report(self):
        return subprocess.run([sys.executable, "-I", str(SCRIPT), str(self.root)], capture_output=True, text=True,
                              env={**os.environ, "T_COMMIT": COMMIT, "T_REF": REF})

    def test_reports_the_tested_commit_and_its_three_images(self):
        result = self.report()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), [f"source {COMMIT}", *self.rows])

    def test_rejects_unrelated_receipts_before_reporting_any_images(self):
        path, api = self.receipts["api"]
        for field, wrong in (("sourceRevision", "b" * 40), ("reference", "main"),
                             ("repository", "registry.tilo.so/tilo/lounge"), ("digest", "sha256:" + "0" * 64),
                             ("image", "registry.tilo.so/tilo/api:latest")):
            with self.subTest(field=field):
                path.write_text(json.dumps({**api, field: wrong}))
                result = self.report()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("api", result.stderr)
                self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
