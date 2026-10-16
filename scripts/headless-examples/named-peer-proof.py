#!/usr/bin/env python3
"""The caret says who is editing, not which machine is.

A peer reaches a document as its transport identity, and until now that
is what got drawn over their caret: `peer@5537458103208628978`. A number
says a machine is here. It does not say who is here, which is the only
thing the person reading the screen wanted to know.

The name has to come from the **inviter**, and that is not a matter of
taste. A peer that announced its own name could announce any name, and
it would render beside a caret inside a document this room's
capabilities gate. So the inviter names the peer they are inviting, the
name goes into the **room document** keyed by peer id — the same
replicated, authority-checked place the capability grants live — and
every member reads it from there. The peer id stays the identity; the
name is a label on it.

This runs two real compositors with separate identities, separate homes
and separate device keys. quinten invites xaver **by name**, xaver joins
with the token, both open the real word processor against their own
session, and xaver types. The claim is read off quinten's screen.

# What is asserted, and where

- The name is on the wire: a client attached to quinten's broker,
  announcing the room, receives a roster entry naming xaver — produced
  by quinten's compositor out of quinten's room document, in response to
  a presence frame from xaver's machine.
- The identity is unchanged: that roster entry is keyed by the same
  `peer@<handle>` the presence frames carry, so a peer that gains a name
  does not change colour.
- And the pixels: quinten's session is snapshotted with xaver's caret in
  it. A name that is in the state and not on the screen is the failure
  this exists to catch, so the PNG is the product.

Usage:
  ./named-peer-proof.py [--keep] [--out DIR]

Exits non-zero if any claim fails. A "GAP" is something this run could
not show, reported rather than glossed; it does not fail the run.
"""
import argparse
import base64
import json
import os
import pathlib
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time

from reaper import die_with_parent

REPO = pathlib.Path(__file__).resolve().parents[2]
SHELL = REPO / "target" / "debug" / "shell"
WORD_PROCESSOR = REPO / "target" / "debug" / "word-processor"
APP_ID = "so.tilo.word-processor"
ACTIONS = "actions"

# What quinten calls xaver when he invites him. The whole point of the
# run is that this string, and nothing derived from a peer id, is what
# ends up on screen.
INVITED_AS = "Xaver"

failures: list[str] = []
gaps: list[str] = []


def check(label: str, ok: bool, detail: str = "") -> bool:
    print(f"  {'PASS' if ok else 'FAIL'}  {label}" + (f" — {detail}" if detail else ""))
    if not ok:
        failures.append(label)
    return bool(ok)


def gap(what: str) -> None:
    print(f"  GAP   {what}")
    gaps.append(what)


def step(what: str) -> None:
    print(f"\n· {what}")


def wait_for(pred, timeout=30.0, poll=0.3):
    end = time.time() + timeout
    while time.time() < end:
        try:
            got = pred()
        except Exception:
            got = None
        if got:
            return got
        time.sleep(poll)
    return None


def freshness(binary: pathlib.Path, sources: list[pathlib.Path]) -> str | None:
    """Is `binary` older than anything it is built from?

    A stale `target/debug` binary produces a verdict about code that is
    not the code under test, and it produces it silently — the run looks
    exactly like a run that worked. So the answer is computed and
    printed rather than assumed.
    """
    if not binary.exists():
        return f"{binary} does not exist"
    built = binary.stat().st_mtime
    newer = [s for s in sources if s.exists() and s.stat().st_mtime > built]
    if newer:
        names = ", ".join(str(s.relative_to(REPO)) for s in newer[:3])
        return f"{binary.name} predates {names}"
    return None


