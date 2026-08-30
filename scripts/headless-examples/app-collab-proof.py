#!/usr/bin/env python3
"""What actually crosses between two sessions when an app collaborates.

An app that has adopted records — the word processor is the one this
matters for — collaborates by attaching to its session's broker socket,
announcing `Hello { app_id, cluster_id, record }`, and then sending
frames. `cluster_id` is the **room** id, which is the whole reason two
machines can address one cluster: the launcher passes the room rather
than the local workspace handle precisely so a peer's app and ours land
together (`shell/src/shell/launcher.rs`).

This script attaches such a client to each of two real compositors in
one room and asks, frame type by frame type, what arrives at the other
end.

# What this proves, and what it does not

The payloads here are **opaque byte blobs**, not Automerge changes.
Every hop between the two apps treats the payload as opaque — the
broker, `NeighbourMsg::AppCrdtIncremental`, the room gate, the far
broker's fan-out — so a blob that makes the trip proves the transport
those hops form. It does **not** prove that two documents converge:
that additionally needs both apps to hold the same record and to apply
what arrives, which only the real app can demonstrate.

So: this is the proof that the pipe exists and is connected. Document
convergence through the real app is a separate claim and needs a
separate proof.

# Why there is a same-session control

Each frame is sent with a second client attached to the **sender's own**
broker. A frame that reaches the control but not the far session is one
the Iroh bridge does not carry. A frame that reaches neither was
rejected by the broker — malformed, or off-route. Without the control
those two failures are indistinguishable, and the second would send you
hunting through the network layer for a bug in your own JSON.

Usage:
  ./app-collab-proof.py [--keep]

Exits non-zero if any claim fails. A "GAP" is a thing this run could not
show, reported rather than glossed; it does not fail the run.
"""
import argparse
import base64
import json
import os
import shutil
import socket
import struct
import subprocess

from reaper import die_with_parent
import sys
import tempfile
import threading
import time
import uuid

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SHELL = os.path.join(REPO, "target", "debug", "shell")

# One app, one document, announced identically by both sessions. These are
# what the broker routes on; two clients that disagree on any of the three
# are two different conversations.
APP_ID = "dev.qos.word-processor"
RECORD = "file:/work/shared.odt"

failures: list[str] = []
gaps: list[str] = []


def check(label: str, ok: bool, detail: str = "") -> bool:
    print(f"  {'PASS' if ok else 'FAIL'}  {label}" + (f" — {detail}" if detail else ""))
    if not ok:
        failures.append(label)
    return ok


def gap(what: str) -> None:
    print(f"  GAP   {what}")
    gaps.append(what)


def wait_for(pred, timeout=40.0, poll=0.4):
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


class Session:
    """One real compositor, isolated from the machine and from its peer."""

    def __init__(self, name: str, root: str):
        self.name = name
        self.dir = os.path.join(root, name)
        os.makedirs(self.dir, exist_ok=True)
        self.socket = os.path.join(self.dir, "control.sock")
        self.collab = os.path.join(self.dir, "collab.sock")
        env = {
            **os.environ,
            "QOS_TEST_CONTROL": self.socket,
            # Bind the broker's app-facing JSON socket somewhere this
            # script can reach. In a real session apps take the capnp
            # descriptor from the launcher instead.
            "QUICK_COLLAB_SOCK": self.collab,
            "QOS_STATE_PATH": os.path.join(self.dir, "config", "state.json"),
            "XDG_DATA_HOME": os.path.join(self.dir, "data"),
            "XDG_CACHE_HOME": os.path.join(self.dir, "cache"),
            "XDG_STATE_HOME": os.path.join(self.dir, "state"),
            "XDG_CONFIG_HOME": os.path.join(self.dir, "config"),
            "XDG_RUNTIME_DIR": os.path.join(self.dir, "run"),
            # Without its own device key a session's peer-sync turns
            # itself off, and the only symptom is a missing peer.* action.
            "QOS_PEER_KEY_PATH": os.path.join(self.dir, "keys", "peer-key"),
            "NIRIOS_HEADLESS_SIZE": "1280x800",
        }
        env.pop("DISPLAY", None)
        env.pop("WAYLAND_DISPLAY", None)
        os.makedirs(env["XDG_RUNTIME_DIR"], mode=0o700, exist_ok=True)
        self.log_path = os.path.join(self.dir, "shell.log")
        self.log = open(self.log_path, "w")
        self.proc = subprocess.Popen(
            [SHELL, "--backend", "headless", "--enable-peer-sync"],
            env=env, stdout=self.log, stderr=subprocess.STDOUT,
            preexec_fn=die_with_parent(),
        )

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

    def call(self, payload: dict) -> dict:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(40)
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

    def chrome(self) -> dict:
        return self.call({"op": "state_dump"})["state"]["chrome"]

    def invoke(self, ident: str) -> dict:
        return self.call({"op": "actions_invoke", "provider": "actions", "id": ident})

    def stop(self):
        try:
            self.proc.terminate()
            self.proc.wait(timeout=6)
        except Exception:
            try:
                self.proc.kill()
            except Exception:
                pass
        try:
            self.log.close()
        except Exception:
            pass


