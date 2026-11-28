#!/usr/bin/env python3
"""An app's first hour, walked in the versions its step file names.

A Rust app under `apps/` is deleted only once the tilolang app under `apps/tl/`
does what a person did with it at least as well. What a person did is a walk of
their own: open the app, make something, find it again, look at it in the dark.
This walks that same first hour in each version the step file names — the Rust
one and the tilolang one before the change that retires the Rust app, the
tilolang one alone after it — in headless sessions of their own, and snapshots
every step, so the runs can be put side by side and judged.

One step file per app (`app-walks/<app>.json`) holds the steps, and both
versions are driven by it: the presses, typing, searches and theme changes are
the same, and only the control each version answers to differs, which the step
file names per version. The steps themselves are the app's own; a new app's
walk is a new file, not a new script.

    scripts/headless-examples/app-walk.py notes
    scripts/headless-examples/app-walk.py notes --only tl     # one version
    scripts/headless-examples/app-walk.py --list              # the walks there are

What each step says is OUT/walk.json: for each version, the steps in order,
each with its snapshot, how long the app took to answer, how many notes it
held afterwards, and — where a step expected a control — what the app offered
instead. A step that cannot be walked in a version is recorded as such (the
control is missing, the note never appeared, the file chooser asks something
else) and the walk goes on: what a person finds missing is the finding. A step
that leaves the app's window drawing nothing of the app under its bar is one
too, whatever its waits found: a blank window is a finding in its own right.

The Rust version is the session build's own app (`TILO_NESTED_APPS`,
dist/dev/nested-session-build.sh); the tilolang one is `tilolang build` of the
app's package against that session's runtime, installed beside it as a signed
package. Both run as the image runs them, through the session's launcher and
sandbox: nothing starts an app binary directly and nothing touches this
machine's desktop (lavapipe alone, a private runtime directory, a throwaway
identity).

It needs the session build and the `tilolang` tool, which the nested-session
build makes (or `--build` makes them first, through
TILO_NESTED_SESSION_BUILD_EXEC where this machine has no toolchain).

Usage:
  scripts/headless-examples/app-walk.py notes [--out DIR] [--state DIR]
                                      [--only rust|tl] [--build] [--keep]
"""

from __future__ import annotations

import argparse
import array
import base64
import contextlib
import fnmatch
import functools
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
from collections.abc import Callable
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
# The session this walks and the tools that act in it: the one harness every
# check of a desktop with the tree's apps starts (scripts/headless-examples/real_apps.py).
import real_apps  # noqa: E402
from real_apps import (  # noqa: E402
    BUILD, Failed, Refused, Session, address, as_account, prepare, record, run, until)

WALKS = HERE / "app-walks"
# What a control's identity joins: the record kind it acts on, and the operation,
# in the app's own ids (`apps/tl/<app>/app.ids`). A control that runs an app's
# action and a field that edits a record publish it.
ID_SEP = "\x1f"
VARIANTS = ("rust", "tl")
# How long a person waits for an app that is on their device, from asking for it to the frame
# that shows its window: the product's bar for every app. Each walk records the opens that started
# its app (`Walk.opens`), read from the session's own launch timings; the `app-walk/open` check
# holds them to the bar (`--opens`), apart from the walks, which are red for their flows alone.
OPEN_BAR_MS = 100
# The person's settings, kept by the session's settings provider whether the app that shows
# them is open or not: the appearance every app draws, and the action that changes it.
SETTINGS = "tilo.settings"
APPEARANCE = "settings/appearance"
# The shell's own verbs: a window is focused and closed by the tab it holds.
SHELL = "so.tilo.session"
SHELL_RESOURCE = "desktop:shell"
# What asks the person: a call that needs their yes waits as its asked
# `tilo.invocations` record, which the workspace's invocations read beside the
# calls it kept, and the person answers with the record's own `resolve`. A
# program's file chooser is such a record; its answer is the files chosen.
INVOCATIONS_APP = "tilo.invocations"
ASKED_RESOLVE = "resolve"
# The small picture a walk of an app that shows photographs imports: the same
# 64-by-64 PNG in two colours that `ui::headless::FIXTURE_PICTURE` carries, kept
# here as its bytes so a file chooser can be handed a real image.
FIXTURE_PICTURE_PNG = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAEAAAABAAQMAAACQp+OdAAAABlBMVEXyyHpLP7ovf/CwAAAAf0lE"
    "QVR42j3RsRHAMAhDUcj+QzEKI1BScJAi+nHjdyp0NvjZdx7dBxYMaFAgQbh6XMlRuGBAgwIJwtTj"
    "So6HLRjQoECCMPW4kuODCwY0KJAgTD2u5BjUggENCiQIU48rOQa+YECDAglCgz329WPBgAYFEgR7"
    "Zu8/FgxoUCABNS8YGz171qSmDgAAAABJRU5ErkJggg==")


class PageServer(SimpleHTTPRequestHandler):
    """The pages a walk serves on this machine's loopback, without the request
    each line `http.server` would print into the walk's log."""

    def log_message(self, _format: str, *args) -> None:
        pass


# The stand-in mail server a walk adds a mail account against: the address's own
# domain, the server names a mail program works out from it, the ports it asks
# them on, and the address it takes. The tree's stand-in serves them
# (crates/integrations/account-probe/examples/mail_stand_in.rs), staged the way the
# lounge's mail stories stage one
# (os/containers/tilo-lounge/stories/shell/mail_provider.py).
MAIL_DOMAIN = "story.tilo.test"
MAIL_ADDRESS = f"lab@{MAIL_DOMAIN}"
MAIL_NAMES = [MAIL_DOMAIN, f"imap.{MAIL_DOMAIN}", f"smtp.{MAIL_DOMAIN}"]
MAIL_IMAP = f"127.0.0.1:993"
MAIL_SMTP = f"127.0.0.1:465"
MAIL_PASSWORD = "walks-stand-in"
# The iCloud-shaped double: mail and calendars under one Apple ID and app password, at the
# names Tilo reaches iCloud's own servers by
# (crates/integrations/account-probe/examples/icloud_double.rs).
ICLOUD_ADDRESS = "lab@icloud.com"
ICLOUD_NAMES = ["imap.mail.me.com", "smtp.mail.me.com", "caldav.icloud.com"]
ICLOUD_CALDAV = "127.0.0.1:443"
MAIL_CA_BUNDLES = (
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/ssl/cert.pem",
)


def at_path(value, path: str):
    """What `path` reads in `value`: a dotted walk into dicts and lists (`#len`
    for how many a list or dict holds), the way the desktop's state is read."""
    path, _, measure = path.partition("#")
    at = value
    for key in path.split("."):
        at = at.get(key) if isinstance(at, dict) else (
            at[int(key)] if isinstance(at, list) and key.isdigit() and int(key) < len(at) else None)
    if measure == "len":
        return len(at) if isinstance(at, (list, dict)) else None
    return at


class LocalMailServer:
    """The tree's stand-in mail server, run beside the walk's session as the
    provider of the address a mail account is added against: the server names in
    `/etc/hosts`, its IMAP and SMTP sockets on the ports a mail program asks a
    domain's own servers on, and its certificate in the machine's trust bundles,
    which is what the session's own connection checks it against. Root stages
    it, as the lounge's stories stage a mail provider, and takes it out again
    when the walk ends."""

    program = "mail-stand-in"
    program_env = "TILO_MAIL_STAND_IN"
    address = MAIL_ADDRESS
    names = MAIL_NAMES
    serving_says = "mail stand-in: IMAP at"

    def __init__(self, out: Path, password: str = MAIL_PASSWORD):
        self.dir = out
        self.dir.mkdir(parents=True, exist_ok=True)
        self.password = password
        self.password_file = self.dir / "password"
        self.certificate = self.dir / "certificate.pem"
        self.log = self.dir / "double.log"
        self.proc: subprocess.Popen | None = None
        self.hosts = Path("/etc/hosts")
        self.hosts_before: bytes | None = None
        # Each bundle the certificate joined, and what it held before.
        self.trusted: list[tuple[Path, bytes]] = []

    def binary(self) -> Path:
        for candidate in (os.environ.get(self.program_env),
                          str(BUILD / "bin" / self.program)):
            if candidate and Path(candidate).is_file() and os.access(candidate, os.X_OK):
                return Path(candidate)
        raise Failed(f"no {self.program} to run: the nested-session build stages it as "
                     f"bin/{self.program}, or name it with {self.program_env}")

    def served(self) -> list[str]:
        """The listeners beyond IMAP and SMTP this provider serves on."""
        return []

    def stage(self) -> dict:
        if os.geteuid() != 0:
            raise Failed("staging the mail provider takes root: /etc/hosts, the trust "
                         "bundles and the mail ports (the walk's check runs as root)")
        self.password_file.write_text(self.password + "\n")
        self.password_file.chmod(0o600)
        self.name_itself()
        try:
            self.start()
            self.trust()
        except BaseException:
            self.stop()
            raise
        return {"address": self.address, "password": self.password,
                "imap": MAIL_IMAP, "smtp": MAIL_SMTP, "log": str(self.log)}

    def name_itself(self) -> None:
        self.hosts_before = self.hosts.read_bytes()
        block = ["", "# a walk's mail provider, at this machine's own loopback"]
        block += [f"127.0.0.1 {name}" for name in self.names]
        self.hosts.write_bytes(self.hosts_before + ("\n".join(block) + "\n").encode())

    def start(self) -> None:
        argv = [str(self.binary()), "--login", self.address,
                "--password-file", str(self.password_file),
                "--write-certificate", str(self.certificate),
                "--imap", MAIL_IMAP, "--smtp", MAIL_SMTP, *self.served()]
        for name in self.names:
            argv += ["--name", name]
        with self.log.open("w") as said:
            self.proc = subprocess.Popen(argv, stdout=said, stderr=subprocess.STDOUT,
                                         start_new_session=True)

        def serving() -> str | None:
            text = self.log.read_text(errors="replace") if self.log.is_file() else ""
            if self.serving_says in text:
                return text
            if self.proc is not None and self.proc.poll() is not None:
                raise Failed(f"{self.program} exited: {text[-1500:]}")
            return None

        until_value(f"{self.program} to answer", serving, 60, 0.1)

    def trust(self) -> None:
        certificate = self.certificate.read_bytes()
        present = [Path(path) for path in MAIL_CA_BUNDLES if Path(path).exists()]
        if not present:
            raise Failed(f"this machine has no CA bundle among {MAIL_CA_BUNDLES}")
        for path in present:
            bundle = path.resolve()
            before = bundle.read_bytes()
            self.trusted.append((bundle, before))
            bundle.write_bytes(before + b"\n" + certificate)

    def stop(self) -> None:
        if self.proc is not None and self.proc.poll() is None:
            try:
                os.killpg(self.proc.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                self.proc.wait(15)
            except subprocess.TimeoutExpired:
                with contextlib.suppress(ProcessLookupError):
                    os.killpg(self.proc.pid, signal.SIGKILL)
                self.proc.wait(5)
        self.proc = None
        if self.hosts_before is not None:
            with contextlib.suppress(OSError):
                self.hosts.write_bytes(self.hosts_before)
            self.hosts_before = None
        for bundle, before in self.trusted:
            with contextlib.suppress(OSError):
                bundle.write_bytes(before)
        self.trusted.clear()


class ICloudDouble(LocalMailServer):
    """An iCloud account's provider, staged as the stand-in mail server is: its mail servers
    and a calendar server holding one calendar, "Personal", under the same Apple ID and app
    password, at the names Tilo reaches iCloud by."""

    program = "icloud-double"
    program_env = "TILO_ICLOUD_DOUBLE"
    address = ICLOUD_ADDRESS
    names = ICLOUD_NAMES
    serving_says = "icloud double: IMAP at"

    def served(self) -> list[str]:
        return ["--caldav", ICLOUD_CALDAV]


def until_value(what: str, check, deadline: float = 30.0, step: float = 0.1):
    """`until`, with the deadline named per call: every step here is bounded by
    what a person would wait, and a control that never comes is a finding."""
    return until(what, check, deadline, step)


def machine_acl(state: Path) -> Path:
    """The capability list the machine's files daemon enforces, in this walk's state: the tree's
    shipped list seeds it, as it seeds a device's ``/var/lib/tilo/fs-capabilities.json``.

    The session decides the list its mount enforces and the daemon keeps what it was told for
    the mount's next start (``crates/core/fs/src/caps_file.rs``). Pointing ``--acl`` at the
    tree's own copy would write the session's list into the tree a build's provenance is of
    (``dist/dev/build-provenance.sh``), and the next session's prepare would refuse the build it
    was given; a list kept here belongs to the device the walk is. ``dist/dev/nested-session.sh``
    keeps its list the same way."""
    path = state / "files/fs-capabilities.json"
    if not path.is_file():
        path.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(REPO / "os/usr/share/tilo/fs-capabilities.json", path)
    return path


class MachineFiles:
    """The machine's files daemon, started the way a device's service starts it.

    The walk's session runs as the person, as a seat's does, and a FUSE mount by an ordinary
    account is refused where the machine gives the person no fusermount3 privilege. So the
    daemon is the machine's, as it is on a device: root, mounting the person's workspace at
    ``/var/workspace`` with them as its owner and the device's capability list in force, seeded
    from the shipped one (``machine_acl``), its document-media socket group-connectable by the
    launcher. The session is told ``TILO_SESSION_MACHINE_FILES`` and uses what this serves
    instead of starting one of its own (``dist/dev/nested-session.sh``).

    The machine's start waits for the person's identity, which the session mints as it comes
    up: the daemon presents that identity, so it starts beside the session and the session
    waits for it (the blob socket and pid below) before it starts the desktop. A daemon that
    fails ends the walk at once with what it said (``failure``): the session was told to
    rely on it, and a session waiting out its start would only report a timeout. Only a
    caller that is root and named a session account starts one; a walk on a developer's own
    desktop keeps the session's own daemon."""

    #: Where the daemon announces its main pid, as a service manager announces a unit's: the
    #: session reads it to record the login's files endpoint.
    PIDFILE = Path("/run/tilo/fs.pid")
    #: Why the daemon did not start, when it did not: the session reads it and serves its own
    #: workspace instead of waiting out the socket.
    FAILED = Path("/run/tilo/fs.failed")
    #: Where the daemon serves document media, and the workspace it mounts.
    BLOB_SOCKET = Path("/run/tilo/fs-blobs.sock")
    WORKSPACE = Path("/var/workspace")
    #: How long the machine may take to start its daemon: the session mints an identity first.
    START_WITHIN = 300.0

    def __init__(self, state: Path, out: Path, apps: str = "",
                 tapps: list[Path] | tuple = ()):
        self.state = state
        self.out = out
        #: The session's apps and the walk's tilolang packages, which the daemon serves from
        #: its own store.
        self.apps = apps
        self.tapps = list(tapps)
        self.account = real_apps.session_account()
        self.proc: subprocess.Popen | None = None
        self.log = None
        self.thread: threading.Thread | None = None
        self.stopping = threading.Event()
        #: Why the machine's daemon did not start, for the walk's report; None when it did.
        self.trouble: str | None = None

    def env(self) -> dict:
        """What the session is told so it uses the machine's workspace, and serves none."""
        return {"TILO_SESSION_MACHINE_FILES": "1",
                "TILO_WORKSPACE_ROOTS": str(self.WORKSPACE),
                "TILO_FS_BLOB_SOCKET": str(self.BLOB_SOCKET)}

    def start(self) -> dict:
        """Begin starting the machine's daemon, and what the session is told of it."""
        if self.account is None:
            return {}
        with contextlib.suppress(OSError):
            self.FAILED.unlink(missing_ok=True)
        self.thread = threading.Thread(target=self._serve, name="walk-machine-files",
                                       daemon=True)
        self.thread.start()
        return self.env()

    def _serve(self) -> None:
        try:
            self._await_identity()
            if self.stopping.is_set():
                return
            self._launch()
        except BaseException as error:  # the walk reports it, and serves its own workspace
            self.trouble = str(error)
            with contextlib.suppress(OSError):
                self.FAILED.write_text(f"{error}\n")
            print(f"==> the machine's files workspace did not start: {error}", file=sys.stderr)

    def _await_identity(self) -> None:
        """Wait for the person's identity, which the session mints: the daemon presents it and
        starts nowhere without it.

        The key stays the session account's, owner-only, as the image provisions it: root reads
        it through its own privilege where it needs to, and no group is given it."""
        certs = self.state / "identity/device/certs"
        deadline = time.monotonic() + self.START_WITHIN
        while not any(certs.glob("*.bin")):
            if self.stopping.is_set():
                return
            if time.monotonic() >= deadline:
                raise Failed(f"the session minted no identity at {certs}")
            time.sleep(0.1)

    def _launch(self) -> None:
        logs = self.out / "logs"
        logs.mkdir(parents=True, exist_ok=True)
        argv = [
            str(BUILD / "bin/fs"), "serve",
            "--mount-point", str(self.WORKSPACE),
            "--cache-dir", "/var/cache/quick-fs",
            "--storage-sock", "/run/tilo/fs-storage/storage.sock",
            "--blob-sock", str(self.BLOB_SOCKET),
            "--acl", str(machine_acl(self.state)),
            "--member", "person:local",
            "--backend", "fuse",
            "--native-owner", f"{self.account.pw_uid}:{self.account.pw_gid}",
        ]
        env = {**os.environ,
               "TILO_IDENTITY_ROOT": str(self.state / "identity"),
               # The login's sockets, so the mount dials the session's workspace service as
               # the login's daemon would; the seat's directory is the one the session gets.
               "XDG_RUNTIME_DIR": (os.environ.get("TILO_SESSION_RUNTIME")
                                   or f"/run/user/{self.account.pw_uid}"),
               "TILO_MACHINE": "hosted",
               "RUST_LOG": os.environ.get("RUST_LOG", "warn,tilo_fs=info")}
        # What a daemon the walk before this one left behind: a mount that answers nothing
        # cannot be mounted over, and a socket with no server would pass for one.
        if os.path.ismount(self.WORKSPACE):
            with contextlib.suppress(OSError, subprocess.SubprocessError):
                subprocess.run(["umount", "--lazy", str(self.WORKSPACE)],
                               capture_output=True, timeout=30, check=False)
        self.BLOB_SOCKET.unlink(missing_ok=True)
        self.log = (logs / "machine-files.log").open("w")
        self.proc = subprocess.Popen(argv, stdout=self.log, stderr=subprocess.STDOUT,
                                     env=env, start_new_session=True)
        self.PIDFILE.write_text(f"{self.proc.pid}\n")
        until("the machine's document media socket",
              lambda: self.BLOB_SOCKET.is_socket()
              or (self.proc.poll() is not None and self._died()), 120.0, 0.1)
        until("the machine's workspace mount", lambda: os.path.ismount(self.WORKSPACE),
              120.0, 0.1)

    def _died(self):
        """The daemon ended, so the wait for its socket cannot be met: raise what it said."""
        said = self.log_path().read_text(errors="replace") if self.log_path().is_file() else ""
        raise Failed(f"the machine's files daemon ended ({self.proc.returncode}): {said[-1200:]}")

    def log_path(self) -> Path:
        return self.out / "logs" / "machine-files.log"

    def failure(self) -> str | None:
        """Why the machine's daemon did not start, once it did not: a session told to rely
        on it must not wait out its start for a workspace that never comes, nor walk as
        though it had one."""
        return self.trouble

    def stop(self) -> None:
        """End the machine's daemon and unmount its workspace, as a walk that ends leaves neither."""
        if self.account is None:
            # No machine was started: the session served its own, and this machine's own
            # workspace (a developer's desktop) is not the walk's to unmount.
            return
        self.stopping.set()
        proc = self.proc
        if proc is not None and proc.poll() is None:
            with contextlib.suppress(ProcessLookupError):
                os.killpg(proc.pid, signal.SIGTERM)
            try:
                proc.wait(30)
            except subprocess.TimeoutExpired:
                with contextlib.suppress(ProcessLookupError):
                    os.killpg(proc.pid, signal.SIGKILL)
                proc.wait(10)
        self.proc = None
        for marker in (self.PIDFILE, self.FAILED):
            with contextlib.suppress(OSError):
                marker.unlink(missing_ok=True)
        if os.path.ismount(self.WORKSPACE):
            with contextlib.suppress(OSError, subprocess.SubprocessError):
                subprocess.run(["umount", "--lazy", str(self.WORKSPACE)],
                               capture_output=True, timeout=30, check=False)
        if self.log is not None:
            self.log.close()
            self.log = None


def daemon_ask(socket_path: Path, path: str, args: dict,
               within: float = 20.0) -> tuple[dict, int | None]:
    """Ask this computer's daemon one of its own actions over its native endpoint, as the
    account this process runs as, and answer what the action answered and the descriptor the
    answer carried.

    The request names the typed answer (`so.tilo.mcp`), so the action's own answer comes back as
    data rather than as the words it is said with; `tilo_do` wraps it with the call it made, and
    that call's answer is what is answered here. A daemon that replaces its endpoint as its
    person's part starts leaves a connection made meanwhile unanswered, and one that is still
    starting has none to make: each try is a connection of its own, until `within`."""
    deadline = time.monotonic() + within
    while True:
        try:
            return asked_once(socket_path, path, args)
        except (TimeoutError, ConnectionError, OSError) as error:
            if time.monotonic() >= deadline:
                raise Failed(f"the machine's daemon answered nothing to {path}: "
                             f"{error}") from None
            time.sleep(0.5)


def asked_once(socket_path: Path, path: str, args: dict) -> tuple[dict, int | None]:
    """[`daemon_ask`], once: one connection, one request, its answer."""
    line = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": {"name": "tilo_do",
                                  "arguments": {"path": path, "args": args},
                                  # The typed answer, not the words it is said with.
                                  "_meta": {"so.tilo.mcp": {"structured": True}}}})
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
        # An action the daemon answers answers at once: a request it does not answer is a
        # failure said now, never one the walk waits out.
        stream.settimeout(10)
        stream.connect(str(socket_path))
        stream.sendall((line + "\n").encode())
        answered, handed = b"", []
        while b"\n" not in answered:
            data, ancillary, _flags, _address = stream.recvmsg(
                1 << 16, socket.CMSG_SPACE(4 * 4))
            for level, kind, payload in ancillary:
                if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
                    fds = array.array("i")
                    fds.frombytes(payload[:len(payload) - (len(payload) % fds.itemsize)])
                    handed.extend(fds)
            if not data:
                break
            answered += data
    said = answered.split(b"\n", 1)[0].decode(errors="replace")
    if not said:
        raise Failed(f"the machine's daemon answered nothing to {path}")
    try:
        answer = json.loads(said)
    except ValueError:
        raise Failed(f"the machine's daemon's answer is not JSON: {said}") from None
    if "error" in answer:
        raise Failed(f"the machine's daemon refused {path}: {said}")
    result = answer.get("result") or {}
    text = "".join(part.get("text") or "" for part in result.get("content") or [])
    if result.get("isError") is True:
        raise Failed(f"the machine's daemon refused {path}: {text or said}")
    # The typed answer the tool carries, else the words it answered them as: the words may say
    # `ok` above the action's own answer.
    typed = result.get("structuredContent")
    said_answer = typed if isinstance(typed, dict) and typed else None
    if said_answer is None:
        if text.startswith("ok\n"):
            text = text[3:]
        try:
            said_answer = json.loads(text) if text else {}
        except ValueError:
            said_answer = {}
    if not isinstance(said_answer, dict):
        said_answer = {}
    # `tilo_do` answers with the action it called, whether it ran, and the action's own answer
    # under `result`; a call that failed says so instead of answering.
    if said_answer.get("status") == "failed":
        raise Failed(f"the machine's daemon refused {path}: {text or said}")
    if isinstance(said_answer.get("result"), dict):
        said_answer = said_answer["result"]
    return said_answer, (handed[0] if handed else None)