class Session:
    """One real compositor, isolated from the machine and from its peer."""

    def __init__(self, name: str, root: str, runtime_root: str):
        self.name = name
        self.dir = os.path.join(root, name)
        os.makedirs(self.dir, exist_ok=True)
        # Short paths, both of them. A `sun_path` is 108 bytes, and the
        # compositor's control socket and its Wayland socket both live
        # under directories named here — a run rooted somewhere verbose
        # fails at bind with an error about the path rather than about
        # the length.
        self.socket = os.path.join(runtime_root, f"{name[0]}.sock")
        self.collab_sock = os.path.join(runtime_root, f"{name[0]}c.sock")
        self.runtime = os.path.join(runtime_root, f"r{name[0]}")
        os.makedirs(self.runtime, mode=0o700, exist_ok=True)
        self.home = os.path.join(self.dir, "home")
        os.makedirs(self.home, exist_ok=True)
        env = {
            **os.environ,
            "TILO_TEST_CONTROL": self.socket,
            "TILO_STATE_PATH": os.path.join(self.dir, "config", "state.json"),
            "HOME": self.home,
            "XDG_DATA_HOME": os.path.join(self.dir, "data"),
            "XDG_CACHE_HOME": os.path.join(self.dir, "cache"),
            "XDG_STATE_HOME": os.path.join(self.dir, "state"),
            "XDG_CONFIG_HOME": os.path.join(self.dir, "config"),
            "XDG_RUNTIME_DIR": self.runtime,
            # Without its own device key a session's peer-sync turns
            # itself off, and the only visible symptom is that
            # `peer.invite` is never exported — which reads as a
            # capability refusal rather than a missing prerequisite.
            "TILO_PEER_KEY_PATH": os.path.join(self.dir, "keys", "peer-key"),
            # Distinct identities, or both sessions are the same person
            # and every claim about "somebody else" is meaningless.
            "TILO_USER_ID": name,
            # The app-facing broker socket, named rather than defaulted
            # so the word processor this script starts can be pointed at
            # exactly this session's.
            "QUICK_COLLAB_SOCK": self.collab_sock,
            "TILO_HEADLESS_SIZE": "1600x1000",
            "LC_ALL": "en_US.UTF-8",
            "LANG": "en_US.UTF-8",
        }
        env.pop("DISPLAY", None)
        env.pop("WAYLAND_DISPLAY", None)
        self.env = env
        self.log_path = os.path.join(self.dir, "shell.log")
        self.log = open(self.log_path, "w")
        self.proc = subprocess.Popen(
            # `--mode native` names the smithay compositor, the only
            # session there is and one a Wayland client can attach to;
            # the flag is kept so this proof states what it drives.
            [str(SHELL), "--mode", "native", "--backend", "headless",
             "--enable-peer-sync"],
            env=env, stdout=self.log, stderr=subprocess.STDOUT,
            preexec_fn=die_with_parent(),
        )
        self.app: subprocess.Popen | None = None
        self.app_log = None

    def wait_up(self, timeout=60.0) -> bool:
        end = time.time() + timeout
        while time.time() < end:
            if os.path.exists(self.socket):
                time.sleep(1.0)
                return True
            if self.proc.poll() is not None:
                return False
            time.sleep(0.3)
        return False

    def call(self, payload: dict, timeout=60) -> dict:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(timeout)
        s.connect(self.socket)
        s.sendall((json.dumps(payload) + "\n").encode())
        buf = b""
        while not buf.endswith(b"\n"):
            chunk = s.recv(65536)
            if not chunk:
                break
            buf += chunk
        s.close()
        return json.loads(buf.decode())

    # -- convenience -------------------------------------------------
    def state(self) -> dict:
        return self.call({"op": "state_dump"})["state"]

    def chrome(self) -> dict:
        return self.state()["chrome"]

    def room(self) -> str | None:
        tree = self.state()["workspace_tree"]
        active = tree.get("active")
        for ws in tree.get("workspaces", []):
            if ws.get("id") == active:
                return ws.get("room")
        return None

    def key(self, k: str):
        self.call({"op": "key", "key": k})

    def type_text(self, t: str):
        self.call({"op": "type", "text": t})

    def click_at(self, x: float, y: float):
        self.call({"op": "click_at", "x": x, "y": y})

    def enumerate(self, query: str = "") -> list:
        r = self.call({"op": "actions_enumerate", "query": query})
        return r.get("answer", {}).get("actions", []) if "answer" in r else []

    def invoke(self, ident: str, args: dict | None = None) -> dict:
        payload = {"op": "actions_invoke", "provider": ACTIONS, "id": ident}
        if args is not None:
            payload["args"] = args
        return self.call(payload)

    def snapshot(self, path: str) -> dict:
        return self.call({"op": "snapshot", "path": path}, timeout=180)

    def wayland_display(self) -> str | None:
        path = os.path.join(self.runtime, "tilo-session", "wayland-display")
        try:
            with open(path) as fh:
                return fh.read().strip() or None
        except OSError:
            return None

    def start_word_processor(self, room: str) -> bool:
        """Run the real word processor as a client of this session.

        Started directly rather than through the launcher: what is under
        test is the label over a peer's caret, which is drawn by the app
        out of its own `PresenceCtx`, and a bwrap sandbox around the same
        binary changes nothing about that while adding a registry, a
        manifest and an install to the list of things that can fail
        first. The binary is the one built from this tree, which is the
        part that does matter and is checked above.
        """
        display = self.wayland_display()
        if not display:
            return False
        env = {
            **self.env,
            "WAYLAND_DISPLAY": display,
            # How the toolkit addresses the broker: which app, and which
            # room. The room is the workspace id, which is the whole reason
            # two machines' apps can reach each other.
            "QUICK_APP_ID": APP_ID,
            "TILO_PACKAGE_ID": APP_ID,
            "TILO_WORKSPACE_ID": room,
            "QUICK_COLLAB_SOCK": self.collab_sock,
        }
        self.app_log = open(os.path.join(self.dir, "word-processor.log"), "w")
        self.app = subprocess.Popen(
            [str(WORD_PROCESSOR)], env=env,
            stdout=self.app_log, stderr=subprocess.STDOUT,
            preexec_fn=die_with_parent(),
        )
        return True

    def app_painted(self, timeout=90.0) -> bool:
        """Has a foreign surface actually reached the strip?"""
        def painted():
            tree = self.state()["workspace_tree"]
            active = tree.get("active")
            for ws in tree.get("workspaces", []):
                if ws.get("id") != active:
                    continue
                for strip in ws.get("strips", []):
                    for group in strip.get("window_groups", []):
                        if group.get("tabs"):
                            return True
            return None
        return bool(wait_for(painted, timeout=timeout, poll=1.0))

    def stop(self):
        for proc in (self.app, self.proc):
            if proc is None:
                continue
            try:
                proc.terminate()
                proc.wait(timeout=8)
            except Exception:
                try:
                    proc.kill()
                except Exception:
                    pass
        for fh in (self.app_log, self.log):
            if fh:
                try:
                    fh.close()
                except Exception:
                    pass


