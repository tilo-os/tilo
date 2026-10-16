#!/usr/bin/env python3
"""Presence crosses between two live sessions, in both directions.

The one-way rig checks that a guest can see the host. This covers only one
direction: a host and a guest are not symmetric. The guest joined the
host's room, so the host's pointer goes out under a room the guest is a
member of. The reverse only holds if joining also moved the guest *into*
that room as the workspace it is looking at — and if it did not, the
guest's pointer is published under a room the host has never heard of,
dropped on arrival, and the host watches an empty desktop while the
guest sees a cursor and everyone assumes it works.

This checks both directions across two compositors:

  - the guest paints the host's cursor, and
  - the host paints the guest's, and
  - both are still there seconds after either pointer stopped, and
  - both carry the name the room gave them, not a raw handle, and
  - both can be found in the store by the person's name, and
  - neither session paints anybody at the origin.

Every claim is read from the other session's own presence store — the
one the overlay paints from — and both ends are photographed.

Usage:
  ./peers-cross-both-ways.py [--keep] [--out DIR]

Exits non-zero if any claim fails.
"""
import argparse
import base64
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
    """One real compositor, isolated from the machine and from its peer."""

    def __init__(self, name: str, root: str, runtime_root: str):
        self.name = name
        self.dir = os.path.join(root, name)
        os.makedirs(self.dir, exist_ok=True)
        # `sun_path` is 108 bytes, and both the control socket and the
        # Wayland socket live under this — a verbose root fails at bind
        # with an error about the path rather than about its length.
        self.socket = os.path.join(runtime_root, f"{name[0]}.sock")
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
            # Without its own device key peer-sync turns itself off, and
            # the only visible symptom is that `peer.invite` is never
            # exported — which reads as a refusal, not a prerequisite.
            "TILO_PEER_KEY_PATH": os.path.join(self.dir, "keys", "peer-key"),
            # Distinct identities, or both sessions are the same person
            # and "somebody else's cursor" means nothing.
            "TILO_USER_ID": name,
            "TILO_HEADLESS_SIZE": "1280x800",
            "LC_ALL": "en_US.UTF-8",
            "LANG": "en_US.UTF-8",
        }
        env.pop("DISPLAY", None)
        env.pop("WAYLAND_DISPLAY", None)
        self.log_path = os.path.join(self.dir, "shell.log")
        self.log = open(self.log_path, "w")
        self.proc = subprocess.Popen(
            # `--mode native` named rather than defaulted: it is the
            # smithay compositor and the only mode that attaches a
            # presence relay at all, so leaving it to the default would
            # make this rig's verdict depend on what the default is.
            [str(SHELL), "--mode", "native", "--backend", "headless",
             "--enable-peer-sync"],
            env=env, stdout=self.log, stderr=subprocess.STDOUT,
            preexec_fn=die_with_parent(),
        )

    def wait_up(self, timeout=60.0) -> bool:
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

    def state(self) -> dict:
        return self.call({"op": "state_dump"})["state"]

    def chrome(self) -> dict:
        return self.state()["chrome"]

    def pointers(self) -> list:
        return (self.state().get("peers") or {}).get("pointers") or []

    def tree(self) -> dict:
        return self.state()["workspace_tree"]

    def rooms(self) -> list:
        return [w.get("room") for w in (self.tree().get("workspaces") or [])]

    def active_room(self):
        tree = self.tree()
        for w in tree.get("workspaces") or []:
            if w.get("id") == tree.get("active"):
                return w.get("room")
        return None

    def key(self, k: str):
        self.call({"op": "key", "key": k})

    def type_text(self, t: str):
        self.call({"op": "type", "text": t})

    def move_to(self, x: float, y: float):
        self.call({"op": "pointer_move", "x": x, "y": y})

    def invoke(self, ident: str, args: dict | None = None) -> dict:
        payload = {"op": "actions_invoke", "provider": ACTIONS, "id": ident}
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
    host.invoke("peer.invite")
    token = wait_for(lambda: host.chrome()["peer_sync"]["invite_token"], timeout=25)
    host.key("Escape")
    time.sleep(0.4)
    return token


def peer_id_of(token: str) -> str | None:
    """The peer id inside a session's own invite.

    A token names the machine that issued it, so a session's invite is
    also how that session says who it is — which is what lets one person
    hand another the identity they are about to be invited under.
    """
    try:
        pad = "=" * (-len(token) % 4)
        blob = json.loads(base64.urlsafe_b64decode(token + pad).decode())
        return blob.get("owner_peer_id") or None
    except Exception:
        return None