class AppClient:
    """A record-backed app attached to one session's broker.

    Speaks the same length-prefixed JSON `CollabRequest` / `CollabEvent`
    protocol `ui::collaboration` speaks, and announces the same address,
    so the broker cannot tell it from the real toolkit.
    """

    def __init__(self, path: str, room: str):
        self.rx: list[dict] = []
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
        self.send({"Hello": {"app_id": APP_ID, "cluster_id": room, "record": RECORD}})
        threading.Thread(target=self._read, daemon=True).start()

    def send(self, obj: dict) -> None:
        body = json.dumps(obj).encode()
        self.sock.sendall(struct.pack("<I", len(body)) + body)

    def _read(self) -> None:
        f = self.sock.makefile("rb")
        while True:
            hdr = f.read(4)
            if not hdr or len(hdr) < 4:
                return
            n = struct.unpack("<I", hdr)[0]
            body = f.read(n)
            if not body:
                return
            try:
                self.rx.append(json.loads(body.decode()))
            except Exception:
                pass

    def of_kind(self, kind: str) -> list:
        return [m[kind] for m in list(self.rx) if kind in m]

    def got_blob(self, blob: list) -> bool:
        want = bytes(blob)
        return any(bytes(d["data"]) == want for d in self.of_kind("CRDTIncremental"))

    def presence(self, payload_kind: str) -> list:
        """Received presence updates whose payload is `payload_kind`.

        Matched on the payload variant rather than on arrival order: a
        caret and an authorship run are different halves of the beat and
        a run that carried one but not the other has to be visible as
        such.
        """
        return [
            u for u in self.of_kind("RemotePresenceUpdate")
            if isinstance(u.get("payload"), dict) and payload_kind in u["payload"]
        ]

    def kinds(self) -> list:
        return sorted({k for m in list(self.rx) for k in m})


