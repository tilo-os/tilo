"""authselect-apply-changes runs only when something it applies from changed.

    python3 -m unittest os/tests/test_authselect_unchanged.py
"""
import configparser
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "os/usr/libexec/tilo-authselect-unchanged"
DROP_IN = ROOT / "os/usr/lib/systemd/system/authselect-apply-changes.service.d/10-tilo-skip-unchanged.conf"


class AuthselectUnchangedTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        self.write("usr/share/authselect/default/local/nsswitch.conf", "passwd: files\n")
        self.write("etc/authselect/authselect.conf", "local\nwith-silent-lastlog\n")

    def write(self, relative, text):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def run_script(self, mode):
        env = {**os.environ, "TILO_AUTHSELECT_ROOT": str(self.root)}
        return subprocess.run(["sh", str(SCRIPT), mode], env=env, capture_output=True, text=True).returncode

    def skips(self):
        """What the drop-in's ExecCondition decides: True to skip the unit."""
        return self.run_script("check") == 0

    def test_a_boot_with_no_recorded_apply_runs_it(self):
        self.assertFalse(self.skips())

    def test_an_unchanged_boot_after_an_apply_skips_it(self):
        self.assertEqual(self.run_script("record"), 0)
        self.assertTrue(self.skips())

    def test_a_changed_profile_runs_it_again(self):
        self.run_script("record")
        self.write("usr/share/authselect/default/local/nsswitch.conf", "passwd: files systemd\n")
        self.assertFalse(self.skips())

    def test_a_changed_selection_or_custom_profile_runs_it_again(self):
        self.run_script("record")
        self.write("etc/authselect/authselect.conf", "local\nwith-fingerprint\n")
        self.assertFalse(self.skips())
        self.run_script("record")
        self.write("etc/authselect/custom/mine/nsswitch.conf", "passwd: files\n")
        self.assertFalse(self.skips())

    def test_a_customised_setup_with_unchanged_profiles_is_not_reapplied(self):
        self.write("etc/authselect/authselect.conf", "custom/mine\n")
        self.write("etc/authselect/custom/mine/nsswitch.conf", "passwd: files\n")
        self.run_script("record")
        self.assertTrue(self.skips())

    def test_the_drop_in_skips_only_on_an_unchanged_boot_and_records_after_a_run(self):
        unit = configparser.ConfigParser(interpolation=None, strict=False)
        unit.read_string(DROP_IN.read_text())
        condition = unit["Service"]["ExecCondition"]
        self.assertIn("tilo-authselect-unchanged check && exit 1; exit 0", condition)
        self.assertEqual(unit["Service"]["ExecStartPost"], "/usr/libexec/tilo-authselect-unchanged record")
        image = (ROOT / "os/Containerfile").read_text()
        self.assertIn("authselect-apply-changes.service.d/10-tilo-skip-unchanged.conf", image)
        self.assertIn("os/usr/libexec/tilo-authselect-unchanged", image)


if __name__ == "__main__":
    unittest.main()