def mint_named_invite(host: Session, peer: str, name: str) -> str | None:
    """Invite a peer *by name*.

    A plain `peer.invite` says nothing about who its token is for, so
    the room learns only the host's own name and attribution resolves
    in one direction: the guest sees a name and the host sees a handle.
    Naming the invitee is what makes the room able to attribute both
    cursors to people.
    """
    resp = host.invoke("peer.invite.named", {"peer": peer, "name": name})
    if "err" in resp:
        print(f"        peer.invite.named refused: {json.dumps(resp['err'])[:160]}")
        return None
    token = wait_for(lambda: host.chrome()["peer_sync"]["invite_token"], timeout=30)
    host.key("Escape")
    time.sleep(0.4)
    return token


def join(guest: Session, token: str) -> bool:
    resp = guest.invoke("peer.connect")
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


def drift_to(s: Session, x: float, y: float, frm=(300.0, 240.0), steps=16):
    """Move like a hand, not like a teleport.

    The publisher is level-triggered and throttled, so a single jump
    lands one sample. A drift produces the stream a real pointer does,
    and exercises the throttle on the way.
    """
    for i in range(steps):
        t = (i + 1) / steps
        s.move_to(frm[0] + (x - frm[0]) * t, frm[1] + (y - frm[1]) * t)
        time.sleep(0.05)
    s.move_to(x, y)


def near(rows: list, at, tol=45.0) -> list:
    return [r for r in rows
            if abs(float(r.get("x", 1e9)) - at[0]) < tol
            and abs(float(r.get("y", 1e9)) - at[1]) < tol]


