#!/usr/bin/env python3
"""The image supplies the authenticated offline Flathub default to Flatpak."""
import base64
import configparser
import hashlib
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
DESCRIPTOR = "os/usr/share/flatpak/remotes.d/flathub.flatpakrepo"


class FlatpakDefaults(unittest.TestCase):
    def test_predefined_default_is_enabled_and_uses_the_official_signing_key(self):
        descriptor = configparser.ConfigParser()
        descriptor.read(ROOT / DESCRIPTOR)
        remote = descriptor["Flatpak Repo"]
        self.assertEqual(remote["Title"], "Flathub")
        self.assertEqual(remote["Url"], "https://dl.flathub.org/repo/")
        self.assertFalse(remote.getboolean("Disabled", fallback=False))
        key = base64.b64decode(remote["GPGKey"], validate=True)
        # Public key bytes from the official descriptor, not a network fetch
        # during boot; primary fingerprint 6E5C05D979C76DAF93C081354184DD4D907A7CAE.
        self.assertEqual(hashlib.sha256(key).hexdigest(),
                         "8bdc20abc4e19c0796460beb5bfe0e7aa4138716999e19c6f2dbdd78cc41aeaa")
        self.assertNotIn("NoGPGVerify", remote)

    def test_current_image_delivers_the_offline_predefined_remote(self):
        image = (ROOT / "os/Containerfile").read_text()
        self.assertIn(f"COPY {DESCRIPTOR} /usr/share/flatpak/remotes.d/flathub.flatpakrepo", image)
        self.assertTrue(any(line.strip().removesuffix(chr(92)).strip() == "flatpak"
                            for line in image.splitlines()))
        # The predefined-remotes mechanism owns one-time application. A boot
        # remote-add would recreate an intentionally removed source.
        for unit in (ROOT / "os/usr/lib/systemd").rglob("*.service"):
            self.assertNotIn("flathub.flatpakrepo", unit.read_text(), str(unit))


if __name__ == "__main__":
    unittest.main()
