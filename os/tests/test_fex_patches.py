#!/usr/bin/env python3
"""Exercise checksum, source identity and application of downstream patches."""

import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

DEV = Path(__file__).resolve().parents[1] / "dev"
spec = importlib.util.spec_from_file_location("fex_patches", DEV / "apply-fex-patches.py")
PATCHES = importlib.util.module_from_spec(spec)
spec.loader.exec_module(PATCHES)


class PatchTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.source = self.root / "source"
        self.source.mkdir()
        self.series = self.root / "patches"
        self.series.mkdir()
        self.git("init", "-q")
        self.git("config", "user.name", "Fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        self.git("config", "core.autocrlf", "false")
        (self.source / "file.txt").write_text("old\n")
        self.git("add", "file.txt")
        self.git("commit", "-qm", "fixture")
        (self.source / "file.txt").write_text("new\n")
        patch = subprocess.check_output(["git", "-C", str(self.source), "diff", "--binary"])
        self.git("restore", "file.txt")
        (self.series / "fix.patch").write_bytes(patch)
        self.manifest = {"schema_version": 1, "base_revision": self.git("rev-parse", "HEAD"),
                         "patches": [{"file": "fix.patch", "sha256": hashlib.sha256(patch).hexdigest()}]}
        self.save()

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.source), *args], text=True).strip()

    def save(self):
        (self.series / "series.json").write_text(json.dumps(self.manifest))

    def test_verified_patch_applies_and_records_identity(self):
        self.assertEqual(PATCHES.apply(self.source, self.series), self.manifest)
        self.assertEqual((self.source / "file.txt").read_text(), "new\n")
        self.assertIn("file.txt", self.git("diff", "--cached", "--name-only"))

    def test_corrupt_patch_fails_before_source_mutation(self):
        (self.series / "fix.patch").write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "checksum"):
            PATCHES.apply(self.source, self.series)
        self.assertEqual(self.git("status", "--porcelain"), "")

    def test_other_revision_is_rejected(self):
        self.manifest["base_revision"] = "0" * 40
        self.save()
        with self.assertRaisesRegex(ValueError, "base"):
            PATCHES.apply(self.source, self.series)

    def test_dirty_input_is_preserved(self):
        (self.source / "untracked").write_text("keep")
        with self.assertRaisesRegex(ValueError, "clean"):
            PATCHES.apply(self.source, self.series)
        self.assertEqual((self.source / "untracked").read_text(), "keep")

    def test_escape_and_duplicate_names_are_rejected(self):
        self.manifest["patches"][0]["file"] = "../fix.patch"
        self.save()
        with self.assertRaisesRegex(ValueError, "filename"):
            PATCHES.patch_record(self.series)
        self.manifest["patches"][0]["file"] = "fix.patch"
        self.manifest["patches"] *= 2
        self.save()
        with self.assertRaisesRegex(ValueError, "duplicate"):
            PATCHES.patch_record(self.series)

    def test_reapplying_does_not_silently_accept_modified_source(self):
        PATCHES.apply(self.source, self.series)
        with self.assertRaisesRegex(ValueError, "clean"):
            PATCHES.apply(self.source, self.series)


if __name__ == "__main__":
    unittest.main()
