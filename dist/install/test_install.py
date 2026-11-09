"""The `tilo` install script (dist/install/tilo.sh, served as https://tilo.sh):
it installs a release only when its SHA256SUMS carries a signature by the
release key and the archive matches it, needs no root, and trusts the key
Tilo's releases are signed with (keys/cosign.pub).

Each case runs the real script under `sh` against a release on disk
(`file://`), signed with a throwaway P-256 key given as a mirror's key.
"""

import hashlib
import io
import os
import pathlib
import subprocess
import tarfile
import tempfile
import unittest

REPO = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = REPO / "dist/install/tilo.sh"


def run(*args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, **kwargs)


def machine():
    system = {"Linux": "linux", "Darwin": "macos"}[os.uname().sysname]
    arch = {"x86_64": "x86_64", "amd64": "x86_64", "aarch64": "aarch64", "arm64": "aarch64"}[os.uname().machine]
    return f"tilo-{system}-{arch}.tar.gz"


class Install(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        root = pathlib.Path(self.scratch.name)
        self.key = root / "release.key"
        self.pub = root / "release.pub"
        run("openssl", "ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", self.key)
        run("openssl", "ec", "-in", self.key, "-pubout", "-out", self.pub)
        self.release = root / "releases/latest"
        self.release.mkdir(parents=True)
        self.home = root / "home"
        self.home.mkdir()
        self.archive = self.release / machine()
        self.publish(b"#!/bin/sh\necho tilo 0.0.0\n")

    def tearDown(self):
        self.scratch.cleanup()

    def publish(self, program, *, sums=None):
        data = io.BytesIO()
        with tarfile.open(fileobj=data, mode="w:gz") as tar:
            info = tarfile.TarInfo("tilo")
            info.size = len(program)
            info.mode = 0o755
            tar.addfile(info, io.BytesIO(program))
        self.archive.write_bytes(data.getvalue())
        digest = hashlib.sha256(data.getvalue()).hexdigest()
        (self.release / "SHA256SUMS").write_text(sums or f"{digest}  {self.archive.name}\n")
        run("openssl", "dgst", "-sha256", "-sign", self.key, "-out", self.release / "SHA256SUMS.sig", self.release / "SHA256SUMS")

    def install(self, **env):
        return subprocess.run(
            ["sh", SCRIPT],
            capture_output=True,
            text=True,
            env={
                "PATH": os.environ["PATH"],
                "HOME": str(self.home),
                "TILO_RELEASES": f"file://{self.release.parent}",
                "TILO_RELEASE_KEY": str(self.pub),
                **env,
            },
        )

    @property
    def installed(self):
        return self.home / ".local/bin/tilo"

    def test_a_signed_release_installs_to_the_home_directory_without_root(self):
        done = self.install()
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(run(self.installed).stdout, b"tilo 0.0.0\n")
        self.assertEqual(self.installed.stat().st_mode & 0o777, 0o755)
        self.assertIn("Add", done.stderr, "says how to reach it when its directory is not on PATH")
        self.assertIn("tilo login", done.stderr, "says what comes next")
        self.assertEqual(sorted(p.name for p in self.installed.parent.iterdir()), ["tilo"], "no leftovers")

    def test_another_directory_and_an_update_in_place(self):
        target = self.home / "bin"
        self.assertEqual(self.install(TILO_INSTALL_DIR=str(target)).returncode, 0)
        self.publish(b"#!/bin/sh\necho tilo 0.0.1\n")
        done = self.install(TILO_INSTALL_DIR=str(target), PATH=f"{target}:{os.environ['PATH']}")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(run(target / "tilo").stdout, b"tilo 0.0.1\n")
        self.assertNotIn("Add", done.stderr)

    def test_an_archive_that_does_not_match_the_signed_sums_installs_nothing(self):
        self.archive.write_bytes(self.archive.read_bytes() + b"\0")
        done = self.install()
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("does not match the signed release", done.stderr)
        self.assertFalse(self.installed.exists())

    def test_sums_signed_by_another_key_install_nothing(self):
        other = pathlib.Path(self.scratch.name) / "other.key"
        run("openssl", "ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", other)
        run("openssl", "dgst", "-sha256", "-sign", other, "-out", self.release / "SHA256SUMS.sig", self.release / "SHA256SUMS")
        done = self.install()
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("does not verify", done.stderr)
        self.assertFalse(self.installed.exists())

    def test_without_a_mirror_key_only_tilos_release_key_is_trusted(self):
        done = self.install(TILO_RELEASE_KEY="")
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("does not verify", done.stderr)
        self.assertFalse(self.installed.exists())

    def test_a_release_without_this_machine_installs_nothing(self):
        self.publish(b"x", sums="0" * 64 + "  tilo-plan9-mips.tar.gz\n")
        done = self.install()
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("has no", done.stderr)
        self.assertFalse(self.installed.exists())

    def test_the_script_trusts_the_key_releases_are_signed_with(self):
        script = SCRIPT.read_text()
        key = (REPO / "keys/cosign.pub").read_text().strip()
        self.assertIn(key, script, "keys/cosign.pub changed: the install script must trust the same key")
        self.assertTrue(script.rstrip().endswith('main "$@"'), "a download cut short runs nothing")


if __name__ == "__main__":
    unittest.main()