#: What a device daemon of the walk logs: its own lines, and what each program it starts says on
#: its error stream (a tilolang process's engine refusal, which nothing else reports).
DAEMON_LOG = "warn,tilo_runtime=info,tilo::app_output=info"


class MachineDaemon:
    """This computer's device daemon, started the way Tilo OS's system unit starts it, and the
    login that starts the session as the person.

    On Tilo OS the daemon is the system's: root runs it, every account reaches its socket, and
    the login starts a person's desktop with the link the daemon answers the sign-in with, never
    a daemon of the person's own. The walk plays that login through the daemon's own actions, as
    `pam_tilo` does: root says the session account signed in, the account founds its person with
    its first `tilo` command, and root says it signed in again, taking the link the answer
    carries and handing it to the session as `TILO_SHELL_TOKEN`, the variable the PAM module names.

    The daemon keeps the person's workspaces and records in root-owned system state, as a device
    does; the session changes them only through the link."""

    #: The system unit's runtime directory (`RuntimeDirectory=tilo-runtime`), which the image's
    #: tmpfiles/systemd makes and the walk makes for it.
    RUNTIME = Path("/run/tilo-runtime")
    NATIVE = RUNTIME / "native.sock"
    #: The unit's `Environment=HOME=` and `Environment=TILO_RUNTIME_PROFILE=`, which the walk
    #: names by `--profile` instead: a profile named in the environment is a profile named apart,
    #: whose sockets move with it, and the unit's daemon listens where every account finds it.
    PROFILE = Path("/var/lib/tilo/state")
    #: How long the daemon may take to answer, and the person's services to run before their
    #: shell's link is handed out.
    START_WITHIN = 120.0
    LINK_WITHIN = 180.0
    def __init__(self, state: Path, out: Path, apps: str = "",
                 tapps: list[Path] | tuple = ()):
        self.state = state
        self.out = out
        #: The session's apps and the walk's tilolang packages, which the daemon serves from
        #: its own store.
        self.apps = apps
        self.tapps = list(tapps)
        self.account = real_apps.session_account()
        #: The store the daemon and the session share, and the paths beside it, once the daemon
        #: starts: what the session is told so both install into, and launch from, one store.
        self.store_env: dict = {}
        self.proc: subprocess.Popen | None = None
        self.log = None
        #: Why the machine's daemon, or the login, did not come up, for the walk's report.
        self.trouble: str | None = None

    def env(self) -> dict:
        """What the session is told so it starts no daemon of its own, and the store both it and
        the daemon install apps into: a device's user store, which the shell and the daemon that
        serves it share, so a style the person installs in their session is one the daemon
        publishes in their workspace without a restart."""
        return {"TILO_SESSION_MACHINE_DAEMON": "1", **self.store_env}

    def failure(self) -> str | None:
        return self.trouble

    def start(self) -> dict:
        """Start the machine's daemon and wait for its native endpoint."""
        if self.account is None:
            return {}
        try:
            self._launch()
        except BaseException as error:
            self.trouble = str(error)
            print(f"==> the machine's device daemon did not start: {error}", file=sys.stderr)
            raise
        return self.env()

    def _launch(self) -> None:
        # The unit's `RuntimeDirectory` and its state: the one open to every account, the other
        # private to the account the daemon runs as, which the daemon refuses to serve otherwise.
        self.RUNTIME.mkdir(parents=True, exist_ok=True)
        # Each version walks a computer of its own, with its own identity root: the daemon starts
        # from no state of the last one's. A record that this account already founded a person,
        # whose identity lives in the last version's root, would answer its sign-in with that
        # person and never a shell, since their part cannot start without their identity.
        if self.PROFILE.exists():
            shutil.rmtree(self.PROFILE)
        self.PROFILE.mkdir(parents=True, exist_ok=True)
        self.PROFILE.chmod(0o700)
        logs = self.out / "logs"
        logs.mkdir(parents=True, exist_ok=True)
        system, publishers, user = self._own_store()
        env = {**os.environ,
               # The unit's own environment, over the walk's identity root.
               "HOME": str(self.PROFILE),
               "TILO_IDENTITY_ROOT": str(self.state / "identity"),
               # The store the session installs into and launches from, as a device's user store,
               # the system store every account reads (/usr/share/tilo-apps on a device), and the
               # publishers the image trusts.
               "TILO_APPS_DATA_DIR": str(user),
               "TILO_APPS_SYSTEM_DIR": str(system),
               "TILO_RUNTIME_BUNDLE_DIR": str(user / "runtimes"),
               "TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE": str(publishers),
               "XDG_RUNTIME_DIR": str(self.RUNTIME),
               "RUST_LOG": os.environ.get("RUST_LOG", DAEMON_LOG)}
        # What the session is told to read and write the store by, as a device's login is: the
        # one store the shell installs into is the one the daemon publishes from.
        self.store_env = {
            "TILO_APPS_DATA_DIR": str(user),
            "TILO_APPS_SYSTEM_DIR": str(system),
            "TILO_RUNTIME_BUNDLE_DIR": str(user / "runtimes"),
            "TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE": str(publishers),
        }
        env.pop("XDG_DATA_HOME", None)
        self.log = (logs / "machine-daemon.log").open("w")
        self.proc = subprocess.Popen([str(BUILD / "bin/tilo-runtime"),
                                      "--profile", str(self.PROFILE)],
                                     stdout=self.log, stderr=subprocess.STDOUT,
                                     env=env, start_new_session=True,
                                     # What the daemon writes in the person's store (the
                                     # registry above all) is root's, and every account reads
                                     # it, as it reads a package's files: the session replaces
                                     # it by renaming over it in its own directory.
                                     umask=0o022)
        until("the machine's device daemon",
              lambda: self.NATIVE.exists()
              or (self.proc.poll() is not None and self._died()), self.START_WITHIN, 0.1)

    #: Where the daemon keeps what a device's image ships beside it: the user and system stores
    #: (the image's /usr/share/tilo-apps) and the publishers it trusts, outside its profile, which
    #: each start begins anew.
    IMAGE = Path("/var/lib/tilo/walk-image")

    def _own_store(self) -> tuple[Path, Path, Path]:
        """Put what the session prepared into the device's stores, as a device's image and the
        daemon's own installs put them: the installed apps and runtimes in the user store both
        the session and the daemon write, the system store and the trusted publishers where every
        account reads them. The session prepared them in the person's state, which is the
        person's alone (0700); a device's login and daemon share the user store, so what the
        session installs is what the daemon publishes (`$TILO_APPS_DATA_DIR`). The store's
        directories are the person's, and the daemon runs with the umask that leaves what it
        writes there readable to them. Answers the system store, the
        publishers file and the user store."""
        # Everything the session will run installed first, as its own start installs it, so the
        # copy below holds every app the person has, not only those an earlier step prepared.
        argv = [str(real_apps.SESSION), "--no-build", "--prepare-only", "--headless",
                "--apps", self.apps, "--state", str(self.state)]
        for tapp in self.tapps:
            argv += ["--install-tapp", str(tapp)]
        run(argv, cwd=real_apps.REPO)
        if self.IMAGE.exists():
            shutil.rmtree(self.IMAGE)
        user, system = self.IMAGE / "user", self.IMAGE / "system"
        publishers = self.IMAGE / "keys/runtime-publishers.txt"
        self.IMAGE.mkdir(parents=True, exist_ok=True)
        for source, target in ((self.state / "apps", user),
                               (self.state / "system", system)):
            if source.is_dir():
                shutil.copytree(source, target, symlinks=True)
            else:
                target.mkdir(parents=True, exist_ok=True)
        publishers.parent.mkdir(parents=True, exist_ok=True)
        if (self.state / "keys/runtime-publishers.txt").is_file():
            shutil.copyfile(self.state / "keys/runtime-publishers.txt", publishers)
        else:
            publishers.touch()
        # The person's store is the person's: the session installs there, and the store chmods
        # what it makes. Every account reads the system store, and what the image ships stays the
        # image's. The daemon runs as root and reads or writes the person's store beside them.
        owner = self.account.pw_uid if self.account is not None else os.getuid()
        group = self.account.pw_gid if self.account is not None else os.getgid()
        for path in [self.IMAGE, *self.IMAGE.rglob("*")]:
            within_user = path == user or user in path.parents
            os.chown(path, owner if within_user else 0, group, follow_symlinks=False)
            if path.is_symlink():
                continue
            if path.is_dir():
                path.chmod(0o755 if within_user else 0o750)
            else:
                # A program the store keeps, a runtime's own included, stays one: the launcher
                # runs it from the store.
                runs = path.stat().st_mode & 0o111
                if within_user:
                    path.chmod(0o755 if runs else 0o644)
                else:
                    path.chmod(0o550 if runs else 0o440)
        self.IMAGE.chmod(0o750)
        return system, publishers, user

    def _died(self):
        said = self.log_path().read_text(errors="replace") if self.log_path().is_file() else ""
        raise Failed(f"the machine's device daemon ended ({self.proc.returncode}): {said[-1600:]}")

    def log_path(self) -> Path:
        return self.out / "logs" / "machine-daemon.log"

    def _account_env(self) -> dict:
        """The environment the session script gives a command of the person's: its own state
        tree, its identity, and the seat's runtime directory."""
        return {
            "HOME": str(self.state / "home"),
            "XDG_CONFIG_HOME": str(self.state / "config"),
            "XDG_DATA_HOME": str(self.state / "data"),
            "XDG_STATE_HOME": str(self.state / "state"),
            "XDG_CACHE_HOME": str(self.state / "cache"),
            "XDG_RUNTIME_DIR": (os.environ.get("TILO_SESSION_RUNTIME")
                                or f"/run/user/{self.account.pw_uid}"),
            "TMPDIR": str(self.state / "tmp"),
            "TILO_IDENTITY_ROOT": str(self.state / "identity"),
        }

    def _signed_in(self, uid: int) -> tuple[dict, int | None]:
        """Say the account `uid` signed in, as `pam_tilo` does, and answer what the daemon said
        of the person and the descriptor the answer carried."""
        person, fd = daemon_ask(self.NATIVE, "seats#signed-in", {"uid": uid})
        if not person:
            raise Failed("the machine's daemon answered no person for the sign-in")
        return person, fd

    def login(self, _session_env: dict) -> tuple[dict, dict]:
        """The login: seat the session's account and take the one-use secret its sign-in answers
        with. Answered as the environment the session gains and the descriptors handed on.

        An account with no person yet is answered a founding secret: the session redeems it and
        founds its person itself at startup, as a session a sign-in started does, since the daemon
        admits the founding call only from the process the secret was handed to. A person's
        services that are still starting answer no secret yet, so the sign-in is asked again."""
        if self.account is None:
            return {}, {}
        uid = self.account.pw_uid
        deadline = time.monotonic() + self.LINK_WITHIN
        while True:
            said, _ = self._signed_in(uid)
            token = said.get("token")
            if said.get("shell") is True and token:
                return {"TILO_SHELL_TOKEN": token}, {}
            if time.monotonic() >= deadline:
                raise Failed(
                    "the machine's daemon served no secret for the person's shell: "
                    f"{json.dumps(said)}")
            time.sleep(0.5)

    def stop(self) -> None:
        if self.account is None:
            return
        proc = self.proc
        if proc is not None and proc.poll() is None:
            with contextlib.suppress(ProcessLookupError):
                os.killpg(proc.pid, signal.SIGTERM)
            try:
                proc.wait(30)
            except subprocess.TimeoutExpired:
                with contextlib.suppress(ProcessLookupError):
                    os.killpg(proc.pid, signal.SIGKILL)
                proc.wait(10)
        self.proc = None
        if self.log is not None:
            self.log.close()
            self.log = None


def start_signed_in(session: Session, state: Path, out: Path, *, apps: str, tapps: list[Path],
                    tag: str, env: dict, session_args: list[str] | None = None,
                    then: Callable[[], None] | None = None) -> tuple["MachineFiles",
                                                                      "MachineDaemon"]:
    """Start `session` on `state` as a device's login starts it: the machine's device daemon and
    files daemon beside it, the person signed in through the daemon before the desktop starts,
    as Tilo OS signs a person in. `then` runs once the desktop is up, still under the same
    cleanup (a walk's further devices). A start that fails stops all three.

    A session started any other way has no person signed in to the machine, and the machine's
    daemon offers it none of the person's apps."""
    machine = MachineFiles(state, out)
    daemon = MachineDaemon(state, out, apps, tapps)
    env = {**env, **daemon.start()}
    trouble = lambda: machine.failure() or daemon.failure()  # noqa: E731

    # The person signs in before the machine's files daemon asks the device anything: what it
    # serves is the person's, and a daemon with no person signed in has none.
    def logged_in():
        added, fds = daemon.login(env)
        return {**machine.start(), **added}, fds
    try:
        session.start(apps=apps, fresh=True, out=out / "logs", tag=tag, tapps=tapps,
                      session_args=session_args, env=env, abort=trouble, prepare=logged_in)
        if then is not None:
            then()
    except BaseException:
        session.stop()
        machine.stop()
        daemon.stop()
        raise
    return machine, daemon


# --- what the app holds, read back ------------------------------------------------------------

def app_action(session: Session, app: str, operation: str, resource: str | None = None):
    """The action `operation` the window of `app` publishes: found by the name
    the app gives it, never by the words its title reads in. Its application is
    `app` exactly (`so.tilo.notes` never matches `so.tilo.notes`). Only that
    application's rows are listed."""
    for row in session.actions(app=app):
        at = address(row)
        if (row.get("app_id") == app and at is not None and at[1] == operation
                and resource in (None, at[0])):
            return row
    return None


def row_for(rows: list[dict], resource: str, action: str, kind: str | None = None) -> dict | None:
    """The catalogue row offering `resource`'s `action`, whichever provider lists
    it: a workspace's actions are published under the workspace's own provider
    id, which is not the same string on the next run, so a walk names the
    resource and the action and lets the desktop say who lists them. Should two
    providers offer it, the one whose request is answered first wins — the walk
    called it, so what it does is one thing.

    `kind` names a resource of that kind instead: a run mints a shell's own
    record ids (`tl:so.tilo.shell:root-cejnvssb`), so the walk gives the kind
    and the row it stands on is found whichever id the run gave it."""
    for row in rows:
        at = address(row)
        if at is None or at[1] != action:
            continue
        if at[0] == resource or (
                kind is not None and (row.get("resource") or {}).get("kind") == kind):
            return row
    return None


def value_of(answer: dict | str):
    try:
        return answer["Completed"]["output"]["value"]
    except (KeyError, TypeError):
        raise Failed(f"an answer without a value: {json.dumps(answer)[:2000]}") from None


def read_back(session: Session, app: str) -> dict:
    """The app's own read — the one on the resource its "Show this window" stands
    on — as this person reads it."""
    own = until_value(f"{app}'s 'Show this window'",
                      lambda: app_action(session, app, "activate-window"), 60)
    own = address(own)[0]
    action = until_value(f"{app}'s read on {own}",
                         lambda: app_action(session, app, "read", own), 60)
    return value_of(session.invoke_row(action))


def held_in(read, wanted: list[str]) -> list[str]:
    """The words of `wanted` a read of the app's own record does not hold: what
    the app says it keeps, read as one text, must name each of them. Words that
    are there come back empty, so a step fails on exactly what is missing."""
    text = read if isinstance(read, str) else json.dumps(read, ensure_ascii=False)
    return [word for word in wanted if word not in text]


def notes_held(read: dict) -> int | None:
    """How many notes a read of the app's own record lists, where the read says:
    the records whose kind is a note, or the collection of notes it holds under
    `notes` or `pages`. An app whose own read is of its notebook alone (its
    notes kept in a part beside it) answers None: what it holds is not this."""
    if not isinstance(read, dict):
        return None
    records = read.get("records")
    if isinstance(records, list) and records:
        counted = [r for r in records if isinstance(r, dict)
                   and str(r.get("kind", "")).rsplit(".", 1)[-1] in ("Note", "Page")]
        if counted:
            return len(counted)
    found: list[int] = []

    def walk(value, key: str | None) -> None:
        if isinstance(value, dict):
            items, count = value.get("items"), value.get("count")
            if isinstance(items, list) and (
                    (key or "").lower() in ("notes", "pages")
                    or any(isinstance(item, dict)
                           and str(item.get("type", "")).rsplit(".", 1)[-1] in ("Note", "Page")
                           for item in items)):
                found.append(count if isinstance(count, int) else len(items))
            for inner, inner_value in value.items():
                walk(inner_value, inner)
        elif isinstance(value, list):
            for inner in value:
                walk(inner, key)

    walk(read, None)
    return found[0] if found else None


# --- the versions a walk names -------------------------------------------------------------------

def source_of(spec: dict, variant: str) -> str | None:
    """The tree path whose presence means the version still exists: what the
    version names as its `source`, else its tilolang package's directory, else
    its native app's. A version whose source is gone was retired by the change
    under test and is not walked; the walk compares what replaced it."""
    v = spec[variant]
    if v.get("source"):
        return v["source"]
    if v.get("package"):
        return f"apps/tl/{v['package']}"
    native = v.get("native") or (spec.get("native") if variant == "rust" else None)
    if native:
        return f"apps/{native.split()[0]}"
    return None


# A version's `packages` naming this builds every tilolang app under apps/tl ([`tl_apps`]).
EVERY_TL_APP = "*"

# What the session says of a tilolang window that opened and shows nothing of its app: a spare
# window process that ended before it was bound or could not start, and a window whose instance
# its app's tilolang process was never handed.
UNOPENED = (
    "a spare window ended unbound",
    "no spare window",
    "a tilolang window's instance runs nowhere",
    "a tilolang window's instance was not handed to its process",
    "a tilolang window's instance could not be handed over",
)


def tl_apps() -> list[tuple[str, str]]:
    """Every tilolang app under apps/tl a person can open, as its directory and its id: each
    directory with an `app.tl`, the shell aside (it is the desktop, never one of its windows)."""
    found = []
    for directory in sorted((REPO / "apps/tl").iterdir()):
        source = directory / "app.tl"
        if not source.is_file():
            continue
        named = re.search(r"^app\s+([a-z0-9.\-]+)", source.read_text(), re.M)
        if named and named.group(1) != "so.tilo.shell":
            found.append((directory.name, named.group(1)))
    return found


def packages_of(spec: dict, variant: str) -> list[str]:
    """The tilolang packages this version's session installs: its own and the ones every
    version needs, each once; `*` names every tilolang app ([`tl_apps`])."""
    v = spec[variant]
    packages = list(v.get("packages") or ([v["package"]] if v.get("package") else []))
    packages += [p for p in spec.get("packages") or [] if p not in packages]
    if EVERY_TL_APP in packages:
        packages = [p for p in packages if p != EVERY_TL_APP]
        packages += [name for name, _ident in tl_apps() if name not in packages]
    return packages


def present(spec: dict, variant: str) -> bool:
    source = source_of(spec, variant)
    return source is None or (REPO / source).exists()


def apps_of(spec: dict, variant: str) -> str:
    """The session build's apps this version's session installs: the ones every
    version of the walk needs (`apps`, such as Files for a launcher walk), and
    this version's own native app."""
    v = spec[variant]
    native = v.get("native") or (spec.get("native", "") if variant == "rust" else "")
    return " ".join(name for name in f"{spec.get('apps', '')} {native}".split())


def live_of(spec: dict, named: list[str]) -> str:
    """The version a person gets today: the step file's `live`, where it is still
    in the tree, else the one version left."""
    live = spec.get("live")
    if live in named:
        return live
    return "tl" if "tl" in named else named[0]


# --- the walk ---------------------------------------------------------------------------------

def pick(value, variant: str):
    """The value a step means in `variant`: a dict naming the versions differs
    per version, anything else is the same for both."""
    if isinstance(value, dict) and set(value) & set(VARIANTS):
        if variant not in value:
            raise Failed(f"the step names no {variant} value: {json.dumps(value)}")
        return value[variant]
    return value


def alternative(value, variant: str):
    """One of several controls a step tries, or None when this one is written
    for a version other than `variant`."""
    if isinstance(value, dict) and set(value) & set(VARIANTS) and variant not in value:
        return None
    return pick(value, variant)


def targets_of(session: Session, selectors=()) -> list[dict]:
    """The controls the session shows, narrowed to those `selectors` name: a name, an identity
    or the start of a name is asked of the session, which returns those controls alone. A
    selector that picks among the controls of one name (`menuitem`) asks for that name's, and
    one that names what an identity does (`doing`) for those identities. No selectors lists
    every control: a step accounts for the whole surface only where it says so."""
    names, identities, prefixes = [], [], []
    doing = None
    whole = False
    for selector in selectors:
        if isinstance(selector, dict):
            if set(selector) == {"prefix"}:
                prefixes.append(selector["prefix"])
            elif set(selector) == {"menuitem"}:
                names.append(selector["menuitem"])
            elif set(selector) == {"doing"}:
                doing = selector["doing"]
            else:
                whole = True
        elif ID_SEP in str(selector):
            identities.append(selector)
        else:
            names.append(selector)
    if not selectors or whole:
        return session.targets()
    return session.targets(names=names, identities=identities, prefixes=prefixes, doing=doing)


def named_in(targets: list[dict], selector) -> dict | None:
    """The control `selector` names among `targets`, as `control` names one: an identity, a
    name, the first control whose name starts as `{"prefix": ..}` says, the one an identity
    does (`{"doing": ..}`), or the widest drawn one of a `{"menuitem": ..}` name."""
    if isinstance(selector, dict) and "menuitem" in selector:
        found = [
            target
            for target in targets
            if target.get("name") == selector["menuitem"] and drawn(target)
        ]
        return max(found, key=lambda target: target["rect"][2]) if found else None
    return next((target for target in targets if matches(target, selector)), None)


def control(session: Session, selector) -> dict | None:
    """The control `selector` names: an identity (a record kind and its
    operation), a name the widget answers to, `{"prefix": "note-"}` for the
    first control whose name starts that way (a row the app names by its id),
    `{"doing": "dpj7scfe"}` for the control that edits the field or runs the
    action with that id, on whatever record the window shows it for — the end
    of an identity is stable where the record it stands on is not — or
    `{"menuitem": "Tag…"}` for the widest drawn control of that name: the row a
    menu draws for its command, not a button of the window that says the same
    words. The session is asked for those controls alone, not every one it shows."""
    return named_in(targets_of(session, [selector]), selector)


def matches(target: dict, selector) -> bool:
    """Whether `target` is one `selector` names: the same forms `control` takes,
    minus the menu item's widest-of-a-name choice."""
    identity = target.get("identity") or ""
    if isinstance(selector, dict):
        if "prefix" in selector:
            return str(target.get("name", "")).startswith(selector["prefix"])
        return ID_SEP in identity and identity.rsplit(ID_SEP, 1)[-1] == selector.get("doing")
    if ID_SEP in str(selector):
        return identity == selector
    return target.get("name") == selector


def provider_addresses(said: str) -> dict:
    """The addresses a fake model provider server prints, by the name each line
    gives them (`OPENAI_API=…` becomes `openai-api`)."""
    found = {}
    for line in said.splitlines():
        name, _, value = line.partition("=")
        if name and value:
            found[name.strip().lower().replace("_", "-")] = value.strip()
    return found


def machine_sample() -> dict:
    """How busy the machine the walk runs on is now: its one-minute load and its cores, and the
    CPU time counters an open's share of stolen time is read from (on a virtual machine, the time
    its host ran something else while this machine had work: the node's own contention, which
    the machine's load does not show)."""
    with open("/proc/stat") as stat:
        times = [int(t) for t in stat.readline().split()[1:9]]
    return {"at": time.time(), "load": os.getloadavg()[0], "cpus": os.cpu_count(),
            "steal": times[7], "total": sum(times)}


