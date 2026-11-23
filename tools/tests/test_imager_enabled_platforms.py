"""The standalone installer's release on t: every platform's downloads are built and
installed before anything is published, and the real release check refuses an
incomplete, corrupt or (on a tag) unsigned set."""
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
ASSETS = ROOT / "tools/ci/imager-release/assets.sh"
ARCHES = ("x86_64", "aarch64")


def checks():
    """Every check of t.toml by name, its matrix expanded."""
    found = {}
    for check in tomllib.loads((ROOT / "t.toml").read_text())["check"]:
        arches = check.get("matrix", {}).get("arch", [None])
        for arch in arches:
            expand = (lambda text: text) if arch is None else (lambda text: text.replace("{arch}", arch))
            found[expand(check["name"])] = {**check, "needs": [expand(need) for need in check.get("needs", [])]}
    return found


def release(directory, signed):
    names = []
    for arch in ARCHES:
        names += [f"tilo-installer-linux-{arch}.img.zst"]
        names += [f"tilo-imager-linux-{arch}.{ext}" for ext in ("AppImage", "tar.gz", "flatpak")]
        names += [f"tilo-imager-windows-{arch}.{ext}" for ext in ("exe", "zip")]
        names += [f"tilo-imager-setup-windows-{arch}.exe"]
        names += [f"tilo-imager-macos-{arch}.{ext}" for ext in ("dmg", "zip")]
    for name in names:
        (directory / name).write_bytes(b"fixture")
        (directory / f"{name}.sha256").write_text(f"{hashlib.sha256(b'fixture').hexdigest()}  {name}\n")
        if signed:
            (directory / f"{name}.asc").write_text("signature fixture")
    (directory / "winget").mkdir()
    for name in ("Tilo.Imager.installer.yaml", "Tilo.Imager.yaml", "Tilo.Imager.locale.en-US.yaml"):
        (directory / "winget" / name).write_text("manifest fixture")


def verify(directory, ref=""):
    environment = {**os.environ, "T_REF": ref}
    return subprocess.run(["bash", str(ASSETS), "verify", str(directory)],
                          capture_output=True, text=True, env=environment)


class InstallerRelease(unittest.TestCase):
    def test_every_platform_is_built_and_installed_before_a_tag_publishes(self):
        found = checks()
        for arch in ARCHES:
            for platform in ("linux", "windows", "macos"):
                self.assertIn(f"package/{platform}-{arch}", found["release-assets/imager"]["needs"])
            self.assertIn(f"medium/imager-{arch}", found["release-assets/imager"]["needs"])
            self.assertIn(f"package/linux-{arch}", found[f"install/linux-{arch}"]["needs"])
            # Only the tag's build holds the capture identities, and only it is signed.
            self.assertNotIn("secrets", found[f"package/linux-{arch}"])
            self.assertEqual(["tag:imager-v*"], found[f"release/linux-{arch}"]["on"])
            self.assertIn(f"release/linux-{arch}", found[f"sign/linux-{arch}"]["needs"])
        publish = found["publish-github-release"]
        self.assertEqual(["tag:imager-v*"], publish["on"])
        for gate in ("release-assets/imager", "install/linux-x86_64", "install/linux-aarch64",
                     "wine/windows-x86_64-install", "install/macos-x86_64", "install/macos-aarch64",
                     "sign/linux-x86_64", "sign/windows-x86_64"):
            self.assertIn(gate, publish["needs"])
        for surface in ("publish-winget", "publish-homebrew-tap"):
            self.assertIn("publish-github-release", found[surface]["needs"])
            self.assertEqual(["tag:imager-v*"], found[surface]["on"])
        # Main's newest head builds every download: a break shows the day it happens.
        for name in ("package/linux-x86_64", "build/windows-x86_64", "release-assets/imager"):
            self.assertIn("idle", found[name]["on"])

    def test_the_release_check_refuses_what_a_person_should_not_download(self):
        with tempfile.TemporaryDirectory() as temp:
            assets = Path(temp)
            release(assets, signed=False)
            with self.subTest(phase="main's dry run takes the complete unsigned set"):
                good = verify(assets)
                self.assertEqual(0, good.returncode, good.stderr)
            with self.subTest(phase="a tag refuses an unsigned Linux download"):
                unsigned = verify(assets, "tag:imager-v1.2.3")
                self.assertNotEqual(0, unsigned.returncode)
                self.assertIn("missing release signature", unsigned.stderr)
            required = assets / "tilo-imager-macos-aarch64.dmg"
            with self.subTest(phase="a missing download is refused"):
                required.unlink()
                missing = verify(assets)
                self.assertNotEqual(0, missing.returncode)
                self.assertIn("missing release artifact: tilo-imager-macos-aarch64.dmg", missing.stderr)
            with self.subTest(phase="a download whose bytes changed is refused"):
                required.write_bytes(b"corrupt")
                corrupt = verify(assets)
                self.assertNotEqual(0, corrupt.returncode)
                self.assertIn("invalid release digest", corrupt.stderr)
        with tempfile.TemporaryDirectory() as temp:
            assets = Path(temp)
            release(assets, signed=True)
            with self.subTest(phase="a tag takes the complete signed set"):
                signed = verify(assets, "tag:imager-v1.2.3")
                self.assertEqual(0, signed.returncode, signed.stderr)


if __name__ == "__main__":
    unittest.main()