def blob(tag: str) -> list:
    return list(f"{tag}-{uuid.uuid4().hex[:8]}".encode())


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", action="store_true", help="leave sessions running")
    args = ap.parse_args()

    if not os.path.exists(SHELL):
        print(f"FAIL: compositor not built at {SHELL}", file=sys.stderr)
        print("      run: cargo build -p shell --bin shell", file=sys.stderr)
        return 1

    root = tempfile.mkdtemp(prefix="qos-app-collab-")
    sessions: list[Session] = []
    try:
        print("\n[1] two real sessions, isolated from this machine and each other")
        alice = Session("alice", root); sessions.append(alice)
        bob = Session("bob", root); sessions.append(bob)
        for s in sessions:
            if not check(f"{s.name} came up", s.wait_up(), f"see {s.log_path}"):
                return 1

        print("\n[2] one room")
        exported = {a["id"] for a in alice.call(
            {"op": "actions_enumerate", "query": ""}).get("answer", {}).get("actions", [])}
        if not check("peer-sync is on (peer.invite is exported)", "peer.invite" in exported):
            print(f"        check {alice.log_path} for 'device key load failed'")
            return 1

        alice.invoke("peer.invite")
        token = wait_for(lambda: alice.chrome()["peer_sync"]["invite_token"], timeout=25)
        if not check("alice minted an invite", bool(token)):
            return 1
        alice.call({"op": "key", "key": "Escape"})
        # The room the two will share. Read out of the token rather than
        # guessed: it is the value the guest adopts, and it is what the
        # apps below must name as their cluster.
        room = json.loads(base64.urlsafe_b64decode(token + "=" * (-len(token) % 4)))["room_id"]

        bob.invoke("peer.connect")
        wait_for(lambda: bob.chrome()["peer_sync"]["connect_input"] is not None, timeout=20)
        bob.call({"op": "type", "text": token})
        bob.call({"op": "key", "key": "Enter"})
        joined = wait_for(
            lambda: (bob.chrome()["peer_sync"]["status"] or "").startswith("Joined"), timeout=25)
        check("bob joined the room", bool(joined))
        bob.call({"op": "key", "key": "Escape"})
        print(f"        room = {room}")

        print("\n[3] a record-backed app on each session, all naming one document")
        a_app = AppClient(alice.collab, room)
        # The control: same session as the sender, same route. It is what
        # separates "the bridge does not carry this" from "the broker
        # refused it".
        a_control = AppClient(alice.collab, room)
        b_app = AppClient(bob.collab, room)
        time.sleep(3.0)
        check("all three clients attached", True, f"app_id={APP_ID} record={RECORD}")

        print("\n[4] a document delta, alice → bob")
        one = blob("ALICE")
        a_app.send({"SyncIncremental": {"data": one}})
        check("alice's own session fanned it out",
              bool(wait_for(lambda: a_control.got_blob(one), timeout=20)))
        check("bob's app received it",
              bool(wait_for(lambda: b_app.got_blob(one), timeout=45)),
              "crossed the network")

        print("\n[5] and back, bob → alice")
        two = blob("BOB")
        b_app.send({"SyncIncremental": {"data": two}})
        check("alice's app received it",
              bool(wait_for(lambda: a_app.got_blob(two), timeout=45)),
              "both directions carry")

        print("\n[6] both at once — neither delta may displace the other")
        mine, theirs = blob("CONCURRENT-A"), blob("CONCURRENT-B")
        a_app.send({"SyncIncremental": {"data": mine}})
        b_app.send({"SyncIncremental": {"data": theirs}})
        check("bob received alice's", bool(wait_for(lambda: b_app.got_blob(mine), timeout=45)))
        check("alice received bob's", bool(wait_for(lambda: a_app.got_blob(theirs), timeout=45)))

        print("\n[7] presence — the half of collaboration you can see")
        # Published exactly as `ui::App` publishes them: a caret as the
        # insertion point moves, a selection as it widens, and a `Typing`
        # run naming the span just inserted. That last one is what the
        # receiver renders as "text they just wrote, in their colour"
        # (`PresenceCtx::record_peer_typing` → `draw_peer_authorship`), so
        # it is asserted separately rather than folded into "presence".
        scope = {"Tab": {"id": "doc-body"}}
        a_app.send({"Presence": {"key": scope, "payload": {"Caret": {"pos": 7}}}})
        local = wait_for(lambda: a_control.presence("Caret"), timeout=20)
        # Asserted first, because it is what makes the remote line mean
        # anything: the frame is well-formed, on-route, and locally fanned.
        check("the presence frame is accepted and fanned out locally", bool(local),
              json.dumps(local[0])[:110] if local else "the broker did not fan it")
        check("bob's app received alice's caret",
              bool(wait_for(lambda: b_app.presence("Caret"), timeout=45)),
              "presence crosses the network")

        a_app.send({"Presence": {"key": scope,
                                 "payload": {"Selection": {"start": 3, "end": 11}}}})
        check("bob's app received alice's selection",
              bool(wait_for(lambda: b_app.presence("Selection"), timeout=45)))

        a_app.send({"Presence": {"key": scope,
                                 "payload": {"Typing": {"start": 11, "end": 18}}}})
        got_typing = wait_for(lambda: b_app.presence("Typing"), timeout=45)
        check("bob's app received the run alice just typed", bool(got_typing),
              "this is what renders as authorship in the author's colour")

        # Two peers must not collapse onto one identity: the colour a
        # caret is drawn in is `user_color(user_id)`, so a shared id would
        # paint two people the same and a name would label the wrong one.
        if got_typing:
            remote_uid = got_typing[0].get("user_id")
            local_uid = local[0].get("user_id") if local else None
            check("the remote peer is attributed to its own identity",
                  bool(remote_uid) and remote_uid != local_uid,
                  f"remote={remote_uid!r} local={local_uid!r}")

        print(f"\n        alice's control saw: {a_control.kinds()}")
        print(f"        bob's app saw:       {b_app.kinds()}")

        print()
        if failures:
            print(f"FAILED: {len(failures)} claim(s): {', '.join(failures)}")
            return 1
        print("Every claim held, read back from the session that did not send.")
        print("Payloads here are opaque blobs: this proves the transport, not "
              "that two documents converge.")
        if gaps:
            print(f"{len(gaps)} thing(s) this run could not show:")
            for g in gaps:
                print(f"  - {g}")
        return 0
    finally:
        if args.keep:
            print(f"\nsessions left running under {root}")
        else:
            for s in sessions:
                s.stop()
            shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