def machine_during(samples: list[dict], at: float) -> dict:
    """The machine beside an open at `at`: its load and cores at the ask before it, and the share
    of its CPU time its host took from that ask to the next sample."""
    before = [s for s in samples if s["at"] <= at + 0.05]
    if not before:
        return {}
    start = before[-1]
    after = next((s for s in samples if s["at"] > start["at"]), None)
    found = {"load": round(start["load"], 1), "cpus": start["cpus"]}
    if after is not None and after["total"] > start["total"]:
        found["steal_pct"] = round(100 * (after["steal"] - start["steal"])
                                   / (after["total"] - start["total"]), 1)
    return found


def session_env(state: Path) -> dict:
    """What a walk's session is told about the stand-ins beside it: where the
    wireless display broker listens, so the display sheet finds the walk's
    stand-in rather than a service this machine does not run."""
    return {"TILO_PROJECTOR_RUNTIME_DIR": str(state / "projector"),
            # Each open's timings, which the walk holds to the open bar (`Walk.opens`).
            "RUST_LOG": f"{os.environ.get('RUST_LOG') or 'warn'},tilo::launch_latency=info"}


def resolved(served: dict, value):
    """`value` with every `{"served": "name.path"}` in it replaced by what the
    run answered under `name`: a provider's id, a stand-in's address, which no
    step file can know before the run makes them."""
    if isinstance(value, dict) and "served" in value:
        at = served
        for key in str(value["served"]).split("."):
            at = at.get(key) if isinstance(at, dict) else None
        if at is None:
            raise Failed(f"nothing the run served is called {value['served']!r}")
        return at
    if isinstance(value, dict):
        return {key: resolved(served, inner) for key, inner in value.items()}
    if isinstance(value, list):
        return [resolved(served, inner) for inner in value]
    return value


def drawn(target: dict) -> bool:
    """Whether `target` is drawn anywhere: a control a window holds open but does
    not show — a menu's items while its menu is closed — stands nowhere, and a
    press aimed at it lands on nothing."""
    x, y, w, h = target["rect"]
    return w > 0 and h > 0 and x >= 0 and y >= 0


def row_of(session: Session, window: dict | None, name: str) -> dict | None:
    """The row `name` in the app window's own sidebar: a control with those words
    drawn in the window's leading column, below its header — never the desktop's
    tab of a window named by the same words."""
    if window is None or window.get("rect") is None:
        return None
    x, y, w, _h = window["rect"]
    for target in targets_of(session, [name]):
        if target.get("name") != name or not drawn(target):
            continue
        tx, ty, _tw, _th = target["rect"]
        if ty > y + 30 and tx < x + min(320.0, w / 2):
            return target
    return None


def show_sidebar(session: Session, window: dict | None, name: str) -> None:
    """Nothing when the row `name` stands in the window's sidebar already; else press
    the window's own Show sidebar, where the window is narrow enough to show one
    column at a time and keeps its places behind that button: what a person does
    before they pick a place there."""
    if row_of(session, window, name) is not None:
        return
    if window is None or window.get("rect") is None:
        return
    x, y, w, h = window["rect"]
    for target in targets_of(session, ["Show sidebar"]):
        if target.get("name") != "Show sidebar" or not drawn(target):
            continue
        tx, ty, tw, th = target["rect"]
        if tx >= x and ty >= y and tx + tw <= x + w and ty + th <= y + h:
            session.click_at(tx + tw / 2, ty + th / 2)
            return


def press(session: Session, selector, verb: str = "activate") -> dict:
    """Press the control `selector` names, as a person's pointer does; `verb`
    opens its context menu where the control offers one."""
    target = control(session, selector)
    if target is None:
        raise Failed(f"no control named {selector!r} is offered")
    identity = target.get("identity") or ""
    if ID_SEP in identity:
        return session.click_doing(identity, verb)
    return session.click_named(target["name"], verb)


def hover(session: Session, selector: str, deadline: float = 20.0) -> dict:
    """Rest the pointer on the control `selector` names: what a row's own
    controls appear for."""
    target = until_value(f"the control {selector!r} to rest on",
                         lambda: control(session, selector), deadline)
    x, y, w, h = target["rect"]
    session.control({"op": "pointer_move", "x": x + w / 2, "y": y + h / 2})
    return target


# --- a person's other devices -----------------------------------------------------------------

# The provider the device's daemon issues the one-use code a new device joins with under: the
# action Settings' Add a device asks it for. The walk asks the daemon itself, as the page does;
# the listing gives a device provider's rows only when it is named, never in the whole catalogue.
DEVICE_TICKET = ("tilo.device", "devices", "issue")
# The credential a device's daemon spends at its first start to become its person's: the name the
# unit hands it in `$CREDENTIALS_DIRECTORY`.
TICKET_CREDENTIAL = "tilo.ticket"


def profile_socket(runtime: str, profile: Path) -> Path:
    """The native socket of the daemon serving `profile` on a computer whose daemon is the
    system's: `tilo_device_identity::login::daemon::profile_sockets` names it by an FNV-1a hash
    of the profile's path in the account's runtime directory."""
    held = 0xCBF2_9CE4_8422_2325
    for byte in str(profile).encode():
        held ^= byte
        held = (held * 0x0000_0100_0000_01B3) & 0xFFFF_FFFF_FFFF_FFFF
    return Path(runtime) / "tilo" / f"profile-{held:016x}" / "native.sock"


def devices_of(spec: dict) -> list[dict]:
    """The devices a walk names, in order, each `{"name", "session_args"?}`: `"devices":
    ["laptop", "phone"]`, or an entry as an object where one device starts differently (the
    laptop with two displays). The first is the device every op runs on unless it says
    otherwise; a walk naming none has one device, and its name is empty."""
    named = spec.get("devices") or []
    return [entry if isinstance(entry, dict) else {"name": entry} for entry in named] \
        or [{"name": ""}]


def session_args_of(spec: dict, variant: str, device: str | None = None) -> list[str] | None:
    """How a device's session starts in `variant`: the version's own arguments, then the device's
    (`--output` for a second display). `device` is the first device when None."""
    entries = devices_of(spec)
    entry = next((e for e in entries if e["name"] == device), None) if device else entries[0]
    args = list(spec[variant].get("session_args") or []) + list((entry or {}).get("session_args")
                                                                or [])
    return args or None


# Where a walk's devices hear each other: the sessions are beside each other on this host's
# loopback, and the network a check's machine has carries no multicast between them (as the
# session's own multi-device tests start their devices, crates/desktop/session/tests/common).
LAN_NETWORKS_ENV = "TILO_LAN_NETWORKS"
# What a walk of several devices' sessions say: why a join or a sync did not land is the finding.
DEVICES_LOG = ("warn,tilo::launch_latency=info,tilo::join_flow=debug,tilo_session::layout=info,"
               "tilo_peer_state=debug,tilo_session::workspaces=debug,"
               "tilo_session::person_resources=debug,tilo_endpoint_reachability=debug,"
               "tilo_network_actor=debug,tilo_session::sync=debug")


def devices_env(spec: dict) -> dict | None:
    """The environment each device's session of the walk starts with: None for a walk of one.
    A further device is named as the walk names it when it pairs (`phone`); the first is the
    machine's own."""
    if len(devices_of(spec)) < 2:
        return None
    return {LAN_NETWORKS_ENV: "127.0.0.1", "RUST_LOG": os.environ.get("RUST_LOG", DEVICES_LOG)}


def first_device_env(state: Path, spec: dict) -> dict:
    """The environment the walk's own session starts with: where its local services listen, and for a walk of
    several devices what lets them meet; a walk of one adds nothing for devices."""
    return {**session_env(state), **(devices_env(spec) or {})}


def split_on(op: dict, default: str | None) -> tuple[str | None, dict]:
    """The device an op runs on and the op without that word: `{"on": "phone", "must": [...]}`
    runs `must` on the phone, an op saying nothing runs on the step's device (`default`, the
    step's own `"on"`), and None means the first device."""
    if "on" not in op:
        return default, op
    rest = {key: value for key, value in op.items() if key != "on"}
    return op["on"], rest


def at_path(value, path: str):
    """What `path` names in an action's answer: dotted keys and list places
    (`layout.displays.0.rect`), `[key=value]` for the first entry of a list whose
    field reads so (`devices[label=phone].id`), and `#len` at the end for how many a
    list or map holds. None where the answer has no such place."""
    path, _, measure = path.partition("#")
    at = value
    for part in filter(None, path.split(".")):
        key, _, test = part.partition("[")
        if key:
            if isinstance(at, dict):
                at = at.get(key)
            elif isinstance(at, list) and key.isdigit() and int(key) < len(at):
                at = at[int(key)]
            else:
                return None
        if test:
            field, _, wanted = test.rstrip("]").partition("=")
            if not isinstance(at, list):
                return None
            at = next((item for item in at if isinstance(item, dict)
                       and str(item.get(field)) == wanted), None)
    if measure == "len":
        return len(at) if isinstance(at, (list, dict)) else None
    return at


def choices_of(row: dict) -> dict:
    """The first answer a catalogue row's action offers for each parameter that offers some: the
    values its `choices` name (a device's handle, a folder to save to), else its schema's `enum`,
    or the `const` of its first `oneOf`. What a person picks from a list, the walk picks the first
    of."""
    found = {}
    for param in (row.get("action") or {}).get("params") or []:
        schema = param.get("schema") or {}
        offered = (param.get("choices")
                   or schema.get("enum")
                   or [o.get("const") for o in schema.get("oneOf") or []
                       if isinstance(o, dict) and "const" in o])
        if offered:
            found[param["name"]] = offered[0]
    return found


def filled(arg, kept: dict):
    """`arg` with each `{name}` a walk kept replaced by its value: a string that is
    only `{name}` becomes the value itself (a revision stays a number), one that
    names it among other words gets it as text."""
    if isinstance(arg, str):
        for name, value in kept.items():
            if arg == f"{{{name}}}":
                return value
        for name, value in kept.items():
            arg = arg.replace(f"{{{name}}}", value if isinstance(value, str)
                              else json.dumps(value))
        return arg
    if isinstance(arg, dict):
        return {key: filled(inner, kept) for key, inner in arg.items()}
    if isinstance(arg, list):
        return [filled(inner, kept) for inner in arg]
    return arg


class Devices:
    """The person's devices a walk drives: one headless session each, of the same person, on
    this host. The first is the walk's own session; each further one is a session on a state
    of its own, with its own device key, added the way a person adds one (Add a device on the
    first, the one-use code it prints spent as the new device's ticket at its first start), and
    shown the workspace the first one is in. They meet over the local network, as two devices of
    one person on one network do."""

    def __init__(self, first: str, session: Session):
        self.order = [first]
        self.sessions: dict[str, Session] = {first: session}
        #: The device daemons each further device started: the session's, as a device's own
        #: service is, and stopped with the walk.
        self.daemons: list[subprocess.Popen] = []
        # What fell short of making the devices one person's in one workspace: every step after
        # it says what that leaves out, so the walk goes on and this is kept in its report.
        self.trouble: list[str] = []
        # How long each further device took to become the person's and come up, by phase
        # (`prepare`, `pair`, `start`, `meet`), in milliseconds: what a walk of devices waits.
        self.took: dict[str, dict[str, int]] = {}

    def lap(self, name: str, phase: str, began: float) -> float:
        now = time.monotonic()
        self.took.setdefault(name, {})[phase] = round((now - began) * 1000)
        return now

    @property
    def first(self) -> str:
        return self.order[0]

    def session(self, name: str | None) -> Session:
        name = self.first if name in (None, "") else name
        if name not in self.sessions:
            raise Failed(f"the step names a device the walk has not: {name!r} "
                         f"(it has {', '.join(self.order)})")
        return self.sessions[name]

    def further(self) -> list[str]:
        return self.order[1:]

    def add(self, name: str, state: Path, *, apps: str, tapps: list[Path], out: Path, tag: str,
            session_args: list[str] | None, env: dict | None = None) -> Session:
        """Add `name` as the person's next device and start its session on `state`, which
        `prepare_state` has made: the first device prints the one-use code a new computer joins
        with, and the new device's daemon spends it as its `tilo.ticket` credential at its first
        start, as a machine set up without anyone at it does. Its session then starts as that
        person, in the workspace the first one shows."""
        first = self.sessions[self.first]
        logs = out / "logs"
        logs.mkdir(parents=True, exist_ok=True)
        began = time.monotonic()
        # The code the daemon prints for a new device of the person's: the device action Settings'
        # Add a device runs, asked through the desktop's own dispatch. A walk joins devices without
        # opening the page, and the page and the walk ask the same daemon for the same kind of
        # one-use code.
        answer = first.invoke(*DEVICE_TICKET, {})
        ticket = value_of(answer)
        if isinstance(ticket, dict):
            ticket = ticket.get("ticket")
        if not str(ticket).startswith("tilo1"):
            raise Failed(
                f"the new device's code: Add a device answered {json.dumps(answer)[:300]}")
        if not str(ticket).startswith("tilo1"):
            raise Failed("Add a device copied no ticket")
        # The ticket stays out of every log and argument: it is handed to the new device's daemon
        # as the credential it spends once, from a private directory.
        credentials = state / "credentials"
        credentials.mkdir(mode=0o700, parents=True, exist_ok=True)
        ticket_file = credentials / TICKET_CREDENTIAL
        ticket_file.touch(mode=0o600)
        ticket_file.write_text(str(ticket))
        began = self.lap(name, "pair", began)
        # The device's own daemon, as a device's service is: it serves the profile named in its
        # environment, under this login's runtime directory, and spends the ticket at its start.
        account = real_apps.session_account()
        session_env = {**(env or {}),
                       "TILO_SESSION_MACHINE_DAEMON": "1",
                       "TILO_RUNTIME_PROFILE": str(state / "profile")}
        if account is None:
            # No account of its own to run a daemon as: the session starts the daemon it would
            # start on a developer's machine, over its own state.
            session_env.pop("TILO_SESSION_MACHINE_DAEMON")
            session_env.pop("TILO_RUNTIME_PROFILE")
        else:
            real_apps.hand_over(state, account)
        if account is not None:
            daemon_env = {**os.environ, **(env or {}),
                          **real_apps.account_env(account, state),
                          "TILO_RUNTIME_PROFILE": str(state / "profile"),
                          "CREDENTIALS_DIRECTORY": str(credentials),
                          "TILO_APPS_DATA_DIR": str(state / "apps"),
                          "TILO_APPS_SYSTEM_DIR": str(state / "system"),
                          "TILO_RUNTIME_BUNDLE_DIR": str(state / "apps/runtimes"),
                          "TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE":
                              str(state / "keys/runtime-publishers.txt"),
                          "RUST_LOG": os.environ.get("RUST_LOG", DAEMON_LOG)}
            daemon_log = (logs / f"device-daemon-{tag}.log").open("w")
            daemon = subprocess.Popen(
                [*as_account(account), str(BUILD / "bin/tilo-runtime")],
                stdout=daemon_log, stderr=subprocess.STDOUT, env=daemon_env,
                start_new_session=True)
            self.daemons.append(daemon)
            socket = profile_socket(daemon_env["XDG_RUNTIME_DIR"], state / "profile")

            def daemon_died():
                said = ""
                with contextlib.suppress(OSError):
                    said = (logs / f"device-daemon-{tag}.log").read_text(
                        errors="replace")[-1200:]
                raise Failed(
                    f"the {name}'s device daemon ended ({daemon.returncode}): {said}")

            until(f"the {name}'s device daemon",
                  lambda: socket.exists()
                  or (daemon.poll() is not None and daemon_died()), 120, 0.1)
        session = Session(state)
        session.start(apps=apps, fresh=True, out=logs, tag=tag, tapps=tapps,
                      session_args=session_args, env=session_env)
        began = self.lap(name, "start", began)
        root = "identity/user-identity/pubkey.bin"
        if (state / root).read_bytes() != (first.state / root).read_bytes():
            raise Failed(f"the {name} joined as a different person")
        self.order.append(name)
        self.sessions[name] = session
        # The sheet the person added the device from is closed, as they close it.
        try:
            first.key("Escape")
        except Failed:
            pass
        try:
            self.show_same_workspace(name)
            self.lap(name, "meet", began)
        except Failed as error:
            self.lap(name, "meet", began)
            self.trouble.append(f"{name}: {error}")
            # What each device holds of the person, where they did not meet: their identity's
            # files (which of them founds the person's own workspace) and their workspaces.
            for device, at in ((self.first, first), (name, session)):
                identity = at.state / "config/tilo/identity"
                held = ((at.state_dump() or {}).get("held_workspaces")) if at.running() else None
                self.trouble.append(
                    f"{device}: identity holds {sorted(p.name for p in identity.iterdir()) if identity.is_dir() else None}; "
                    f"holds the workspaces {json.dumps(held)}")
            print(f"  the {name} is not in the {self.first}'s workspace: {error}", file=sys.stderr)
        return session

    def show_same_workspace(self, name: str) -> str:
        """Bring the workspace the first device shows to the front on `name`, as the person
        picks it there: a workspace that arrived from their list stands behind the one the
        device made for itself."""
        first, other = self.sessions[self.first], self.sessions[name]

        def front(session: Session) -> str | None:
            return ((session.state_dump() or {}).get("workspace_tree") or {}).get("active")

        workspace = until_value(f"the workspace the {self.first} shows", lambda: front(first), 30)

        def held() -> bool:
            tree = (other.state_dump() or {}).get("workspace_tree") or {}
            return any(w.get("id") == workspace for w in tree.get("workspaces") or [])

        until_value(f"the {name} to hold the {self.first}'s workspace", held, 60, 0.25)
        if front(other) != workspace:
            other.invoke(SHELL, "desktop:chrome", "switch-workspace", {"workspace": workspace})
            until_value(f"the {name} to show the {self.first}'s workspace",
                        lambda: front(other) == workspace, 30)
        return workspace

    def stop(self) -> None:
        """Stop every further device's session and its daemon; the first is the walk's to stop."""
        for name in self.further():
            self.sessions[name].stop()
        for daemon in self.daemons:
            with contextlib.suppress(ProcessLookupError):
                os.killpg(daemon.pid, signal.SIGTERM)
            with contextlib.suppress(subprocess.TimeoutExpired):
                daemon.wait(30)
        self.daemons.clear()


def start_devices(spec: dict, variant: str, session: Session, state: Path, out: Path,
                  apps: str, tapps: list[Path]) -> Devices | None:
    """The person's further devices the walk names, each paired to the first device's session
    `session` and started, or None for a walk of one device. Each device's state sits beside the
    first one's (`state-tl` and `state-tl-phone`)."""
    entries = devices_of(spec)
    if len(entries) < 2:
        return None
    devices = Devices(entries[0]["name"], session)
    try:
        for entry in entries[1:]:
            name = entry["name"]
            began = time.monotonic()
            own = state.parent / f"{state.name}-{name}"
            prepare_state(own, apps, build=False)
            devices.lap(name, "prepare", began)
            devices.add(name, own, apps=apps, tapps=tapps, out=out, tag=f"{variant}-{name}",
                        session_args=session_args_of(spec, variant, name),
                        env=devices_env(spec))
            print(f"==> {variant}: the {name} paired and up in "
                  f"{round((time.monotonic() - began) * 1000)} ms", file=sys.stderr)
    except BaseException:
        devices.stop()
        raise
    return devices


