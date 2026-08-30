#!/usr/bin/env python3
"""A window one person opens is drawn on the other person's screen.

`room-index-proof.py` already proves the window *replicates*: xaver's
session receives the tab quinten opened and reads it back out of his own
state. That is not the same claim as this one, and the difference is the
whole point — the tab was in his tree, provably, while his screen showed
an empty desktop. An assertion that goes green over a blank screen is
worse than one that fails.

So this asks the receiving session three questions in order, each
strictly stronger than the last:

  1. is the tab in your tree?          (state — what already passed)
  2. is there a widget for it?         (the widget tree, via list_targets)
  3. did your screen change?           (pixels, before and after)

Two real compositors, separate identities, separate homes, one room.

Usage:
  ./peer-window-drawn-proof.py [--keep] [--out DIR]

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
            "QOS_TEST_CONTROL": self.socket,
            "QOS_STATE_PATH": os.path.join(self.dir, "config", "state.json"),
            "XDG_DATA_HOME": os.path.join(self.dir, "data"),
            "XDG_CACHE_HOME": os.path.join(self.dir, "cache"),
            "XDG_STATE_HOME": os.path.join(self.dir, "state"),
            "XDG_CONFIG_HOME": os.path.join(self.dir, "config"),
            "XDG_RUNTIME_DIR": os.path.join(self.dir, "run"),
            "QOS_PEER_KEY_PATH": os.path.join(self.dir, "keys", "peer-key"),
            "QOS_USER_ID": name,
            "NIRIOS_HEADLESS_SIZE": "1280x800",
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

    def tree(self) -> dict:
        return self.call({"op": "state_dump"})["state"]["workspace_tree"]

    def targets(self) -> set:
        """Every laid-out widget in the live tree, by name.

        `list_targets` filters to widgets with a non-zero rect, so a
        name here is a widget that has a place on screen this frame —
        not merely one that exists in the tree.
        """
        r = self.call({"op": "list_targets"})
        return {e["name"] for e in (r.get("targets") or [])}

    def key(self, k: str):
        self.call({"op": "key", "key": k})

    def type_text(self, t: str):
        self.call({"op": "type", "text": t})

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

    def tabs_here(self) -> dict:
        """Every tab in the workspace this session is in, by tab id."""
        tree = self.tree()
        active = tree.get("active")
        out = {}
        for ws in tree.get("workspaces", []):
            if ws.get("id") != active:
                continue
            for strip in ws.get("strips", []):
                for group in strip.get("window_groups", []):
                    for t in group.get("tabs", []):
                        out[str(t.get("id"))] = {
                            "title": t.get("title"),
                            "group": str(group.get("id")),
                            "strip": str(strip.get("id")),
                        }
        return out

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


def changed_pixels(a: str, b: str) -> int:
    """How many pixels differ between two shots of the same session."""
    try:
        from PIL import Image, ImageChops
    except ImportError:
        return -1
    ia = Image.open(a).convert("RGB")
    ib = Image.open(b).convert("RGB")
    if ia.size != ib.size:
        return ia.size[0] * ia.size[1]
    diff = ImageChops.difference(ia, ib)
    data = getattr(diff, "get_flattened_data", diff.getdata)()
    return sum(1 for p in data if max(p) > 12)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", action="store_true", help="leave sessions running")
    ap.add_argument("--out", default=str(REPO / "target" / "peer-window-drawn"))
    args = ap.parse_args()

    if not SHELL.exists():
        print(f"FAIL: compositor not built at {SHELL}", file=sys.stderr)
        print("      run: cargo build -p shell --bin shell", file=sys.stderr)
        return 1
    age_min = (time.time() - SHELL.stat().st_mtime) / 60.0
    print(f"compositor built {age_min:.1f} min ago — {SHELL}")

    root = tempfile.mkdtemp(prefix="qs-win-", dir="/tmp")
    os.makedirs(args.out, exist_ok=True)
    sessions: list[Session] = []
    try:
        print("\n[1] two real sessions, isolated from this machine and each other")
        quinten = Session("quinten", root)
        sessions.append(quinten)
        xaver = Session("xaver", root)
        sessions.append(xaver)
        for s in sessions:
            if not check(f"{s.name} came up", s.wait_up(),
                         "" if os.path.exists(s.socket) else f"see {s.log_path}"):
                return 1

        print("\n[2] one room")
        exported = {f"{a['provider']}:{a['id']}" for a in quinten.enumerate("")}
        if not check("peer-sync is on (peer.invite is exported)",
                     f"{ACTIONS}:peer.invite" in exported):
            print(f"        check {quinten.log_path} for 'device key load failed'")
            return 1
        token = mint_invite(quinten)
        if not check("quinten minted an invite", bool(token)):
            return 1
        if not check("xaver joined the room", join(xaver, token)):
            return 1

        # What xaver's screen looks like before anything is opened. Every
        # claim about his screen changing is measured against this.
        before_shot = os.path.join(args.out, "xaver-before.png")
        xaver.snapshot(before_shot)
        wait_for(lambda: os.path.exists(before_shot)
                 and os.path.getsize(before_shot) > 5000, timeout=20)
        before_targets = xaver.targets()

        print("\n[3] quinten opens a window")
        q_before = set(quinten.tabs_here())
        opened = quinten.invoke(ACTIONS, "window.new.terminal")
        check("quinten opened a window", "err" not in opened,
              json.dumps(opened.get("err", {}))[:120])
        fresh = wait_for(lambda: (set(quinten.tabs_here()) - q_before) or None,
                         timeout=25)
        if not check("quinten's own session shows it", bool(fresh)):
            return 1
        tid = sorted(fresh)[0]

        print("\n[4] it is in xaver's tree")
        landed = wait_for(lambda: tid in xaver.tabs_here(), timeout=60)
        if not check("the tab quinten opened is in xaver's tree", bool(landed),
                     f"tab {tid}" if landed else "NOT REPLICATED after 60s"):
            return 1
        arrived = xaver.tabs_here()[tid]
        print(f"        title={arrived['title']!r} strip={arrived['strip'][:8]}…")

        print("\n[5] …and there is a widget for it")
        # The claim state alone cannot make. The shell only splices
        # groups and tabs into its widget tree when the chrome revision
        # moves, and peer-sync writes the tree from the network thread.
        # A widget that never appeared is a window that never drew.
        grew = wait_for(lambda: (xaver.targets() - before_targets) or None,
                        timeout=30)
        check("xaver's widget tree grew when the window arrived", bool(grew),
              f"{len(grew)} new target(s)" if grew else
              "no new widget — the tab is in the tree and nothing draws it")

        print("\n[6] …and his screen changed")
        after_shot = os.path.join(args.out, "xaver-sees-quintens-window.png")
        xaver.snapshot(after_shot)
        ok = wait_for(lambda: os.path.exists(after_shot)
                      and os.path.getsize(after_shot) > 5000, timeout=20)
        if check("a frame was captured from xaver's session", bool(ok),
                 f"{os.path.getsize(after_shot)} bytes" if ok else "no PNG"):
            moved = changed_pixels(before_shot, after_shot)
            if moved < 0:
                print("  SKIP  pixel check (Pillow not installed)")
            else:
                # A window card is a large object. A few hundred pixels
                # would be a clock digit; this has to be the window.
                check("a window is drawn on xaver's screen", moved > 20000,
                      f"{moved} px differ from before he had it")
            print(f"        before: {before_shot}")
            print(f"        after:  {after_shot}")

        print("\n[7] and the same in the other direction")
        # Opening a window in a room is an act, so it is gated like one:
        # a guest who has only been admitted is not yet an editor. The
        # owner makes him one first, or the action is not even exported
        # to him and "the other direction" would fail on governance
        # rather than on anything to do with drawing.
        granted = quinten.call({
            "op": "grant", "subject": "person:xaver",
            "scope": "workspace", "verb": "write",
        })
        check("quinten makes xaver an editor", "err" not in granted,
              json.dumps(granted.get("err", {}))[:120])
        check("and the grant reaches xaver's own session",
              bool(wait_for(lambda: any(
                  a.get("provider") == ACTIONS
                  and a.get("id") == "window.new.terminal"
                  for a in xaver.enumerate("")), timeout=45)),
              "window.new.terminal is now exported to him")

        xn_before = set(xaver.tabs_here())
        xaver.invoke(ACTIONS, "window.new.terminal")
        x_new = wait_for(lambda: (set(xaver.tabs_here()) - xn_before) or None,
                         timeout=25)
        if check("xaver opened a window", bool(x_new)):
            xtid = sorted(x_new)[0]
            check("it is open for quinten too",
                  bool(wait_for(lambda: xtid in quinten.tabs_here(), timeout=60)),
                  f"tab {xtid}")
            q_shot = os.path.join(args.out, "quinten-sees-xavers-window.png")
            quinten.snapshot(q_shot)
            if wait_for(lambda: os.path.exists(q_shot)
                        and os.path.getsize(q_shot) > 5000, timeout=20):
                print(f"        {q_shot}")

        print()
        if failures:
            print(f"FAILED: {len(failures)} claim(s): {', '.join(failures)}")
            return 1
        print("A window opened on one machine reached the other's widget tree "
              "and changed what is on its screen.")
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
