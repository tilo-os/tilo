"""verify-rpm-files.sh against a stand-in rpm, without an image."""

import os
from pathlib import Path
import stat
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/verify-rpm-files.sh"


class VerifyRpmFilesTests(unittest.TestCase):
    def run_with(self, verify_output, epoch="100"):
        with tempfile.TemporaryDirectory() as tmp:
            rpm = Path(tmp) / "rpm"
            listing = Path(tmp) / "verify.txt"
            listing.write_text(verify_output)
            rpm.write_text(
                "#!/bin/sh\n"
                'if [ "$1" = -qa ]; then printf "50 base-1.0\\n100 ours-1.0\\n103 ours-2.0\\n"; exit 0; fi\n'
                f'printf "%s\\n" "$*" > {tmp}/args\n'
                f"cat {listing}\n"
                "exit 1\n"
            )
            rpm.chmod(rpm.stat().st_mode | stat.S_IEXEC)
            env = dict(os.environ, PATH=f"{tmp}:{os.environ['PATH']}", SOURCE_DATE_EPOCH=epoch)
            result = subprocess.run(["sh", str(SCRIPT)], env=env, capture_output=True, text=True)
            args = (Path(tmp) / "args").read_text() if (Path(tmp) / "args").exists() else ""
            return result, args

    def test_only_this_builds_packages_are_verified(self):
        result, args = self.run_with("")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("ours-1.0", args)
        self.assertIn("ours-2.0", args)
        self.assertNotIn("base-1.0", args)

    def test_lost_file_data_fails(self):
        result, _ = self.run_with("..5......    /usr/lib64/libmfx-gen.so.1.2.16\n")
        self.assertEqual(result.returncode, 1)
        self.assertIn("libmfx-gen.so.1.2.16", result.stderr)
        result, _ = self.run_with("S.5......    /usr/bin/tool\n")
        self.assertEqual(result.returncode, 1)

    def test_lost_data_is_located(self):
        with tempfile.TemporaryDirectory() as tmp:
            lost = Path(tmp) / "lib.so"
            lost.write_bytes(b"x" * 65536 + bytes(4096 * 10) + b"y" * 5000)
            result, _ = self.run_with(f"..5......    {lost}\n")
        self.assertEqual(result.returncode, 1)
        self.assertIn("10 of 28 4 KiB blocks all zero: 16-25", result.stderr)

    def test_expected_changes_pass(self):
        listing = "\n".join([
            "missing     /usr/share/doc/ours/README",
            "S.5......  c /etc/ours.conf",
            "S.5......  g /var/lib/ours/state",
            "S.5......    /usr/lib64/python3.14/__pycache__/ast.cpython-314.pyc",
            ".M.......    /usr/lib64",
        ]) + "\n"
        result, _ = self.run_with(listing)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_unset_epoch_skips(self):
        result, args = self.run_with("..5......    /usr/bin/tool\n", epoch="0")
        self.assertEqual(result.returncode, 0)
        self.assertEqual(args, "")


if __name__ == "__main__":
    unittest.main()
