#!/usr/bin/env python3
"""Exercise checksum, source identity and application of downstream patches."""

import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from restart_fixture import restart_fixture

DEV = Path(__file__).resolve().parents[1] / "dev"
spec = importlib.util.spec_from_file_location("fex_patches", DEV / "apply-fex-patches.py")
PATCHES = importlib.util.module_from_spec(spec)
spec.loader.exec_module(PATCHES)


class PatchTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # The git fixture dominates setUp: build it once and give every test
        # (and merged phase) its own copy, so no test shares mutable state.
        template = tempfile.TemporaryDirectory()
        cls.addClassCleanup(template.cleanup)
        cls.template = Path(template.name) / "fixture"
        fixture = object.__new__(cls)
        fixture.root = cls.template
        fixture.source = fixture.root / "source"
        fixture.source.mkdir(parents=True)
        fixture.series = fixture.root / "patches"
        fixture.series.mkdir()
        fixture.git("init", "-q")
        fixture.git("config", "user.name", "Fixture")
        fixture.git("config", "user.email", "fixture@example.invalid")
        fixture.git("config", "core.autocrlf", "false")
        (fixture.source / "file.txt").write_text("old\n")
        fixture.git("add", "file.txt")
        fixture.git("commit", "-qm", "fixture")
        (fixture.source / "file.txt").write_text("new\n")
        patch = subprocess.check_output(["git", "-C", str(fixture.source), "diff", "--binary"])
        fixture.git("restore", "file.txt")
        (fixture.series / "fix.patch").write_bytes(patch)
        fixture.manifest = {"schema_version": 1, "base_revision": fixture.git("rev-parse", "HEAD"),
                            "patches": [{"file": "fix.patch", "sha256": hashlib.sha256(patch).hexdigest()}]}
        fixture.save()
        cls.template_manifest = json.dumps(fixture.manifest)

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name) / "fixture"
        shutil.copytree(self.template, self.root, symlinks=True)
        self.source = self.root / "source"
        self.series = self.root / "patches"
        self.manifest = json.loads(self.template_manifest)

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.source), *args], text=True).strip()

    def save(self):
        (self.series / "series.json").write_text(json.dumps(self.manifest))

    def test_patch_application(self):
        with self.subTest(phase="verified_patch_applies_and_records_identity"):
            self.assertEqual(PATCHES.apply(self.source, self.series), self.manifest)
            self.assertEqual((self.source / "file.txt").read_text(), "new\n")
            self.assertIn("file.txt", self.git("diff", "--cached", "--name-only"))
        restart_fixture(self)
        with self.subTest(phase="reapplying_does_not_silently_accept_modified_source"):
            PATCHES.apply(self.source, self.series)
            with self.assertRaisesRegex(ValueError, "clean"):
                PATCHES.apply(self.source, self.series)
        restart_fixture(self)
        with self.subTest(phase="dirty_input_is_preserved"):
            (self.source / "untracked").write_text("keep")
            with self.assertRaisesRegex(ValueError, "clean"):
                PATCHES.apply(self.source, self.series)
            self.assertEqual((self.source / "untracked").read_text(), "keep")

    def test_later_patch_failure_preserves_clean_checkout_and_index(self):
        bad = (self.series / 'fix.patch').read_bytes().replace(b'-old\n', b'-absent\n')
        (self.series / 'later.patch').write_bytes(bad)
        self.manifest['patches'].append({
            'file': 'later.patch', 'sha256': hashlib.sha256(bad).hexdigest()})
        self.save()
        with self.assertRaises(subprocess.CalledProcessError):
            PATCHES.apply(self.source, self.series)
        self.assertEqual((self.source / 'file.txt').read_text(), 'old\n')
        self.assertEqual(self.git('status', '--porcelain'), '')

    def test_dependent_patches_apply_in_order(self):
        later = (self.series / 'fix.patch').read_bytes().replace(b'-old\n', b'-new\n').replace(b'+new\n', b'+last\n')
        (self.series / 'later.patch').write_bytes(later)
        self.manifest['patches'].append({
            'file': 'later.patch', 'sha256': hashlib.sha256(later).hexdigest()})
        self.save()
        PATCHES.apply(self.source, self.series)
        self.assertEqual((self.source / 'file.txt').read_text(), 'last\n')
        self.assertEqual(self.git('diff', '--name-only'), '')
        self.assertEqual(self.git('diff', '--cached', '--name-only'), 'file.txt')

    def test_patch_rejections(self):
        with self.subTest(phase="corrupt_patch_fails_before_source_mutation"):
            (self.series / "fix.patch").write_bytes(b"changed")
            with self.assertRaisesRegex(ValueError, "checksum"):
                PATCHES.apply(self.source, self.series)
            self.assertEqual(self.git("status", "--porcelain"), "")
        restart_fixture(self)
        with self.subTest(phase="other_revision_is_rejected"):
            self.manifest["base_revision"] = "0" * 40
            self.save()
            with self.assertRaisesRegex(ValueError, "base"):
                PATCHES.apply(self.source, self.series)
        restart_fixture(self)
        with self.subTest(phase="escape_and_duplicate_names_are_rejected"):
            self.manifest["patches"][0]["file"] = "../fix.patch"
            self.save()
            with self.assertRaisesRegex(ValueError, "filename"):
                PATCHES.patch_record(self.series)
            self.manifest["patches"][0]["file"] = "fix.patch"
            self.manifest["patches"] *= 2
            self.save()
            with self.assertRaisesRegex(ValueError, "duplicate"):
                PATCHES.patch_record(self.series)


if __name__ == "__main__":
    unittest.main()
