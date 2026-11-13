"""The `tilo` install script (dist/install/tilo.sh, served as https://tilo.sh):
it installs a release only when the archive matches its SHA256SUMS and, where
openssl exists, the SHA256SUMS carries a signature by the release key; it needs
no root and trusts the key Tilo's releases are signed with (keys/cosign.pub).

Each case runs the real script under `sh` against a release on disk
(`file://`), signed with a throwaway P-256 key given as a mirror's key.
"""

import hashlib
import io
import os
import pathlib
import shutil
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


@unittest.skipUnless(shutil.which("openssl"), "the install script verifies with openssl, which this image lacks")
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

    def install(self, *args, **env):
        return subprocess.run(
            ["sh", "-s", *args],
            input=SCRIPT.read_text(),
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

    def test_plain_http_only_from_this_computer(self):
        import functools
        import http.server
        import threading

        handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=str(self.release.parent))
        handler.log_message = lambda *args: None
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.shutdown)
        port = server.server_address[1]
        done = self.install(TILO_RELEASES=f"http://127.0.0.1:{port}")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(run(self.installed).stdout, b"tilo 0.0.0\n")
        self.installed.unlink()
        # A name that only starts like this computer's leaves it: refused
        # before anything is asked of it.
        done = self.install(TILO_RELEASES=f"http://127.0.0.1.example:{port}")
        self.assertNotEqual(done.returncode, 0)
        self.assertFalse(self.installed.exists())

    def test_arguments_run_the_installed_command_by_its_path_where_path_does_not_name_it(self):
        # `curl … | sh -s login CODE` on a fresh machine: ~/.local/bin is not on PATH, and the
        # agent's shell never rereads the profile that would add it.
        self.publish(b'#!/bin/sh\necho "ran tilo $*"; read -r line && echo "read $line"\nexit 3\n')
        done = self.install("login", "K7Q-MX2P", "--site", "https://tilo.example")
        self.assertEqual(done.returncode, 3, "the command's own status is the install's")
        self.assertEqual(done.stdout, "ran tilo login K7Q-MX2P --site https://tilo.example\n",
                         "its input is not the rest of the script")
        self.assertNotIn(str(self.installed.parent), os.environ["PATH"].split(":"))
        self.assertNotIn("Next:", done.stderr, "what comes next is the command it ran")

    def test_a_site_serving_it_with_its_own_key_in_place_needs_nothing_else(self):
        # A copy of tilo.sh (os/containers/tilo-sh/stack.sh) serves this script with the key it
        # signs its releases with in place of Tilo's, so its setup command works as pasted, in a
        # shell that set nothing, and runs the tilo it installed, not an older one on PATH.
        old = pathlib.Path(self.scratch.name) / "old"
        old.mkdir()
        (old / "tilo").write_text("#!/bin/sh\necho old tilo\n")
        (old / "tilo").chmod(0o755)
        self.publish(b'#!/bin/sh\necho "ran tilo $*"\n')
        script = SCRIPT.read_text().replace((REPO / "keys/cosign.pub").read_text().strip(), self.pub.read_text().strip())
        done = subprocess.run(
            ["sh", "-s", "login", "K7Q-MX2P"],
            input=script,
            capture_output=True,
            text=True,
            env={"PATH": f"{old}:{os.environ['PATH']}", "HOME": str(self.home), "TILO_RELEASES": f"file://{self.release.parent}"},
        )
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(done.stdout, "ran tilo login K7Q-MX2P\n")

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
        self.assertIn("does not match the release's SHA256SUMS", done.stderr)
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

    def without_openssl(self):
        """A PATH holding every tool the installer may use except openssl, as on
        stock Fedora."""
        bin_dir = pathlib.Path(self.scratch.name) / "no-openssl-bin"
        bin_dir.mkdir(exist_ok=True)
        for tool in ("sh", "uname", "tar", "gzip", "curl", "wget", "sha256sum", "shasum", "awk", "cut", "mktemp",
                     "rm", "cp", "chmod", "mv", "mkdir", "cat", "printf"):
            found = shutil.which(tool)
            if found and not (bin_dir / tool).exists():
                (bin_dir / tool).symlink_to(found)
        return str(bin_dir)

    def test_without_openssl_the_sums_alone_decide(self):
        (self.release / "SHA256SUMS.sig").unlink()
        done = self.install(PATH=self.without_openssl())
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(run(self.installed).stdout, b"tilo 0.0.0\n")
        self.archive.write_bytes(self.archive.read_bytes() + b"\0")
        self.installed.unlink()
        done = self.install(PATH=self.without_openssl())
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("does not match the release's SHA256SUMS", done.stderr)
        self.assertFalse(self.installed.exists())

    def test_a_release_without_this_machine_installs_nothing(self):
        self.publish(b"x", sums="0" * 64 + "  tilo-plan9-mips.tar.gz\n")
        done = self.install()
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("has no", done.stderr)
        self.assertFalse(self.installed.exists())

