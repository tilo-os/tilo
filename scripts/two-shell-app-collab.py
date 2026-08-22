#!/usr/bin/env python3
"""Two qOS sessions, one shared workspace, real apps collaborating.

`two-session-collab-demo.py` proves the two sessions become one room and
that the room's own state replicates. It never starts an app, so it says
nothing about whether the things a person actually uses collaborate.

This one starts two real compositors, joins the second to the first's
workspace by signed invite, and then launches a real application process
into each session — against that session's own broker, reached the way an
app reaches it. An edit typed into one session's window is read back from
the *other* session's process, and both are photographed.

    scripts/two-shell-app-collab.py
    scripts/two-shell-app-collab.py --app file-explorer --keep
    scripts/two-shell-app-collab.py --one-shell     # both apps, one session

Requires `cargo build -p shell -p word-processor -p file-explorer`.
No display.
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# Long paths are the thing that breaks this harness first: a Unix socket
# address is capped at ~108 bytes and the compositor puts its broker at
# <runtime>/qos/services/collab.sock. A runtime dir under the repo or a
# scratch tree overflows that, the broker fails to bind, and every app
# then starts with collaboration silently off. Keep these short.
RUNTIME_BASE = os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")

APPS = {
    "word-processor": {
        "bin": "word-processor",
        "package": "dev.qos.word-processor",
        # The document both windows open. Its path is its record id, which
        # is the collaboration address, so both sides must name it
        # identically.
        "needs_doc": True,
    },
    "file-explorer": {
        "bin": "file-explorer",
        "package": "dev.qos.file-explorer",
        "needs_doc": False,
    },
}

FAILURES: list[str] = []


def step(n: str, text: str) -> None:
    print(f"\n=== {n}  {text}", flush=True)


def claim(description: str, actual, expected) -> None:
    good = actual == expected
    print(f"  [{'ok' if good else 'FAIL'}] {description}"
          f"{'' if good else f'  (got {actual!r}, wanted {expected!r})'}", flush=True)
    if not good:
        FAILURES.append(description)


def note(text: str) -> None:
    print(f"       {text}", flush=True)


# --------------------------------------------------------------------
# control socket
# --------------------------------------------------------------------


def call(sock_path: str, payload: dict, timeout: float = 30.0) -> dict:
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(sock_path)
    try:
        s.sendall(json.dumps(payload).encode() + b"\n")
        buf = b""
        while b"\n" not in buf:
            chunk = s.recv(1 << 20)
            if not chunk:
                raise RuntimeError("compositor closed the connection mid-reply")
            buf += chunk
        return json.loads(buf.split(b"\n", 1)[0])
    finally:
        s.close()


def ok(resp: dict) -> dict:
    if "err" in resp:
        raise RuntimeError(f"compositor refused: {resp['err']}")
    return resp


class Session:
    """One running compositor plus the app processes launched into it."""

    def __init__(self, name: str, user: str, run_dir: str, runtime_dir: str,
                 sock: str, binary: str, size: str):
        self.name = name
        self.user = user
        self.dir = run_dir
        self.runtime_dir = runtime_dir
        self.sock = sock
        self.binary = binary
        self.size = size
        self.proc: subprocess.Popen | None = None
        self.apps: list[tuple[str, subprocess.Popen, str]] = []
        self.log_path = os.path.join(run_dir, "compositor.log")
        self._log = None
        self.wayland: str | None = None

    @property
    def collab_sock(self) -> str:
        return os.path.join(self.runtime_dir, "qos", "services", "collab.sock")

    @property
    def collab_json_sock(self) -> str:
        return os.path.join(self.runtime_dir, "qos", "services", "collab-json.sock")

    # -- lifecycle ---------------------------------------------------

    def base_env(self) -> dict:
        env = dict(os.environ)
        # Nothing inherited may make the two sessions share state, and
        # nothing may point an app at a broker other than its own session's.
        for k in ("DISPLAY", "WAYLAND_DISPLAY", "QUICK_COLLAB_SOCK",
                  "QUICK_COLLAB_CAPNP_SOCK", "QOS_SVC_COLLAB_FD"):
            env.pop(k, None)
        env.update(
            XDG_RUNTIME_DIR=self.runtime_dir,
            XDG_CONFIG_HOME=os.path.join(self.dir, "config"),
            XDG_DATA_HOME=os.path.join(self.dir, "data"),
            XDG_CACHE_HOME=os.path.join(self.dir, "cache"),
        )
        return env

    def start(self) -> None:
        for sub in ("config", "data", "cache", "keys"):
            os.makedirs(os.path.join(self.dir, sub), exist_ok=True)
        os.makedirs(self.runtime_dir, mode=0o700, exist_ok=True)
        env = self.base_env()
        env.update(
            QOS_USER_ID=self.user,
            # Without a writable key path the compositor refuses to run
            # peer sync at all ("peer sync stays off rather than syncing an
            # unsalted id space"), and the two sessions never become one
            # room however the invite goes.
            QOS_PEER_KEY_PATH=os.path.join(self.dir, "keys", "peer-key"),
            QOS_FS_ACL_PATH=os.path.join(self.dir, "config", "fs-capabilities.json"),
            QOS_TEST_CONTROL=self.sock,
            NIRIOS_HEADLESS_SIZE=self.size,
            RUST_LOG=env.get("RUST_LOG", "info"),
        )
        self._log = open(self.log_path, "w")
        self.proc = subprocess.Popen(
            [self.binary, "--backend", "headless", "--enable-peer-sync"],
            cwd=REPO, env=env, stdout=self._log, stderr=subprocess.STDOUT,
            preexec_fn=os.setsid,
        )

    def wait_ready(self, timeout: float = 120.0) -> None:
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.proc and self.proc.poll() is not None:
                raise RuntimeError(
                    f"{self.name} exited before binding its control socket; "
                    f"see {self.log_path}")
            if os.path.exists(self.sock):
                try:
                    self.state()
                    break
                except (OSError, RuntimeError):
                    pass
            time.sleep(0.25)
        else:
            raise RuntimeError(f"{self.name} never answered on {self.sock}")
        # The compositor publishes the wayland socket name it settled on;
        # it is not wayland-0, because the outer session already holds that.
        socks = [f for f in os.listdir(self.runtime_dir)
                 if f.startswith("wayland-") and not f.endswith(".lock")]
        if not socks:
            raise RuntimeError(f"{self.name} published no wayland socket")
        self.wayland = sorted(socks)[0]

    def stop(self) -> None:
        for _, p, _ in self.apps:
            if p.poll() is None:
                try:
                    os.killpg(os.getpgid(p.pid), signal.SIGTERM)
                except Exception:
                    p.terminate()
        if self.proc and self.proc.poll() is None:
            try:
                os.killpg(os.getpgid(self.proc.pid), signal.SIGTERM)
            except Exception:
                self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        if self._log:
            self._log.close()

    # -- driving -----------------------------------------------------

    def state(self) -> dict:
        return ok(call(self.sock, {"op": "state_dump"}))["state"]

    def chrome(self) -> dict:
        return self.state()["chrome"]

    def tree(self) -> dict:
        return self.state()["workspace_tree"]

    def active_workspace(self) -> dict:
        tree = self.tree()
        for ws in tree["workspaces"]:
            if ws["id"] == tree["active"]:
                return ws
        raise RuntimeError(f"{self.name}: no active workspace")

    def foreign_tabs(self) -> list[dict]:
        out = []
        for strip in self.active_workspace()["strips"]:
            for grp in strip["window_groups"]:
                for tab in grp["tabs"]:
                    if "Foreign" in tab["tile"]:
                        out.append({"id": tab["id"], "title": tab["title"],
                                    "surface": tab["tile"]["Foreign"]["surface_id"],
                                    "active": grp["active_tab"] == tab["id"]})
        return out

    def frames(self) -> dict:
        """Per-surface frame counters. 0 means the placeholder, not the app."""
        return ok(call(self.sock, {"op": "foreign_frames"}))

    def settle(self, seconds: float = 0.6) -> None:
        time.sleep(seconds)

    def key(self, name: str) -> None:
        ok(call(self.sock, {"op": "key", "key": name}))

    def type_text(self, text: str) -> None:
        for i in range(0, len(text), 200):
            ok(call(self.sock, {"op": "type", "text": text[i:i + 200]}))

    def click_at(self, x: float, y: float) -> None:
        ok(call(self.sock, {"op": "click_at", "x": x, "y": y}))
        self.settle(0.4)

    def modifiers(self, ctrl=False, shift=False, alt=False, logo=False) -> None:
        ok(call(self.sock, {"op": "modifiers", "ctrl": ctrl, "shift": shift,
                            "alt": alt, "logo": logo}))

    def snapshot(self, path: str) -> str:
        os.makedirs(os.path.dirname(path), exist_ok=True)
        ok(call(self.sock, {"op": "snapshot", "path": path}))
        return path

    def open_palette(self, machine_wide: bool = True) -> None:
        self.modifiers(logo=machine_wide, ctrl=not machine_wide)
        self.key("k")
        self.modifiers()
        self.settle(0.4)

    def run_palette_command(self, query: str, expect_id: str | None = None,
                            machine_wide: bool = True) -> str | None:
        self.open_palette(machine_wide)
        self.type_text(query)
        self.settle(0.7)
        selected = self.chrome()["palette"]["selected_id"]
        if expect_id is not None and selected != expect_id:
            rows = [r["id"] for r in self.chrome()["palette"]["rows"]][:6]
            raise RuntimeError(
                f"{self.name}: palette query {query!r} selected {selected!r}, "
                f"expected {expect_id!r} (rows: {rows})")
        self.key("Enter")
        self.settle()
        return selected

    # -- apps --------------------------------------------------------

    def launch_app(self, app: str, peer_id: str, room: str,
                   doc: str | None, log_dir: str) -> subprocess.Popen:
        """Start a real app process against this session's own broker.

        The app is addressed exactly as the launcher addresses one:
        `QOS_PACKAGE_ID` is the routing identity the broker keys document
        traffic on, `QOS_WORKSPACE_ID` is the room. `QUICK_APP_ID` is this
        window's own identity — it drives the Automerge actor, so two
        windows are two distinguishable authors.
        """
        spec = APPS[app]
        env = self.base_env()
        env.update(
            WAYLAND_DISPLAY=self.wayland,
            QUICK_COLLAB_SOCK=self.collab_json_sock,
            QOS_PACKAGE_ID=spec["package"],
            QUICK_APP_ID=peer_id,
            QOS_WORKSPACE_ID=room,
            QUICK_COLLAB_DEBUG="1",
            RUST_LOG="warn",
        )
        argv = [os.path.join(REPO, "target", "debug", spec["bin"])]
        if spec["needs_doc"]:
            if doc is None:
                raise RuntimeError(f"{app} needs a document path")
            argv.append(doc)
        log_path = os.path.join(log_dir, f"{self.name}-{peer_id}-{app}.log")
        lg = open(log_path, "w")
        p = subprocess.Popen(argv, cwd=REPO, env=env, stdout=lg,
                             stderr=subprocess.STDOUT, preexec_fn=os.setsid)
        self.apps.append((peer_id, p, log_path))
        return p

    def app_log(self, peer_id: str) -> str:
        for pid, _, path in self.apps:
            if pid == peer_id:
                return open(path).read()
        return ""

    def wait_for_app_frame(self, timeout: float = 120.0) -> bool:
        """Wait until a foreign surface has actually painted.

        Adoption and first frame are different events, and the
        "waiting for first frame" placeholder is a perfectly stable image —
        photographing it looks like success. In a debug build the shader
        compile alone is tens of seconds.
        """
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                fr = self.frames()
                seqs = [s.get("frame_seq", 0)
                        for s in fr.get("surfaces", fr.get("frames", []))]
                if seqs and max(seqs) > 0:
                    return True
            except Exception:
                pass
            time.sleep(1.0)
        return False


# --------------------------------------------------------------------


def peer_id_of(log_path: str) -> str | None:
    import re
    try:
        blob = open(log_path).read()
    except OSError:
        return None
    m = re.search(r"local peer id[: ]+([0-9A-Za-z]+)", blob)
    return m.group(1) if m else None


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--app", default="word-processor", choices=sorted(APPS))
    ap.add_argument("--out", default=None, help="where to keep logs + shots")
    ap.add_argument("--keep", action="store_true", help="leave the sessions running")
    ap.add_argument("--one-shell", action="store_true",
                    help="both app windows in a single session (skips the invite)")
    ap.add_argument("--size", default="1280x800")
    ap.add_argument("--shell-bin", default=os.path.join(REPO, "target/debug/shell"))
    args = ap.parse_args()

    if not os.path.exists(args.shell_bin):
        print(f"missing {args.shell_bin}; run: cargo build -p shell", file=sys.stderr)
        return 2
    app_bin = os.path.join(REPO, "target", "debug", APPS[args.app]["bin"])
    if not os.path.exists(app_bin):
        print(f"missing {app_bin}; run: cargo build -p {APPS[args.app]['bin']}",
              file=sys.stderr)
        return 2

    out = args.out or os.path.join("/tmp", f"qos-app-collab-{os.getpid()}")
    os.makedirs(out, exist_ok=True)
    print(f"artefacts: {out}")

    pid = os.getpid()
    host = Session("host", "alice", os.path.join(out, "host"),
                   os.path.join(RUNTIME_BASE, f"qa-h{pid}"),
                   os.path.join(RUNTIME_BASE, f"qa-h{pid}.sock"),
                   args.shell_bin, args.size)
    peer = Session("peer", "bob", os.path.join(out, "peer"),
                   os.path.join(RUNTIME_BASE, f"qa-p{pid}"),
                   os.path.join(RUNTIME_BASE, f"qa-p{pid}.sock"),
                   args.shell_bin, args.size)
    sessions = [host] if args.one_shell else [host, peer]

    doc = None
    if APPS[args.app]["needs_doc"]:
        doc = os.path.join(out, "shared-report.md")
        with open(doc, "w") as f:
            f.write("Shared document.\n")

    try:
        step("1", "start the sessions")
        for s in sessions:
            s.start()
        for s in sessions:
            s.wait_ready()
            note(f"{s.name}: wayland={s.wayland} broker={s.collab_sock}")
            c = s.chrome()["collab"]
            claim(f"{s.name}'s broker is available", c["available"], True)
            claim(f"{s.name}'s broker is the one the launcher would dial",
                  c["socket"], s.collab_sock)

        if not args.one_shell:
            step("2", "join the peer to the host's workspace")
            host.run_palette_command("invite peer", expect_id="result:0")
            token = None
            deadline = time.time() + 30
            while time.time() < deadline:
                token = host.chrome().get("peer_sync", {}).get("invite_token")
                if token:
                    break
                time.sleep(0.5)
            claim("the host minted an invite", bool(token), True)
            if not token:
                raise RuntimeError("no invite token; see the host log")
            peer.run_palette_command("connect to peer", expect_id="result:0")
            peer.type_text(token)
            peer.key("Enter")
            deadline = time.time() + 60
            joined = False
            while time.time() < deadline:
                st = peer.chrome().get("peer_sync", {}).get("status") or ""
                if st.startswith("Joined"):
                    joined = True
                    break
                time.sleep(0.5)
            claim("the peer joined", joined, True)
            claim("both sessions are in one room",
                  peer.active_workspace()["room"], host.active_workspace()["room"])

        room = host.active_workspace()["room"]
        note(f"room = {room}")

        step("3", f"launch a real {args.app} into each session")
        if args.one_shell:
            host.launch_app(args.app, "alice", room, doc, out)
            time.sleep(20)
            host.launch_app(args.app, "bob", room, doc, out)
            targets = [(host, "alice"), (host, "bob")]
        else:
            host.launch_app(args.app, "alice", room, doc, out)
            peer.launch_app(args.app, "bob", room, doc, out)
            targets = [(host, "alice"), (peer, "bob")]
        for s in sessions:
            painted = s.wait_for_app_frame()
            claim(f"{s.name}'s app painted a real frame", painted, True)

        step("4", "type into one window")
        writer, writer_id = targets[0]
        reader, reader_id = targets[-1]
        writer.click_at(600, 377)
        marker = "the-edit-crossed"
        writer.type_text(f" {marker}")
        time.sleep(8)

        step("5", "read it back from the other process")
        reader_log = reader.app_log(reader_id)
        got = "recv CRDTIncremental" in reader_log or "recv CRDTUpdate" in reader_log
        claim(f"{reader_id}'s process applied a remote change", got, True)
        for _, pidname, path in [(a, b, c) for s in sessions for (a, b, c) in s.apps]:
            pass
        for s in sessions:
            for pid_name, _, path in s.apps:
                note(f"{s.name}/{pid_name}: " +
                     " | ".join(l for l in open(path).read().splitlines()
                                if l.startswith("[app]"))[:600])

        step("6", "photograph both")
        for s, pid_name in targets:
            shot = os.path.join(out, f"{s.name}-{pid_name}.png")
            s.snapshot(shot)
            note(f"wrote {shot}")

    finally:
        if not args.keep:
            for s in sessions:
                s.stop()
            for s in sessions:
                shutil.rmtree(s.runtime_dir, ignore_errors=True)
        else:
            print("\n--keep: sessions left running. Control sockets:")
            for s in sessions:
                print(f"  {s.name}: {s.sock}")

    print()
    if FAILURES:
        print(f"{len(FAILURES)} claim(s) failed:")
        for f in FAILURES:
            print(f"  - {f}")
        return 1
    print("every claim held.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
