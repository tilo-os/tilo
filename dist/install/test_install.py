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
import socket
import subprocess
import tarfile
import tempfile
import time
import unittest
from concurrent.futures import ThreadPoolExecutor

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

    def publish(self, program, *, sums=None, task_program=None, task_program_link=None, daemon=None):
        """The release's archive for this machine: `tilo`, and beside it `tilolang-process` where a
        task program is given (a Linux release's), or a link of that name, and `tilo-runtime`
        where a daemon is given."""
        data = io.BytesIO()
        with tarfile.open(fileobj=data, mode="w:gz") as tar:
            for name, member in (("tilo", program), ("tilolang-process", task_program), ("tilo-runtime", daemon)):
                if member is None:
                    continue
                info = tarfile.TarInfo(name)
                info.size = len(member)
                info.mode = 0o755
                tar.addfile(info, io.BytesIO(member))
            if task_program_link is not None:
                link = tarfile.TarInfo("tilolang-process")
                link.type = tarfile.SYMTYPE
                link.linkname = task_program_link
                tar.addfile(link)
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
                "XDG_RUNTIME_DIR": str(self.home),
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
        self.assertIn(str(self.installed), done.stderr, "names it by its full path off PATH")
        self.assertIn("tilo login", done.stderr, "says what comes next")
        self.assertIn("Set up with your agent", done.stderr, "says where the code comes from")
        self.assertNotIn("agent connect", done.stderr, "login adds Tilo's tools to the agent itself")
        self.assertEqual(sorted(p.name for p in self.installed.parent.iterdir()), ["tilo"], "no leftovers")

    def test_a_pinned_prerelease_runs_its_own_login_and_keeps_the_default_release(self):
        # Two independently signed archives, as served from /releases on the same site.
        current = self.release
        self.release = current.with_name("v0.3.1-assistant.1")
        self.release.mkdir()
        self.archive = self.release / machine()
        self.publish(b'#!/bin/sh\nprintf "candidate %s\\n" "$*"\n')
        selected = self.install("login", "ticket", TILO_VERSION=self.release.name)
        self.assertEqual(selected.returncode, 0, selected.stderr)
        self.assertEqual(selected.stdout, "candidate login ticket\n")
        default = self.install()
        self.assertEqual(default.returncode, 0, default.stderr)
        self.assertEqual(run(self.installed).stdout, b"tilo 0.0.0\n")

    @property
    def task_program(self):
        return self.installed.parent / "tilolang-process"

    def test_a_release_with_the_task_program_installs_it_beside_tilo(self):
        # A Linux release: `tilo mcp` starts a task's programs in tilolang-process, which it looks
        # for beside itself.
        self.publish(b"#!/bin/sh\necho tilo 0.0.2\n", task_program=b"#!/bin/sh\necho task program 0.0.2\n")
        done = self.install()
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(subprocess.check_output([self.installed]), b"tilo 0.0.2\n")
        self.assertEqual(subprocess.check_output([self.task_program]), b"task program 0.0.2\n")
        self.assertEqual(self.task_program.stat().st_mode & 0o777, 0o755)
        self.assertEqual(sorted(p.name for p in self.installed.parent.iterdir()), ["tilo", "tilolang-process"], "no leftovers")
        # An update replaces both.
        self.publish(b"#!/bin/sh\necho tilo 0.0.3\n", task_program=b"#!/bin/sh\necho task program 0.0.3\n")
        self.assertEqual(self.install().returncode, 0)
        self.assertEqual(subprocess.check_output([self.installed]), b"tilo 0.0.3\n")
        self.assertEqual(subprocess.check_output([self.task_program]), b"task program 0.0.3\n")

    def test_a_release_with_the_daemon_installs_it_beside_tilo(self):
        # `tilo login` and `tilo mcp` start this computer's daemon from beside tilo.
        self.publish(b"#!/bin/sh\necho tilo 0.0.4\n", daemon=b"#!/bin/sh\necho daemon 0.0.4\n")
        done = self.install()
        self.assertEqual(done.returncode, 0, done.stderr)
        daemon = self.installed.parent / "tilo-runtime"
        self.assertEqual(subprocess.check_output([daemon]), b"daemon 0.0.4\n")
        self.assertEqual(daemon.stat().st_mode & 0o777, 0o755)
        self.assertEqual(sorted(p.name for p in self.installed.parent.iterdir()), ["tilo", "tilo-runtime"], "no leftovers")

    def test_a_release_that_is_refused_installs_neither_program(self):
        self.publish(b"#!/bin/sh\n", task_program=b"#!/bin/sh\n")
        self.archive.write_bytes(self.archive.read_bytes() + b"\0")
        done = self.install()
        self.assertNotEqual(done.returncode, 0)
        self.assertFalse(self.installed.exists())
        self.assertFalse(self.task_program.exists())

    def test_a_task_program_that_is_no_plain_file_installs_nothing(self):
        self.publish(b"#!/bin/sh\n", task_program_link="/bin/sh")
        done = self.install()
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("holds no plain tilolang-process", done.stderr)
        self.assertFalse(self.installed.exists())
        self.assertFalse(self.task_program.exists() or self.task_program.is_symlink())

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

    def test_hosted_setup_keeps_the_other_cli_and_refuses_other_profiles(self):
        version = "v0.3.1-assistant.1"
        target = self.home / ".local/share/tilo/cli" / version
        settings = {"TILO_VERSION": version, "TILO_HOSTED_SETUP": "1", "TILO_INSTALL_DIR": str(target)}
        self.installed.parent.mkdir(parents=True)
        self.installed.write_bytes(b"existing default CLI")
        self.release = self.release.with_name(version)
        self.release.mkdir()
        self.archive = self.release / machine()
        login_dir = (self.home / ("Library/Application Support" if os.uname().sysname == "Darwin" else ".local/state")) / "tilo/device"
        login = login_dir / "login.json"
        # macOS keeps state and data in the same place. Asking for CLI help makes
        # the real, empty device directory before the person's first login.
        keys = login_dir.parent
        user = "ad" * 32
        settings.update({"TILO_IDENTITY_ROOT": str(keys), "FIXTURE_LOGIN": str(login), "FIXTURE_USER": user})
        self.publish(b'''#!/bin/sh
if [ "$*" = "device public-key" ]; then printf '%064d\\n' 1; exit; fi
if [ "$1" = login ]; then
  mkdir -p "$TILO_IDENTITY_ROOT/device/certs" "$(dirname "$FIXTURE_LOGIN")"
  printf '  "user": "%s",\\n' "$FIXTURE_USER" > "$FIXTURE_LOGIN"
  printf 'this device' > "$TILO_IDENTITY_ROOT/device/certs/$FIXTURE_USER.bin"
fi
printf "%s %s" "$0" "$*"
''', daemon=b"candidate daemon")
        login.parent.mkdir(parents=True)
        login.write_text("existing login")

        refused = self.install("login", "ticket", **settings)
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("fresh agent account", refused.stderr)
        self.assertFalse(target.exists())
        self.assertEqual(login.read_text(), "existing login")
        login.unlink()
        self.assertNotEqual(self.install("login", "ticket", **{**settings, "TILO_INSTALL_DIR": str(self.installed.parent)}).returncode, 0)

        missing = self.install("login", "ticket", PATH=self.without_openssl(), **settings)
        self.assertNotEqual(missing.returncode, 0)
        self.assertIn("openssl", missing.stderr)
        self.assertFalse(target.exists())
        for name in (".unknown", "\n", "holder.lock", "login.json"):
            entry = login_dir / name
            entry.write_bytes(b"")
            refused = self.install("login", "ticket", **settings)
            self.assertNotEqual(refused.returncode, 0, name)
            entry.unlink()
        (login_dir / "certs").mkdir()
        self.assertNotEqual(self.install("login", "ticket", **settings).returncode, 0)
        (login_dir / "certs").rmdir()
        (login_dir / ".dangling").symlink_to(self.home / "missing")
        self.assertNotEqual(self.install("login", "ticket", **settings).returncode, 0)
        (login_dir / ".dangling").unlink()
        login_dir.rmdir()
        empty = self.home / "empty"
        empty.mkdir()
        login_dir.symlink_to(empty, target_is_directory=True)
        self.assertNotEqual(self.install("login", "ticket", **settings).returncode, 0)
        login_dir.unlink()
        login_dir.mkdir()
        with tempfile.TemporaryDirectory(dir="/tmp") as runtime:
            sockets = pathlib.Path(runtime) / "tilo"
            sockets.mkdir()
            with socket.socket(socket.AF_UNIX) as peer:
                peer.bind(str(sockets / "session-0000000000000001.api.sock"))
                refused = self.install("login", "ticket", **settings, XDG_RUNTIME_DIR=runtime)
                self.assertNotEqual(refused.returncode, 0, refused.stderr)
                self.assertIn("fresh agent account", refused.stderr)
        manager = self.home / ("launchctl" if os.uname().sysname == "Darwin" else "systemctl")
        manager.write_text('#!/bin/sh\ncase "$*" in *so.tilo.peer|*tilo-peer.service) exit 0;; *) exit 1;; esac\n')
        manager.chmod(0o755)
        active = self.install("login", "ticket", **settings, PATH=f"{self.home}:{os.environ['PATH']}")
        self.assertNotEqual(active.returncode, 0, active.stderr)
        self.assertIn("fresh agent account", active.stderr)
        # Installing identical signed files in advance cannot authorize someone else's device.
        empty_scaffold = self.install(**settings)
        self.assertEqual(empty_scaffold.returncode, 0, empty_scaffold.stderr)
        login.write_text("another installation's login")
        denied = self.install("login", "ticket", **settings)
        self.assertNotEqual(denied.returncode, 0)
        self.assertIn("fresh agent account", denied.stderr)
        self.assertEqual(login.read_text(), "another installation's login")
        login.unlink()
        private = keys / "device-private/keypair.bin"
        private.parent.mkdir(parents=True)
        private.write_bytes(b"another device key")
        self.assertNotEqual(self.install("login", "ticket", **settings).returncode, 0)
        private.unlink()
        private.parent.rmdir()
        for args in (("login", "ticket"), ("device", "add", "another-ticket")):
            ok = self.install(*args, **settings)
            self.assertEqual(ok.returncode, 0, ok.stderr)
            self.assertEqual(ok.stdout, f"{target}/tilo {' '.join(args)}")
        original = self.install("login", "ticket", **settings)
        self.assertEqual(original.returncode, 0, original.stderr)
        self.assertIn("already", (original.stdout + original.stderr).lower())
        self.assertNotIn(f"{target}/tilo login", original.stdout)
        # A different login, or a daemon started outside the service manager, cannot
        # take over a returning candidate's installation.
        cert = keys / "device/certs" / f"{user}.bin"
        cert.write_bytes(b"replaced certificate")
        self.assertNotEqual(self.install("device", "add", "ticket", **settings).returncode, 0)
        cert.write_bytes(b"this device")
        import fcntl
        with (login_dir / "holder.lock").open("w") as lock:
            lock.write(str(os.getpid()))
            lock.flush()
            fcntl.flock(lock, fcntl.LOCK_EX)
            blocked = self.install("device", "add", "ticket", **settings)
            self.assertNotEqual(blocked.returncode, 0)
            self.assertIn("fresh agent account", blocked.stderr)
        self.assertEqual(self.install("device", "add", "ticket", **settings).returncode, 0)
        (target / "tilo").write_bytes(b"different executable")
        changed = self.install("login", "ticket", **settings)
        self.assertNotEqual(changed.returncode, 0)
        self.assertIn("differs from the signed CLI", changed.stderr)
        self.assertEqual((target / "tilo").read_bytes(), b"different executable")
        self.assertEqual(self.installed.read_bytes(), b"existing default CLI")

    def test_an_interrupted_hosted_login_finishes_only_for_its_original_device_and_ticket(self):
        version = "v0.3.1-assistant.1"
        self.release = self.release.with_name(version)
        self.release.mkdir()
        self.archive = self.release / machine()
        target = self.home / ".local/share/tilo/cli" / version
        state = self.home / ("Library/Application Support" if os.uname().sysname == "Darwin" else ".local/state")
        state = state / "tilo/device"
        keys = self.home / "keys"
        key = keys / "device-private/keypair.bin"
        ticket = "tilo1-original-single-use-ticket"
        person = "ab" * 32
        device = "12" * 32
        log = self.home / "actions"
        settings = {"TILO_VERSION": version, "TILO_HOSTED_SETUP": "1", "TILO_INSTALL_DIR": str(target),
                    "TILO_IDENTITY_ROOT": str(keys), "STATE": str(state), "TICKET": ticket, "PERSON": person,
                    "DEVICE": device, "ACTIONS": str(log)}
        # The signed program models the boundary between approval and starting the daemon.
        # A returned ticket report requires the issuer's original person, key and ticket.
        program = br'''#!/bin/sh
set -eu
key=$TILO_IDENTITY_ROOT/device-private/keypair.bin
cert=$TILO_IDENTITY_ROOT/device/certs/$PERSON.bin
if [ "$*" = "device public-key" ]; then
  mkdir -p "$(dirname "$key")"
  [ -e "$key" ] || printf '%s\n' "$DEVICE" > "$key"
  cat "$key"
  exit
fi
if [ "$1" = device ] && [ "$2" = add ]; then
  echo device-add >> "$ACTIONS"
  exit
fi
[ "$1" = login ] || exit 2
if [ "${EXPLICIT_LOGIN_CONSENT:-}" = 1 ]; then
  [ "${TILO_HOSTED_SETUP:-}" != 1 ] && [ "${TILO_HOSTED_RESUME:-}" != 1 ] || exit 6
  echo ordinary-consent >> "$ACTIONS"
  exit
fi
[ "$TILO_HOSTED_ARCHIVE_SHA" = "$ARCHIVE" ] && [ "$TILO_HOSTED_SETUP" = 1 ] || exit 7
if [ "${TILO_HOSTED_RESUME:-}" = 1 ]; then
  [ "$2" = "$TICKET" ] && [ "$(cat "$key")" = "$DEVICE" ] &&
    grep -Fq "$PERSON" "$STATE/login.json" && [ "$(cat "$cert")" = "$DEVICE" ] || exit 3
  [ "${ISSUER_REFUSES:-}" != 1 ] || exit 3
  printf 'resumed\n' >> "$ACTIONS"
  exit
fi
echo enroll >> "$ACTIONS"
[ ! -e "$STATE/login.json" ] || exit 4
"$0" device public-key >/dev/null
mkdir -p "$STATE" "$(dirname "$cert")"
printf '{\n  "user": "%s"\n}\n' "$PERSON" > "$STATE/login.json"
printf '%s\n' "$DEVICE" > "$cert"
echo 'could not start enrolled device' >&2
exit 5
'''
        self.publish(program, daemon=b"pinned runtime")
        settings["ARCHIVE"] = hashlib.sha256(self.archive.read_bytes()).hexdigest()
        first = self.install("login", ticket, **settings)
        self.assertNotEqual(first.returncode, 0)
        self.assertIn("could not start enrolled device", first.stderr)
        self.assertEqual(log.read_text(), "enroll\n")
        login = state / "login.json"
        original_login = login.read_bytes()
        cert = keys / "device/certs" / f"{person}.bin"
        original_cert = cert.read_bytes()
        self.assertTrue(all(ticket.encode() not in path.read_bytes() for path in state.rglob("*") if path.is_file()))

        other_version = "v0.3.1-assistant.2"
        shutil.copytree(self.release, self.release.with_name(other_version))
        for name, args, overrides in (
            ("other ticket", ("login", "tilo1-another-ticket"), {}),
            ("other version", ("login", ticket), {"TILO_VERSION": other_version,
                                                  "TILO_INSTALL_DIR": str(target.with_name(other_version))}),
            ("browser sign-in", ("device", "add", "another-ticket"), {}),
        ):
            with self.subTest(name=name):
                denied = self.install(*args, **{**settings, **overrides})
                self.assertNotEqual(denied.returncode, 0, name)
                self.assertEqual(log.read_text(), "enroll\n")

        def denied():
            attempted = self.install("login", ticket, **settings)
            self.assertNotEqual(attempted.returncode, 0, attempted.stdout)
            self.assertEqual(log.read_text(), "enroll\n")

        receipt = state / "hosted-cli"
        original_receipt = self.home / "original-receipt"
        receipt.rename(original_receipt)
        denied()
        original_receipt.rename(receipt)
        binary = target / "tilo"
        different = program + b"# different binary\n"
        binary.write_bytes(different)
        denied()
        # A different binary newly signed at the same release URL is still a different install.
        packed = {p: p.read_bytes() for p in self.release.iterdir()}
        self.publish(different, daemon=b"pinned runtime")
        denied()
        for path, data in packed.items():
            path.write_bytes(data)
        binary.write_bytes(program)
        key.write_text("34" * 32)
        denied()
        key.write_text(device + "\n")
        other_person = "cd" * 32
        login.write_bytes(original_login.replace(person.encode(), other_person.encode()))
        (cert.parent / f"{other_person}.bin").write_bytes(original_cert)
        denied()
        (cert.parent / f"{other_person}.bin").unlink()
        login.write_bytes(original_login)

        refused = self.install("login", ticket, **settings, ISSUER_REFUSES="1")
        self.assertNotEqual(refused.returncode, 0)
        self.assertEqual(log.read_text(), "enroll\n")
        self.assertNotEqual(self.install("device", "add", "another-ticket", **settings).returncode, 0)
        completed = self.install("login", ticket, **settings)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(log.read_text(), "enroll\nresumed\n")
        self.assertEqual(login.read_bytes(), original_login)
        self.assertEqual(cert.read_bytes(), original_cert)

        def original_prompt_is_finished():
            previous = log.read_text()
            duplicate = self.install("login", ticket, **settings, ISSUER_REFUSES="1")
            self.assertEqual(duplicate.returncode, 0, duplicate.stderr)
            self.assertIn("already", (duplicate.stdout + duplicate.stderr).lower())
            self.assertEqual(log.read_text(), previous, "a finished prompt must not call login again")

        original_prompt_is_finished()
        self.assertNotEqual(self.install("login", "different-ticket", **settings).returncode, 0)
        lines = receipt.read_text().splitlines()
        self.assertEqual(len(lines), 3)
        for value in (hashlib.sha256(self.archive.read_bytes()).hexdigest(),
                      hashlib.sha256(ticket.encode()).hexdigest(), device):
            self.assertIn(value, lines[2].split())
        self.assertNotIn(ticket, receipt.read_text())
        linked = self.install("device", "add", "browser-ticket", **settings)
        self.assertEqual(linked.returncode, 0, linked.stderr)
        self.assertEqual(receipt.read_text().splitlines()[2], lines[2])
        original_prompt_is_finished()

        key.write_text("34" * 32)
        self.assertNotEqual(self.install("login", ticket, **settings).returncode, 0)
        key.write_text(device + "\n")
        self.publish(different, daemon=b"pinned runtime")
        binary.write_bytes(different)
        self.assertNotEqual(self.install("login", ticket, **settings).returncode, 0)
        self.assertEqual(log.read_text(), "enroll\nresumed\ndevice-add\n")
        for path, data in packed.items():
            path.write_bytes(data)
        binary.write_bytes(program)
        # The earlier installer recorded only the certified profile and the daemon generation.
        receipt.write_text("\n".join(lines[:2]) + "\n")
        legacy = self.install("device", "add", "browser-ticket", **settings)
        self.assertEqual(legacy.returncode, 0, legacy.stderr)
        self.assertEqual(log.read_text(), "enroll\nresumed\ndevice-add\ndevice-add\n")
        self.assertFalse("".join(receipt.read_text().splitlines()[2:]).strip())
        new_login = self.install("login", "newly-offered-ticket", **settings, EXPLICIT_LOGIN_CONSENT="1")
        self.assertEqual(new_login.returncode, 0, new_login.stderr)
        self.assertEqual(log.read_text().splitlines()[-1], "ordinary-consent")
        self.assertEqual(receipt.read_text().splitlines(), lines[:2])

    def hosted_setup_before_certification(self):
        version = "v0.3.1-assistant.1"
        self.release = self.release.with_name(version)
        self.release.mkdir()
        self.archive = self.release / machine()
        base = self.home / ("Library/Application Support" if os.uname().sysname == "Darwin" else ".local/state")
        state = base / "tilo/device"
        keys = state.parent if os.uname().sysname == "Darwin" else self.home / "keys"
        attempts = self.home / "attempts"
        program = br'''#!/bin/sh
set -eu
umask 077
key=$TILO_IDENTITY_ROOT/device-private/keypair.bin
if [ "$*" = "device public-key" ]; then
  mkdir -p "$TILO_IDENTITY_ROOT/device-private" "$TILO_IDENTITY_ROOT/device" "$TILO_IDENTITY_ROOT/user-identity"
  if [ ! -f "$key" ]; then
    [ "${TILO_HOSTED_EXISTING_KEY:-}" != 1 ] || exit 19
    printf '%032d' 1 > "$key"
  fi
  openssl dgst -sha256 -r "$key" | cut -d' ' -f1
  exit
fi
[ "$1" = login ] && [ "$TILO_HOSTED_SETUP" = 1 ] && [ "${TILO_HOSTED_RESUME:-}" != 1 ] || exit 18
echo attempt >> "$ATTEMPTS"
case "$RESULT" in
  retryable) exit 5 ;;
  terminal) exit 76 ;;
  park)
    touch "$HOME/attempt-running"
    n=0
    while [ ! -f "$HOME/finish-attempt" ] && [ "$n" -lt 200 ]; do sleep .05; n=$((n + 1)); done
    exit 5 ;;
  approved)
    mkdir -p "$STATE" "$TILO_IDENTITY_ROOT/device/certs"
    printf '{\n  "user": "%s"\n}\n' "$PERSON" > "$STATE/login.json"
    printf 'device certificate' > "$TILO_IDENTITY_ROOT/device/certs/$PERSON.bin" ;;
  *) exit 2 ;;
esac
'''
        self.publish(program, daemon=b"signed runtime")
        settings = {"TILO_VERSION": version, "TILO_HOSTED_SETUP": "1",
                    "TILO_INSTALL_DIR": str(self.home / ".local/share/tilo/cli" / version),
                    "TILO_IDENTITY_ROOT": str(keys), "STATE": str(state), "PERSON": "ab" * 32,
                    "ATTEMPTS": str(attempts)}
        return settings, state, keys, attempts

    def test_an_uncertified_device_retries_its_ticket_and_replaces_one_only_after_a_terminal_answer(self):
        settings, state, keys, attempts = self.hosted_setup_before_certification()
        original = "tilo1-test-original"
        replacement = "tilo1-test-replacement"

        def ask(ticket, result):
            return self.install("login", ticket, **settings, RESULT=result)

        def refused(ticket):
            calls = attempts.read_bytes()
            answer = ask(ticket, "approved")
            self.assertNotEqual(answer.returncode, 0)
            self.assertEqual(attempts.read_bytes(), calls, "uncertain state must be refused before asking to enroll")

        first = ask(original, "retryable")
        self.assertEqual(first.returncode, 5, first.stderr)
        key = keys / "device-private/keypair.bin"
        initial_key = key.read_bytes()
        refused(replacement)
        same = ask(original, "retryable")
        self.assertEqual(same.returncode, 5, same.stderr)
        self.assertEqual(attempts.read_text().splitlines(), ["attempt", "attempt"])
        refused(replacement)
        incomplete = state / "login.json"
        incomplete.write_text('{}')
        refused(original)
        incomplete.unlink()
        terminal = ask(original, "terminal")
        self.assertEqual(terminal.returncode, 76, terminal.stderr)

        other_version = "v0.3.1-assistant.2"
        shutil.copytree(self.release, self.release.with_name(other_version))
        target = pathlib.Path(settings["TILO_INSTALL_DIR"])
        other = {**settings, "TILO_VERSION": other_version, "TILO_INSTALL_DIR": str(target.with_name(other_version))}
        self.assertNotEqual(self.install("login", replacement, **other, RESULT="approved").returncode, 0)
        self.assertEqual(attempts.read_text().splitlines(), ["attempt"] * 3)
        saved = {p: p.read_bytes() for p in self.release.iterdir()}
        binary = target / "tilo"
        program = binary.read_bytes()
        self.publish(program + b"# other release\n", daemon=b"signed runtime")
        binary.write_bytes(program + b"# other release\n")
        refused(replacement)
        binary.write_bytes(program)
        for path, data in saved.items():
            path.write_bytes(data)
        for path in (state / "login.json", keys / "device/cert.bin", keys / "user-identity/pubkey.bin",
                     keys / "device/.hidden", keys / "device-private/.other-key"):
            with self.subTest(entry=path.name):
                path.write_bytes(b"partial identity")
                refused(replacement)
                path.unlink()
        foreign = keys / "device/certs" / ("12" * 32 + ".bin")
        foreign.parent.mkdir()
        foreign.write_bytes(b"another certificate")
        refused(replacement)
        foreign.unlink()
        foreign.parent.rmdir()
        kept_key = self.home / "original-key"
        key.rename(kept_key)
        refused(replacement)
        self.assertFalse(key.exists(), "checking a returning device must not create a new key")
        kept_key.rename(key)
        key.write_bytes(b"2" * 32)
        refused(replacement)
        key.write_bytes(initial_key)
        approved = ask(replacement, "approved")
        self.assertEqual(approved.returncode, 0, approved.stderr)
        self.assertEqual(attempts.read_text().splitlines(), ["attempt"] * 4)
        self.assertEqual(key.read_bytes(), initial_key)
        refused(original)

    def test_two_setup_prompts_cannot_ask_to_certify_the_same_device_at_once(self):
        settings, _, _, attempts = self.hosted_setup_before_certification()
        with ThreadPoolExecutor(max_workers=1) as worker:
            first = worker.submit(self.install, "login", "tilo1-test-original", **settings, RESULT="park")
            try:
                deadline = time.monotonic() + 20
                entered = self.home / "attempt-running"
                while not entered.exists() and not first.done() and time.monotonic() < deadline:
                    time.sleep(.05)
                self.assertTrue(entered.exists(), "the original setup must be waiting for the person's answer")
                for ticket in ("tilo1-test-original", "tilo1-other"):
                    second = self.install("login", ticket, **settings, RESULT="approved")
                    self.assertNotEqual(second.returncode, 0)
                    self.assertEqual(attempts.read_text().splitlines(), ["attempt"])
            finally:
                (self.home / "finish-attempt").touch()
            self.assertEqual(first.result(timeout=10).returncode, 5)
        retry = self.install("login", "tilo1-test-original", **settings, RESULT="approved")
        self.assertEqual(retry.returncode, 0, retry.stderr)
        self.assertEqual(attempts.read_text().splitlines(), ["attempt", "attempt"])

    @unittest.skipUnless(os.uname().sysname == "Linux" and shutil.which("cc"), "Linux daemon process identity")
    def test_a_returning_hosted_login_recognizes_its_undumpable_daemon(self):
        # The real runtime takes holder.lock, identifies itself, then reads the device key.
        # Linux hides /proc/PID/exe from the owning user once that process is undumpable.
        executable = self.home / "holder"
        source = r"""
#include <fcntl.h>
#include <stdio.h>
#include <sys/file.h>
#include <sys/prctl.h>
#include <unistd.h>
int main(int argc, char **argv) {
    int fd = open(argv[1], O_WRONLY | O_CREAT, 0600);
    if (fd < 0 || flock(fd, LOCK_EX | LOCK_NB) || ftruncate(fd, 0)
        || prctl(PR_SET_DUMPABLE, 0) || dprintf(fd, "%d", getpid()) < 1) return 1;
    for (;;) pause();
}
"""
        run("cc", "-x", "c", "-", "-o", executable, input=source.encode())
        self.release = self.release.with_name("v0.3.1-assistant.1")
        self.release.mkdir()
        self.archive = self.release / machine()
        target = self.home / ".local/share/tilo/cli" / self.release.name
        state = self.home / ".local/state/tilo/device"
        holder = state / "holder.lock"
        program = b'''#!/bin/sh
set -eu
if [ "$*" = "device public-key" ]; then printf '%064d\\n' 1; exit; fi
if [ "$1" = login ] && [ "${TILO_HOSTED_RESUME:-}" != 1 ]; then
    [ ! -f "$STATE/login.json" ] || exit 9
    mkdir -p "$STATE" "$TILO_IDENTITY_ROOT/device/certs"
    printf '  "user": "%s",\\n' "$PERSON" > "$STATE/login.json"
    printf 'device certificate' > "$TILO_IDENTITY_ROOT/device/certs/$PERSON.bin"
fi
if [ ! -s "$STATE/holder.lock" ]; then
    "$TILO_INSTALL_DIR/tilo-runtime" "$STATE/holder.lock" </dev/null >/dev/null 2>&1 &
    for i in 1 2 3 4 5; do [ -s "$STATE/holder.lock" ] && break; sleep .1; done
fi
if [ "${INTERRUPT_STARTUP:-}" = 1 ]; then exit 5; fi
'''
        self.publish(program, daemon=executable.read_bytes())
        env = {"TILO_VERSION": self.release.name, "TILO_INSTALL_DIR": str(target),
               "TILO_HOSTED_SETUP": "1", "TILO_IDENTITY_ROOT": str(self.home / "identity"),
               "STATE": str(state), "PERSON": "ab" * 32}
        capabilities = next(line.split()[1] for line in pathlib.Path("/proc/self/status").read_text().splitlines()
                            if line.startswith("CapEff:"))
        can_ptrace = bool(int(capabilities, 16) & (1 << 19))  # CAP_SYS_PTRACE

        def stop_holder():
            if holder.exists() and holder.read_text():
                pid = int(holder.read_text())
                try:
                    os.kill(pid, 15)
                except ProcessLookupError:
                    pass
                for _ in range(100):
                    if subprocess.run(["flock", "-sn", holder, "true"]).returncode == 0:
                        break
                    time.sleep(.01)
                holder.unlink()
        try:
            first = self.install("login", "ticket", **env, INTERRUPT_STARTUP="1")
            self.assertEqual(first.returncode, 5, first.stderr)
            first_pid = holder.read_text()
            if not can_ptrace:
                with self.assertRaises(PermissionError):
                    os.readlink(f"/proc/{first_pid}/exe")
            resumed = self.install("login", "ticket", **env)
            self.assertEqual(resumed.returncode, 0, resumed.stderr)
            self.assertEqual(holder.read_text(), first_pid)
            again = self.install("device", "add", "returning", **env)
            self.assertEqual(again.returncode, 0, again.stderr)
            self.assertEqual(holder.read_text(), first_pid)
            stop_holder()
            reboot = self.install("device", "add", "after-restart", **env)
            self.assertEqual(reboot.returncode, 0, reboot.stderr)
            self.assertNotEqual(holder.read_text(), first_pid)
            self.assertEqual(self.install("device", "add", "returning", **env).returncode, 0)
            stop_holder()
            # A user service can start the protected daemon before the installer runs.
            # The manager must identify both this process and its verified executable.
            service = self.home / ".config/systemd/user/tilo-peer.service"
            service.parent.mkdir(parents=True)
            service.write_text(f"ExecStart={target}/tilo-runtime device-daemon\n")
            manager = self.home / "systemctl"
            manager.write_text("""#!/bin/sh
case "$5" in
  MainPID) cat "$STATE/holder.lock" ;;
  ExecStart) printf '{ path=%s/tilo-runtime ; argv[]=... ; }' "$TILO_INSTALL_DIR" ;;
  *) exit 1 ;;
esac
""")
            manager.chmod(0o755)
            with subprocess.Popen([target / "tilo-runtime", holder]) as service_process:
                try:
                    for _ in range(100):
                        if holder.exists() and holder.read_text():
                            break
                        time.sleep(.01)
                    if not can_ptrace:
                        unknown = self.install("device", "add", "unmanaged", **env)
                        self.assertNotEqual(unknown.returncode, 0, unknown.stderr)
                    env["PATH"] = f"{self.home}:{os.environ['PATH']}"
                    managed = self.install("device", "add", "after-service-restart", **env)
                    self.assertEqual(managed.returncode, 0, managed.stderr)
                finally:
                    service_process.terminate()
            holder.unlink()
        finally:
            stop_holder()

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
            # A Linux archive carries the task program beside the command; a Mac's holds none,
            # since no task program starts there.
            task_program = root / "tilolang-process"
            task_program.write_bytes(b"#!/bin/sh\necho released task program\n")
            task_program.chmod(0o755)
            # Every archive carries the daemon `tilo` starts, packed from beside the command.
            daemon = root / "tilo-runtime"
            daemon.write_bytes(b"#!/bin/sh\necho released daemon\n")
            daemon.chmod(0o755)
            builds = root / "builds"
            builds.mkdir()
            for system in ("linux-x86_64", "linux-aarch64", "macos-aarch64", "macos-x86_64"):
                beside = [task_program] if system.startswith("linux") else []
                run("bash", self.RELEASE, "pack", program, builds / f"tilo-{system}.tar.gz", *beside, env={**os.environ, "TMPDIR": scratch})
            for system, members in (("linux-x86_64", ["tilo", "tilo-runtime", "tilolang-process"]), ("macos-aarch64", ["tilo", "tilo-runtime"])):
                with tarfile.open(builds / f"tilo-{system}.tar.gz") as packed:
                    self.assertEqual(packed.getnames(), members)
            release = root / "releases/v1.2.3"
            run("bash", self.RELEASE, "sums", release, builds, env={**os.environ, "TMPDIR": scratch})
            again = root / "again"
            run("bash", self.RELEASE, "pack", program, again, task_program, env={**os.environ, "TMPDIR": scratch})
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
            self.assertEqual(run(home / ".local/bin/tilo-runtime").stdout, b"released daemon\n")
            beside = home / ".local/bin/tilolang-process"
            if machine().startswith("tilo-linux"):
                self.assertEqual(subprocess.check_output([beside]), b"released task program\n")
            else:
                self.assertFalse(beside.exists(), "a Mac's archive holds no task program")
            names = sorted(line.split()[1] for line in (release / "SHA256SUMS").read_text().splitlines())
            self.assertEqual(names, sorted(f"tilo-{s}.tar.gz" for s in ("linux-aarch64", "linux-x86_64", "macos-aarch64", "macos-x86_64")))

    def test_a_cli_only_tag_cannot_publish_a_final_release(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = pathlib.Path(scratch)
            done = subprocess.run(
                ["bash", self.RELEASE, "publish", root],
                capture_output=True, text=True,
                env={**os.environ, "TMPDIR": scratch, "T_REF": "tag:v+cli-1.2.3",
                     "TILO_PUBLISH_DRY_RUN": str(root / "site")},
            )
            self.assertNotEqual(done.returncode, 0)
            self.assertIn("a CLI-only tag must name a prerelease", done.stderr)
            self.assertFalse((root / "site").exists())

    def test_a_hosted_package_cannot_be_signed_or_published_under_its_numeric_alias(self):
        with tempfile.TemporaryDirectory() as scratch:
            release = pathlib.Path(scratch)
            (release / "SHA256SUMS").write_text("test manifest")
            for operation in ("sign", "publish"):
                with self.subTest(operation=operation):
                    result = subprocess.run(["bash", self.RELEASE, operation, release],
                                            capture_output=True, text=True,
                                            env={"PATH": os.defpath, "TMPDIR": scratch,
                                                 "T_REF": "tag:v0.3.1-assistant.1"})
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("requires its protected v+cli- tag", result.stderr)

    def test_preparation_and_the_protected_tag_select_the_same_binary_version(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = pathlib.Path(scratch)
            version = "v0.3.1-assistant.1"
            env = {"PATH": os.defpath, "TMPDIR": scratch}
            output = root / "macos-aarch64"
            def selected(**extra):
                return subprocess.run(["bash", self.RELEASE, "build-version", output],
                                      capture_output=True, text=True, env={**env, **extra})
            self.assertEqual(selected().stdout.strip(), "")
            self.assertEqual(selected(T_INPUT_HOSTED_VERSION=version).stdout.strip(), version)
            self.assertEqual(selected(T_REF="tag:v0.5.0").stdout.strip(), "v0.5.0")
            run("bash", self.RELEASE, "hosted-policy", root / "hosted-trust", env={
                **env, "T_REF": "tag:v+cli-0.3.1-assistant.1", "T_COMMIT": "a" * 40,
                "TILO_HOSTED_APPLE_TEAM_ID": "TEAM123456", "TILO_HOSTED_APPLE_LEAF_SHA256": "f" * 64})
            # Nonpublishing dependencies do not receive T_REF in this trusted run.
            self.assertEqual(selected().stdout.strip(), version)
            self.assertEqual(selected(T_INPUT_HOSTED_VERSION=version).stdout.strip(), version)
            self.assertNotEqual(selected(T_INPUT_HOSTED_VERSION="v1.2.3-preview.1").returncode, 0)

    def test_production_refuses_the_raw_cross_build_before_signing_or_upload(self):
        with tempfile.TemporaryDirectory() as scratch:
            release = pathlib.Path(scratch) / "release"
            release.mkdir()
            (release / "SHA256SUMS.sig").write_text("fixture")
            names = [f"tilo-{system}-{arch}.tar.gz" for system in ("linux", "macos")
                     for arch in ("aarch64", "x86_64")]
            for name in names:
                (release / name).write_bytes(name.encode())
            (release / "SHA256SUMS").write_text("".join(
                f"{hashlib.sha256(name.encode()).hexdigest()}  {name}\n" for name in names))
            for operation in ("sign", "publish"):
                with self.subTest(operation=operation):
                    result = subprocess.run(["bash", self.RELEASE, operation, release],
                                            capture_output=True, text=True,
                                            env={"PATH": os.defpath, "TMPDIR": scratch,
                                                 "T_COMMIT": "a" * 40, "T_REF": "tag:v+cli-0.3.1-assistant.1"})
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("requires the verified Mac release", result.stderr)
                    self.assertEqual((release / "SHA256SUMS.sig").read_text(), "fixture")

    def test_a_hosted_release_requires_every_platform_in_its_signed_manifest(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = pathlib.Path(scratch)
            names = [f"tilo-{system}-{arch}.tar.gz" for system in ("linux", "macos")
                     for arch in ("x86_64", "aarch64")]
            env = {**os.environ, "TMPDIR": scratch, "T_REF": "tag:v+cli-0.3.1-assistant.1",
                   "TILO_PUBLISH_DRY_RUN": str(root / "site")}
            for name in names:
                (root / name).write_bytes(b"archive")
            for missing in names:
                with self.subTest(missing=missing):
                    (root / "SHA256SUMS").write_text("".join(
                        f"{hashlib.sha256(b'archive').hexdigest()}  {name}\n"
                        for name in names if name != missing))
                    done = subprocess.run(["bash", self.RELEASE, "sign", root], capture_output=True,
                                          text=True, env=env)
                    self.assertNotEqual(done.returncode, 0)
                    self.assertIn(f"requires {missing} in the signed release", done.stderr)
                    self.assertFalse((root / "SHA256SUMS.sig").exists())

    def test_a_release_without_both_linux_archives_is_refused(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = pathlib.Path(scratch)
            program = root / "tilo"
            program.write_bytes(b"x")
            (root / "tilo-runtime").write_bytes(b"x")
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
