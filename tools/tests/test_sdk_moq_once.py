"""Exercise the shared MoQ setup and licenses through its production CLI."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
HELPER = ROOT / "tools/ci/prepare_sdk_moq.py"


class SharedMoqTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.helper = self.root / "tools/ci/prepare_sdk_moq.py"
        self.helper.parent.mkdir(parents=True)
        shutil.copyfile(HELPER, self.helper)
        self.builder = self.root / "scripts/moq-browser"
        self.builder.mkdir(parents=True)
        (self.builder / "package-lock.json").write_text('{"lockfileVersion":3}')
        self.assets = self.root / "crates/browser/tilo-browser-runtime/web"
        self.assets.mkdir(parents=True)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.trace = self.root / "trace"
        for name in ("npm", "node"):
            launcher = self.bin / name
            launcher.write_text(
                "#!/usr/bin/env python3\nimport pathlib,json,os,sys\n"
                "name=pathlib.Path(sys.argv[0]).name\n"
                "with open(os.environ['TRACE'],'a') as f:f.write(json.dumps([name,*sys.argv[1:]])+'\\n')\n"
                "status=int(os.environ.get(name.upper()+'_EXIT','0'))\n"
                "if status:sys.exit(status)\n"
                "if name=='node':\n"
                " root=pathlib.Path.cwd().parents[1]\n"
                " out=root/'crates/browser/tilo-browser-runtime/web'\n"
                " (out/'media-moq-library.mjs').write_text('export const pinned = true;\\n')\n"
                " (out/'media-moq-library.LICENSE.txt').write_text('Matching pinned licenses\\n')\n"
            )
            launcher.chmod(0o755)
        self.env = {**os.environ, "PATH": str(self.bin) + os.pathsep + os.environ["PATH"],
                    "TRACE": str(self.trace)}

    def invoke(self, *args):
        return subprocess.run([sys.executable, str(self.helper), *map(str, args)],
                              env=self.env, capture_output=True, text=True)

    def test_one_pinned_install_and_build_serves_two_complete_bundles(self):
        prepared = self.invoke("prepare")
        self.assertEqual(prepared.returncode, 0, prepared.stderr)
        for app in ("calculator", "chat"):
            bundle = self.root / app
            bundle.mkdir()
            shutil.copy2(self.assets / "media-moq-library.mjs", bundle)
            self.assertGreaterEqual((bundle / "media-moq-library.mjs").stat().st_mtime_ns,
                                    (self.builder / "package-lock.json").stat().st_mtime_ns)
            result = self.invoke("license", bundle)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((bundle / "media-moq-library.LICENSE.txt").read_bytes(),
                             (self.assets / "media-moq-library.LICENSE.txt").read_bytes())
        calls = [json.loads(line) for line in self.trace.read_text().splitlines()]
        self.assertEqual(calls, [["npm", "ci", "--no-audit", "--no-fund", "--silent"],
                                 ["node", "build.mjs"]])

    def test_failed_install_stops_before_build_or_copy(self):
        self.env["NPM_EXIT"] = "17"
        result = self.invoke("prepare")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(self.trace.read_text().splitlines()), 1)
        self.assertFalse((self.assets / "media-moq-library.mjs").exists())

    def test_changed_bundle_module_fails_without_adding_wrong_licenses(self):
        self.assertEqual(self.invoke("prepare").returncode, 0)
        bundle = self.root / "changed"
        bundle.mkdir()
        (bundle / "media-moq-library.mjs").write_text("different module")
        result = self.invoke("license", bundle)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((bundle / "media-moq-library.LICENSE.txt").exists())

    def test_missing_matching_licenses_cannot_qualify(self):
        self.assertEqual(self.invoke("prepare").returncode, 0)
        (self.assets / "media-moq-library.LICENSE.txt").unlink()
        bundle = self.root / "unlicensed"
        bundle.mkdir()
        shutil.copy2(self.assets / "media-moq-library.mjs", bundle)
        self.assertNotEqual(self.invoke("license", bundle).returncode, 0)


if __name__ == "__main__":
    unittest.main()
