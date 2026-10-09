#!/usr/bin/env python3
"""Native offline initialization matrix: python3 os/tests/flatpak_default_initialization_test.py.

Requires actual Flatpak, GPG and bubblewrap user namespaces; missing capabilities
fail. The digest-pinned ci-fast image does not currently include Flatpak.
"""
import base64
import configparser
import shlex
import shutil
import subprocess
import tempfile
from pathlib import Path
import unittest

from test_flatpak_defaults import ROOT, DESCRIPTOR


class FlatpakFirstLogin(unittest.TestCase):
    """Actual native CLI; only a private fixture repository is writable."""

    def setUp(self):
        self.assertIsNotNone(shutil.which("bwrap"), "bubblewrap is required; never skip behavior")
        self.assertTrue(Path("/usr/bin/flatpak").is_file(), "native Flatpak is required")
        self.directory = tempfile.TemporaryDirectory(prefix="tilo-flatpak-defaults-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        for part in ("install", "data/remotes.d", "config/remotes.d"):
            (self.root / part).mkdir(parents=True)
        self.unit = configparser.ConfigParser(interpolation=None)
        self.unit.read_string((ROOT / "os/usr/lib/systemd/system/tilo-flatpak-defaults.service").read_text())
        self.command = shlex.split(self.unit["Service"]["ExecStart"])
        # Namespace uid0 reproduces the root oneshot without acquiring host
        # root. Host root is read-only; /run has no bus; no network is present.
        self.sandbox = [
            "bwrap", "--unshare-user", "--uid", "0", "--gid", "0",
            "--unshare-pid", "--unshare-net", "--ro-bind", "/", "/",
            "--tmpfs", "/run", "--tmpfs", "/var", "--tmpfs", "/tmp",
            "--proc", "/proc", "--dev", "/dev", "--bind", str(self.root), "/tmp/fixture",
            "--clearenv", "--setenv", "PATH", "/usr/bin:/bin", "--setenv", "LC_ALL", "C",
            "--setenv", "FLATPAK_SYSTEM_DIR", "/tmp/fixture/install",
            "--setenv", "FLATPAK_DATA_DIR", "/tmp/fixture/data",
            "--setenv", "FLATPAK_CONFIG_DIR", "/tmp/fixture/config",
            "--setenv", "HOME", "/tmp", "--setenv", "XDG_CACHE_HOME", "/tmp/cache",
            "--setenv", "XDG_RUNTIME_DIR", "/run", "--die-with-parent", "--",
        ]

    def execute(self, command, *, success=True):
        result = subprocess.run(self.sandbox + command, capture_output=True, text=True, timeout=30)
        if success:
            self.assertEqual(result.returncode, 0, result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, "invalid trust material must refuse")
        return result

    def flatpak(self, *args):
        return self.execute(["/usr/bin/flatpak", *args])

    def ship_default(self):
        shutil.copyfile(ROOT / DESCRIPTOR, self.root / "data/remotes.d/flathub.flatpakrepo")

    def config(self):
        config = configparser.ConfigParser(interpolation=None)
        config.read(self.root / "install/repo/config")
        return config

    def assert_official(self):
        config = self.config()
        remote = config['remote "flathub"']
        self.assertEqual(remote["url"], "https://dl.flathub.org/repo/")
        self.assertTrue(remote.getboolean("gpg-verify"))
        self.assertTrue(remote.getboolean("gpg-verify-summary"))
        self.assertFalse(remote.getboolean("xa.disable", fallback=False))
        self.assertIn("flathub", config["core"]["xa.applied-remotes"].split(";"))
        self.assertTrue((self.root / "install/repo/flathub.trustedkeys.gpg").is_file())
        return config

    def test_first_login_has_official_default_offline(self):
        self.ship_default()
        self.assertIn("flathub", self.execute(self.command).stdout.splitlines())
        self.assert_official()
        before = (self.root / "install/repo/config").read_bytes()
        self.execute(self.command)
        self.assertEqual((self.root / "install/repo/config").read_bytes(), before)

    def test_upgrade_of_existing_repository_imports_new_default(self):
        legacy = (ROOT / DESCRIPTOR).read_text().replace("https://dl.flathub.org/repo/", "https://example.invalid/legacy/")
        (self.root / "legacy.flatpakrepo").write_text(legacy)
        self.flatpak("remote-add", "--system", "legacy", "/tmp/fixture/legacy.flatpakrepo")
        self.assertNotIn('remote "flathub"', self.config())
        self.ship_default()
        self.execute(self.command)
        config = self.assert_official()
        self.assertEqual(config['remote "legacy"']["url"], "https://example.invalid/legacy/")

    def test_disabled_choice_survives_next_boot_initialization(self):
        self.ship_default()
        self.execute(self.command)
        self.flatpak("remote-modify", "--system", "--disable", "flathub")
        before = (self.root / "install/repo/config").read_bytes()
        self.execute(self.command)
        self.assertTrue(self.config()['remote "flathub"'].getboolean("xa.disable"))
        self.assertEqual((self.root / "install/repo/config").read_bytes(), before)

    def test_removed_choice_survives_next_boot_initialization(self):
        self.ship_default()
        self.execute(self.command)
        self.flatpak("remote-delete", "--system", "flathub")
        before = (self.root / "install/repo/config").read_bytes()
        self.assertNotIn("flathub", self.execute(self.command).stdout.splitlines())
        self.assertNotIn('remote "flathub"', self.config())
        self.assertIn("flathub", self.config()["core"]["xa.applied-remotes"].split(";"))
        self.assertEqual((self.root / "install/repo/config").read_bytes(), before)

    def test_hidden_choice_survives_next_boot_initialization(self):
        self.ship_default()
        self.execute(self.command)
        self.flatpak("remote-modify", "--system", "--no-enumerate", "flathub")
        before = (self.root / "install/repo/config").read_bytes()
        self.execute(self.command)
        self.assertTrue(self.config()['remote "flathub"'].getboolean("xa.noenumerate"))
        self.assertEqual((self.root / "install/repo/config").read_bytes(), before)

    def test_invalid_key_refuses_without_marking_default_applied(self):
        descriptor = configparser.ConfigParser()
        descriptor.optionxform = str
        descriptor.read(ROOT / DESCRIPTOR)
        descriptor["Flatpak Repo"]["GPGKey"] = base64.b64encode(b"invalid public key").decode()
        with (self.root / "data/remotes.d/flathub.flatpakrepo").open("w") as output:
            descriptor.write(output)
        self.execute(self.command, success=False)
        self.assertNotIn('remote "flathub"', self.config())
        self.assertNotIn("flathub", self.config()["core"].get("xa.applied-remotes", "").split(";"))


if __name__ == "__main__":
    unittest.main()
