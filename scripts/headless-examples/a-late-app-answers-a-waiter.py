#!/usr/bin/env python3
"""A participant already in the room gets the document of an app that starts later.

The room's convergence handshake is one-directional in time. An application
publishes its whole document only in reply to somebody else's `Arrived`
(`ui/src/app.rs`, `RemotePresenceEffect::PeerArrived`); joining, it announces
itself and asks, and it volunteers nothing. So whoever was in the room first
holds the wrong end of it: they have already asked, the answer they got was
from whoever was up at the time, and nothing about the new application's
arrival sends them its state.

A participant that keeps asking should paper over that, and one does --
`qos-agent` re-asks every 400ms while it waits for an application it opened
(`ASK_WHILE_OPENING`, `apps/agent/src/session.rs`). It still comes away with
nothing about one run in six, which is what this script is for: it puts a
waiter in the room, starts a real application afterwards, and reads back
whether the waiter ever ends up holding what that application published.

Nothing here is simulated. The broker is the one the compositor binds, on both
its transports; the waiter is a JSON socket doing exactly what the agent's
`ask_for_document` does; and the application is the real photo editor process,
attached the way the launcher attaches one -- a connected fd handed over by
number.

Both orderings are run, because only their *difference* means anything:

  * **app first** -- the control. A run where this fails is a broken harness.
  * **waiter first** -- the shape the film hits.

Usage:
  ./a-late-app-answers-a-waiter.py [--keep] [--wait SECONDS]

Build first:

    cargo build --bin shell --bin photos --bin qos-doc-text

Exits non-zero when a waiter that was in the room first never receives the
document of the application that joined after it.
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import uuid
import zlib

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
TARGET = os.environ.get("CARGO_TARGET_DIR") or os.path.join(REPO, "target")

PHOTOS_APP = "dev.qos.photos"

#: How often the waiter asks again. The agent's `ASK_WHILE_OPENING`, so the
#: cadence this proves is the cadence the agent actually uses.
ASK_EVERY = 0.4


def note(msg: str) -> None:
    print(f"   {msg}", flush=True)


def claim(ok: bool, msg: str) -> bool:
    print(f"{'PASS' if ok else 'FAIL'}  {msg}", flush=True)
    return ok


def binary(name: str) -> str:
    for profile in ("debug", "demo", "release"):
        p = os.path.join(TARGET, profile, name)
        if os.path.exists(p):
            return p
    sys.exit(f"no {name} binary under {TARGET}; cargo build --bin {name}")


# -- the photograph -----------------------------------------------------------

def a_photograph(width: int = 320, height: int = 200) -> bytes:
    """A real PNG, written by hand so this needs no image library."""
    def chunk(kind: bytes, body: bytes) -> bytes:
        return (struct.pack(">I", len(body)) + kind + body
                + struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF))

    raw = b"".join(
        b"\x00" + b"".join(bytes([(x * 7) % 256, (y * 5) % 256, 128])
                           for x in range(width))
        for y in range(height)
    )
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 6))
            + chunk(b"IEND", b""))


# -- the waiter ---------------------------------------------------------------

class Waiter:
    """A participant in the room, asking for its documents the way the agent does.

    `app_id` is empty on purpose: a connection that announces no application is
    cluster-scoped, so every app's state is addressed to it. That is the agent's
    own address (`--app ""`), and narrowing it here would prove something about
    a different participant.
    """

    def __init__(self, sock_path: str, room: str, keep_asking: bool = True) -> None:
        self.room = room
        self.frames: list = []
        self.stop = threading.Event()
        self.asks = 0
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(sock_path)
        self._send({"Hello": {"app_id": "", "cluster_id": room,
                              "record": None, "handle": "waiter"}})
        threading.Thread(target=self._read, daemon=True).start()
        if keep_asking:
            threading.Thread(target=self._ask_forever, daemon=True).start()
        else:
            # Once, at the door, and never again. Not a weakened waiter: it is
            # what every ordinary participant is — an app announces `Arrived`
            # when it joins (`ui::App::set_presence_broadcast_scope`) and asks
            # nothing after that. Only the agent re-asks, and only while it is
            # waiting on an application it opened.
            self._ask_once()

    def _ask_once(self) -> None:
        self._send({"Presence": {"key": {"Workspace": {"id": self.room}},
                                 "payload": "Arrived"}})
        self.asks += 1

    def _send(self, obj: dict) -> None:
        body = json.dumps(obj).encode()
        self.sock.sendall(len(body).to_bytes(4, "little") + body)

    def _ask_forever(self) -> None:
        # Byte for byte what `ask_for_document` sends: a scope-keyed `Arrived`,
        # which is the request every document holder answers with its full state.
        while not self.stop.is_set():
            try:
                self._send({"Presence": {"key": {"Workspace": {"id": self.room}},
                                         "payload": "Arrived"}})
                self.asks += 1
            except OSError:
                return
            self.stop.wait(ASK_EVERY)

    def _read(self) -> None:
        try:
            f = self.sock.makefile("rb")
            while not self.stop.is_set():
                hdr = f.read(4)
                if not hdr or len(hdr) < 4:
                    return
                body = f.read(int.from_bytes(hdr, "little"))
                if not body:
                    return
                try:
                    self.frames.append(json.loads(body.decode()))
                except ValueError:
                    pass
        except OSError:
            pass

    def documents(self) -> list:
        return [bytes(m["CRDTUpdate"]["data"])
                for m in list(self.frames) if "CRDTUpdate" in m]

    def tally(self) -> dict:
        """What the waiter has been sent, by kind.

        Presence is broken down by the *key* variant as well as the payload,
        because the two are not the same question. A `View` key paired with an
        `Arrived` payload is somebody's viewport moving -- the toolkit has no
        `PresencePayload` variant for a view, so it rides an arrival as an
        inert carrier (`ui::App::broadcast_view_presence`) -- and reading it as
        a join is how one scroll becomes a whole document on the wire.
        """
        out: dict = {}
        for m in list(self.frames):
            for kind, body in m.items():
                if kind == "RemotePresenceUpdate":
                    key = body.get("key")
                    key_name = next(iter(key)) if isinstance(key, dict) else str(key)
                    payload = body.get("payload")
                    pay_name = (next(iter(payload)) if isinstance(payload, dict)
                                else str(payload))
                    label = f"{kind}[{key_name}/{pay_name}]"
                else:
                    label = kind
                out[label] = out.get(label, 0) + 1
        return out

    def close(self) -> None:
        self.stop.set()
        try:
            self.sock.close()
        except OSError:
            pass


def holds(waiter: Waiter, needle: str) -> bool:
    """Whether any document the waiter has been sent carries `needle`.

    Asked of the decoded document rather than of its bytes: Automerge
    DEFLATE-compresses its columns, so a string that is in the document is
    absent from the blob and grepping answers "no" for a document that has it.
    """
    text = binary("qos-doc-text")
    for blob in waiter.documents():
        out = subprocess.run([text, "-", "--contains", needle],
                             input=blob, capture_output=True, timeout=60)
        if out.returncode == 0:
            return True
    return False


def wait_until(predicate, seconds: float, step: float = 0.25) -> bool:
    deadline = time.time() + seconds
    while time.time() < deadline:
        if predicate():
            return True
        time.sleep(step)
    return predicate()


# -- the session --------------------------------------------------------------

def call(sock_path: str, payload: dict, timeout: float = 25.0) -> dict:
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(sock_path)
    try:
        s.sendall((json.dumps(payload) + "\n").encode())
        buf = b""
        while not buf.endswith(b"\n"):
            part = s.recv(65536)
            if not part:
                break
            buf += part
        return json.loads(buf.decode() or "{}")
    finally:
        s.close()


def dial(path: str, tries: int = 120) -> socket.socket:
    last = None
    for _ in range(tries):
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            s.connect(path)
            os.set_inheritable(s.fileno(), True)
            return s
        except OSError as e:
            last = e
            s.close()
            time.sleep(0.1)
    raise RuntimeError(f"the session broker at {path} never accepted: {last}")


def spawn_app(app_bin: str, app_id: str, env: dict, collab_sock: str,
              wayland: str, home: str, log, argv: list) -> subprocess.Popen:
    """Start an application the way the launcher would: a connected fd, by number."""
    conn = dial(collab_sock)
    e = dict(env)
    e["QOS_SVC_COLLAB_FD"] = str(conn.fileno())
    e["WAYLAND_DISPLAY"] = wayland
    e["QOS_PACKAGE_ID"] = app_id
    e["HOME"] = home
    os.makedirs(home, exist_ok=True)
    p = subprocess.Popen([app_bin] + argv, env=e, stdout=log,
                         stderr=subprocess.STDOUT, pass_fds=(conn.fileno(),))
    conn.close()
    return p


def stop(p: subprocess.Popen) -> None:
    p.terminate()
    try:
        p.wait(timeout=10)
    except subprocess.TimeoutExpired:
        p.kill()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--wait", type=float, default=30.0,
                    help="how long a waiter is given, in seconds. The agent's "
                         "own bound is 30.")
    args = ap.parse_args()

    shell_bin = binary("shell")
    photos_bin = binary("photos")
    binary("qos-doc-text")

    os.makedirs(os.path.join(TARGET, "late-app-runs"), exist_ok=True)
    run_dir = tempfile.mkdtemp(prefix="run-",
                               dir=os.path.join(TARGET, "late-app-runs"))
    # Sockets live under the runtime dir and nowhere else: a unix socket path
    # is bounded by SUN_LEN (~107 bytes) and a worktree path spends most of
    # that before the session has named anything. The compositor's own bind
    # fails outright past it, which reads as "the compositor would not start"
    # rather than as a path that is too long.
    runtime_dir = tempfile.mkdtemp(prefix="qw",
                                   dir=os.environ.get("XDG_RUNTIME_DIR")
                                   or f"/run/user/{os.getuid()}")
    os.chmod(runtime_dir, 0o700)
    home = os.path.join(run_dir, "home")
    os.makedirs(home, exist_ok=True)
    control = os.path.join(runtime_dir, "control.sock")
    collab_sock = os.path.join(runtime_dir, "qos", "services", "collab.sock")
    json_sock = os.path.join(runtime_dir, "qos", "services", "collab-json.sock")

    env = dict(os.environ)
    for k in ("DISPLAY", "WAYLAND_DISPLAY", "QUICK_COLLAB_SOCK"):
        env.pop(k, None)
    env.update({
        "XDG_RUNTIME_DIR": runtime_dir,
        "HOME": home,
        "QOS_USER_HOME": home,
        "XDG_CONFIG_HOME": os.path.join(run_dir, "config"),
        "XDG_DATA_HOME": os.path.join(run_dir, "data"),
        "XDG_CACHE_HOME": os.path.join(run_dir, "cache"),
        "QOS_TEST_CONTROL": control,
        "QOS_USER_ID": "quinten",
        "QOS_STATE_PATH": os.path.join(run_dir, "config", "qos", "state.json"),
        "LC_ALL": "en_GB.UTF-8",
        "LANG": "en_GB.UTF-8",
        "RUST_LOG": "info",
    })
    env.pop("LANGUAGE", None)

    shell_log_path = os.path.join(run_dir, "compositor.log")
    shell_log = open(shell_log_path, "wb")
    app_log = open(os.path.join(run_dir, "apps.log"), "wb")

    comp = subprocess.Popen([shell_bin, "--backend", "headless"], env=env,
                            stdout=shell_log, stderr=subprocess.STDOUT)
    started = []
    waiters = []
    ok = True
    try:
        for _ in range(600):
            if os.path.exists(control) and os.path.exists(collab_sock) \
                    and os.path.exists(json_sock):
                break
            if comp.poll() is not None:
                print(f"the compositor exited before binding; see {shell_log_path}")
                return 2
            time.sleep(0.1)
        if not os.path.exists(json_sock):
            print(f"no broker socket at {json_sock}; see {shell_log_path}")
            return 2

        display_file = os.path.join(runtime_dir, "qos-session", "wayland-display")
        if not wait_until(lambda: os.path.exists(display_file), 20.0):
            print(f"the compositor published no wayland socket; see {shell_log_path}")
            return 2
        wayland = open(display_file).read().strip()

        # The room the session's own workspace is in, read off its tree. An app
        # takes its room from `QOS_WORKSPACE_ID`, so pinning one out here would
        # put the apps and the waiter in a room the session is not in, and the
        # answer would be about that.
        dump = call(control, {"op": "state_dump"})
        tree = ((dump.get("state") or dump.get("answer") or {})
                .get("workspace_tree") or {})
        active = tree.get("active")
        room = next((w.get("room") for w in tree.get("workspaces") or []
                     if w.get("id") == active), None)
        if not room:
            print(f"no active workspace in the state dump; see {shell_log_path}")
            return 2
        note(f"the session's room is {room}")
        env["QOS_WORKSPACE_ID"] = room

        # Two photographs, one per ordering, so "which application answered" is
        # never in doubt: each document names its own file and nothing else.
        photos = {}
        for which in ("waiter-first", "app-first", "ask-once"):
            p = os.path.join(home, f"{which}-{uuid.uuid4().hex[:8]}.png")
            with open(p, "wb") as fh:
                fh.write(a_photograph())
            photos[which] = p

        print("\n1. the app first, then the waiter -- the ordering that works")
        first = spawn_app(photos_bin, PHOTOS_APP, env, collab_sock, wayland,
                          home, app_log, ["--open", photos["app-first"]])
        started.append(first)
        time.sleep(8.0)
        w2 = Waiter(json_sock, room)
        waiters.append(w2)
        got_control = wait_until(lambda: holds(w2, photos["app-first"]), args.wait)
        note(f"the waiter asked {w2.asks} time(s) and was sent "
             f"{len(w2.documents())} document(s)")
        note(f"what reached it: {w2.tally()}")
        w2.close()
        ok &= claim(got_control,
                    "a participant that joins an application already in the "
                    "room is sent its document")
        stop(first)
        started.remove(first)
        time.sleep(2.0)

        print("\n2. the waiter first, then the app -- the ordering the film hits")
        w1 = Waiter(json_sock, room)
        waiters.append(w1)
        time.sleep(1.5)
        note(f"the waiter asked {w1.asks} time(s) before the app existed")
        late = spawn_app(photos_bin, PHOTOS_APP, env, collab_sock, wayland,
                         home, app_log, ["--open", photos["waiter-first"]])
        started.append(late)
        got_late = wait_until(lambda: holds(w1, photos["waiter-first"]), args.wait)
        note(f"the waiter asked {w1.asks} time(s) in total and was sent "
             f"{len(w1.documents())} document(s)")
        note(f"what reached it: {w1.tally()}")
        w1.close()
        ok &= claim(got_late,
                    "a participant already in the room is sent the document of "
                    "an application that joins after it")
        print("\n3. a waiter that asks once, then the app -- what an ordinary "
              "participant is")
        w3 = Waiter(json_sock, room, keep_asking=False)
        waiters.append(w3)
        time.sleep(1.5)
        third = spawn_app(photos_bin, PHOTOS_APP, env, collab_sock, wayland,
                          home, app_log, ["--open", photos["ask-once"]])
        started.append(third)
        got_once = wait_until(lambda: holds(w3, photos["ask-once"]), args.wait)
        note(f"the waiter asked {w3.asks} time(s) and was sent "
             f"{len(w3.documents())} document(s)")
        note(f"what reached it: {w3.tally()}")
        w3.close()
        ok &= claim(got_once,
                    "a participant that asked once, before the application "
                    "existed, is still sent its document")
    finally:
        for w in waiters:
            w.close()
        for p in started:
            stop(p)
        stop(comp)
        shell_log.close()
        app_log.close()
        shutil.rmtree(runtime_dir, ignore_errors=True)
        if args.keep or not ok:
            print(f"\nlogs under {run_dir}")
        else:
            shutil.rmtree(run_dir, ignore_errors=True)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