class BrokerClient:
    """A client of one session's broker, speaking the app protocol.

    The same length-prefixed JSON `CollabRequest`/`CollabEvent` dialect
    `ui::collaboration` speaks, announcing the same `(app_id,
    cluster_id)` address the word processor announces. What it receives
    is therefore what a real app in that session receives — which is the
    point: the roster entry read back here is produced by the
    compositor, out of its room document, and not by this script.
    """

    def __init__(self, path: str, room: str, handle: str):
        self.frames: list[dict] = []
        self.err: str | None = None
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(15.0)
        self.sock.connect(path)
        self._send({"Hello": {"app_id": APP_ID, "cluster_id": room,
                              "record": None, "handle": handle}})
        threading.Thread(target=self._read, daemon=True).start()

    def _send(self, obj: dict) -> None:
        body = json.dumps(obj).encode()
        self.sock.sendall(struct.pack("<I", len(body)) + body)

    def _read(self) -> None:
        try:
            f = self.sock.makefile("rb")
            while True:
                hdr = f.read(4)
                if not hdr or len(hdr) < 4:
                    return
                body = f.read(struct.unpack("<I", hdr)[0])
                if not body:
                    return
                try:
                    self.frames.append(json.loads(body.decode()))
                except Exception:
                    pass
        except Exception as exc:  # noqa: BLE001
            self.err = f"{type(exc).__name__}: {exc}"

    def roster_names(self) -> list[tuple[str, str]]:
        """Every (peer id, name) pair a `Context` frame has carried."""
        out = []
        for f in self.frames:
            ctx = f.get("Context") if isinstance(f, dict) else None
            if not ctx:
                continue
            for c in ctx.get("collaborators", []) or []:
                out.append((c.get("id", ""), c.get("name", "")))
        return out

    def presence_peers(self) -> list[str]:
        """Every peer id a `RemotePresenceUpdate` has been attributed to."""
        out = []
        for f in self.frames:
            upd = f.get("RemotePresenceUpdate") if isinstance(f, dict) else None
            if upd:
                out.append(upd.get("user_id", ""))
        return out

    def close(self):
        try:
            self.sock.close()
        except Exception:
            pass


