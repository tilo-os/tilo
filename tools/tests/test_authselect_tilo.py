import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]

spec = importlib.util.spec_from_file_location("authselect_tilo", REPO / "os/scripts/authselect-tilo.py")
authselect_tilo = importlib.util.module_from_spec(spec)
assert spec.loader is not None
sys.modules[spec.name] = authselect_tilo
spec.loader.exec_module(authselect_tilo)

# The auth and password phases of authselect's `local` system-auth template.
SYSTEM_AUTH = """\
auth        required                                     pam_env.so
auth        required                                     pam_faildelay.so delay=2000000
auth        sufficient                                   pam_fprintd.so                                         {include if "with-fingerprint"}
auth        sufficient                                   pam_unix.so {if not "without-nullok":nullok}
auth        sufficient                                   pam_systemd_home.so                                    {include if "with-systemd-homed"}
auth        required                                     pam_deny.so

account     required                                     pam_unix.so

password    sufficient                                   pam_systemd_home.so                                    {include if "with-systemd-homed"}
password    requisite                                    pam_pwquality.so
password    sufficient                                   pam_unix.so yescrypt shadow {if not "without-nullok":nullok} use_authtok
password    required                                     pam_deny.so

session     required                                     pam_unix.so
"""


class AuthselectTiloTests(unittest.TestCase):
    def profile(self, text: str) -> Path:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        for name in authselect_tilo.TEMPLATES:
            (root / name).write_text(text, encoding="utf-8")
        return root

    def test_pam_tilo_comes_right_before_the_password_hash_check(self):
        root = self.profile(SYSTEM_AUTH)
        self.assertEqual(authselect_tilo.main(["authselect-tilo.py", str(root)]), 0)
        for name in authselect_tilo.TEMPLATES:
            lines = (root / name).read_text(encoding="utf-8").splitlines()
            auth = [line.split()[-1] if "pam_tilo" in line else line.split()[2] for line in lines
                    if line.lstrip("-").startswith("auth")]
            self.assertEqual(auth.index("pam_tilo.so") + 1, auth.index("pam_unix.so"))
            self.assertLess(auth.index("pam_fprintd.so"), auth.index("pam_tilo.so"))
            password = [line for line in lines if line.lstrip("-").startswith("password")]
            self.assertIn("pam_tilo.so", password[0], "nothing in the password phase runs first")
            self.assertIn(authselect_tilo.CONTROL, password[0])

    def test_running_twice_changes_nothing(self):
        root = self.profile(SYSTEM_AUTH)
        authselect_tilo.main(["authselect-tilo.py", str(root)])
        once = (root / "system-auth").read_text(encoding="utf-8")
        authselect_tilo.main(["authselect-tilo.py", str(root)])
        self.assertEqual((root / "system-auth").read_text(encoding="utf-8"), once)
        self.assertEqual(once.count("pam_tilo.so"), 2)

    def test_a_template_without_pam_unix_fails_the_build(self):
        root = self.profile("auth required pam_deny.so\npassword required pam_deny.so\n")
        with self.assertRaises(SystemExit):
            authselect_tilo.main(["authselect-tilo.py", str(root)])

    def test_the_shipped_templates_take_it_when_authselect_is_installed(self):
        shipped = Path("/usr/share/authselect/default/local")
        if not (shipped / "system-auth").is_file():
            self.skipTest("authselect is not installed here")
        for name in authselect_tilo.TEMPLATES:
            edited = authselect_tilo.edit((shipped / name).read_text(encoding="utf-8"), name)
            self.assertEqual(edited.count("pam_tilo.so"), 2)


if __name__ == "__main__":
    unittest.main()
