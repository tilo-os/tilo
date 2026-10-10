#!/usr/bin/env python3
"""Exercise the authenticated prior-collection extraction guard without network."""
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
RUNTIME = "runtime/tilo-runtime/2026.07+native.07ebd4fdaafcf63c676f16737f3fb233"


class PriorCollectionTest(unittest.TestCase):
    def reuse(self, member, *, link=False, prior_root="a" * 64):
        with tempfile.TemporaryDirectory(prefix="tilo-collection-test-") as directory:
            root = Path(directory)
            current = root / "current"
            runtime = current / RUNTIME
            runtime.mkdir(parents=True)
            (runtime / "bundle.toml").write_text('root_hash = "' + "a" * 64 + '"\n')
            def app(name):
                return {"name": name, "id": "so.tilo." + name, "package": name,
                        "version": "0.1.0", "runtime": "2026.07"}
            music, files = app("music"), app("files")
            index = current / "index.json"
            index.write_text(json.dumps({"apps": [music]}))
            plan = root / "plan.json"
            plan.write_text(json.dumps({"catalog": [music, files]}))
            archive = root / "collection.tar.gz"
            with tarfile.open(archive, "w:gz") as bundle:
                def add(name, data):
                    entry = tarfile.TarInfo(name)
                    entry.size = len(data)
                    bundle.addfile(entry, io.BytesIO(data))
                add("tapp/files.tapp", b"prior signed app fixture")
                add(RUNTIME + "/bundle.toml", ('root_hash = "' + prior_root + '"\n').encode())
                add("index.json", json.dumps({"apps": [files]}).encode())
                if link:
                    entry = tarfile.TarInfo(member)
                    entry.type = tarfile.SYMTYPE
                    entry.linkname = "outside"
                    bundle.addfile(entry)
                else:
                    add(member, b"font fixture")
            stub = root / "bin"
            stub.mkdir()
            curl = stub / "curl"
            curl.write_text("#!/usr/bin/env python3\nimport os,shutil,sys\n"
                            "assert '--proto' in sys.argv and '=https' in sys.argv\n"
                            "shutil.copyfile(os.environ['FIXTURE_ARCHIVE'], "
                            "sys.argv[sys.argv.index('--output') + 1])\n")
            curl.chmod(0o755)
            env = os.environ.copy()
            env.update({"PATH": str(stub) + os.pathsep + env["PATH"],
                        "TILO_PACKAGE_TOKEN": "fixture-only",
                        "TILO_PACKAGE_PUBLIC_BASE_URL": "https://packages.example.invalid/api/packages/tilo/generic",
                        "FIXTURE_ARCHIVE": str(archive), "RUNNER_TEMP": str(root)})
            destination = root / "tapps"
            result = subprocess.run(
                ["bash", str(ROOT / "tools/reuse_prior_app_collection.sh"), str(index),
                 str(plan), str(destination), "x86_64",
                 env["TILO_PACKAGE_PUBLIC_BASE_URL"] + "/collection/fixture/apps.tar.gz",
                 hashlib.sha256(archive.read_bytes()).hexdigest()],
                env=env, text=True, capture_output=True, timeout=10,
            )
            copied = (destination / "files.tapp").exists()
            if result.returncode == 0:
                self.assertEqual((destination / "files.tapp").read_bytes(), b"prior signed app fixture")
                self.assertEqual(len(json.loads(plan.read_text())["catalog"]), 2)
            self.assertFalse((root / "outside").exists())
            return result, copied

    def test_variable_fonts_are_reused(self):
        for name in ("AtkinsonHyperlegibleNext-Italic[wght].ttf", "Roboto[wdth,wght].ttf",
                     "Custom[GRAD].otf"):
            with self.subTest(name=name):
                result, copied = self.reuse(RUNTIME + "/lib/fonts/" + name)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertTrue(copied)

    def test_ordinary_runtime_members_remain_supported(self):
        result, copied = self.reuse(RUNTIME + "/lib/fonts/Regular.ttf")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(copied)

    def test_unsafe_or_malformed_members_are_rejected(self):
        for member in (
            "../outside", "/tmp/outside", RUNTIME + "/../outside",
            RUNTIME + "//lib/fonts/Test[wght].ttf",
            RUNTIME + "/lib/fonts/Test[].ttf", RUNTIME + "/lib/fonts/Test[wght.ttf",
            RUNTIME + "/lib/fonts/Test[wght]].ttf", RUNTIME + "/lib/fonts/Test[wght,].ttf",
            RUNTIME + "/lib/fonts/Test[weight].ttf", RUNTIME + "/lib/fonts/Test[wght].so",
            RUNTIME + "/lib/Test[wght].ttf", "tapp/Test[wght].tapp",
        ):
            with self.subTest(member=member):
                result, copied = self.reuse(member)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("unexpected prior collection member", result.stderr)
                self.assertFalse(copied)

    def test_valid_font_does_not_allow_symlinks(self):
        result, copied = self.reuse(RUNTIME + "/lib/fonts/Test[wght].ttf", link=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("links or special files", result.stderr)
        self.assertFalse(copied)

    def test_valid_font_does_not_allow_runtime_changes(self):
        result, copied = self.reuse(RUNTIME + "/lib/fonts/Test[wght].ttf", prior_root="b" * 64)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("shared runtime changed", result.stderr)
        self.assertFalse(copied)


if __name__ == "__main__":
    unittest.main()