def saturated_near(png: str, at, radius: int = 70) -> int:
    """Count vivid pixels near `at`; Pillow is required for pixel evidence.

    The desktop is a low-saturation surface and a peer's caret and name
    pill are drawn in a palette hue, so this makes "it is on screen" a
    claim about the image rather than about the state meant to produce
    one.
    """
    try:
        from PIL import Image
    except ImportError as error:
        raise RuntimeError("cursor pixel proof requires Pillow") from error
    im = Image.open(png).convert("RGB")
    w, h = im.size
    px = im.load()
    cx, cy = int(at[0]), int(at[1])
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
    ap.add_argument("--out", default=str(REPO / "target" / "peers-both-ways"))
    args = ap.parse_args()

    if not SHELL.exists():
        print(f"FAIL: compositor not built at {SHELL}", file=sys.stderr)
        print("      run: cargo build -p shell --bin shell", file=sys.stderr)
        return 1
    # A stale binary produces a verdict about code that is not under
    # test, and produces it silently, so say how old it is.
    print(f"compositor built {(time.time() - SHELL.stat().st_mtime) / 60:.1f} min "
          f"ago — {SHELL}")

    root = tempfile.mkdtemp(prefix="qs-both-", dir="/tmp")
    runtime_root = tempfile.mkdtemp(prefix="qs-", dir="/tmp")
    sessions: list[Session] = []
    try:
        print("\n[1] two real sessions")
        quinten = Session("quinten", root, runtime_root)
        sessions.append(quinten)
        xaver = Session("xaver", root, runtime_root)
        sessions.append(xaver)
        for s in sessions:
            if not check(f"{s.name} came up", s.wait_up(), f"see {s.log_path}"):
                return 1

        print("\n[2] one room, nothing open in it")
        exported = {f"{a['provider']}:{a['id']}" for a in quinten.enumerate("")}
        if not check("peer-sync is on", f"{ACTIONS}:peer.invite" in exported):
            print(f"        check {quinten.log_path} for 'device key load failed'")
            return 1
        # Named, both ways. An unnamed invite leaves the room knowing
        # only the host's name, and then attribution resolves in one
        # direction — the guest sees a person and the host sees a
        # handle. The invitee's own invite is how it says who it is.
        own = mint_invite(xaver)
        xaver_peer = peer_id_of(own or "")
        if not check("xaver's peer id is known", bool(xaver_peer)):
            return 1
        token = mint_named_invite(quinten, xaver_peer, "xaver")
        if not check("quinten minted an invite naming xaver", bool(token)):
            return 1
        if not check("xaver joined the room", join(xaver, token)):
            return 1

        host_room = quinten.active_room()
        guest_room = xaver.active_room()
        print(f"        quinten is looking at room {host_room}")
        print(f"        xaver   is looking at room {guest_room}")
        print(f"        quinten holds rooms {quinten.rooms()}")
        print(f"        xaver   holds rooms {xaver.rooms()}")
        # The asymmetry that makes the reverse direction a separate
        # question. A session publishes its pointer under the room it is
        # *looking at*, and folds in only rooms it is a member of, so if
        # the guest is still looking at a private room of its own its
        # cursor is addressed to a room the host has never heard of.
        # Asserted on a room that was actually read, or "both are
        # looking at None" would pass while nothing was being compared.
        check("both are looking at one real room",
              bool(host_room) and host_room == guest_room,
              f"{host_room} vs {guest_room}")

        print("\n[3] both move, on an empty desktop")
        WHERE_Q = (880.0, 520.0)
        WHERE_X = (420.0, 300.0)
        drift_to(quinten, *WHERE_Q)
        drift_to(xaver, *WHERE_X, frm=(700.0, 600.0))
        time.sleep(1.0)

        print("\n[4] the guest paints the host")
        on_xaver = wait_for(lambda: near(xaver.pointers(), WHERE_Q) or None, timeout=40)
        check("xaver holds quinten's cursor where quinten put it", bool(on_xaver),
              json.dumps(xaver.pointers())[:300])
        if on_xaver:
            check("and the room names him", on_xaver[0].get("name") == "quinten",
                  json.dumps(on_xaver[0]))
            check("not a raw handle",
                  not str(on_xaver[0].get("name", "")).startswith("peer@"),
                  json.dumps(on_xaver[0]))

        print("\n[5] and the host paints the guest — the other way")
        on_quinten = wait_for(lambda: near(quinten.pointers(), WHERE_X) or None,
                              timeout=40)
        check("quinten holds xaver's cursor where xaver put it", bool(on_quinten),
              json.dumps(quinten.pointers())[:300])
        if on_quinten:
            check("and the room names him", on_quinten[0].get("name") == "xaver",
                  json.dumps(on_quinten[0]))
            check("not a raw handle",
                  not str(on_quinten[0].get("name", "")).startswith("peer@"),
                  json.dumps(on_quinten[0]))

        print("\n[6] and they stay there with nobody moving")
        # An edge-triggered cursor looks identical to this one for a
        # second and then leaves an empty desktop behind. Nothing is
        # moved here on purpose.
        time.sleep(3.5)
        check("quinten's cursor is still on xaver's screen 3s after it stopped",
              bool(near(xaver.pointers(), WHERE_Q)),
              json.dumps(xaver.pointers())[:300])
        check("xaver's cursor is still on quinten's screen 3s after it stopped",
              bool(near(quinten.pointers(), WHERE_X)),
              json.dumps(quinten.pointers())[:300])

        print("\n[7] a person can be found by their name")
        # The question every reader of this store actually asks: "which
        # of these rows is quinten's". A row carries two things — the
        # identity, which is the handle the frames arrive under, and the
        # name, which is what the room calls that handle — and only one
        # of them is a person. A harness that looks a person up in the
        # other field finds nothing while the cursor is on screen,
        # correctly drawn and correctly named, and reports that presence
        # never crossed. That has happened, so it is asserted here.
        for watcher, rows, who in ((xaver, xaver.pointers(), "quinten"),
                                   (quinten, quinten.pointers(), "xaver")):
            hit = [r for r in rows if who in (r.get("name"), r.get("user"))]
            check(f"{watcher.name}'s store can be asked for {who} by name",
                  bool(hit), json.dumps(rows)[:300])

        print("\n[8] nobody is painted at the origin")
        for s, rows in (("xaver", xaver.pointers()), ("quinten", quinten.pointers())):
            ghosts = [r for r in rows
                      if abs(float(r.get("x", 9))) < 1 and abs(float(r.get("y", 9))) < 1]
            check(f"{s}'s screen has no cursor at the origin", not ghosts,
                  json.dumps(ghosts)[:200])

        print("\n[9] the pictures")
        os.makedirs(args.out, exist_ok=True)
        for s, at, other in ((xaver, WHERE_Q, "quinten"), (quinten, WHERE_X, "xaver")):
            p = os.path.join(args.out, f"{s.name}-sees-{other}.png")
            s.snapshot(p)
            ok = wait_for(lambda p=p: os.path.exists(p) and os.path.getsize(p) > 5000,
                          timeout=20)
            if check(f"a frame was captured from {s.name}'s session", bool(ok)):
                hits = saturated_near(p, at)
                check(f"{other}'s cursor is drawn on {s.name}'s screen", hits > 40,
                      f"{hits} vivid px within 70 of {at}")
                print(f"        {p}")

        print()
        if failures:
            print(f"FAILED: {len(failures)} claim(s): {', '.join(failures)}")
            return 1
        print("Two sessions, each painting the other, both named by the room.")
        return 0
    finally:
        if args.keep:
            print(f"\nsessions left running under {root}")
        else:
            for s in sessions:
                s.stop()
            shutil.rmtree(root, ignore_errors=True)
            shutil.rmtree(runtime_root, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
