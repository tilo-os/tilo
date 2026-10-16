#!/usr/bin/env python3
"""Two people in one room see each other's pointer on the desktop.

Not inside an app — on the desktop. Over the strip, over the chrome,
between windows, with nothing open at all. That is the case the
session's own publisher has to cover: presence that only travels while
a local app is publishing says nothing on an empty desktop, and an
empty desktop is what two people share for the first seconds after they
join.

Two real compositors, separate identities, separate homes, one room.
ada moves his pointer; the claim is read back from *bob's*
session — first the peer list his overlay is about to paint, then the
pixels it painted.

The picture is the point. A peer can be present in the state and absent
from the screen, which is exactly the failure this proves against, so
the last checks sample the PNG itself: saturated pixels where the peer
is, and none there once he points elsewhere.

Usage:
  ./desktop-peer-cursor-proof.py [--keep] [--out DIR]

Exits non-zero if any claim fails.
"""
import argparse
import json
import os
import pathlib
import shutil
import socket
import subprocess
import sys
import tempfile
import time

from reaper import die_with_parent

REPO = pathlib.Path(__file__).resolve().parents[2]
SHELL = REPO / "target" / "debug" / "shell"
ACTIONS = "actions"

failures: list[str] = []


def check(label: str, ok: bool, detail: str = "") -> bool:
    print(f"  {'PASS' if ok else 'FAIL'}  {label}" + (f" — {detail}" if detail else ""))
    if not ok:
        failures.append(label)
    return bool(ok)


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


class Session:
    """One real compositor, isolated from the machine and from its peers."""

    def __init__(self, name: str, root: str):
        self.name = name
        self.dir = os.path.join(root, name)
        os.makedirs(self.dir, exist_ok=True)
        self.socket = os.path.join(self.dir, "control.sock")
        env = {
            **os.environ,
            "TILO_TEST_CONTROL": self.socket,
            "TILO_STATE_PATH": os.path.join(self.dir, "config", "state.json"),
            "XDG_DATA_HOME": os.path.join(self.dir, "data"),
            "XDG_CACHE_HOME": os.path.join(self.dir, "cache"),
            "XDG_STATE_HOME": os.path.join(self.dir, "state"),
            "XDG_CONFIG_HOME": os.path.join(self.dir, "config"),
            "XDG_RUNTIME_DIR": os.path.join(self.dir, "run"),
            # Without its own device key a session's peer-sync turns
            # itself off, and the only visible symptom is that
            # `peer.invite` is never exported.
            "TILO_PEER_KEY_PATH": os.path.join(self.dir, "keys", "peer-key"),
            # Distinct identities, or both sessions are the same person
            # and "someone else's cursor" means nothing.
            "TILO_USER_ID": name,
            "TILO_HEADLESS_SIZE": "1280x800",
        }
        env.pop("DISPLAY", None)
        env.pop("WAYLAND_DISPLAY", None)
        os.makedirs(env["XDG_RUNTIME_DIR"], mode=0o700, exist_ok=True)
        self.log_path = os.path.join(self.dir, "shell.log")
        self.log = open(self.log_path, "w")
        self.proc = subprocess.Popen(
            [str(SHELL), "--backend", "headless", "--enable-peer-sync"],
            env=env, stdout=self.log, stderr=subprocess.STDOUT,
            preexec_fn=die_with_parent(),
        )

    def wait_up(self, timeout=45.0) -> bool:
        end = time.time() + timeout
        while time.time() < end:
            if os.path.exists(self.socket):
                time.sleep(0.8)
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

    def pointers(self) -> list:
        """The peer pointers this session's overlay reads from."""
        peers = self.call({"op": "state_dump"})["state"].get("peers") or {}
        return peers.get("pointers") or []

    def key(self, k: str):
        self.call({"op": "key", "key": k})

    def type_text(self, t: str):
        self.call({"op": "type", "text": t})

    def move_to(self, x: float, y: float):
        self.call({"op": "pointer_move", "x": x, "y": y})

    def invoke(self, provider: str, ident: str, args: dict | None = None) -> dict:
        payload = {"op": "actions_invoke", "provider": provider, "id": ident}
        if args is not None:
            payload["args"] = args
        return self.call(payload)

    def enumerate(self, query: str = "") -> list:
        r = self.call({"op": "actions_enumerate", "query": query})
        return r.get("answer", {}).get("actions", []) if "answer" in r else []

    def snapshot(self, path: str) -> dict:
        return self.call({"op": "snapshot", "path": path})

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


def mint_invite(host: Session) -> str | None:
    host.invoke(ACTIONS, "peer.invite")
    token = wait_for(lambda: host.chrome()["peer_sync"]["invite_token"], timeout=25)
    host.key("Escape")
    time.sleep(0.4)
    return token


def join(guest: Session, token: str) -> bool:
    resp = guest.invoke(ACTIONS, "peer.connect")
    if "err" in resp:
        return False
    if not wait_for(lambda: guest.chrome()["peer_sync"]["connect_input"] is not None,
                    timeout=20):
        return False
    guest.type_text(token)
    guest.key("Enter")
    ok = wait_for(
        lambda: (guest.chrome()["peer_sync"]["status"] or "").startswith("Joined"),
        timeout=25)
    guest.key("Escape")
    time.sleep(0.4)
    return bool(ok)