@unittest.skipUnless(shutil.which("openssl"), "the install script verifies with openssl, which this image lacks")
class Release(unittest.TestCase):
    """A release as tools/ci/publish/cli-release.sh lays it out: its archives packed and summed by
    it, SHA256SUMS signed as the release key signs it (a DER ECDSA P-256 signature over SHA-256,
    here by a local key; the rehearsal check signs it through cosign as KMS does)."""

    RELEASE = REPO / "tools/ci/publish/cli-release.sh"

    def test_the_installer_installs_what_the_release_tooling_packs_and_sums(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = pathlib.Path(scratch)
            program = root / "tilo"
            program.write_bytes(b"#!/bin/sh\necho released tilo\n")
            program.chmod(0o755)
            builds = root / "builds"
            builds.mkdir()
            for system in ("linux-x86_64", "linux-aarch64", "macos-aarch64", "macos-x86_64"):
                run("bash", self.RELEASE, "pack", program, builds / f"tilo-{system}.tar.gz", env={**os.environ, "TMPDIR": scratch})
            release = root / "releases/v1.2.3"
            run("bash", self.RELEASE, "sums", release, builds, env={**os.environ, "TMPDIR": scratch})
            again = root / "again"
            run("bash", self.RELEASE, "pack", program, again, env={**os.environ, "TMPDIR": scratch})
            self.assertEqual(again.read_bytes(), (builds / "tilo-linux-x86_64.tar.gz").read_bytes(), "packing is reproducible")
            key, pub = root / "release.key", root / "release.pub"
            run("openssl", "ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", key)
            run("openssl", "ec", "-in", key, "-pubout", "-out", pub)
            run("openssl", "dgst", "-sha256", "-sign", key, "-out", release / "SHA256SUMS.sig", release / "SHA256SUMS")
            home = root / "home"
            home.mkdir()
            done = subprocess.run(
                ["sh", SCRIPT],
                capture_output=True,
                text=True,
                env={
                    "PATH": os.environ["PATH"],
                    "HOME": str(home),
                    "TILO_RELEASES": f"file://{root / 'releases'}",
                    "TILO_VERSION": "v1.2.3",
                    "TILO_RELEASE_KEY": str(pub),
                },
            )
            self.assertEqual(done.returncode, 0, done.stderr)
            self.assertEqual(run(home / ".local/bin/tilo").stdout, b"released tilo\n")
            names = sorted(line.split()[1] for line in (release / "SHA256SUMS").read_text().splitlines())
            self.assertEqual(names, sorted(f"tilo-{s}.tar.gz" for s in ("linux-aarch64", "linux-x86_64", "macos-aarch64", "macos-x86_64")))

    def test_a_release_without_both_linux_archives_is_refused(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = pathlib.Path(scratch)
            program = root / "tilo"
            program.write_bytes(b"x")
            run("bash", self.RELEASE, "pack", program, root / "tilo-linux-x86_64.tar.gz", env={**os.environ, "TMPDIR": scratch})
            done = subprocess.run(["bash", self.RELEASE, "sums", root / "out", root], capture_output=True, text=True, env={**os.environ, "TMPDIR": scratch})
            self.assertNotEqual(done.returncode, 0)
            self.assertIn("tilo-linux-aarch64.tar.gz", done.stderr)


class Trust(unittest.TestCase):
    def test_the_script_trusts_the_key_releases_are_signed_with(self):
        script = SCRIPT.read_text()
        key = (REPO / "keys/cosign.pub").read_text().strip()
        self.assertIn(key, script, "keys/cosign.pub changed: the install script must trust the same key")
        self.assertTrue(script.rstrip().endswith('main "$@"'), "a download cut short runs nothing")


if __name__ == "__main__":
    unittest.main()