def mint_invite(host: Session) -> str | None:
    host.invoke("peer.invite")
    token = wait_for(lambda: host.chrome()["peer_sync"]["invite_token"], timeout=30)
    host.key("Escape")
    time.sleep(0.4)
    return token


def mint_named_invite(host: Session, peer: str, name: str) -> tuple[str | None, dict]:
    resp = host.invoke("peer.invite.named", {"peer": peer, "name": name})
    if "err" in resp:
        return None, resp
    token = wait_for(lambda: host.chrome()["peer_sync"]["invite_token"], timeout=30)
    host.key("Escape")
    time.sleep(0.4)
    return token, resp


def join(guest: Session, token: str) -> bool:
    resp = guest.invoke("peer.connect")
    if "err" in resp:
        return False
    if not wait_for(lambda: guest.chrome()["peer_sync"]["connect_input"] is not None,
                    timeout=25):
        return False
    guest.type_text(token)
    guest.key("Enter")
    ok = wait_for(
        lambda: (guest.chrome()["peer_sync"]["status"] or "").startswith("Joined"),
        timeout=30)
    guest.key("Escape")
    time.sleep(0.4)
    return bool(ok)


def peer_id_of(token: str) -> str | None:
    """The peer id inside a session's own invite.

    A token names the machine that issued it, so a session's invite is
    also how that session says who it is — which is what lets one person
    hand another the identity they are about to be invited under,
    without anything new on the wire.
    """
    try:
        pad = "=" * (-len(token) % 4)
        blob = json.loads(base64.urlsafe_b64decode(token + pad).decode())
        return blob.get("owner_peer_id") or None
    except Exception:
        return None


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--out", default=None,
                    help="where the screenshots land (default: a temp dir)")
    args = ap.parse_args()

    # -- what is being run -------------------------------------------
    print("Binaries under test")
    shell_sources = [
        REPO / "crates" / "session" / "shell" / "src" / "shell" / "state.rs",
        REPO / "crates" / "session" / "shell" / "src" / "shell" / "collab_broker.rs",
        REPO / "crates" / "session" / "shell" / "src" / "shell" / "network.rs",
        REPO / "crates" / "session" / "shell" / "src" / "shell" / "search" / "providers" / "actions.rs",
    ]
    app_sources = [
        REPO / "apps" / "word-processor" / "src" / "widget.rs",
        REPO / "crates" / "sdk" / "ui-core" / "src" / "presence" / "mod.rs",
        REPO / "crates" / "sdk" / "ui-core" / "src" / "app.rs",
    ]
    stale = [s for s in (freshness(SHELL, shell_sources),
                         freshness(WORD_PROCESSOR, app_sources)) if s]
    for s in stale:
        print(f"  STALE {s}")
    if stale:
        print("\nFAIL: a stale binary answers about code that is not under test.\n"
              "  cargo build -p shell -p word-processor", file=sys.stderr)
        return 2
    print(f"  {SHELL}")
    print(f"  {WORD_PROCESSOR}")

    root = tempfile.mkdtemp(prefix="tilo-named-")
    # Sockets live somewhere short: `sun_path` is 108 bytes and both the
    # control socket and the Wayland socket hang off these directories.
    runtime_root = tempfile.mkdtemp(prefix="qs-", dir="/tmp")
    shots = os.path.abspath(args.out) if args.out else os.path.join(root, "shots")
    os.makedirs(shots, exist_ok=True)
    print(f"\nrun directory: {root}\nscreenshots:   {shots}")

    quinten = Session("quinten", root, runtime_root)
    xaver = Session("xaver", root, runtime_root)
    watcher: BrokerClient | None = None
    try:
        step("two sessions come up")
        for s in (quinten, xaver):
            if not check(f"{s.name}'s compositor is up", s.wait_up()):
                return 1

        exported = {f"{a.get('provider')}:{a.get('id')}" for a in quinten.enumerate("")}
        if not check("peer-sync is on (peer.invite.named is exported)",
                     f"{ACTIONS}:peer.invite.named" in exported):
            print("     without peer-sync there is no peer to name")
            return 1

        step("xaver says who he is — his own invite names his peer id")
        xaver_token = mint_invite(xaver)
        xaver_peer = peer_id_of(xaver_token or "")
        if not check("xaver's peer id is known", bool(xaver_peer), xaver_peer or ""):
            return 1

        step(f"quinten invites him, as {INVITED_AS!r}")
        token, resp = mint_named_invite(quinten, xaver_peer, INVITED_AS)
        check("quinten's invite was accepted by the room he administers",
              "err" not in resp, json.dumps(resp.get("err", ""))[:120])
        if not check("quinten minted an invite token", bool(token)):
            return 1

        step("xaver joins with it")
        if not check("xaver joined quinten's room", join(xaver, token)):
            return 1
        room = quinten.room()
        check("both sessions address one room", xaver.room() == room, room or "")

        step("a client of quinten's broker listens on that room")
        # Attached before either app starts, so nothing it reports can be
        # a frame it caused.
        watcher = BrokerClient(quinten.collab_sock, room, "named-peer-proof")

        step("both open the word processor against their own session")
        for s in (quinten, xaver):
            if not check(f"{s.name}'s word processor started",
                         s.start_word_processor(room)):
                gap(f"{s.name} has no Wayland display published, so no app could "
                    f"attach to his session")
                return 1
        for s in (quinten, xaver):
            check(f"{s.name}'s word processor painted", s.app_painted())
        # Both apps have to have announced their address before either
        # types, or the first keystrokes go out to nobody and it reads
        # afterwards as a transport failure.
        time.sleep(8.0)

        step("quinten writes, so there is a document for xaver to stand in")
        # Inside the page, not under it. The layout bar sits below the
        # canvas, and a click that lands there focuses chrome — so the
        # text goes somewhere that is not the document and the run reads
        # afterwards exactly like a document that failed to sync.
        quinten.click_at(600.0, 380.0)
        time.sleep(1.5)
        quinten.type_text("The room knows who is in it.")
        time.sleep(3.0)
        quinten.snapshot(os.path.join(shots, "01-quinten-typed.png"))

        step("and xaver puts his caret in it")
        xaver.click_at(600.0, 380.0)
        time.sleep(1.5)
        xaver.type_text(" Xaver was here.")
        time.sleep(4.0)
        xaver.snapshot(os.path.join(shots, "02-xaver-typed.png"))

        step("what reached quinten's session")
        peers = wait_for(lambda: watcher.presence_peers() or None, timeout=60)
        check("a peer's presence reached quinten's apps", bool(peers),
              ", ".join(sorted(set(peers or []))[:2]))

        named = wait_for(
            lambda: [p for p in watcher.roster_names() if p[1] == INVITED_AS] or None,
            timeout=60)
        check(f"quinten's session announces that peer as {INVITED_AS!r}",
              bool(named), str(named[0]) if named else "")

        if named and peers:
            # The identity has to be the *same* one the presence frames
            # carry, or the name labels nobody: colour is keyed on the
            # id, so a roster entry under a different id would put a
            # second, differently-coloured stranger on the screen.
            check("the name is attached to the id the caret arrives under",
                  named[0][0] in set(peers), named[0][0])
            check("and the id is still the peer id, not the name",
                  named[0][0].startswith("peer@"), named[0][0])

        step("the screen")
        final = os.path.join(shots, "03-quinten-sees-xaver.png")
        quinten.snapshot(final)
        check("quinten's screen was captured", os.path.exists(final)
              and os.path.getsize(final) > 0, final)
        print(f"\n  look at: {final}")

    finally:
        if watcher:
            watcher.close()
        for s in (xaver, quinten):
            s.stop()
        if not args.keep:
            shutil.rmtree(runtime_root, ignore_errors=True)
            if not args.out:
                print(f"\n(screenshots were under {root}; pass --out to keep them)")
            shutil.rmtree(root, ignore_errors=True)
        else:
            print(f"\nkept: {root}")

    print()
    for g in gaps:
        print(f"GAP  {g}")
    if failures:
        print(f"\nFAILED ({len(failures)}):")
        for f in failures:
            print(f"  - {f}")
        return 1
    print("all claims hold")
    return 0


if __name__ == "__main__":
    sys.exit(main())