def saturated_near(png: str, near, radius: int = 70) -> int:
    """Count vivid pixels around `near`.

    The desktop is a low-saturation surface; a peer's caret and the
    underline on their name pill are drawn in a palette hue. Counting
    vivid pixels in that neighbourhood makes "it is on screen" a claim
    about the image rather than about the state meant to produce it.
    Pillow is required: without pixel evidence this proof cannot pass.
    """
    try:
        from PIL import Image
    except ImportError as error:
        raise RuntimeError("cursor pixel proof requires Pillow") from error
    im = Image.open(png).convert("RGB")
    w, h = im.size
    px = im.load()
    cx, cy = int(near[0]), int(near[1])
    hits = 0
    for y in range(max(0, cy - radius), min(h, cy + radius)):
        for x in range(max(0, cx - radius), min(w, cx + radius)):
            r, g, b = px[x, y]
            mx, mn = max(r, g, b), min(r, g, b)
            if mx >= 90 and (mx - mn) >= 60:
                hits += 1
    return hits


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", action="store_true", help="leave sessions running")
    ap.add_argument("--out", default=str(REPO / "target" / "desktop-peer-cursor"))
    args = ap.parse_args()

    if not SHELL.exists():
        print(f"FAIL: compositor not built at {SHELL}", file=sys.stderr)
        print("      run: cargo build -p shell --bin shell", file=sys.stderr)
        return 1
    # A stale binary makes every verdict below meaningless, so say how
    # old it is rather than leaving the reader to assume.
    age_min = (time.time() - SHELL.stat().st_mtime) / 60.0
    print(f"compositor built {age_min:.1f} min ago — {SHELL}")

    root = tempfile.mkdtemp(prefix="qs-cur-")
    sessions: list[Session] = []
    try:
        print("\n[1] two real sessions, isolated from this machine and each other")
        ada = Session("ada", root)
        sessions.append(ada)
        bob = Session("bob", root)
        sessions.append(bob)
        for s in sessions:
            if not check(f"{s.name} came up", s.wait_up(),
                         "" if os.path.exists(s.socket) else f"see {s.log_path}"):
                return 1

        print("\n[2] one room, and nothing open in it")
        exported = {f"{a['provider']}:{a['id']}" for a in ada.enumerate("")}
        if not check("peer-sync is on (peer.invite is exported)",
                     f"{ACTIONS}:peer.invite" in exported):
            print(f"        check {ada.log_path} for 'device key load failed'")
            return 1
        token = mint_invite(ada)
        if not check("ada minted an invite", bool(token)):
            return 1
        if not check("bob joined the room", join(bob, token)):
            return 1

        # Nobody opens anything. This is the beat: two people who have
        # just joined, looking at a desktop with nothing on it.
        print("\n[3] ada moves his pointer across the empty desktop")
        target = (760.0, 470.0)
        for i in range(24):
            t = (i + 1) / 24.0
            ada.move_to(300.0 + (target[0] - 300.0) * t,
                            240.0 + (target[1] - 240.0) * t)
            time.sleep(0.06)
        ada.move_to(*target)
        time.sleep(0.6)

        print("\n[4] bob's session is asked who it is about to paint")
        seen = wait_for(lambda: bob.pointers() or None, timeout=45)
        check("bob's overlay has a peer pointer to paint", bool(seen),
              json.dumps(seen)[:200] if seen else "NO PEER after 45s")
        if seen:
            check("and it is where ada pointed",
                  any(abs(p.get("x", 0) - target[0]) < 40
                      and abs(p.get("y", 0) - target[1]) < 40
                      for p in seen),
                  json.dumps(seen)[:200])
            # Who the pill says it is. The name is a governance fact —
            # written into the room document by the room's authority and
            # refused from anyone else — so this is a claim about what
            # the room says, not about what the sending session claimed
            # about itself. ada hosts this room, so the room names
            # him when he mints the invite.
            here = [p for p in seen
                    if abs(p.get("x", 0) - target[0]) < 40
                    and abs(p.get("y", 0) - target[1]) < 40]
            check("and the room says it is ada",
                  any(p.get("name") == "ada" for p in here),
                  json.dumps(here)[:200])
            check("labelled by the room, not by a raw handle",
                  all(not str(p.get("name", "")).startswith("peer@")
                      for p in here),
                  json.dumps(here)[:200])

        print("\n[5] and the picture it painted")
        os.makedirs(args.out, exist_ok=True)
        shot = os.path.join(args.out, "bob-sees-adas-cursor.png")
        bob.snapshot(shot)
        ok = wait_for(lambda: os.path.exists(shot) and os.path.getsize(shot) > 5000,
                      timeout=20)
        hits = -1
        if check("a frame was captured from bob's session", bool(ok),
                 f"{os.path.getsize(shot)} bytes" if ok else "no PNG"):
            hits = saturated_near(shot, target)
            check("ada's cursor is drawn where he pointed", hits > 40,
                  f"{hits} vivid px within 70 of {target}")
            print(f"        {shot}")

        # The control. Without it, a check that passes on a merely
        # colourful desktop looks the same as one that passes on a peer.
        print("\n[6] control — the same spot once he points elsewhere")
        alone = os.path.join(args.out, "control-peer-moved-away.png")
        ada.move_to(120.0, 180.0)
        time.sleep(0.8)
        bob.snapshot(alone)
        control_ok = wait_for(
            lambda: os.path.exists(alone) and os.path.getsize(alone) > 5000,
            timeout=20)
        if check("the moved-away control frame was captured", bool(control_ok),
                 alone) and hits >= 0:
            base = saturated_near(alone, target)
            check("the spot he left is bare", base < 40,
                  f"{base} vivid px there with the peer moved away")
            print(f"        {alone}")

        print()
        if failures:
            print(f"FAILED: {len(failures)} claim(s): {', '.join(failures)}")
            return 1
        print("A peer's pointer crossed the desktop and was drawn on the "
              "other machine, with nothing open on either.")
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