class Walk:
    """One version's walk: its session, its steps and what each one found."""

    def __init__(self, spec: dict, variant: str, out: Path, state: Path,
                 discover: bool = False, session: Session | None = None,
                 devices: Devices | None = None, machine: "MachineFiles | None" = None):
        self.spec = spec
        self.variant = variant
        self.v = spec[variant]
        self.out = out
        self.state = state
        # A walk being written: every step keeps the controls the version offered,
        # so the step file can name the one that is really there.
        self.discover = discover
        self.shots = out / "shots" / variant
        # A family's walks share one session per version: it is started once, and
        # each walk starts where the reset after the one before left the desktop.
        self.shared = session is not None
        self.session = session or Session(state)
        self.steps: list[dict] = []
        self.window: dict | None = None
        # The stand-in mail server a step started, stopped when the walk ends.
        self.mail: LocalMailServer | None = None
        # The frames a step's window drew when its `window_frames` first ran.
        self.frames_seen: dict[str, int] = {}
        # The stand-in services this walk started beside the session (op `serve`),
        # stopped when its session goes.
        self.services: list = []
        # The machine's files daemon and its device daemon, when this walk started its own
        # session on a machine it roots (a check's VM): a family's are the family's, stopped
        # with its session.
        self.machine: MachineFiles | None = machine
        self.daemon: MachineDaemon | None = None
        # What the run answered and a later step may name: a stand-in's address,
        # a provider's id (`{"served": "provider.id"}`), a page server's address.
        self.served: dict = {}
        # The pages a `serve` step put on this machine's loopback.
        self.pages: ThreadingHTTPServer | None = None
        # The person's further devices, where the walk names them (`"devices"`): a family's
        # are started once with its session, a walk of its own starts them after its own.
        self.devices = devices
        # The device the op being run is on (None: the first), and the theme it shows.
        self.device: str | None = None
        self.step_on: str | None = None
        # The theme the walk last left each device in, once it has set one: what a theme
        # op is held against, so asking for the theme already shown changes nothing.
        self.dark: bool | None = None
        self.darks: dict[str, bool | None] = {}
        # How long the walk has waited for the shell to draw a theme it switched to: part of a
        # step's time a baseline that never waited for it does not hold.
        self.theme_waited_ms = 0
        # What an op kept of an answer (`op_use`'s `keep`), for the ops after it.
        self.kept: dict = {}
        # The controls the step's last op looked at, where it looked at all of them: the
        # step's end reads that look rather than asking for the same controls again. Any op
        # after it changes the surface, and puts this back.
        self.listed: list[dict] | None = None
        # When the walk's first step began, by the session log's clock: its opens are the
        # launches of its app the log records after it.
        self.began_epoch = time.time()
        # Whether a step asked for this walk's app: a walk that did and finds no open of it in the
        # session's launch timings records that, rather than nothing measured.
        self.asked_open = False
        # How busy the machine was at each ask for this walk's app, and when the walk read its
        # opens: what each open's time is read beside.
        self.samples: list[dict] = []

    def asking(self) -> None:
        """A step asks for this walk's app now."""
        self.asked_open = True
        self.samples.append(machine_sample())

    def opens(self) -> list[dict]:
        """Every open in this walk that started its app, as the session's launch timings
        record it: the first is the app's cold open, each later one follows the app's quit (its
        last window closed, or it was ended). Each says how long from the ask to the frame that
        showed the window (`ms`), and where that went: the launcher's start (`spawn_ms`) and the
        window's mapping (`map_ms`). An open with no presented frame says so (`never_shown`)."""
        ident = self.v.get("id")
        if ident is None:
            return []
        # A launch is named by its id within the session that ran it: a walk that restarts the
        # session counts launches from one again.
        by_launch: dict[tuple[str, str], dict] = {}
        for line in real_apps.launch_lines(self.session.log()):
            if line.get("package_id") != ident or line["epoch"] < self.began_epoch - 0.05:
                continue
            key = (line.get("launch_generation", ""), line.get("launch_id", ""))
            by_launch.setdefault(key, {})[line["stage"]] = line
        samples = [*self.samples, machine_sample()]
        found = []
        for (_generation, launch_id), stages in by_launch.items():
            if "spawned" not in stages:
                continue
            record = {"open": "cold" if not found else "after a quit", "launch_id": launch_id,
                      "spawn_ms": round(int(stages["spawned"]["request_to_spawn_us"]) / 1000)}
            if "mapped" in stages:
                record["map_ms"] = round(int(stages["mapped"]["spawn_to_map_us"]) / 1000)
            if "presented" in stages:
                record["ms"] = round(int(stages["presented"]["request_to_first_present_us"]) / 1000)
            else:
                record["never_shown"] = True
            record.update(machine_during(samples, stages["spawned"]["epoch"]))
            found.append(record)
        if self.asked_open and not found:
            found.append({"open": "cold", "never_shown": True, "unlogged": True})
        return found

    def targets(self) -> list[dict]:
        """The controls the desktop offers now, as the walk records them where a
        step could not be walked: a name or an identity, and where it is."""
        return [{"name": t.get("name"), "identity": t.get("identity"), "rect": t.get("rect"),
                 "level": t.get("level")}
                for t in self.session.targets()]

    def actions(self, only_app: bool = False) -> list[dict]:
        """The actions the desktop lists now, in the words a person's palette
        reads: what a verb with no control still is. Every application's, unless
        `only_app` keeps this app's — listed as that application's alone — so a
        step that could not be walked says what was published beside what it
        wanted."""
        rows = self.session.actions(app=self.v.get("id") if only_app else None)
        return [{"title": (a.get("action") or {}).get("action_label"),
                 "operation": (address(a) or ("", ""))[1],
                 "application": a.get("app_id"), "resource": (address(a) or ("", ""))[0]}
                for a in rows
                if address(a) and (not only_app or a.get("app_id") == self.v.get("id"))]

    def shot(self, name: str) -> str:
        path = self.shots / f"{name}.png"
        self.session.snapshot(path)
        return str(path)

    def require_shown(self, targets: list[dict] | None = None) -> None:
        """The app's window shows the app: under the window's own bar it draws at least one
        named control. A window with only its bar is what a person meets when the app's view
        failed — a blank window — and no step waiting on this or that control catches it when
        the step's state is one the app still renders something of. Every step the walk leaves
        a window open in reads this, so a blank window fails wherever it appears, in an app the
        walk had a step for and in one it did not. A window on a strip that is not in front is
        not one a person reads: it is drawn in that strip's band, and the controls it holds stand
        where the app laid them out, so nothing of them is read against the band's frame.
        `targets` is the step's one listing of the
        controls, where it has read one already: a step looks at the surface once and both
        checks read that look."""
        window = self.window_of()
        if window is None or window.get("rect") is None:
            return
        if window.get("active") is not True:
            return
        x, y, w, h = window["rect"]
        if targets is None:
            targets = self.session.targets()
        for target in targets:
            name = (target.get("name") or "").strip()
            if not name or not drawn(target):
                continue
            tx, ty, tw, th = target["rect"]
            # The window's own bar, and the shell's chrome about the window, are not the
            # app's: what it draws stands under them, inside its frame.
            if ty < y + 40:
                continue
            if tx >= x and ty >= y and tx + tw <= x + w and ty + th <= y + h:
                return
        raise Failed(
            f"the window of {self.v.get('id')} draws nothing of the app under its bar: "
            "a blank window")

    def window_of(self) -> dict | None:
        """The window of the app this walk opens, as the shell holds it."""
        ident = self.v.get("id")
        if ident is None:
            return None
        found = next((w for w in self.session.windows(ident) if w.get("app_id") == ident), None)
        if found is not None:
            self.window = found
        return found

    def launch(self, wait: bool = True, deadline: float = 120.0,
               answer: str | None = None) -> dict:
        """Open the app through the desktop's own launcher. Without `wait`, the
        walk goes on the moment the launcher takes it: what a person presses
        before the app is ready. `answer` is the control a question before the
        app's process asks (the network its manifest declares): the person's
        yes, pressed as they press it, after which the app starts. Answers where
        the wait went: the catalogue listing that finds the app's row and the
        invoke that starts it (`listing_ms`, `invoke_ms` of the ask), the window
        (`window_ms`) and the app's own process and window (`up_ms`)."""
        self.asking()
        began = time.monotonic()
        self.session.open(self.v["id"])
        asked = round((time.monotonic() - began) * 1000)
        took = {**self.session.opened_took, "asked_ms": asked}
        if answer is not None:
            selector = {"prefix": answer}
            until_value(f"the question {answer!r} before {self.v['id']} starts",
                        lambda: control(self.session, selector), deadline)
            press(self.session, selector)
        if wait:
            until_value(f"the window of {self.v['id']}", self.window_of, deadline)
            # The app's own process in the session's store, where the launcher
            # runs it from: a window without one is the shell's, not the app's.
            windowed = round((time.monotonic() - began) * 1000)
            took["window_ms"] = windowed - asked
            try:
                self.session.answering(self.v["id"], deadline)
            except Failed:
                pass
            took["up_ms"] = round((time.monotonic() - began) * 1000) - windowed
        return took

    def close_window(self) -> None:
        window = self.window_of()
        if window is None:
            return
        self.session.invoke(SHELL, SHELL_RESOURCE, "close-tab",
                            {"tab": str(window["tab"])})
        until_value(f"the window of {self.v['id']} to close",
                    lambda: self.window_of() is None, 30)

    def quit_app(self) -> None:
        """End the app, not only its window: the next launch is a cold start,
        which is what a person who starts a note the moment it opens meets.
        A person quits an app by closing its last window, and the app ends with
        it; one that stays is ended as a person ends an app that will not."""
        window = self.window_of()
        if window is None:
            return
        self.close_window()
        try:
            until_value(f"the app {self.v['id']} to end",
                        lambda: not self.session.processes(self.v["id"]), 15)
        except Failed:
            # The app did not end on its own: a force quit is the person's own,
            # never a crash the shell reports.
            self.session.invoke(SHELL, SHELL_RESOURCE, "force-quit-app",
                                {"tab": str(window["tab"])})
            until_value(f"the app {self.v['id']} to end",
                        lambda: not self.session.processes(self.v["id"]), 30)

    def dark_now(self) -> tuple[str | None, bool | None]:
        """The appearance mode the person's settings name (`None` where they name none, and the
        host's scheme decides), and whether the shell draws dark now (`None` without a shell)."""
        found = self.session.control({"op": "settings"})
        settings = found.get("answer") or {}
        mode = (settings.get("appearance") or {}).get("mode")
        return (mode if mode in ("dark", "light") else None), found.get("draws_dark")

    def set_dark(self, dark: bool, before_switch=None) -> bool:
        """Switch the desktop's appearance through the action the person's Settings use,
        addressed where the settings provider keeps it: finding it by listing every action on
        the desktop waits on a catalogue that grows with what the person keeps, and the action
        has one address. Settings that name no mode name the one asked for, so every later op
        is held against a theme the walk chose; colours the desktop already draws are not
        waited on. `before_switch`, when given, runs just before the switch. Whether what the
        desktop draws changed."""
        def switch():
            if before_switch is not None:
                before_switch()
            self.session.invoke(SETTINGS, APPEARANCE, "set-dark-mode", {"on": dark})
            self.dark = dark

        if self.dark is None:
            mode, draws_dark = self.dark_now()
            drawn = draws_dark if draws_dark is not None else mode == "dark"
            if mode is None:
                switch()
                return drawn != dark
            self.dark = drawn
        if self.dark == dark:
            return False
        switch()
        return True

    def switch_shell_theme(self, dark: bool) -> bool:
        """Switch a walk of the shell to the theme named and wait for the shell to draw in it:
        it wears it, and has presented a frame since the switch. A theme it already draws
        waits for nothing. Whether what it draws changed."""
        before = []
        switched = self.set_dark(dark, lambda: before.append(self.session.presented_frames()))
        if switched:
            waited = time.monotonic()
            try:
                until_value(
                    f"the session to draw the {'dark' if dark else 'light'} theme",
                    lambda: self.dark_now()[1] in (None, dark)
                    and self.session.presented_frames() > before[0], 20, 0.02)
            except Failed:
                pass
            self.theme_waited_ms += round((time.monotonic() - waited) * 1000)
        return switched

    def theme_shot(self, name: str, dark: bool, spent=None) -> str:
        """The state `name` again, in the theme named: the person switches it and
        looks at what they had. `spent`, when given, hears how long the switch took."""
        if self.v.get("id") is None:
            # A walk of the shell has no app window to count frames of: the session itself
            # says how many frames it has presented, and which theme its shell wears.
            switch_began = time.monotonic()
            self.switch_shell_theme(dark)
            if spent is not None:
                spent("dark_switch", switch_began)
            return self.shot(f"{name}-{'dark' if dark else 'light'}")
        before = (self.window_of() or {}).get("frames") or 0
        switch_began = time.monotonic()
        switched = self.set_dark(dark)
        if spent is not None:
            spent("dark_switch", switch_began)
        # A new theme reaches a window on the next frame it draws: wait for the
        # app's own frame count to move, then shoot what it drew. A theme that was
        # already shown draws nothing again for it.
        if switched:
            try:
                until_value(f"the app to draw again in the {'dark' if dark else 'light'} theme",
                            lambda: ((self.window_of() or {}).get("frames") or 0) > before,
                            20, 0.05)
            except Failed:
                pass
        return self.shot(f"{name}-{'dark' if dark else 'light'}")

    # -- the ops a step file may name ------------------------------------------------------

    def op(self, op: dict) -> dict:
        """One op of a step, as the step file writes it, and what it found. An op saying
        `"on": "phone"` (or every op of a step saying it) runs on that device of the walk's:
        each op acts on `self.session`, which is that device's session while it runs."""
        device, op = split_on(op, self.step_on)
        (name, value), = op.items()
        value = self.with_files(value)
        # An op after one that listed every control changes the surface: what it saw is not
        # what the step ends on.
        self.listed = None
        handler = getattr(self, f"op_{name}", None)
        if handler is None:
            raise Failed(f"the step names an op this walk has no way to do: {name!r}")
        if device in (None, "") or (self.devices is not None and device == self.devices.first):
            return handler(value) or {}
        if self.devices is None:
            raise Failed(f"the op names the device {device!r}, and the walk names no devices")
        return self.on_device(device, lambda: handler(value) or {})

    #: What a step writes for the folder of the workspace the session runs, where the person's
    #: files are (`files`): the workspace's id is minted per run, so no step can name it.
    FILES = "$FILES"

    def with_files(self, value):
        """`value` with every string's leading `$FILES` read as the person's files folder."""
        if isinstance(value, str):
            if value == self.FILES or value.startswith(self.FILES + "/"):
                return str(self.files()) + value[len(self.FILES):]
            return value
        if isinstance(value, list):
            return [self.with_files(item) for item in value]
        if isinstance(value, dict):
            return {key: self.with_files(item) for key, item in value.items()}
        return value

    def on_device(self, device: str, run):
        """`run()` with `device`'s session as the walk's: what it presses, reads and shoots is
        that device's. The first device's session and theme are back after."""
        own, own_dark = self.session, self.dark
        self.darks[self.devices.first] = own_dark
        self.session = self.devices.session(device)
        self.dark = self.darks.get(device)
        self.device = device
        try:
            return run()
        finally:
            self.darks[device] = self.dark
            self.session, self.dark = own, own_dark
            self.device = None

    def op_launch(self, value) -> dict:
        """Open the app, waiting for its window unless the value says not to, and
        answering the question the app asks before its process starts when the
        value names the answer (`{"answer": "Allow"}`)."""
        wait = value if isinstance(value, bool) else value.get("wait", True)
        answer = value.get("answer") if isinstance(value, dict) else None
        began = time.monotonic()
        took = self.launch(wait=wait, deadline=float(value.get("deadline", 120))
                           if isinstance(value, dict) else 120.0, answer=answer)
        waited = None
        if wait:
            waited = round((time.monotonic() - began) * 1000)
        return {"opened": self.v["id"], "waited_ms": waited, **took}

    def op_press(self, value) -> dict:
        selector = pick(value, self.variant)
        found = control(self.session, selector)
        press(self.session, selector)
        return {"pressed": selector, "offered": found is not None}

    def op_early_press(self, value) -> dict:
        """Open the app and press its control the moment it is on screen: what a
        person does who starts a note right after opening Notes, before the app
        has caught up. A press that is taken makes a note; a press that is lost
        leaves the notebook as it was, and the count says which."""
        wanted = value.get("controls") or [value["control"]]
        offered = [s for s in map(lambda o: alternative(o, self.variant), wanted) if s]
        if value.get("close", True):
            # Closed the way a person closes a window, then opened again: the
            # launch is a cold one, and the window left behind is theirs.
            self.close_window()
        began = time.monotonic()
        self.asking()
        self.session.open(self.v["id"])

        def shown():
            if self.window_of() is None:
                return None
            listed = targets_of(self.session, offered)
            for selector in offered:
                found = named_in(listed, selector)
                if found is not None:
                    return found
            return None

        target = until_value(f"a control of {self.v['id']} on screen", shown,
                             float(value.get("deadline", 120)), 0.02)
        x, y, w, h = target["rect"]
        self.session.click_at(x + w / 2, y + h / 2)
        pressed = round((time.monotonic() - began) * 1000)
        until_value(f"the window of {self.v['id']}", self.window_of, 120)
        try:
            self.session.answering(self.v["id"], 120)
        except Failed:
            pass
        return {"pressed": target.get("identity") or target["name"],
                "pressed_ms_after_launch": pressed, "rect": target["rect"]}

    def op_press_any(self, value) -> dict:
        """Press the first of `selectors` the version offers, and say which: a
        step whose control one version draws and the other does not. Finding
        none is the finding, and the walk goes on."""
        offered = [s for s in map(lambda o: alternative(o, self.variant), value) if s]
        listed = targets_of(self.session, offered)
        for selector in offered:
            if named_in(listed, selector) is not None:
                press(self.session, selector)
                return {"pressed": selector}
        return {"pressed": None, "offered_none": offered}

    def op_offered(self, value) -> dict:
        """Which of `selectors` the version offers now, without pressing one."""
        offered = [s for s in map(lambda o: alternative(o, self.variant), value) if s]
        listed = targets_of(self.session, offered)
        for selector in offered:
            if named_in(listed, selector) is not None:
                return {"offered": selector}
        return {"offered": None, "offered_none": offered}

    def op_hover(self, value) -> dict:
        selector = pick(value, self.variant)
        hover(self.session, selector)
        return {"rested_on": selector}

    def op_point(self, value) -> dict:
        """Rest the pointer at a place of the window, `[x, y]`: what a person
        does to bring out the controls a surface hides until the pointer is over
        it. The point is recorded, so a step that found no control there says
        where the pointer was waiting."""
        x, y = value
        self.session.control({"op": "pointer_move", "x": float(x), "y": float(y)})
        return {"point": [x, y]}

    def op_click(self, value) -> dict:
        """Press at the middle of the control `selector` names: a field, or a
        control whose own press the walk cannot address by name."""
        selector = pick(value, self.variant)
        target = until_value(f"the control {selector!r}", lambda: control(self.session, selector))
        x, y, w, h = target["rect"]
        self.session.click_at(x + w / 2, y + h / 2)
        return {"clicked": selector, "rect": target["rect"]}

    def op_click_any(self, value) -> dict:
        """Click the first of `selectors` the version offers, and say which: a
        field takes the keyboard from a click where its action does not. A
        control a window holds but does not show — a menu's items while its menu
        is closed — stands nowhere, and is not one to click."""
        offered = [s for s in map(lambda o: alternative(o, self.variant), value) if s]
        listed = targets_of(self.session, offered)
        for selector in offered:
            target = named_in(listed, selector)
            if target is not None and drawn(target):
                x, y, w, h = target["rect"]
                self.session.click_at(x + w / 2, y + h / 2)
                return {"clicked": selector, "rect": target["rect"]}
        return {"clicked": None, "offered_none": offered}

    def op_select_row(self, value) -> dict:
        """Press the row `name` in the app window's own sidebar, where a person
        picks the note their window turns to."""
        name = pick(value, self.variant)
        window = self.window_of()
        show_sidebar(self.session, window, name)
        target = until_value(f"the row {name!r} in the window",
                             lambda: row_of(self.session, self.window_of(), name), 30)
        x, y, w, h = target["rect"]
        self.session.click_at(x + w / 2, y + h / 2)
        return {"selected_row": name, "rect": target["rect"]}

    def op_row_menu(self, value) -> dict:
        """Open the menu of the row `name` in the app window's own sidebar, the
        way a person opens it: a second-button press on the row."""
        name = pick(value, self.variant)
        window = self.window_of()
        show_sidebar(self.session, window, name)
        target = until_value(f"the row {name!r} in the window",
                             lambda: row_of(self.session, self.window_of(), name), 30)
        x, y, w, h = target["rect"]
        self.session.right_click_at(x + w / 2, y + h / 2)
        return {"menu_of_row": name, "rect": target["rect"]}

    def op_right_click(self, value) -> dict:
        """A second-button press where the control `selector` names is drawn: a
        row's or an area's own menu, opened the way a person opens it."""
        selector = pick(value, self.variant)
        target = until_value(f"the control {selector!r}", lambda: control(self.session, selector))
        x, y, w, h = target["rect"]
        self.session.right_click_at(x + w / 2, y + h / 2)
        return {"right_clicked": selector, "rect": target["rect"]}

    def op_drag(self, value) -> dict:
        """Drag from one point of the window to another, as a person marks a passage: a press,
        moves along the way, and the release. `{"reading": n}` drags across line `n` of the
        window's reading page instead, where its text is drawn, and `{"window": [[fx, fy],
        [fx, fy]]}` names the points as fractions of the window's rect, so a drag on a grid
        lands where the person means whatever the window's size. A list of points is a stroke
        through each of them."""
        if isinstance(value, dict) and "window" in value:
            window = self.window_of() or {}
            rect = window.get("rect")
            if not rect:
                raise Failed("the app has no window to drag in")
            x, y, w, h = rect
            (fx0, fy0), (fx1, fy1) = value["window"]
            start = [x + fx0 * w, y + fy0 * h]
            end = [x + fx1 * w, y + fy1 * h]
        elif isinstance(value, dict) and "reading" in value:
            window = self.window_of() or {}
            rect = window.get("rect")
            if not rect:
                raise Failed("no window to mark a passage in")
            x, y, w, h = rect
            side = min(320.0, w * 0.28)
            lines = value["reading"]
            # One line, or the first and last of a passage crossing several.
            first, last = (lines, lines) if not isinstance(lines, list) else (lines[0], lines[-1])
            left = x + side + 24.0
            start = [left, y + 132.0 + float(first) * 26.0]
            end = [left + min(360.0, w - side - 80.0), y + 132.0 + float(last) * 26.0]
        elif isinstance(value, list):
            # A stroke through the window's points, `[[x, y], [x, y], ...]`, as a brush moves.
            start, end = value[0], value[-1]
        else:
            start, end = value["from"], value["to"]
        path = value if isinstance(value, list) else [start, end]
        self.session.control({"op": "pointer_move", "x": float(start[0]), "y": float(start[1])})
        self.session.control({"op": "pointer_button", "pressed": True, "button": "left"})
        for a, b in zip(path, path[1:]):
            for step in range(1, 7):
                x = a[0] + (b[0] - a[0]) * step / 6
                y = a[1] + (b[1] - a[1]) * step / 6
                self.session.control({"op": "pointer_move", "x": float(x), "y": float(y)})
        self.session.control({"op": "pointer_button", "pressed": False, "button": "left"})
        return {"dragged": path}

    def op_reading(self, _value) -> dict:
        """The rectangle the window's reading page is drawn in, so a step can mark a passage
        across a line of it: the window's content, beside the sidebar and under the bar."""
        window = self.window_of() or {}
        rect = window.get("rect")
        if not rect:
            raise Failed("no window to read in")
        x, y, w, h = rect
        return {"reading": [x + min(320.0, w * 0.28), y + 120.0, x + w - 40.0, y + h - 120.0]}

    def op_right_click_at(self, value) -> dict:
        """A second-button press at a point of the window, for the menu empty
        space offers and nothing names: `[x, y]`, as the step file writes it."""
        x, y = value
        self.session.right_click_at(float(x), float(y))
        return {"right_clicked_at": [x, y]}

    def op_context_menu(self, value) -> dict:
        """Open the menu the control `selector` names shows, without a point:
        the same menu a right press opens, addressed by what the control is."""
        selector = pick(value, self.variant)
        press(self.session, selector, verb="show-context-menu")
        return {"context_menu": selector}

    def op_type(self, value) -> dict:
        text = pick(value, self.variant)
        self.session.type(text)
        return {"typed": text}

    def op_key(self, value) -> dict:
        self.session.key(pick(value, self.variant) if isinstance(value, dict) else value)
        return {"key": value}

    def op_chord(self, value) -> dict:
        """Keys held while another is typed: `["Control", "n"]`, or
        `["Control", "Shift", "n"]` for a chord of more than one modifier."""
        *modifiers, key = value
        for modifier in modifiers:
            self.session.control({"op": "key_down", "key": modifier})
        try:
            self.session.key(key)
        finally:
            for modifier in reversed(modifiers):
                self.session.control({"op": "key_up", "key": modifier})
        return {"chord": "+".join(value)}

    def op_invoke(self, value) -> dict:
        """Call an action by its address: the app's own verb, for a step the app
        offers no control for. The answer is recorded, a refusal does not end
        the walk — a person who cannot do it from the app cannot do it here
        either, and that is the finding."""
        provider, resource, action = value[0], value[1], value[2]
        args = value[3] if len(value) > 3 else None
        invoked = f"{provider} {resource}#{action}"
        try:
            answer = self.session.invoke(provider, resource, action, args)
        except Failed as error:
            return {"invoked": invoked, "refused": str(error)}
        try:
            return {"invoked": invoked, "value": value_of(answer)}
        except Failed:
            return {"invoked": invoked}

    def op_holds(self, value) -> dict:
        """What the app says it keeps now, read back through its own reads: the
        words named (`["Hello team"]`) must be in one of the reads the app
        publishes, so what the person made is proved from the app's own state
        and not only from what its window draws. An app reads its parts apart —
        a tracker's issues beside its record — so every read it offers is
        tried, and the one that held them is kept."""
        wanted = [pick(word, self.variant) for word in value]
        reads = [row for row in self.session.actions(app=self.v["id"])
                 if row.get("app_id") == self.v["id"]
                 and address(row) is not None and address(row)[1] == "read"]
        if not reads:
            raise Failed(f"{self.v['id']} publishes no read to ask")
        tried: dict[str, str] = {}
        for row in reads:
            resource = address(row)[0]
            try:
                read = value_of(self.session.invoke_row(row))
            except Failed as error:
                tried[resource] = str(error)[:160]
                continue
            missing = held_in(read, wanted)
            if not missing:
                return {"holds": wanted, "read_at": resource}
            tried[resource] = f"does not hold {missing}"
        raise Failed(f"no read of {self.v['id']} holds {wanted}: {tried}")

    def op_notes(self, _value) -> dict:
        """How many notes the app holds now, read back through its own read;
        what the read answered is kept where it does not count as notes, so the
        reading can be seen rather than guessed."""
        try:
            read = read_back(self.session, self.v["id"])
        except Failed as error:
            try:
                listed = self.actions()
            except Failed:
                listed = None
            return {"notes": None, "read_failed": str(error), "actions": listed}
        count = notes_held(read)
        found = {"notes": count}
        if count is None:
            found["read"] = {key: (list(value) if isinstance(value, dict) else type(value).__name__)
                             for key, value in (read or {}).items()} \
                if isinstance(read, dict) else str(read)[:1000]
        return found


    def op_wait_for(self, value) -> dict:
        """Wait for what a person waits for before going on: a control of the
        app's, its window, the actions it publishes, a note it holds, or the
        title its window wears (the note it shows). `{"actions": {"app": ...}}`
        waits on another app's actions, named by its id: a window restored
        beside this walk's app is alive only when that app publishes them."""
        if value == "window":
            until_value(f"the window of {self.v['id']}", self.window_of, 120)
            return {"window": True}
        if value == "actions" or (isinstance(value, dict) and "actions" in value):
            named = value["actions"] if isinstance(value, dict) else self.v["id"]
            if isinstance(named, dict):
                app, within = named["app"], float(named.get("within", 120))
            else:
                app, within = named, 120.0
            # The app is ready when it publishes what it can do: a window that
            # is drawn for a client still connecting publishes nothing.
            def listed():
                return [a for a in self.session.actions(app=app)
                        if a.get("app_id") == app and address(a)]

            return {"actions": len(until_value(
                f"{app} to publish its actions", listed, within))}
        if value == "note":
            # Waited for is the app's own read of its notes answering at all: an
            # empty notebook answers zero, which is an answer.
            def held_now():
                try:
                    count = notes_held(read_back(self.session, self.v["id"]))
                except Failed:
                    return None
                return None if count is None else {"held": count}

            return {"notes": until_value("the app to read its own notes", held_now, 30)["held"]}
        if isinstance(value, dict) and "title" in value:
            # The window names what it shows: the note open reads in the bar of
            # the desktop, where a person sees which note their window is on.
            wanted = value["title"]
            until_value(f"the window to name {wanted!r}",
                        lambda: (self.window_of() or {}).get("title") == wanted, 30)
            return {"title": wanted}
        if isinstance(value, dict) and "shown" in value:
            # A control the window draws: a menu's item stands nowhere until its
            # menu opens, and a press aimed at it before then lands on nothing.
            wanted = value["shown"]
            until_value(
                f"the control {wanted!r} to be drawn",
                lambda: (lambda target: target if target is not None and drawn(target) else None)(
                    control(self.session, wanted)),
                30)
            return {"shown": wanted}
        selector = pick(value, self.variant)
        until_value(f"the control {selector!r}", lambda: control(self.session, selector), 30)
        return {"control": selector}

    def op_tree(self, value) -> dict:
        """What a person sees of the notes' tree: `{"rows": [...]}` that each of
        them is a row the window draws, all at one leading edge (rows at the top
        of the tree), and `{"under": [child, parent]}` that a row of the child's
        words stands indented under the parent's — the note an action made under
        the note it was made in. A row that is not drawn is the finding: the app
        may hold what its window does not show."""
        def row(name: str) -> dict:
            return until_value(
                f"the row {name!r} in the window",
                lambda: row_of(self.session, self.window_of(), name), 30)

        if "under" in value:
            child_name, parent_name = value["under"]
            parent = row(parent_name)
            px, py, _pw, _ph = parent["rect"]

            def under() -> dict | None:
                # The child's words may name other notes too: the row sought is
                # the one drawn under the parent — deeper in the tree's own
                # levels, where the window publishes them, and the row the
                # parent's own leading edge does not reach where it does not.
                parent_level = parent.get("level")

                def below(target: dict) -> bool:
                    if target["rect"][1] <= py or target["rect"][0] >= px + 320.0:
                        return False
                    level = target.get("level")
                    if parent_level is not None and level is not None:
                        return level > parent_level
                    return target["rect"][0] > px

                drawn_under = [
                    target
                    for target in targets_of(self.session, [child_name])
                    if target.get("name") == child_name
                    and drawn(target)
                    and below(target)
                ]
                return min(drawn_under, key=lambda target: target["rect"][1]) if drawn_under else None

            child = until_value(
                f"the row {child_name!r} under {parent_name!r}", under, 30)
            return {"under": value["under"], "child": child["rect"], "parent": parent["rect"]}
        rows = [row(name) for name in value["rows"]]
        lefts = sorted({round(found["rect"][0]) for found in rows})
        if len(lefts) != 1:
            raise Failed(
                f"the rows {value['rows']!r} are not drawn at one leading edge: "
                f"{[(found.get('name'), found['rect']) for found in rows]}")
        return {"rows": value["rows"], "left": lefts[0], "rects": [found["rect"] for found in rows]}

    def op_form(self, value) -> dict:
        """Answer the form the window shows, as a person does: `{"field": "Tag", "text":
        "travel"}` clicks the field the form asks for and types into it, and `{"submit":
        "Tag"}` presses the button that takes the answer, at the foot of the sheet, under the
        field the same words name."""
        answered = {}
        if "field" in value:
            name = value["field"]
            target = until_value(
                f"the form's field {name!r}", lambda: control(self.session, name), 30)
            x, y, w, h = target["rect"]
            self.session.click_at(x + w / 2, y + h / 2)
            self.session.type(pick(value["text"], self.variant))
            answered["field"] = {"name": name, "typed": value["text"]}
        if "submit" in value:
            name = value["submit"]
            def button():
                found = [t for t in targets_of(self.session, [name])
                         if t.get("name") == name]
                return max(found, key=lambda t: t["rect"][1]) if found else None

            target = until_value(f"the form's submit {name!r}", button, 30)
            x, y, w, h = target["rect"]
            self.session.click_at(x + w / 2, y + h / 2)
            answered["submit"] = {"name": name, "rect": target["rect"]}
        return answered

    def op_search(self, value) -> dict:
        """Find a note the way a person does: the system's search, opened on the
        words of the note's title. Its rows are the desktop's own answer, from
        every app that publishes a search for what it holds — the shell's own
        offers ("Search the web for …") name the words without finding them,
        so they do not count as a found note."""
        query = pick(value, self.variant)
        self.session.invoke(SHELL, SHELL_RESOURCE, "search", {"query": query})

        def found():
            answer = self.session.search_rows()
            if answer.get("query") != query or answer.get("searching"):
                return None
            hits = [row.get("title") or "" for row in answer.get("rows") or []
                    if query in (row.get("title") or "")
                    and not (row.get("title") or "").startswith(
                        ("Search the web for", "Send to Assistant"))]
            return hits or None

        hits = until_value(f"the search for {query!r} to find it", found, 45)
        return {"query": query, "rows": hits}

    def op_actions(self, value) -> dict:
        """The actions the app publishes for `value`, as the desktop lists them:
        what a person can reach from the palette, whatever the app's window
        draws. A verb with no control is still a verb here."""
        query = pick(value, self.variant)
        return {"actions": [a for a in self.actions(only_app=True)
                            if query.lower() in str(a.get("title", "")).lower()
                            or query.lower() in str(a.get("operation", "")).lower()]}

    def op_targets(self, _value) -> dict:
        """What the desktop offers now, kept as evidence: the controls this
        version draws, by name and by what each does. A step that ends on this op
        hands that look to its own end, which does not ask for the same controls
        again."""
        listed = self.targets()
        if self.device is None:
            self.listed = listed
        return {"targets": len(listed),
                "app_controls": [t for t in listed
                                 if (t.get("identity") and ID_SEP in t["identity"])]}

    def op_questions(self, _value) -> dict:
        """What is asking the person now: a file chooser, a sheet: the calls
        that wait for their yes, each as the person reads it."""
        return {"questions": [{
            "handle": row.get("id"),
            "app": row.get("action") or "",
            "kind": "asks for files" if (row.get("asked") or {}).get("files") else "asks",
            "label": (row.get("asked") or {}).get("words")
            or (row.get("by") or {}).get("name") or "",
        } for row in self.waiting_asks()]}

    def waiting_asks(self) -> list:
        """The calls waiting for the person's yes, as the workspace's invocations
        read them (`tilo.invocations`): each an invocation whose `asked` names
        the record the person answers with its own `resolve`."""
        workspace = ((self.session.state_dump() or {}).get("workspace_tree") or {}).get("active")
        if not workspace:
            return []
        try:
            reading = record(self.session.control, workspace, INVOCATIONS_APP, "")
        except Refused:
            # A daemon reconnecting, or a workspace not looked at yet: nothing
            # is answered for it now, which is no question waiting.
            return []
        rows = [row
                for held in reading.get("records") or []
                for row in (held.get("value") or {}).get("items") or []]
        return [row for row in rows if row.get("status") == "waiting" and row.get("asked")]

    def op_must(self, value) -> dict:
        """What a person must see now: every control the list names drawn on the
        screen, each within the step's patience (`{"see": [...], "within": 10}`, or
        the list alone). Each entry is a selector as `press` takes one, or a list
        of selectors of which one is enough — the same thing, named differently
        by the versions. One missing is the step's failure, named."""
        wanted = value.get("see") if isinstance(value, dict) else value
        deadline = float(value.get("within", 10)) if isinstance(value, dict) else 10.0
        entries = [alternative(entry, self.variant) for entry in wanted]
        entries = [entry for entry in entries if entry is not None]

        def choices_of(entry) -> list:
            return [s for s in (alternative(selector, self.variant)
                                for selector in (entry if isinstance(entry, list) else [entry]))
                    if s is not None]

        def missed(entry, listed) -> bool:
            choices = choices_of(entry)
            return not any((found := named_in(listed, selector)) is not None
                           and drawn(found) for selector in choices)

        missing: list = []
        end = time.monotonic() + deadline
        # How many times the screen was read before it showed everything, and how long the last
        # read took: what a slow `must` spent waiting for the app and what it spent reading.
        rounds, read_ms = 0, 0
        while True:
            read_began = time.monotonic()
            listed = targets_of(self.session,
                                [s for entry in entries for s in choices_of(entry)])
            missing = [entry for entry in entries if missed(entry, listed)]
            rounds += 1
            read_ms = round((time.monotonic() - read_began) * 1000)
            if not missing or time.monotonic() > end:
                break
            time.sleep(0.05)
        if missing:
            raise Failed(f"not on screen: {json.dumps(missing, ensure_ascii=False)} "
                         f"({rounds} reads, the last {read_ms} ms)")
        return {"saw": entries, "reads": rounds, "read_ms": read_ms}

    def op_count(self, value) -> dict:
        """How many controls the selector in `see` is drawn as, which must be
        `is`: `{"see": "Strips", "is": 2}` — two rows of one name, a list that
        holds the same style twice, a badge whose number of rows is the finding.
        The count must hold within ten seconds (`within` sets another patience),
        so a list still filling in is given its time."""
        selector = pick(value["see"], self.variant)
        wanted = int(value["is"])
        deadline = float(value.get("within", 10))

        def held() -> int:
            return sum(1 for target in targets_of(self.session, [selector])
                       if drawn(target) and matches(target, selector))

        def differs() -> int | None:
            found = held()
            return None if found == wanted else found

        left = differs()
        end = time.monotonic() + deadline
        while left is not None and time.monotonic() < end:
            time.sleep(0.05)
            left = differs()
        if left is not None:
            raise Failed(f"{selector!r} is drawn {left} time(s), not {wanted}")
        return {"count": wanted, "of": selector}

    def op_must_not(self, value) -> dict:
        """What must be gone now: a sheet closed, a banner dismissed. Each
        selector named must stop being drawn within the patience given."""
        wanted = value.get("see") if isinstance(value, dict) else value
        deadline = float(value.get("within", 10)) if isinstance(value, dict) else 10.0
        entries = [e for e in (alternative(entry, self.variant) for entry in wanted) if e]

        def still() -> list:
            listed = targets_of(self.session, entries)
            return [e for e in entries
                    if (lambda t: t is not None and drawn(t))(named_in(listed, e))]

        end = time.monotonic() + deadline
        while still() and time.monotonic() < end:
            time.sleep(0.05)
        left = still()
        if left:
            raise Failed(f"still on screen: {json.dumps(left, ensure_ascii=False)}")
        return {"gone": entries}

    def op_open(self, value) -> dict:
        """Open another app of the session build by its directory name (`files`,
        `chat`) as the workspace offers it, and wait for its window: a shell walk
        needs windows to switch between. `{"app": .., "answer": "Allow"}` answers
        the question the system asks before the app's window, as a person does
        (Mail asks to reach the network before its process starts)."""
        deadline, answer = 120.0, None
        if isinstance(value, dict):
            value, deadline, answer = value["app"], float(value.get("deadline", deadline)), value.get("answer")
        ident = value if "." in value else real_apps.app_id(value)
        began = time.monotonic()
        if answer:
            self.session.open(ident, deadline)
            self.op_wait_for({"shown": {"prefix": answer}})
            self.op_press_any([{"prefix": answer}])
        opened = self.session.launch(ident, deadline)
        found = {"opened": ident, "waited_ms": round((time.monotonic() - began) * 1000)}
        # Where the open's time went: the ask (the action listed and invoked) and the
        # window after it, as `Session.launch` timed them.
        for key in ("asked_ms", "window_ms", "shown_ms"):
            if key in opened:
                found[key] = opened[key]
        return found

    def op_do(self, value) -> dict:
        """Run an action by its address and fail the step when it is refused:
        `[provider, resource, action, args?]`. For what the person does through
        the shell's own verbs, which every version of the shell publishes."""
        provider, resource, action = (pick(item, self.variant) for item in value[:3])
        args = pick(value[3], self.variant) if len(value) > 3 else None
        answer = self.session.invoke(provider, resource, action, args)
        if isinstance(answer, dict) and ("Failed" in answer or "Refused" in answer):
            raise Failed(f"{resource}#{action} answered {json.dumps(answer)[:500]}")
        return {"did": f"{resource}#{action}"}

    def op_act(self, value) -> dict:
        """Run the action `resource#action` the desktop lists now, with `args`,
        whichever provider lists it: `["shortcuts", "create", {...}]`, its
        answer kept as the step found it. For a workspace's own actions, whose
        provider id is the workspace's and not the same on the next run. A
        refusal fails the step, as a person's own press that does nothing is
        the finding.

        `{"do": [...], "as": "provider"}` keeps what it answered under that
        name, for a later step to name with `{"served": "provider.id"}`: a
        provider's id, and a stand-in's address, are the run's own, never the
        step file's."""
        named = isinstance(value, dict)
        parts = value["do"] if named else value
        as_name = value.get("as") if named else None
        resource, action = (pick(item, self.variant) for item in parts[:2])
        args = self.resolve(pick(parts[2], self.variant) if len(parts) > 2 else None)
        def find():
            # The rows that name the action, as a person's palette asks for it: the
            # device's whole catalogue is many pages, each read anew, and a press
            # never waits on all of them.
            rows = self.session.actions(action)
            return row_for(rows, resource, action) or row_for(
                rows, resource, action, kind=resource)

        row = until_value(f"the action {resource}#{action}", find, 30)
        answer = self.session.invoke_row(row, args)
        if isinstance(answer, dict) and ("Failed" in answer or "Refused" in answer):
            raise Failed(f"{resource}#{action} answered {json.dumps(answer)[:500]}")
        try:
            found = value_of(answer)
        except Failed:
            found = None
        if as_name and found is not None:
            self.served[as_name] = found
        return {"acted": f"{resource}#{action}", "provider": row.get("provider"),
                **({"value": found} if found is not None else {})}

    def resolve(self, value):
        """A value only the run knows: `{"served": "provider.id"}` is what the
        step that ran `as` `provider` answered, by the path under it, so a step
        can name an id or an address the desktop made up rather than one the
        file wrote."""
        return resolved(self.served, value)

    def op_serve(self, value) -> dict:
        """Start the stand-in service the capability under test speaks to, for a
        machine that has neither a projector nor a model provider of its own:
        `{"projector": true}` answers the display sheet on the wireless display
        broker's control socket, under the runtime directory the walk gives the
        session; `{"model-providers": true}` serves the loopback model providers
        and answers their addresses, for the walk to configure the person's
        provider with. It runs until this walk's session stops."""
        if value.get("projector"):
            socket = self.state / "projector" / "control.sock"
            self.service("projector-stand-in", ["--socket", str(socket)], wait=lambda: socket.exists())
            self.served["projector"] = str(socket)
            return {"projector": str(socket)}
        if value.get("model-providers"):
            out = self.service("fake-model-providers", ["--listen", "127.0.0.1:0"], lines=4)
            addresses = provider_addresses(out)
            self.served.update(addresses)
            return addresses
        if value.get("pages"):
            # A browser reaches this machine's loopback and refuses a file path:
            # the tree's test pages, served where a page can be opened.
            served = value["pages"]
            folder = REPO / (served.get("dir") or "crates/engines/chromium-host/test-pages")
            handler = functools.partial(PageServer, directory=str(folder))
            self.pages = ThreadingHTTPServer(("127.0.0.1", 0), handler)
            threading.Thread(target=self.pages.serve_forever, daemon=True).start()
            address = f"http://127.0.0.1:{self.pages.server_port}"
            if served.get("page"):
                address = f"{address}/{served['page']}"
            self.served["pages"] = address
            return {"pages": address, "dir": str(folder)}
        raise Failed(f"the step names a stand-in this walk does not know: {json.dumps(value)}")

    def service(self, binary: str, args: list[str], wait=None, lines: int = 0) -> str:
        """Start `binary` of the session build beside the session, with its own
        log, and answer what it said on its first `lines` lines once it is up."""
        path = BUILD / "bin" / binary
        if not path.is_file():
            raise Failed(f"the session build has no {binary} ({path})")
        self.out.mkdir(parents=True, exist_ok=True)
        log_path = self.out / f"{binary}-{self.variant}.log"
        log = log_path.open("wb")
        # The stand-in is the person's own program beside the session, as a
        # service of their login: it runs as the account, and the session
        # reaches its socket and its ports as the person.
        argv = [str(path), *args]
        if self.session.account is not None:
            argv = [*as_account(self.session.account), *argv]
        if not lines:
            proc = subprocess.Popen(argv, stdout=log, stderr=subprocess.STDOUT,
                                    start_new_session=True)
            self.services.append((proc, log))
            until_value(f"{binary} to be up (see {log_path})", wait or (lambda: True), 30)
            return ""
        proc = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=log,
                                start_new_session=True)
        self.services.append((proc, log))
        # Read the pipe itself, never a buffered reader around it: the reader would take
        # every line the program wrote into its own buffer, and a `select` on the pipe would
        # then wait for lines already in hand.
        fd = proc.stdout.fileno()
        os.set_blocking(fd, False)
        said: list[str] = []
        written = b""
        deadline = time.monotonic() + 20
        while len(said) < lines and time.monotonic() < deadline:
            try:
                chunk = os.read(fd, 65536)
            except BlockingIOError:
                chunk = b""
            if chunk:
                written += chunk
                said = [line for line in written.decode(errors="replace").splitlines() if line]
            elif proc.poll() is not None:
                break
            else:
                time.sleep(0.05)
        said = said[:lines]
        log.write((written.decode(errors="replace")).encode())
        if len(said) < lines:
            raise Failed(f"{binary} said {said} and no more (see {log_path})")
        proc.stdout.close()
        return "\n".join(said)

    def stop_services(self) -> None:
        """Stop the stand-ins this walk started: their sessions are the walk's."""
        if self.pages is not None:
            self.pages.shutdown()
            self.pages.server_close()
            self.pages = None
        for proc, log in self.services:
            try:
                os.killpg(proc.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            log.close()
        self.services.clear()
    def published_row(self, value: dict) -> dict | None:
        """The first action the desktop lists that `value` names: its `action` (the operation),
        and where given the `kind` of resource it acts on, the start of that resource's address
        (`resource`), its application (`app`), words the catalogue finds it by (`about`) and the
        label its resource reads exactly (`label`, the row a person sees: `trip-plan.txt`). A row
        stands on an instance a person cannot name in advance (an entry, a transfer, a device),
        so it is found by what it is, never by its id."""
        for row in self.session.actions(value.get("about") or "", app=value.get("app")):
            at = address(row)
            if at is None or at[1] != value["action"]:
                continue
            action = row.get("action") or {}
            kind = action.get("resource_kind") or (row.get("resource") or {}).get("kind") or ""
            if value.get("kind") not in (None, kind):
                continue
            if value.get("app") not in (None, row.get("app_id")):
                continue
            if not at[0].startswith(value.get("resource") or ""):
                continue
            if value.get("label") not in (None, (row.get("resource") or {}).get("label")):
                continue
            return row
        return None

    def op_use(self, value) -> dict:
        """Run the action `value` names by what it is (`published_row`), once the desktop lists
        it, and fail the step when it is refused or never listed: `{"kind":
        "files.nearby-transfer", "action": "accept-nearby", "args": {...}}`. In the args,
        `{resource}` is the address of the resource the action stands on, `{id}` the part of
        it after its kind (`nearby-transfer:7f3a` gives `7f3a`), `{choice:param}` the first
        answer the action offers for its parameter `param` (a folder to save to), and `{name}` what an earlier
        op kept. `"keep": {"name": "path"}` keeps what the answer reads at each path
        (`at_path`), for a later op on any device; `"expect": {"path": value}` is what it must
        read there, and a path kept or expected that the answer lacks fails the step."""
        value = pick(value, self.variant)
        row = until_value(f"{value.get('kind') or ''}#{value['action']} to be published",
                          lambda: self.published_row(value), float(value.get("within", 30)))
        resource = address(row)[0]
        offered = choices_of(row)
        args = pick(value.get("args"), self.variant) if "args" in value else None
        for name in re.findall(r"\{choice:([^}]+)\}", json.dumps(args)):
            if f"choice:{name}" not in offered:
                takes = [(p.get("name"), p.get("choices"))
                         for p in (row.get("action") or {}).get("params") or []]
                raise Failed(
                    f"{resource}#{value['action']} offers no choice for {name!r}: it takes "
                    f"{takes}")
        kept = {**self.kept, "resource": resource, "id": resource.split(":", 1)[-1],
                **{f"choice:{name}": choice for name, choice in offered.items()}}
        args = filled(args, kept) if args is not None else None
        answer = self.session.invoke_row(row, args)
        if isinstance(answer, dict) and ("Failed" in answer or "Refused" in answer):
            raise Failed(f"{resource}#{value['action']} answered {json.dumps(answer)[:500]}")
        found = {"did": f"{resource}#{value['action']}"}
        try:
            found["value"] = value_of(answer)
        except Failed:
            pass
        for name, path in (value.get("keep") or {}).items():
            at = at_path(found.get("value"), path)
            if at is None:
                raise Failed(f"the answer of #{value['action']} has nothing at {path!r}: "
                             f"{json.dumps(found.get('value'))[:1500]}")
            self.kept[name] = at
        wrong = {path: at_path(found.get("value"), path)
                 for path, wanted in (value.get("expect") or {}).items()
                 if at_path(found.get("value"), path) != filled(wanted, self.kept)}
        if wrong:
            raise Failed(f"the answer of #{value['action']} reads {json.dumps(wrong)}, not "
                         f"{json.dumps({k: value['expect'][k] for k in wrong})}")
        return found

    def op_published(self, value) -> dict:
        """The desktop lists the action `value` names (`published_row`) within its patience
        (`"within"`, ten seconds): what a person can reach from the palette or a row's menu
        now, such as an incoming transfer's Save."""
        value = pick(value, self.variant)
        row = until_value(f"{value.get('kind') or ''}#{value['action']} to be published",
                          lambda: self.published_row(value), float(value.get("within", 10)))
        return {"published": f"{address(row)[0]}#{value['action']}",
                "label": (row.get("action") or {}).get("action_label")}

    def op_has_file(self, value) -> dict:
        """The device holds a file named `name` whose bytes are `text`'s, anywhere in its own
        state: where a file sent to it or synced to it landed. The first such file is kept as
        evidence; one of that name with other bytes is the step's failure, named."""
        name, wanted = value["name"], value["text"].encode()
        root = self.session.state
        seen: list[str] = []

        def found() -> Path | None:
            seen.clear()
            for path in root.rglob(name):
                if not path.is_file() or "nearby-transfers" in path.parts:
                    continue
                try:
                    if path.read_bytes() == wanted:
                        return path
                except OSError:
                    continue
                seen.append(str(path.relative_to(root)))
            return None

        try:
            path = until_value(f"{name} with its bytes on this device", found,
                               float(value.get("within", 30)), 0.25)
        except Failed:
            raise Failed(f"no {name} with the bytes sent on this device"
                         + (f"; with other bytes at {seen}" if seen else "")) from None
        return {"has_file": str(path.relative_to(root))}

    def op_push_edge(self, value) -> dict:
        """Push the pointer past this device's screen edge toward the screen of another device
        of the person's that the shared layout puts beside it, as a person moving their mouse
        does: against the edge, a little past it, until the pointer goes on there
        (`shared_input.driving` names that device). `{"within": s}` is the patience. Which
        device and which edge are what this device's own state says, never assumed."""
        patience = float(value.get("within", 20)) if isinstance(value, dict) else 20.0

        def shared() -> dict:
            return (self.session.state_dump() or {}).get("shared_input") or {}

        def arranged():
            shown = [d for d in shared().get("displays") or [] if d.get("online")]
            here = next((d for d in shown if d.get("local")), None)
            there = next((d for d in shown if not d.get("local")), None)
            return (here, there) if here and there else None

        here, there = until_value("this device's layout to place another device's screen "
                                  "beside its own", arranged, patience)
        x, y, w, h = here["rect"]
        tx, ty, tw, th = there["rect"]
        lx, ly, lw, lh = here["local_rect"]
        middle = (lx + lw / 2, ly + lh / 2)

        def edge(by: float) -> tuple[float, float]:
            if tx >= x + w:
                return lx + lw - 1 + by, middle[1]
            if tx + tw <= x:
                return lx - by, middle[1]
            if ty >= y + h:
                return middle[0], ly + lh - 1 + by
            return middle[0], ly - by

        def move(by: float) -> None:
            px, py = edge(by)
            self.session.control({"op": "pointer_move", "x": px, "y": py})

        # The first push past the edge asks the other device for its input; once it lent it,
        # pushes past the edge without coming back go on there (resting on the edge again
        # starts the count over, as it keeps a pointer that only brushes the edge here).
        device = there.get("device")

        def lent():
            if device in (shared().get("leases") or []):
                return True
            move(0.0)
            move(10.0)
            return None

        def crossed():
            move(10.0)
            return shared().get("driving")

        try:
            until_value(f"{device} to lend this device its input", lent, patience, 0.5)
            driving = until_value(f"the pointer to go on to {device}'s screen",
                                  crossed, patience, 0.1)
        except Failed as error:
            said = {key: shared().get(key) for key in ("leases", "driving", "pointer",
                                                        "pointer_shown")}
            raise Failed(f"{error}; this device's shared input reads {json.dumps(said)}") \
                from None
        return {"driving": driving, "toward": there.get("device")}

    def op_windows(self, value) -> dict:
        """How many windows the desktop holds must be `value` (or at least
        `{"at_least": n}`), within ten seconds. The count the wait found is the one
        kept: reading every window once more would only repeat the look it ended on."""
        def count() -> int:
            return len(self.session.windows())

        # A count of none is a count: the wait takes a map, which is never falsy, so a desktop
        # that holds no window is answered rather than waited out.
        def at_least() -> dict | None:
            n = count()
            return {"windows": n} if n >= wanted else None

        def exactly() -> dict | None:
            n = count()
            return {"windows": n} if n == int(value) else None

        if isinstance(value, dict):
            wanted = int(value["at_least"])
            found = until_value(f"at least {wanted} windows", at_least, 10)
        else:
            found = until_value(f"{value} windows", exactly, 10)
        return found

    def op_focused(self, value) -> dict:
        """The window of `value` (an app's directory name or id) has the
        keyboard: what a person sees as the window in front."""
        ident = value if "." in value else real_apps.app_id(value)

        def front():
            return next((w for w in self.session.windows(ident)
                         if w.get("app_id") == ident and w.get("window") and w.get("active")),
                        None)
        until_value(f"the window of {ident} to have the keyboard", front, 10)
        return {"focused": ident}

    def search_hit(self, text: str) -> tuple[int, dict] | None:
        """The open search's row whose title holds `text`, once the search has
        stopped filling in, with its place in the list the person sees. The
        shell's own offers to search the web or ask the assistant name any words
        and find nothing, so they never count."""
        answer = self.session.search_rows()
        if answer.get("searching"):
            return None
        for index, row in enumerate(answer.get("rows") or []):
            title = row.get("title") or ""
            if text.lower() in title.lower() and not title.startswith(
                    ("Search the web for", "Send to Assistant")):
                return index, row
        return None

    def op_result(self, value) -> dict:
        """The open search lists a row of `value`'s words, within ten seconds."""
        text = pick(value, self.variant)
        index, row = until_value(f"a search row of {text!r}", lambda: self.search_hit(text), 10, 0.02)
        return {"row": index, "title": row.get("title")}

    def op_choose_result(self, value) -> dict:
        """Choose the open search's row of `value`'s words, as Enter on it does."""
        text = pick(value, self.variant)
        index, row = until_value(f"a search row of {text!r}", lambda: self.search_hit(text), 10, 0.02)
        self.session.invoke(SHELL, SHELL_RESOURCE, "choose-search-row", {"row": index})
        return {"chose": row.get("title"), "row": index}

    def op_open_every_tl_app(self, value) -> dict:
        """Open every tilolang app this session installed, one after another, as a person opens
        each from the launcher, and hold each to what a person sees: its window maps and draws
        its first frame, and the process showing it is still up once all are open. `value` may
        name the question an app asks before its window (`{"answers": {"mail": "Allow"}}`) and how
        long the apps are left open before they are read again (`"settle_s"`). A window process
        that ends before it shows anything (one that cannot read its link, or exits as it starts)
        is the step's finding, even where another took its place, as is a window whose instance
        its app's tilolang process was never handed (one that cannot start, or exits as it
        starts): such a window draws, and shows nothing of its app."""
        value = value if isinstance(value, dict) else {}
        answers, settle = value.get("answers", {}), float(value.get("settle_s", 3))
        installed = set(packages_of(self.spec, self.variant))
        apps = [(name, ident) for name, ident in tl_apps() if name in installed]
        if not apps:
            raise Failed("the session installed no tilolang app to open")
        opened, failed = {}, []
        for name, ident in apps:
            began = time.monotonic()
            try:
                if name in answers:
                    self.op_open({"app": ident, "answer": answers[name]})
                else:
                    self.session.launch(ident, 60)

                def drawn(ident=ident):
                    window = next((w for w in self.session.windows() if w.get("app_id") == ident),
                                  None)
                    return window if window is not None and (window.get("frames") or 0) > 0 else None

                until_value(f"the first frame of {ident}", drawn, 30, 0.05)
                opened[ident] = round((time.monotonic() - began) * 1000)
            except Failed as error:
                failed.append(f"{ident}: {error}")
        time.sleep(settle)
        shown = {w.get("app_id") for w in self.session.windows()}
        gone = [ident for ident in opened if ident not in shown]
        ended = [line for line in self.session.log().splitlines()
                 if any(said in line for said in UNOPENED)]
        if failed or gone or ended:
            raise Failed("not every tilolang app opened and stayed up:"
                         + "".join(f"\n  did not open: {line}" for line in failed)
                         + "".join(f"\n  its window went: {ident}" for ident in gone)
                         + "".join(f"\n  {line.strip()[-300:]}" for line in ended[:10]))
        return {"opened": opened}

    def op_window_of(self, value) -> dict:
        """A window of the app `value` (directory name or id) is on the desktop."""
        ident = value if "." in value else real_apps.app_id(value)
        until_value(f"a window of {ident}",
                    lambda: any(w.get("app_id") == ident for w in self.session.windows(ident)), 15)
        return {"window_of": ident}

    def op_close_app(self, value) -> dict:
        """Close the window of the app `value`, by the tab it holds."""
        ident = value if "." in value else real_apps.app_id(value)
        window = next((w for w in self.session.windows(ident) if w.get("app_id") == ident), None)
        if window is None:
            raise Failed(f"no window of {ident} to close")
        self.session.invoke(SHELL, SHELL_RESOURCE, "close-tab", {"tab": str(window["tab"])})
        until_value(f"the window of {ident} to close",
                    lambda: all(w.get("app_id") != ident for w in self.session.windows(ident)), 10)
        return {"closed": ident}

    def op_catalogue(self, value) -> dict:
        """Every capability the desktop publishes now, kept in the step: each
        action of the shell and of every app, by the app, the kind of resource it
        acts on and its operation (`so.tilo.file-explorer files.folder#add-bookmark`),
        with the words it reads in and the chord that reaches it. Instance ids are
        left out, so the same capability keys the same on every run and every
        device. `{"apps": [...]}` keeps only those applications'. The comparison
        holds this list to main's: one that went with no place named for it in
        app-walks/moved.json is a regression."""
        only = set(value.get("apps") or []) if isinstance(value, dict) else set()
        self.catalogued = {}
        for row in self.session.actions(""):
            action = row.get("action") or {}
            address_of = action.get("address") or {}
            app = row.get("app_id") or ""
            if not address_of.get("action") or (only and app not in only):
                continue
            kind = action.get("resource_kind") or (row.get("resource") or {}).get("kind") or ""
            key = f"{app} {kind}#{address_of['action']}"
            self.catalogued[key] = {
                "label": action.get("action_label"),
                **({"shortcut": action["shortcut"]} if action.get("shortcut") else {}),
                "params": sorted(p.get("name") for p in action.get("params") or []),
            }
        return {"capabilities": len(self.catalogued)}

    def op_state(self, value) -> dict:
        """What the desktop says it shows, as the session's own state reads it:
        `{"chrome.overlays.overview": true, "notifications#len": 1}` — each dotted
        path into `state_dump` (`#len` for how many a list holds) must read the
        value given within ten seconds (`"within"` sets another patience). Both
        shells publish the same state, so it names the overview being open, the
        session locked, a banner held, never a control of one shell."""
        value = pick(value, self.variant)
        if value is None:
            return {}
        wanted = {k: v for k, v in value.items() if k != "within"}
        deadline = float(value.get("within", 10))

        def read(dump: dict, path: str):
            return at_path(dump, path)

        def differs() -> dict:
            dump = self.session.state_dump()
            return {k: read(dump, k) for k, v in wanted.items() if read(dump, k) != v}

        end = time.monotonic() + deadline
        left = differs()
        while left and time.monotonic() < end:
            time.sleep(0.05)
            left = differs()
        if left:
            raise Failed(f"the desktop's state reads {json.dumps(left)}, "
                         f"not {json.dumps({k: wanted[k] for k in left})}")
        return {"state": wanted}

    def op_restart(self, value) -> dict:
        """End the session and sign in again on the same state, or (`"app"`) quit
        only this walk's app and open it again: what a person meets after a
        reboot or a crash. What they chose and made must still be there, which
        the steps after it assert."""
        began = time.monotonic()
        if value == "app":
            self.quit_app()
            self.launch()
        else:
            self.session.restart()
            # The theme the restarted session comes back in is read again, not assumed.
            self.dark = None
        return {"restarted": value, "took_ms": round((time.monotonic() - began) * 1000)}

    def op_restart_app(self, value) -> dict:
        """Quit the app `value` (a directory name or its id, `true` for this
        walk's own app) and open it again through the desktop's launcher: the
        cold start a person meets when an app stops and they open it again,
        which must still show what they made. Closing its window ends it as a
        person's quit does; an app still running without one is forced to end,
        as a hung app is."""
        ident = self.v["id"] if value is True else (
            value if "." in value else real_apps.app_id(value))
        began = time.monotonic()
        window = next((w for w in self.session.windows(ident) if w.get("app_id") == ident), None)
        if window is not None:
            self.session.invoke(SHELL, SHELL_RESOURCE, "close-tab",
                                {"tab": str(window["tab"])})
            until_value(f"the window of {ident} to close",
                        lambda: all(w.get("app_id") != ident for w in self.session.windows(ident)), 30)
        # An app exits with its last window; one that stays is ended the way a
        # person ends an app that will not.
        try:
            until_value(f"{ident} to end", lambda: not self.session.processes(ident), 15)
            forced = False
        except Failed:
            forced = True
            if window is not None:
                self.session.invoke(SHELL, SHELL_RESOURCE, "force-quit-app",
                                    {"tab": str(window["tab"])})
                until_value(f"{ident} to end", lambda: not self.session.processes(ident), 30)
        if ident == self.v.get("id"):
            self.asking()
        self.session.launch(ident, 120)
        until_value(f"the window of {ident}",
                    lambda: next((w for w in self.session.windows(ident)
                                  if w.get("app_id") == ident), None), 60)
        return {"restarted": ident, "forced": forced,
                "took_ms": round((time.monotonic() - began) * 1000)}

    def op_window_frames(self, value) -> dict:
        """How many frames the window of `value` (a directory name or id, `true`
        for this walk's own app) has drawn, called twice in one step: the second
        call waits for the window to draw again after the ops between them, which
        is what a person sees when a choice reaches an open window without
        reopening it. A window that does not draw again is the step's finding."""
        ident = self.v["id"] if value is True else (
            value if "." in value else real_apps.app_id(value))

        def frames() -> int | None:
            window = next((w for w in self.session.windows(ident) if w.get("app_id") == ident),
                          None)
            return None if window is None else window.get("frames")

        drawn = frames()
        if drawn is None:
            raise Failed(f"no window of {ident} to count the frames of")
        before = self.frames_seen.get(ident)
        if before is None:
            self.frames_seen[ident] = drawn
            return {"frames": drawn}
        until_value(f"the window of {ident} to draw again",
                    lambda: (lambda n: n if n is not None and n > before else None)(frames()),
                    20, 0.05)
        self.frames_seen[ident] = frames()
        return {"frames": frames(), "drew_again": True}

    def op_scroll(self, value) -> dict:
        """Turn the wheel where the control `at` names, or the middle of the app's
        window without one: `{"dy": -400}` turns the page down, as a wheel does,
        and `{"dy": 400, "at": "Appearance"}` turns it back up over that row."""
        dy = float(value["dy"])
        at = value.get("at")
        if at:
            target = until_value(f"the control {at!r}", lambda: control(self.session, at))
            x, y, w, h = target["rect"]
            x, y = x + w / 2, y + h / 2
        else:
            window = self.window_of()
            if window is None or window.get("rect") is None:
                raise Failed(f"no window of {self.v['id']} to scroll in")
            left, top, wide, high = window["rect"]
            x, y = left + wide / 2, top + high / 2
        self.session.control({"op": "scroll", "x": x, "y": y, "dy": dy})
        return {"scrolled": dy, "at": at}

    def op_local_mail_server(self, value) -> dict:
        """Start the tree's stand-in mail server and stage it as the provider of
        the address a mail account is added against (`lab@story.tilo.test`, its
        `imap.` and `smtp.` servers on this machine): one made-up login and three
        made-up messages, a certificate this machine trusts. `value` may name
        the password (`{"password": ...}`); it is stopped when the walk ends. A
        session already running reads the trust bundles it started with, so a
        step that adds an account after this one restarts the session."""
        if self.mail is None:
            self.mail = LocalMailServer(self.out / "mail-stand-in")
        return self.mail.stage()

    def op_local_icloud_server(self, value) -> dict:
        """`local_mail_server` for an iCloud account: the double serves `lab@icloud.com`'s
        mail and its calendar "Personal" under one app password (`{"password": ...}`), at
        iCloud's own server names on this machine. One provider is staged per walk."""
        if self.mail is not None:
            raise Failed("a walk stages one mail provider")
        password = value.get("password", MAIL_PASSWORD) if isinstance(value, dict) else MAIL_PASSWORD
        self.mail = ICloudDouble(self.out / "icloud-double", password)
        return self.mail.stage()

    def op_read(self, value) -> dict:
        """Call a read action and hold what it answered to the choices a person
        made: `{"app": "so.tilo.settings", "operation": "inspect-appearance",
        "args": {...}, "is": {"theme": "dark"}}` finds the action the app
        publishes, whatever resource it is on — a bound resource's id changes
        every run — and requires each dotted path into the answer's value to
        read the value given (`#len` for how many it lists). `contains` holds
        the whole answer's words to a text instead, for a read of a document.
        `"do": [provider, resource, action, args?]` calls an exact address.
        `{"within": seconds}` asks again while the app catches up with typing
        that commits a moment later. A read that is refused, or answers
        something else, is the step's finding."""
        within = float(value.get("within", 0)) if isinstance(value, dict) else 0.0
        end = time.monotonic() + within

        def call() -> tuple[str, object]:
            if "app" in value:
                app, operation = value["app"], value["operation"]
                args = value.get("args")
                row = app_action(self.session, app, operation)
                if row is None and args:
                    # Several rows of one operation (a pane's `open-pane`): the one
                    # whose parameters take the arguments named.
                    names = set(args)
                    row = next((a for a in self.session.actions(operation, app=app)
                                if a.get("app_id") == app and address(a)
                                and address(a)[1] == operation
                                and names <= {p.get("name") for p in
                                              (a.get("action") or {}).get("params") or []}), None)
                if row is None:
                    raise Failed(f"{app} publishes no {operation!r}")
                return f"{app} {operation}", value_of(self.session.invoke_row(row, args))
            call_parts = value["do"]
            provider, resource, action = (pick(item, self.variant) for item in call_parts[:3])
            args = pick(call_parts[3], self.variant) if len(call_parts) > 3 else None
            return (f"{resource}#{action}",
                    value_of(self.session.invoke(provider, resource, action, args)))

        def trouble(invoked: str, answer) -> str | None:
            if "contains" in value:
                text = pick(value["contains"], self.variant)
                if text not in json.dumps(answer, ensure_ascii=False):
                    return (f"{invoked} answered no {text!r}: "
                            f"{json.dumps(answer, ensure_ascii=False)[:2000]}")
            wanted = {path: pick(each, self.variant)
                      for path, each in (value.get("is") or {}).items()}
            wrong = {path: at_path(answer, path) for path, each in wanted.items()
                     if at_path(answer, path) != each}
            if wrong:
                return (f"{invoked} answered {json.dumps(wrong)} for "
                        f"{json.dumps({k: wanted[k] for k in wrong})}")
            return None

        while True:
            invoked, answer = call()
            problem = trouble(invoked, answer)
            if problem is None or time.monotonic() >= end:
                break
            time.sleep(0.2)
        if problem is not None:
            raise Failed(problem)
        return {"read": invoked, "is": {path: pick(each, self.variant)
                                        for path, each in (value.get("is") or {}).items()},
                "answered": json.dumps(answer, ensure_ascii=False)[:4000],
                **({"contains": value["contains"]} if "contains" in value else {})}

    def op_do_app(self, value) -> dict:
        """Run an action the app publishes, found by the app and the operation
        rather than by a resource id that changes every run:
        `{"app": "so.tilo.settings", "operation": "open-pane", "args": {"pane":
        "power"}}`. A refusal fails the step."""
        app, operation = value["app"], value["operation"]
        args = value.get("args")
        names = set(args or ())
        rows = [a for a in self.session.actions(operation, app=app)
                if a.get("app_id") == app and address(a) and address(a)[1] == operation]
        if names:
            rows = [a for a in rows if names <= {p.get("name") for p in
                                                 (a.get("action") or {}).get("params") or []}]
        if not rows:
            raise Failed(f"{app} publishes no {operation!r} taking {sorted(names)}")
        answer = self.session.invoke_row(rows[0], args)
        if isinstance(answer, dict) and ("Failed" in answer or "Refused" in answer):
            raise Failed(f"{app} {operation} answered {json.dumps(answer)[:500]}")
        return {"did": f"{app} {operation}", "args": args}

    def op_press_when_any(self, value) -> dict:
        """Press the first of `selectors` drawn within `within` seconds (ten by
        default), and say which; none coming is not a failure: a question an app
        asks only the first time it opens, which it then goes on without."""
        offered = [s for s in map(lambda o: alternative(o, self.variant), value["selectors"]) if s]
        deadline = float(value.get("within", 10))

        def shown():
            for selector in offered:
                target = control(self.session, selector)
                if target is not None and drawn(target):
                    return target
            return None

        target = until_value(f"one of {offered!r}", shown, deadline)
        x, y, w, h = target["rect"]
        self.session.click_at(x + w / 2, y + h / 2)
        return {"pressed": target.get("name")}

    def op_wait_window(self, value) -> dict:
        """Wait for the window of the app `value` (a directory name or id) to be
        the shell's, within `within` seconds (a minute by default)."""
        ident = value if isinstance(value, str) else value["app"]
        ident = ident if "." in ident else real_apps.app_id(ident)
        deadline = float(value.get("within", 60)) if isinstance(value, dict) else 60.0
        until_value(f"the window of {ident}",
                    lambda: next((w for w in self.session.windows(ident)
                                  if w.get("app_id") == ident), None), deadline)
        return {"window": ident}

    def op_theme(self, value) -> dict:
        if self.v.get("id") is None:
            self.switch_shell_theme(bool(value))
        else:
            self.set_dark(bool(value))
        return {"theme": "dark" if value else "light"}

    def op_close(self, _value) -> dict:
        self.close_window()
        return {"closed": True}

    def files(self) -> Path:
        """Where a person's files are: the folder of the workspace the session runs in the
        mount the machine serves (`TILO_WORKSPACE_ROOTS/.workspaces/<id>`, as a device's
        login gives it), or the session's own home where the walk runs on a machine that
        serves none (a developer's own desktop). The mount's root is no one's: a file
        written there lies outside every workspace. A step's fixture is written in the
        one tree the apps read, never in a second beside it."""
        if self.machine is not None and self.machine.account is not None:
            return self.workspace_folder()
        return self.session.state / "home"

    def workspace_folder(self) -> Path:
        """The folder of the workspace the session runs in the machine's mount, once the
        files daemon serving that workspace has made it."""
        workspace = until_value(
            "the workspace the session runs",
            lambda: ((self.session.state_dump() or {}).get("workspace_tree") or {})
            .get("active"), 60)
        folder = self.machine.WORKSPACE / ".workspaces" / workspace
        until(f"the workspace's folder {folder} in the mount", folder.is_dir, 120.0, 0.2)
        return folder

    def place(self, source: Path, name: str) -> Path:
        """`source`'s bytes at `name` where the person's files are, written as the session
        account's own when a machine serves them (the workspace's mount admits the write
        as the person's), and recorded as the path a step names."""
        destination = self.files() / name
        account = self.session.account
        if account is None or source == destination:
            destination.parent.mkdir(parents=True, exist_ok=True)
            if source != destination:
                shutil.copyfile(source, destination)
            return self.session.own(destination)
        run([*as_account(account), "install", "-d", "-m", "0755", "--",
             str(destination.parent)])
        run([*as_account(account), "install", "-m", "0644", "--", str(source),
             str(destination)])
        return destination

    def fixture(self, name: str) -> Path:
        """The scratch file a fixture is made in before [`place`](Self::place) writes it
        where a person's files are: the walk's own state, which the account may write."""
        path = self.state / "fixtures" / name
        path.parent.mkdir(parents=True, exist_ok=True)
        return path

    def op_file(self, value) -> dict:
        """Write the step's fixture file where a person's file chooser reaches
        it, and record its path: a Markdown file the app imports."""
        name, text = value["name"], value["text"]
        made = self.fixture(name)
        made.write_text(text)
        path = self.place(made, name)
        return {"file": str(path)}

    def op_image(self, value) -> dict:
        """Write a picture where a person's file chooser reaches it, and record
        its path: `{"name": "beach.png"}` writes a 64-by-64 PNG in two colours
        (the same picture the framework's own capture tests show), which an app
        that shows photographs can import."""
        name = pick(value["name"], self.variant) if isinstance(value["name"], dict) \
            else value["name"]
        made = self.fixture(name)
        made.write_bytes(FIXTURE_PICTURE_PNG)
        path = self.place(made, name)
        return {"image": str(path)}

    def op_fixture(self, value) -> dict:
        """Put a step's fixture file where a person's file chooser reaches it,
        as a file of the tree rather than text written by the step: a video has
        bytes a step file cannot say. The step names the file under the walk's
        `fixtures/` and the name it is reached by."""
        name, source = value["name"], value["from"]
        made = self.fixture(name)
        shutil.copyfile(WALKS / "fixtures" / source, made)
        path = self.place(made, name)
        return {"file": str(path), "bytes": path.stat().st_size}

    def op_clip(self, value) -> dict:
        """Make a short VP9/AAC video where a person's file chooser reaches it, so a walk
        needs no committed media. The clip is generated once per walk (GStreamer, else
        ffmpeg), a few hundred kilobytes, and contains no third-party media."""
        name = value["name"]
        made_path = self.fixture(name)
        gst = [
            "gst-launch-1.0", "-e", "-q",
            "mp4mux", "name=mux", "!", "filesink", f"location={made_path}",
            "videotestsrc", "num-buffers=50", "pattern=ball", "!",
            "video/x-raw,width=160,height=120,framerate=25/1", "!",
            "videoconvert", "!", "vp9enc", "!", "queue", "!", "mux.video_0",
            "audiotestsrc", "num-buffers=94", "samplesperbuffer=1024", "wave=silence", "!",
            "audio/x-raw,rate=48000,channels=1", "!", "audioconvert", "!",
            "avenc_aac", "!", "aacparse", "!", "queue", "!", "mux.audio_0",
        ]
        ffmpeg = [
            "ffmpeg", "-y", "-loglevel", "error",
            "-f", "lavfi", "-i", "testsrc=size=160x120:rate=25:duration=2",
            "-f", "lavfi", "-i", "anullsrc=channel_layout=mono:sample_rate=48000",
            "-shortest", "-c:v", "libvpx-vp9", "-b:v", "64k", "-c:a", "aac", str(made_path),
        ]
        made = None
        for program in (gst, ffmpeg):
            if shutil.which(program[0]) is None:
                continue
            try:
                run(program)
            except Failed:
                continue
            if made_path.is_file() and made_path.stat().st_size > 0:
                made = program[0]
                break
        if made is None:
            raise Failed("no gst-launch-1.0 or ffmpeg to make the walk's clip")
        path = self.place(made_path, name)
        return {"file": str(path), "bytes": path.stat().st_size, "made_by": made}

    def op_tone(self, value) -> dict:
        """Write a song's fixture where a person's file chooser reaches it: a few seconds
        of a tone, made here rather than carried in the tree, which keeps no binary of its own.
        The step names the file, how long it runs and its pitch."""
        import math
        import struct
        import wave

        name = value["name"]
        seconds = float(value.get("seconds", 4))
        hertz = float(value.get("hz", 440))
        made = self.fixture(name)
        rate = 8000
        frames = bytearray()
        for index in range(int(rate * seconds)):
            at = index / rate
            sample = 0.7 * math.sin(2 * math.pi * hertz * at)
            frames += struct.pack("<h", int(sample * 9000))
        with wave.open(str(made), "wb") as song:
            song.setnchannels(1)
            song.setsampwidth(2)
            song.setframerate(rate)
            song.writeframes(bytes(frames))
        path = self.place(made, name)
        return {"file": str(path), "bytes": path.stat().st_size}

    def op_pdf(self, value) -> dict:
        """Write a PDF drawing `text` on each of its `pages` (one unless said) in
        Helvetica where a person's files are, so a walk opens a document a reader
        can actually read and page through."""
        name, text = value["name"], value.get("text", "A page")
        pages = int(value.get("pages", 1))
        made = self.fixture(name)
        made.write_bytes(pdf_with_pages(text, pages))
        path = self.place(made, name)
        return {"file": str(path), "bytes": path.stat().st_size, "pages": pages}

    def op_epub(self, value) -> dict:
        """Write a one-chapter EPUB where a person's files are: its title, author
        and chapter, as a book a reader opens."""
        import zipfile

        name = value["name"]
        title = value.get("title", "A book")
        author = value.get("author", "Ada Ferrer")
        chapters = value.get("chapters") or [
            {"title": value.get("chapter_title", title),
             "body": value.get("chapter", "The first chapter.")}
        ]
        made = self.fixture(name)
        container = (
            '<?xml version="1.0"?>\n'
            '<container version="1.0" '
            'xmlns="urn:oasis:names:tc:opendocument:xmlns:container">\n'
            '  <rootfiles><rootfile full-path="OEBPS/content.opf" '
            'media-type="application/oebps-package+xml"/></rootfiles>\n'
            "</container>"
        )
        manifest = "".join(
            f'    <item id="c{index}" href="c{index}.xhtml" '
            'media-type="application/xhtml+xml"/>\n'
            for index in range(1, len(chapters) + 1)
        )
        spine = "".join(f'<itemref idref="c{index}"/>'
                        for index in range(1, len(chapters) + 1))
        package = (
            '<?xml version="1.0"?>\n'
            '<package xmlns="http://www.idpf.org/2007/opf" version="3.0">\n'
            '  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">\n'
            f"    <dc:title>{title}</dc:title>\n"
            f"    <dc:creator>{author}</dc:creator>\n"
            "    <dc:language>en</dc:language>\n"
            "  </metadata>\n"
            "  <manifest>\n"
            f"{manifest}"
            "  </manifest>\n"
            f"  <spine>{spine}</spine>\n"
            "</package>"
        )
        with zipfile.ZipFile(made, "w") as book:
            book.writestr("mimetype", "application/epub+zip")
            book.writestr("META-INF/container.xml", container)
            book.writestr("OEBPS/content.opf", package)
            for index, chapter in enumerate(chapters, 1):
                page = (
                    '<?xml version="1.0"?>\n'
                    '<html xmlns="http://www.w3.org/1999/xhtml"><body>'
                    f"<h1>{chapter['title']}</h1><p>{chapter['body']}</p></body></html>"
                )
                book.writestr(f"OEBPS/c{index}.xhtml", page)
        path = self.place(made, name)
        return {"file": str(path), "bytes": path.stat().st_size,
                "chapters": len(chapters)}

    def op_package(self, value) -> dict:
        """Sign the package `apps/tl/<name>` as a `.tapp` where a person's files are:
        a package the Store reviews when it is opened."""

        name = value["name"] if isinstance(value, dict) else value
        shown = value.get("as") if isinstance(value, dict) else None
        built = build_tapp(name, self.out / "build" / self.variant / "package", self.state)
        path = self.place(built, shown or f"{name}.tapp")
        return {"file": str(path), "bytes": path.stat().st_size}

    def chooser_question(self) -> dict | None:
        """The file chooser asking the person now, if one is: the call that
        waits for their yes and names files for its answer."""
        return next((q for q in self.waiting_asks() if (q.get("asked") or {}).get("files")),
                    None)

    def op_choose(self, value) -> dict:
        """Answer the app's file request with the file it asked for: the person's
        own answer to the question's asked record, which the shell's picker and
        the palette give through the same `resolve`. `{"name": ..}` names a
        fixture where the person's files are; `{"path": ..}` names a file
        anywhere the chooser reaches, such as a workspace's own tree."""
        path = Path(value["path"]).resolve() if "path" in value \
            else (self.files() / value["name"]).resolve()
        question = until_value("the file chooser to ask for a file",
                               self.chooser_question, 60)
        invocation = question.get("id") or ""
        if not invocation:
            raise Failed(f"the file chooser names no asked record: {question!r}")
        self.session.invoke(INVOCATIONS_APP, invocation, ASKED_RESOLVE,
                            {"resolution": "approve", "files": [str(path)]})
        until(f"the file chooser to take {path.name!r}",
              lambda: self.chooser_question() is None, 60)
        return {"chose": str(path), "question": invocation}

    # -- a step ---------------------------------------------------------------------------

    def step(self, index: int, step: dict) -> dict:
        began = time.monotonic()
        theme_waited_before = self.theme_waited_ms
        record = {"id": step["id"], "said": step["said"], "ops": [], "found": []}
        dark = step.get("dark", False)
        ops = step.get("ops") or []
        if isinstance(ops, dict):
            # The same errand, reached differently: a menu here, a context menu
            # there. The step says what the person is doing; the version says how.
            ops = ops[self.variant]
        self.catalogued = None
        self.listed = None
        self.frames_seen = {}
        self.step_on = step.get("on")
        try:
            for op in ops:
                # Each op's own time, so a step over its budget says where the time went.
                op_began = time.monotonic()
                found = self.op(op)
                record["ops"].append({**op, "found": found,
                                      "took_ms": round((time.monotonic() - op_began) * 1000)})
            # Whatever this step waited for, the window it leaves open says the app: a window
            # that drew nothing of the app is a blank window, and no wait on a word catches one
            # whose words the shell draws itself. The step looks at the surface once: the blank
            # check and the controls it records read that same look, and an op that listed
            # every control as the step's last has already taken it.
            targets = self.listed if self.listed is not None else self.session.targets()
            self.require_shown(targets)
            if self.catalogued is not None:
                record["catalogue"] = self.catalogued
            shots_began = time.monotonic()
            # Each part of the shots on its own: the shot, the switch to the dark theme, the dark
            # shot and the switch back, so a step whose shots cost more says which part did.
            shots_spent: dict[str, int] = {}

            def spent(part: str, began_at: float) -> None:
                shots_spent[part] = round((time.monotonic() - began_at) * 1000)

            if step.get("shot", True):
                name = f"{index:02d}-{step['id']}"
                part_began = time.monotonic()
                record["shot"] = self.shot(name)
                spent("shot", part_began)
                if self.devices is not None:
                    # Each further device is shot too: what reached it is the step's point.
                    record["device_shots"] = {
                        device: self.on_device(device, lambda device=device: self.shot(
                            f"{name}-{device}"))
                        for device in self.devices.further()}
                if dark:
                    part_began = time.monotonic()
                    record["shot_dark"] = self.theme_shot(name, True, spent)
                    spent("dark", part_began)
                    part_began = time.monotonic()
                    if self.v.get("id") is None:
                        self.switch_shell_theme(False)
                    else:
                        self.set_dark(False)
                    spent("back_to_light", part_began)
            record["shots_ms"] = round((time.monotonic() - shots_began) * 1000)
            record["shots_spent"] = shots_spent
            record["theme_wait_ms"] = self.theme_waited_ms - theme_waited_before
            budget = step.get("budget_ms")
            took = round((time.monotonic() - began) * 1000)
            if budget is not None and took > budget:
                # Speed is something a person notices: a step slower than its
                # budget is not walked, whatever it ended up showing.
                raise Failed(f"took {took} ms, over its budget of {budget} ms")
            if self.discover:
                record["targets"] = [
                    {k: t[k] for k in ("name", "identity", "rect") if k in t}
                    for t in targets]
            record["took_ms"] = round((time.monotonic() - began) * 1000)
            record["controls"] = sorted({t["name"] for t in targets
                                         if t.get("name") and drawn(t)
                                         and "/" not in t["name"] and "~" not in t["name"]})
        except Failed as error:
            record["failed"] = str(error)
            # Where the time went is the finding of a step over its budget.
            spent = [f"{next(iter(op))} {op['took_ms']} ms" for op in record["ops"]
                     if "took_ms" in op]
            if "shots_ms" in record:
                spent.append(f"shots {record['shots_ms']} ms")
            if spent and "budget" in record["failed"]:
                record["failed"] += f" ({', '.join(spent)})"
            record["took_ms"] = record.get("took_ms") or round((time.monotonic() - began) * 1000)
            # What a failed step looks like is the finding: shoot it, and go on.
            # What the app offered instead is the finding too, so a step that
            # cannot be walked says what it was met with.
            try:
                record["shot"] = self.shot(f"{index:02d}-{step['id']}-as-it-was")
            except Failed:
                pass
            try:
                record["offered"] = self.targets()
                record["actions"] = self.actions()
            except Failed:
                pass
        self.steps.append(record)
        print(f"  {record['id']}: {'failed — ' + record['failed'] if 'failed' in record else 'walked'}"
              f" ({record['took_ms']} ms)", file=sys.stderr)
        return record

    def walk(self) -> dict:
        spec = self.spec
        self.shots.mkdir(parents=True, exist_ok=True)
        began = time.monotonic()
        started = 0
        if not self.shared:
            tapps = self.tapps()
            began = time.monotonic()

            def further_devices():
                self.devices = start_devices(spec, self.variant, self.session, self.state,
                                             self.out, apps_of(spec, self.variant), tapps)
            machine, daemon = start_signed_in(
                self.session, self.state, self.out, apps=apps_of(spec, self.variant),
                tapps=tapps, tag=self.variant, env=first_device_env(self.state, spec),
                session_args=session_args_of(spec, self.variant), then=further_devices)
            self.machine = machine
            self.daemon = daemon
            started = round((time.monotonic() - began) * 1000)
            print(f"==> {self.variant}: session up in {started} ms", file=sys.stderr)
        # The walk's own shots are of the light theme and the ones a step asks of the dark theme
        # are named so: the person's appearance starts light, whatever this device starts in (its
        # own default is dark), so every shot shows the theme its name says and every switch to
        # dark is one the window draws. A walk of the shell waits for the shell to wear it before
        # its first shot; an app's window opens in it.
        if self.v.get("id") is None:
            self.switch_shell_theme(False)
        else:
            self.set_dark(False)
        began = time.monotonic()
        self.began_epoch = time.time()
        try:
            for index, step in enumerate(spec["steps"], start=1):
                self.step(index, step)
        finally:
            found = {"variant": self.variant, "app": self.v.get("id") or self.variant,
                     "session_start_ms": started,
                     "walk_ms": round((time.monotonic() - began) * 1000),
                     "steps": self.steps}
            try:
                found["opens"] = self.opens()
            except OSError as error:
                found["opens_unread"] = str(error)
            if self.devices is not None:
                found["devices_ms"] = self.devices.took
                if self.devices.trouble:
                    found["devices_trouble"] = self.devices.trouble
            try:
                if self.discover:
                    found["actions"] = [a for a in self.session.actions("")
                                        if address(a) and self.v.get("id") in (None, a.get("app_id"))]
                    found["state"] = self.session.state_dump()
                    found["windows"] = self.session.windows()
                    if self.devices is not None:
                        found["devices"] = {
                            device: self.on_device(device, lambda: {
                                "actions": [a for a in self.session.actions("") if address(a)],
                                "state": self.session.state_dump(),
                                "windows": self.session.windows()})
                            for device in self.devices.further()}
            except Failed:
                pass
            if self.mail is not None:
                # The provider a step staged is the run's, not the session's: it
                # goes with the walk, and the machine's own files are put back.
                self.mail.stop()
                self.mail = None
            if any(step.get("failed") for step in self.steps):
                try:
                    self.diagnose()
                except OSError as error:
                    print(f"  diagnose: {error}", file=sys.stderr)
            if not self.shared:
                if self.devices is not None:
                    self.devices.stop()
                self.session.stop()
                self.machine.stop()
                if self.daemon is not None:
                    self.daemon.stop()
            self.stop_services()
        return found

    def reset(self) -> None:
        """Leave the desktop as a fresh sign-in shows it, for the family's next
        walk: every window closed, nothing over the desktop, no notification
        held, the light theme. Every device of the walk's is left so."""
        if self.devices is not None:
            for device in self.devices.further():
                self.on_device(device, self.reset_one)
        self.reset_one()

    def reset_one(self) -> None:
        for _ in range(2):
            try:
                self.session.key("Escape")
            except Failed:
                pass
        # Whatever stands over the desktop is put away by the action that closes a
        # surface, in either shell: a palette a key did not reach, a panel left open.
        try:
            overlays = (self.session.state_dump().get("chrome") or {}).get("overlays") or {}
        except Failed:
            overlays = {}
        for part, shown in overlays.items():
            if not shown:
                continue
            try:
                if part == "search":
                    # The launcher draws search and is the one to close it, not the
                    # session: the shell's search with nothing asked toggles it, as
                    # its key does.
                    self.session.invoke(SHELL, SHELL_RESOURCE, "search", {})
                else:
                    self.session.invoke(SHELL, "desktop:overlays", "close-surface", {"part": part})
            except Failed as error:
                print(f"  reset: closing {part} was refused: {error}", file=sys.stderr)
        try:
            until_value("nothing to stand over the desktop", lambda: not any(
                ((self.session.state_dump().get("chrome") or {}).get("overlays") or {}).values()), 10)
        except Failed as error:
            print(f"  reset: {error}", file=sys.stderr)
        for window in self.session.windows():
            try:
                self.session.invoke(SHELL, SHELL_RESOURCE, "close-tab", {"tab": str(window["tab"])})
            except Failed:
                pass
        try:
            self.session.invoke(SHELL, "session:notifications", "clear")
        except Failed:
            pass
        self.set_dark(False)
        until_value("every window to close", lambda: not self.session.windows(), 20)

    def diagnose(self) -> None:
        """What the walk's session was given, where a check reads it: an app the session never
        offered is a question of what it saw of the device's apps, and the session's own log and
        the stores it reads answer it. Only a failed walk keeps it."""
        kept = self.out / "diagnose"
        kept.mkdir(parents=True, exist_ok=True)
        for name, source in (
            ("session.log", self.state / "logs" / "session.log"),
            ("session-out.log", self.out / "logs" / f"session-{self.variant}.log"),
        ):
            try:
                (kept / name).write_bytes(source.read_bytes())
            except OSError:
                pass
        # The session runs the copy of the tools its state holds (`nested-session.sh` stages
        # them), and falls back to the build's own.
        install = next(
            (tool for tool in (self.state / "bin" / "tilo-install",
                               BUILD / "bin" / "tilo-install") if tool.is_file()),
            None,
        )
        if install is None:
            return
        base = {key: value for key, value in os.environ.items()
                if key not in ("TILO_APPS_SYSTEM_DIR", "TILO_APPS_DATA_DIR")}
        # The two stores a session reads: the person's, where a walk's packages go, and the
        # image's, where the build's own apps live; and both, as a session sees them.
        for name, stores in (
            ("apps", {"TILO_APPS_DATA_DIR": str(self.state / "apps")}),
            ("system", {"TILO_APPS_DATA_DIR": str(self.state / "system")}),
            ("both", {"TILO_APPS_DATA_DIR": str(self.state / "apps"),
                      "TILO_APPS_SYSTEM_DIR": str(self.state / "system")}),
        ):
            try:
                listed = subprocess.run(
                    [str(install), "list"], capture_output=True, text=True, timeout=120,
                    env={**base, **stores})
            except (OSError, subprocess.SubprocessError) as error:
                (kept / f"install-{name}.txt").write_text(f"{error}\n")
                continue
            (kept / f"install-{name}.txt").write_text(listed.stdout + listed.stderr)

    def tapps(self) -> list[Path]:
        """The tilolang app's signed package, built against this state's runtime."""
        return [build_tapp(package, self.out / "build" / self.variant, self.state)
                for package in packages_of(self.spec, self.variant)]


# --- building ---------------------------------------------------------------------------------

def tilolang_tool() -> Path:
    """The `tilolang` CLI, which builds an app's package into a signed Tilo app:
    the one `TILO_TILOLANG` names (a check's own), the session build's, or the
    tree's debug build (dist/dev/nested-session-build.sh makes one)."""
    for tool in (os.environ.get("TILO_TILOLANG"), BUILD / "bin/tilolang",
                 real_apps.TARGET / "debug/tilolang"):
        if tool and Path(tool).is_file():
            return Path(tool)
    raise Failed("no tilolang tool: dist/dev/nested-session-build.sh builds one, "
                 "or name it with TILO_TILOLANG")


def tapp_name(package: str) -> str:
    """The file `tilolang build` names the package `apps/tl/<package>`'s app after: the last
    part of its app id (`so.tilo.file-explorer` is `file-explorer.tapp`, though its package is
    `files`)."""
    declared = re.search(r"^app\s+(\S+)", (REPO / "apps/tl" / package / "app.tl").read_text(),
                         re.M)
    return declared.group(1).rsplit(".", 1)[-1] if declared else package


def build_tapp(name: str, out: Path, state: Path) -> Path:
    """`tilolang build` the package `apps/tl/<name>` against the session's runtime:
    its signed package, which the session installs as an app."""
    tool = tilolang_tool()
    runtime = (state / "apps/runtimes/.version").read_text().strip()
    print(f"==> tilolang build {name}", file=sys.stderr)
    # `--locked`: a walk builds what is committed, and fails on a stale ids lock or catalogue
    # rather than writing one mid-walk, which the session would see as a package no commit has.
    run([str(tool), "build", "--locked", str(REPO / "apps/tl" / name),
         "--search", str(REPO / "apps/tl"),
         "--out", str(out), "--tilo-runtime", f"tilo-runtime/{runtime}"])
    tapp = out / f"{tapp_name(name)}.tapp"
    if not tapp.is_file():
        raise Failed(f"tilolang build made no {tapp}")
    return tapp


def pdf_with_pages(text: str, pages: int = 1) -> bytes:
    """A PDF drawing `text` on each of its `pages` in non-embedded Helvetica, behind a plain
    xref table: the document a reader opens, shows and pages through, made here rather than
    carried in the tree. Object 1 is the catalogue, 2 the page tree, 3 the font, and each
    page then its content stream and its page object, `(4 + 2i, 5 + 2i)` for the `i`th."""
    kids = " ".join(f"{5 + 2 * index} 0 R" for index in range(pages))
    objects = [
        "<< /Type /Catalog /Pages 2 0 R >>",
        f"<< /Type /Pages /Kids [{kids}] /Count {pages} >>",
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
    ]
    for index in range(pages):
        stream = (
            "BT /F1 12 Tf 72 720 Td 14 TL\n"
            f"({text} {index + 1}) Tj T*\n"
            "ET"
        )
        objects.append(f"<< /Length {len(stream)} >>\nstream\n{stream}\nendstream")
        objects.append(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            f"/Resources << /Font << /F1 3 0 R >> >> /Contents {4 + 2 * index} 0 R >>"
        )
    pdf = bytearray(b"%PDF-1.4\n")
    offsets = []
    for number, body in enumerate(objects, 1):
        offsets.append(len(pdf))
        pdf.extend(f"{number} 0 obj\n{body}\nendobj\n".encode())
    xref = len(pdf)
    pdf.extend(f"xref\n0 {len(offsets) + 1}\n0000000000 65535 f \n".encode())
    for offset in offsets:
        pdf.extend(f"{offset:010} 00000 n \n".encode())
    pdf.extend(
        f"trailer\n<< /Size {len(offsets) + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()
    )
    return bytes(pdf)


def prepare_state(state: Path, apps: str, build: bool) -> None:
    real_apps.prepare(state, apps=apps, built=build,
                      remote=os.environ.get("TILO_NESTED_SESSION_BUILD_EXEC"))


# --- the walk file ----------------------------------------------------------------------------

def step_found(record: dict) -> str:
    """What one step found, in a sentence: what it pressed, how many notes the
    app held, or that the version offers nothing for it."""
    said = []
    for op in record.get("ops") or []:
        found = op.get("found") or {}
        if "pressed" in found:
            said.append(f"pressed {found['pressed']}" if found["pressed"]
                        else "no control for it")
        elif "offered" in found:
            said.append(f"offers {found['offered']}" if found["offered"] else "no control for it")
        elif "count" in found and found["count"] is not None:
            said.append(f"{found['count']} held")
        elif "rows" in found:
            said.append(f"search rows {found['rows']}")
        elif "questions" in found:
            said.append(f"asked {len(found['questions'] or [])} question(s)")
        elif "actions" in found:
            named = found["actions"]
            if isinstance(named, int):
                said.append(f"{named} actions listed")
            else:
                said.append("actions: " + (", ".join(
                    str(a.get("title")) for a in named) or "none"))
        elif "clicked" in found:
            said.append(f"clicked {found['clicked']}")
        elif "rested_on" in found:
            said.append(f"rested on {found['rested_on']}")
        elif "pressed_ms_after_launch" in found:
            said.append(f"pressed {found['pressed_ms_after_launch']} ms after launch")
    if "failed" in record:
        said.append(f"stopped: {record['failed']}")
    return ", ".join(said)


def write_report(report: dict, out: Path) -> Path:
    """The walk as a page: each step of both versions side by side, its
    snapshots beside each other, and what each one found."""
    variants = [v for v in VARIANTS if v in report["variants"]]
    lines = [f"# {report['walk'].capitalize()}: the same first hour, "
             f"{' and '.join(variants)}", ""]
    for variant in variants:
        walked = report["variants"][variant]
        lines.append(f"- **{variant}**: `{walked['app']}`, its session up in "
                     f"{walked['session_start_ms']} ms")
    lines += ["", "| Step | " + " | ".join(variants) + " | What each found |",
              "|---|" + "---|" * len(variants) + "---|"]
    steps = {v: {step["id"]: step for step in report["variants"][v]["steps"]}
             for v in variants}
    for step in report["steps"]:
        cells = []
        for v in variants:
            record = steps[v].get(step["id"], {})
            shot = record.get("shot")
            dark = record.get("shot_dark")
            cell = f"![]({Path(shot).relative_to(out)})" if shot else "—"
            if dark:
                cell += f" dark: ![]({Path(dark).relative_to(out)})"
            cells.append(cell)
        found = " / ".join(f"{v}: {step_found(steps[v].get(step['id'], {}))}" for v in variants)
        lines.append(f"| **{step['id']}** — {step['said']} | " + " | ".join(cells)
                     + f" | {found} |")
    lines += ["", f"Opens that started the app, held to {OPEN_BAR_MS} ms from the ask to the frame "
              "that showed its window:", ""]
    for variant in variants:
        for opened in report["variants"][variant].get("opens") or []:
            lines.append(f"- **{variant}** {opened['open']}: {open_words(opened)}")
    lines.append("")
    (out / "walk.md").write_text("\n".join(lines))
    return out / "walk.md"


def open_words(opened: dict) -> str:
    """An open in the words of the walk's page and its verdict: its time against the bar and
    where the time went."""
    if opened.get("unlogged"):
        return "asked for, and the session's launch timings record no open of it"
    where = f"launcher started after {opened['spawn_ms']} ms"
    if "map_ms" in opened:
        where += f", window mapped {opened['map_ms']} ms after that"
    if "ms" not in opened:
        return f"never shown ({where})"
    if "load" in opened:
        where += f"; load {opened['load']} on {opened['cpus']} cores"
        if "steal_pct" in opened:
            where += f", {opened['steal_pct']}% of the CPU taken by the host"
    over = " over the bar" if opened["ms"] > OPEN_BAR_MS else ""
    return f"{opened['ms']} ms{over} ({where})"


def hold_opens(root: Path, out: Path) -> int:
    """The `app-walk/open` check: every open the walks under `root` recorded (each `walk.json`
    their checks kept), one row per app and open, held to the open bar. Writes the rows as a page
    (`opens.md`) and as data (`opens.json`) into `out`; 1 when an open is over the bar, was never
    shown, or a walk could not read its session's launch timings."""
    rows = []
    unread = []
    for path in sorted(root.glob("**/walk.json")):
        if out in path.parents:
            continue
        report = json.loads(path.read_text())
        for variant, walked in (report.get("variants") or {}).items():
            if "opens_unread" in walked:
                unread.append(f"{report['walk']} {variant}: the session's launch timings could not "
                              f"be read: {walked['opens_unread']}")
            for opened in walked.get("opens") or []:
                rows.append({"walk": report["walk"], "variant": variant, "app": walked["app"],
                             **opened})
    slow = [row for row in rows if row.get("ms", OPEN_BAR_MS + 1) > OPEN_BAR_MS]
    lines = [f"# Opens against the {OPEN_BAR_MS} ms bar", "",
             "From asking for an app to the frame that shows its window, in each walk that opened "
             "it: the cold open, and each open after the app was quit.", "",
             "| App | Version | Open | Time | Where it went |", "|---|---|---|---|---|"]
    for row in sorted(rows, key=lambda r: (r["app"], r["variant"], r["open"] != "cold")):
        time_cell = f"{row['ms']} ms" if "ms" in row else "never shown"
        lines.append(f"| {row['app']} | {row['variant']} | {row['open']} | {time_cell} | "
                     f"{open_words(row)} |")
    lines += ["", *[f"- {line}" for line in unread], ""]
    out.mkdir(parents=True, exist_ok=True)
    (out / "opens.md").write_text("\n".join(lines))
    (out / "opens.json").write_text(json.dumps(rows, indent=1) + "\n")
    for row in rows:
        print(f"{'SLOW-OPEN' if row in slow else 'open'} {row['app']} {row['variant']} "
              f"{row['open']}: {open_words(row)}", file=sys.stderr)
    for line in unread:
        print(f"SLOW-OPEN {line}", file=sys.stderr)
    if not rows and not unread:
        print("FAIL: no walk under it recorded an open", file=sys.stderr)
        return 1
    return 1 if slow or unread else 0


def load_walk(name: str | None, path: str | None = None) -> tuple[Path, dict]:
    """The walk `name` names under app-walks/, or the file `path` names: a walk
    being written for an app whose steps are not settled yet."""
    if path:
        at = Path(path)
        if not at.is_file():
            raise Failed(f"no walk file at {at}")
        return at, json.loads(at.read_text())
    at = WALKS / f"{name}.json"
    if not at.is_file():
        raise Failed(f"no walk named {name}: {at} is not there")
    return at, json.loads(at.read_text())


# --- what a lane compares ---------------------------------------------------------------------

BASELINES = WALKS / "baseline"
# A step is slower in a way a person notices when it takes twice as long and a
# second more: below that, a shared build node's load moves it as much.
SLOWER_FACTOR = 2.0
SLOWER_MARGIN_MS = 1000


def unwalked(found: dict) -> list[dict]:
    """The steps of a walk that did not walk: each one fails the check. Nothing
    excuses one, a baseline from main included: a failure main shares stays red
    and claimed, where its fix and its getting worse are both seen. The baseline
    only says which failures are new (`compare`), what main published and how
    long each step took there."""
    return [{"version": variant, "step": step_id, "why": step.get("failed") or "not walked"}
            for variant, report in found.get("versions", {}).items()
            for step_id, step in report.get("steps", {}).items()
            if not step.get("walked")]


def rebaseline(found: dict, before: dict | None) -> dict:
    """The baseline main's newest run makes, beside the one it replaces: what it
    found, except that a step's time is the slowest main has shown for it. One
    run on a shared node can open an app in a second and the next in five, so a
    single quick run would make the next ordinary one read as slower; the
    slowest of main's runs is what a change is held to."""
    if not before:
        return found
    merged = json.loads(json.dumps(found))
    for variant, now in merged.get("versions", {}).items():
        was = (before.get("versions") or {}).get(variant) or {}
        for step_id, step in (now.get("steps") or {}).items():
            old = (was.get("steps") or {}).get(step_id) or {}
            if step.get("walked") and old.get("walked"):
                step["took_ms"] = max(step.get("took_ms") or 0, old.get("took_ms") or 0)
    return merged


def summary(report: dict) -> dict:
    """What a walk found, as the baseline keeps it: per version, per step,
    whether it was walked, how long it took, and the controls drawn after it.
    No snapshot and no absolute path: the shots stay in the run's outputs."""
    return {
        "walk": report["walk"],
        "live": report.get("live"),
        "commit": report.get("commit"),
        "versions": {
            variant: {
                "session_start_ms": found.get("session_start_ms"),
                "walk_ms": found.get("walk_ms"),
                "steps": {
                    step["id"]: {
                        "walked": "failed" not in step,
                        "took_ms": step.get("took_ms"),
                        **({"theme_wait_ms": step["theme_wait_ms"]}
                           if "theme_wait_ms" in step else {}),
                        **({"failed": step["failed"]} if "failed" in step else {}),
                        **({"controls": step["controls"]} if "controls" in step else {}),
                        **({"catalogue": step["catalogue"]} if "catalogue" in step else {}),
                    }
                    for step in found["steps"]
                },
            }
            for variant, found in report["variants"].items()
        },
    }


MOVED = WALKS / "moved.json"


def moved() -> dict:
    """Where each capability that left its place lives now: `{"<app> <kind>#<op>":
    {"to": "<app> <kind>#<op>", "description": "<what it does now>", "test":
    "<path>:<line>"}}`, written by the change that moves or deletes it (core.md,
    "The rule for every piece"). A key with `*` names every capability it matches,
    moved alike."""
    return json.loads(MOVED.read_text()) if MOVED.is_file() else {}


def moved_place(elsewhere: dict, key: str) -> dict | None:
    """Where the capability `key` lives now, by its own entry or one whose pattern it matches."""
    if key in elsewhere:
        return elsewhere[key]
    return next((place for pattern, place in elsewhere.items()
                 if "*" in pattern and fnmatch.fnmatchcase(key, pattern)), None)


def compare(now: dict, before: dict) -> dict:
    """What a person would miss in `now` that `before` had, both summaries.

    Each version walked now is held to the same version before; a version that
    did not exist before (the tilolang shell, the port that replaced a Rust app)
    is held to what a person got then — the baseline's live version. A step
    walked before and not now is a regression, and so is one that became
    noticeably slower. Controls drawn before and gone now are listed beside
    them, for the person reading the shots; across two versions they are named
    differently, so they are listed only between runs of the same version."""
    regressions: list[dict] = []
    redrawn: list[dict] = []
    gone: list[dict] = []
    elsewhere = moved()
    held: dict = {}
    pairs = []
    for variant in now["versions"]:
        against = variant if variant in before["versions"] else before.get("live")
        if against in before["versions"]:
            pairs.append((variant, against))
    # The version a person got is gone (the Rust shell or app deleted): what is
    # live now answers for it too, or what it gave would leave unheard.
    was_live = before.get("live")
    if was_live in before["versions"] and was_live not in now["versions"] and now["versions"]:
        heir = now.get("live") if now.get("live") in now["versions"] else sorted(now["versions"])[0]
        if (heir, was_live) not in pairs:
            pairs.append((heir, was_live))
    for variant, against in pairs:
        found = now["versions"][variant]
        held.setdefault(variant, against)
        base = before["versions"][against]["steps"]
        for step_id, was in base.items():
            if not was.get("walked"):
                continue
            step = found["steps"].get(step_id)
            where = {"version": variant, "against": against, "step": step_id}
            if step is None:
                regressions.append({**where, "why": "the walk no longer has this step"})
                continue
            if not step.get("walked"):
                regressions.append({**where, "why": f"walked before, now: {step.get('failed')}"})
                continue
            took, took_before = step.get("took_ms") or 0, was.get("took_ms") or 0
            slower = max(took_before * SLOWER_FACTOR, took_before + SLOWER_MARGIN_MS)
            # A baseline from before the walk waited for the shell to draw each theme it
            # switched to holds no such wait: that part of the step is a redraw it never
            # measured, not the step getting slower.
            unmeasured = (step.get("theme_wait_ms") or 0) if "theme_wait_ms" not in was else 0
            if took - unmeasured > slower:
                regressions.append({**where, "why": f"slower: {took} ms, was {took_before} ms"})
            elif took > slower:
                redrawn.append({**where, "why": f"{took} ms, was {took_before} ms: {unmeasured} ms "
                                "of it waits for the shell to draw each theme of the light and "
                                "dark shots, which the baseline did not wait for"})
            if "catalogue" in was:
                have = set(step.get("catalogue") or {})
                for key in sorted(set(was["catalogue"]) - have):
                    place = moved_place(elsewhere, key)
                    if place is None or (place.get("to") in was["catalogue"] and place["to"] not in have):
                        regressions.append({**where, "why": f"capability gone: {key} "
                                            f"({was['catalogue'][key].get('label')}), and "
                                            "app-walks/moved.json names no place for it"})
            if against == variant:
                missing = sorted(set(was.get("controls") or []) - set(step.get("controls") or []))
                if missing:
                    gone.append({**where, "controls": missing})
    fixed = [{"version": v, "step": sid}
             for v, against in held.items()
             for sid, step in now["versions"][v]["steps"].items()
             if step.get("walked")
             and not before["versions"][against]["steps"].get(sid, {}).get("walked", False)]
    return {"walk": now["walk"], "held_to": held, "baseline_commit": before.get("commit"),
            "regressions": regressions, "theme_redraws": redrawn, "controls_gone": gone,
            "now_walked": fixed}


def write_comparison(comparison: dict, out: Path) -> Path:
    lines = [f"# {comparison['walk']}: against the baseline of {comparison.get('baseline_commit')}",
             ""]
    for variant, against in comparison["held_to"].items():
        lines.append(f"- **{variant}** held to the baseline's **{against}**")
    lines += ["", f"## Regressions ({len(comparison['regressions'])})", ""]
    lines += [f"- {r['version']} `{r['step']}`: {r['why']}" for r in comparison["regressions"]] \
        or ["None."]
    if comparison.get("theme_redraws"):
        lines += ["", "## Slower only by the theme redraw the walk now waits for (no regression)",
                  ""]
        lines += [f"- {r['version']} `{r['step']}`: {r['why']}"
                  for r in comparison["theme_redraws"]]
    lines += ["", "## Controls drawn before and not now (same version; read the shots)", ""]
    lines += [f"- {g['version']} `{g['step']}`: {', '.join(g['controls'])}"
              for g in comparison["controls_gone"]] or ["None."]
    lines += ["", "## Walked now and not before", ""]
    lines += [f"- {f['version']} `{f['step']}`" for f in comparison["now_walked"]] or ["None."]
    (out / "compare.md").write_text("\n".join(lines) + "\n")
    return out / "compare.md"


def write_side_by_side(now_dir: Path, base_dir: Path | None, out: Path) -> Path:
    """One page for a lane: each step with the baseline's shots (the version a
    person got on main) beside this tree's, per version, and the comparison the
    check made. Shots are linked where the outputs were brought back to."""
    now = json.loads((now_dir / "walk.json").read_text())
    base = json.loads((base_dir / "walk.json").read_text()) if base_dir else None

    def shots_of(report: dict, root: Path, variant: str) -> dict:
        found = {}
        for step in report["variants"].get(variant, {}).get("steps", []):
            cells = []
            for key in ("shot", "shot_dark"):
                if step.get(key):
                    # The run's own path names where it was written on its node:
                    # the same file under the folder its outputs came back to.
                    rel = Path(step[key]).parts
                    at = rel[rel.index("shots"):] if "shots" in rel else rel[-1:]
                    cells.append(f"![]({Path(os.path.relpath(root, out), *at)})")
            mark = "" if "failed" not in step else f" **not walked:** {step['failed']}"
            found[step["id"]] = " ".join(cells) + mark
        return found

    variants = list(now["variants"])
    base_live = (base or {}).get("live")
    columns = ([f"main ({base_live})"] if base else []) + [f"now ({v})" for v in variants]
    lines = [f"# {now['walk']}: main beside this tree", ""]
    compare_md = now_dir / "compare.md"
    if compare_md.is_file():
        lines += [compare_md.read_text().replace("# ", "## ", 1), ""]
    lines += ["| Step | " + " | ".join(columns) + " |", "|---|" + "---|" * len(columns)]
    base_cells = shots_of(base, base_dir, base_live) if base else {}
    now_cells = {v: shots_of(now, now_dir, v) for v in variants}
    for step in now["steps"]:
        row = [base_cells.get(step["id"], "—")] if base else []
        row += [now_cells[v].get(step["id"], "—") for v in variants]
        lines.append(f"| **{step['id']}**: {step['said']} | " + " | ".join(row) + " |")
    (out / "index.md").write_text("\n".join(lines) + "\n")
    return out / "index.md"


def commit_of_tree() -> str | None:
    """The commit the walked tree is at, where it is a git checkout."""
    try:
        return run(["git", "-C", str(REPO), "rev-parse", "HEAD"]).strip() or None
    except Exception:
        return None


def family_of(name: str) -> list[tuple[Path, dict]]:
    """The walks of the family `name`, in the order they run: the files whose
    `family` names it, by their `order` (a walk that locks the screen runs last)
    and then by name. Empty when no walk names that family."""
    found = []
    for path in sorted(WALKS.glob("*.json")):
        if path == MOVED:
            continue
        spec = json.loads(path.read_text())
        if spec.get("family") == name:
            found.append((spec.get("order", 50), path.stem, path, spec))
    return [(path, spec) for _order, _name, path, spec in sorted(found, key=lambda f: f[:2])]


def checks() -> list[str]:
    """What t runs, one check each: every family, and every walk in none."""
    names = set()
    for path in WALKS.glob("*.json"):
        if path == MOVED:
            continue
        spec = json.loads(path.read_text())
        names.add(spec.get("family") or path.stem)
    return sorted(names)


def walks() -> list[str]:
    return sorted(path.stem for path in WALKS.glob("*.json") if path != MOVED)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Walk an app's first hour in each version its step file names.")
    parser.add_argument("app", nargs="?", help="the walk's name under app-walks/ (notes)")
    parser.add_argument("--list", action="store_true", help="print the walks there are")
    parser.add_argument("--only", choices=VARIANTS, help="walk one version alone")
    parser.add_argument("--out", type=Path, help="where the shots, logs and walk.json go")
    parser.add_argument("--file", help="a walk file to read instead of app-walks/APP.json")
    parser.add_argument("--only-walk", metavar="WALK",
                        help="walk one of the family's walks, by its step file's name")
    parser.add_argument("--state", type=Path,
                        help="where each version's session state goes (default: under --out)")
    parser.add_argument("--build", action="store_true",
                        help="build the session and its apps first")
    parser.add_argument("--keep", action="store_true", help="leave the session running")
    parser.add_argument("--discover", action="store_true",
                        help="keep every step's controls in walk.json, while writing a walk")
    parser.add_argument("--compare", nargs="?", const="", metavar="BASELINE",
                        help="hold the walk to a baseline (default: app-walks/baseline/WALK.json)"
                             " and exit 1 on a regression")
    parser.add_argument("--write-baseline", action="store_true",
                        help="write this run's summary as app-walks/baseline/WALK.json")
    parser.add_argument("--side-by-side", type=Path, metavar="NOW",
                        help="write OUT/index.md from a run's outputs (scripts/headless-examples/walk.sh)")
    parser.add_argument("--baseline-dir", type=Path,
                        help="with --side-by-side: the baseline run's outputs")
    parser.add_argument("--opens", type=Path, metavar="WALKS",
                        help="hold every open the walks under WALKS recorded to the open bar, "
                             f"{OPEN_BAR_MS} ms, writing opens.md to --out (the app-walk/open check)")
    options = parser.parse_args()
    if options.opens:
        return hold_opens(options.opens.resolve(), (options.out or Path.cwd()).resolve())
    if options.side_by_side:
        out = (options.out or Path.cwd()).resolve()
        print(write_side_by_side(options.side_by_side.resolve(),
                                 options.baseline_dir.resolve() if options.baseline_dir else None,
                                 out))
        return 0
    if options.list:
        print("\n".join(walks()))
        return 0
    if not options.app and not options.file:
        parser.error("name a walk (--list shows them)")

    try:
        name = options.app
        family = [] if options.file else family_of(name)
        if family:
            walked = family
            if options.only_walk:
                walked = [(path, spec) for path, spec in walked
                          if path.stem == options.only_walk]
                if not walked:
                    raise Failed(f"the {name} family has no walk {options.only_walk!r}")
        else:
            path, spec = load_walk(options.app, options.file)
            name = name or path.stem
            walked = [(path, spec)]
        out = (options.out or HERE.parents[1] / "target" / "app-walk" / name).resolve()
        out.mkdir(parents=True, exist_ok=True)
        if options.build:
            real_apps.build(apps=" ".join(sorted({a for _p, spec in walked for v in VARIANTS
                                                  if v in spec for a in apps_of(spec, v).split()})))
        # Each walk's versions still in the tree: a native app that was deleted in the change
        # its tilolang version took over leaves only its `tl` entry, and is walked alone.
        named = {path.stem: [v for v in VARIANTS if v in spec and present(spec, v)
                             and options.only in (None, v)]
                 for path, spec in walked}
        for path, spec in walked:
            if not [v for v in VARIANTS if v in spec and present(spec, v)]:
                raise Failed(f"the walk names no version still in the tree: {path}")
        reports = {path.stem: {
            "walk": path.stem, "file": str(path), "variants": {},
            "live": live_of(spec, [v for v in VARIANTS if v in spec and present(spec, v)]),
            "commit": commit_of_tree(),
            "steps": [{"id": st["id"], "said": st["said"]} for st in spec["steps"]]}
            for path, spec in walked}
        out_of = {path.stem: (out / path.stem if family else out) for path, _spec in walked}
        for variant in VARIANTS:
            mine = [(path, spec) for path, spec in walked if variant in named[path.stem]]
            if not mine:
                continue
            # Each version walks on a session of its own: one desktop's notes are its own,
            # and the two versions must not meet in a search or a window. A family's walks
            # share that session: it starts once, and each walk begins where a reset left it.
            state = ((options.state or out) / f"state-{variant}").resolve()
            apps = " ".join(sorted({a for _p, spec in mine for a in apps_of(spec, variant).split()}))
            prepare_state(state, apps, build=False)
            first = mine[0][1]
            session = None
            devices = None
            machine = None
            daemon = None
            started = 0
            if family:
                packages = []
                for _p, spec in mine:
                    for package in packages_of(spec, variant):
                        if package not in packages:
                            packages.append(package)
                tapps = [build_tapp(package, out / "build" / variant, state) for package in packages]
                session = Session(state)
                began = time.monotonic()

                def further_devices():
                    nonlocal devices
                    devices = start_devices(first, variant, session, state, out, apps, tapps)
                machine, daemon = start_signed_in(
                    session, state, out, apps=apps, tapps=tapps, tag=variant,
                    env=first_device_env(state, first),
                    session_args=session_args_of(first, variant), then=further_devices)
                started = round((time.monotonic() - began) * 1000)
                print(f"==> {name}: the {variant} session up in {started} ms", file=sys.stderr)
            try:
                for index, (path, spec) in enumerate(mine):
                    print(f"==> {path.stem}: walking the {variant} version", file=sys.stderr)
                    walk = Walk(spec, variant, out_of[path.stem], state,
                                discover=options.discover, session=session, devices=devices,
                                machine=machine)
                    found = walk.walk()
                    if family:
                        found["session_start_ms"] = started
                    reports[path.stem]["variants"][variant] = found
                    print(f"==> {path.stem} ({variant}): {found['walk_ms']} ms", file=sys.stderr)
                    if session is not None and index + 1 < len(mine):
                        try:
                            walk.reset()
                        except Failed as error:
                            print(f"  reset after {path.stem} fell short: {error}", file=sys.stderr)
            finally:
                if devices is not None:
                    devices.stop()
                if session is not None:
                    session.stop()
                if machine is not None:
                    machine.stop()
                if daemon is not None:
                    daemon.stop()
        verdict = 0
        printed = []
        for walk_name, report in reports.items():
            where = out_of[walk_name]
            where.mkdir(parents=True, exist_ok=True)
            (where / "walk.json").write_text(json.dumps(report, indent=1))
            found = summary(report)
            (where / "summary.json").write_text(json.dumps(found, indent=1) + "\n")
            report_path = write_report(report, where)
            if options.write_baseline:
                BASELINES.mkdir(exist_ok=True)
                kept = BASELINES / f"{walk_name}.json"
                before = json.loads(kept.read_text()) if kept.is_file() else None
                kept.write_text(json.dumps(rebaseline(found, before), indent=1) + "\n")
            comparison_path = None
            if options.compare is not None:
                at = Path(options.compare) if options.compare else BASELINES / f"{walk_name}.json"
                if options.compare and not at.is_file():
                    raise Failed(f"no baseline at {at}")
                if at.is_file():
                    comparison = compare(found, json.loads(at.read_text()))
                    (where / "compare.json").write_text(json.dumps(comparison, indent=1) + "\n")
                    comparison_path = write_comparison(comparison, where)
                    for regression in comparison["regressions"]:
                        print(f"REGRESSION {walk_name} {regression['version']} {regression['step']}: "
                              f"{regression['why']}", file=sys.stderr)
                    verdict = verdict or (1 if comparison["regressions"] else 0)
                new = {(r["version"], r["step"]) for r in comparison["regressions"]} \
                    if at.is_file() else set()
                failed = [s for s in unwalked(found) if (s["version"], s["step"]) not in new]
                for step in failed:
                    print(f"FAILED {walk_name} {step['version']} {step['step']}: {step['why']}",
                          file=sys.stderr)
                verdict = verdict or (1 if failed else 0)
            times = {v: f.get("walk_ms") for v, f in report["variants"].items()}
            print(f"==> {walk_name}: walked in {times} ms", file=sys.stderr)
            printed.append({"walk": str(where / "walk.json"), "report": str(report_path),
                            "compare": str(comparison_path) if comparison_path else None,
                            "walk_ms": times})
        print(json.dumps(printed if family else printed[0]))
        return verdict
    except Failed as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
