#!/usr/bin/env python3
"""Two people invited before either has arrived, and both keep their name.

A host who invites one person, waits for them, then invites the next has
always worked. Inviting two back to back did not: the second invitation
overwrote the first, whoever knocked first took the surviving name, and
the person who came after was drawn as `peer@12191268537764683811` — a
number that says a machine is here without saying who is here.

This runs three real compositors with separate identities, separate homes
and separate device keys. quinten invites Xaver from the panel, then
invites Claude from the same panel **before Xaver has redeemed
anything**, and then Claude joins first. The claim is read off quinten's
own roster afterwards: two rows, called Xaver and Claude, and no row
carrying the `peer@` fallback.

Both invitations are minted through the composer a person actually
drives — Invite, type the name, Create invite — so a control that is not
painted, not hit-testable or not wired fails here rather than being
routed around. Neither name ever crosses the wire: they are held on
quinten's machine against each invitation until it is redeemed, and
written into the room document by the room's own authority.

Usage:
  ./two-invites-at-once-proof.py [--keep] [--out DIR]

Exits non-zero if any claim fails. A "GAP" is something this run could
not show, reported rather than glossed; it does not fail the run.
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

#: What quinten types for each of them, in the order he types it. The
#: whole run is about these two strings surviving being asked for
#: together.
FIRST_INVITED = "Xaver"
SECOND_INVITED = "Claude"

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
        except Exception:  # noqa: BLE001
            got = None
        if got:
            return got
        time.sleep(poll)
    return None


class Session:
    """One real compositor, isolated from the machine and from its peers."""

    def __init__(self, name: str, root: str, runtime_root: str):
        self.name = name
        self.dir = os.path.join(root, name)
        os.makedirs(self.dir, exist_ok=True)
        # `sun_path` is 108 bytes and both sockets live under a directory
        # named here, so a run rooted somewhere verbose fails at bind
        # with an error about the path rather than about the length.
        self.socket = os.path.join(runtime_root, f"{name[0]}.sock")
        self.collab_sock = os.path.join(runtime_root, f"{name[0]}c.sock")
        self.runtime = os.path.join(runtime_root, f"r{name[0]}")
        os.makedirs(self.runtime, mode=0o700, exist_ok=True)
        self.home = os.path.join(self.dir, "home")
        os.makedirs(self.home, exist_ok=True)
        env = {
            **os.environ,
            "QOS_TEST_CONTROL": self.socket,
            "QOS_STATE_PATH": os.path.join(self.dir, "config", "state.json"),
            "HOME": self.home,
            "XDG_DATA_HOME": os.path.join(self.dir, "data"),
            "XDG_CACHE_HOME": os.path.join(self.dir, "cache"),
            "XDG_STATE_HOME": os.path.join(self.dir, "state"),
            "XDG_CONFIG_HOME": os.path.join(self.dir, "config"),
            "XDG_RUNTIME_DIR": self.runtime,
            # Without its own device key a session's peer-sync turns
            # itself off, and the only symptom is that `peer.invite` is
            # never exported — which reads as a refusal rather than a
            # missing prerequisite.
            "QOS_PEER_KEY_PATH": os.path.join(self.dir, "keys", "peer-key"),
            "QOS_USER_ID": name,
            "QUICK_COLLAB_SOCK": self.collab_sock,
            "NIRIOS_HEADLESS_SIZE": "1600x1000",
            "LC_ALL": "en_US.UTF-8",
            "LANG": "en_US.UTF-8",
        }
        env.pop("DISPLAY", None)
        env.pop("WAYLAND_DISPLAY", None)
        self.env = env
        self.log_path = os.path.join(self.dir, "shell.log")
        self.log = open(self.log_path, "w")
        self.proc = subprocess.Popen(
            [str(SHELL), "--mode", "native", "--backend", "headless",
             "--enable-peer-sync"],
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

    def peer_sync(self) -> dict:
        return self.chrome()["peer_sync"]

    def active_workspace(self) -> dict:
        tree = self.state()["workspace_tree"]
        active = tree.get("active")
        for ws in tree.get("workspaces", []):
            if ws.get("id") == active:
                return ws
        return {}

    def room(self):
        return self.active_workspace().get("room")

    def roster(self) -> list:
        """Who the room's people panel is listing, off the active
        workspace — the same list the panel paints."""
        return self.active_workspace().get("collaborators") or []

    def named_rect(self, name: str):
        for target in self.call({"op": "list_targets"}).get("targets", []):
            if target.get("name") == name:
                return target.get("rect")
        return None

    def click_named(self, name: str) -> dict:
        return self.call({"op": "click_named", "name": name})

    def type_text(self, text: str) -> None:
        self.call({"op": "type", "text": text})

    def key(self, k: str) -> None:
        self.call({"op": "key", "key": k})

    def invoke(self, ident: str, args: dict | None = None) -> dict:
        payload = {"op": "actions_invoke", "provider": "actions", "id": ident}
        if args is not None:
            payload["args"] = args
        return self.call(payload)

    def snapshot(self, path: str) -> dict:
        return self.call({"op": "snapshot", "path": path}, timeout=180)

    def settle(self, seconds: float = 0.6) -> None:
        time.sleep(seconds)

    def stop(self) -> None:
        try:
            self.proc.terminate()
            self.proc.wait(timeout=10)
        except Exception:  # noqa: BLE001
            self.proc.kill()
        try:
            self.log.close()
        except Exception:  # noqa: BLE001
            pass


def open_collaborators(host: Session) -> bool:
    """Raise the room's people, the way a person does."""
    if host.chrome()["overlays"].get("collaborators"):
        return True
    host.invoke("collaborators.open")
    host.settle(0.8)
    return bool(host.chrome()["overlays"].get("collaborators"))


def dismiss_card(host: Session) -> None:
    host.key("Escape")
    host.settle(0.5)


def invite_from_the_panel(host: Session, name: str) -> str | None:
    """Invite somebody by name, through the composer, with the mouse.

    Every step is a click on a painted control, so a control that is not
    drawn, not hit-testable or not wired fails here instead of being
    routed around.
    """
    if not open_collaborators(host):
        gap(f"the collaborators panel would not open, so {name} could not "
            f"be invited from it")
        return None
    if host.named_rect("collab_invite") is None:
        gap("the panel painted no Invite button")
        return None
    if "err" in host.click_named("collab_invite"):
        gap("the panel's Invite button would not take a click")
        return None
    host.settle(0.8)
    asked = wait_for(lambda: host.peer_sync()["invite_name"] is not None, timeout=15)
    if not check(f"the card asks who the invitation for {name!r} is for", bool(asked), True):
        dismiss_card(host)
        return None
    check("and nothing was minted before it was answered",
          host.peer_sync()["invite_token"] is None, True)
    host.type_text(name)
    host.settle(0.5)
    check(f"the field took the name {name!r}",
          host.peer_sync()["invite_name"] == name, True,
          f"holds {host.peer_sync()['invite_name']!r}")
    if "err" in host.click_named("peer_sync_create"):
        gap("the composer's Create invite button would not take a click")
        dismiss_card(host)
        return None
    token = wait_for(lambda: host.peer_sync()["invite_token"], timeout=30)
    if not check(f"a token was minted for {name!r}", bool(token), True):
        dismiss_card(host)
        return None
    check(f"and the card says whose it is — {name!r}",
          host.peer_sync()["invite_for"] == name, True,
          f"says {host.peer_sync()['invite_for']!r}")
    dismiss_card(host)
    return token


def join_with(guest: Session, token: str) -> bool:
    """Redeem a token through `peer.join` — the address a participant
    with no pointer has instead of a mouse."""
    resp = guest.invoke("peer.join", {"token": token})
    if "err" in resp:
        gap(f"{guest.name}: peer.join refused — {json.dumps(resp['err'])[:160]}")
        return False
    return bool(wait_for(lambda: guest.room(), timeout=20))


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", action="store_true",
                    help="leave the run directory behind")
    ap.add_argument("--out", default=None, help="where to write shots")
    args = ap.parse_args()

    if not SHELL.exists():
        print(f"missing {SHELL} — build it with `cargo build -p shell`")
        return 2

    root = tempfile.mkdtemp(prefix="qos-two-invites-")
    runtime_root = tempfile.mkdtemp(prefix="qtwo-", dir="/tmp")
    out = args.out or os.path.join(root, "shots")
    os.makedirs(out, exist_ok=True)

    quinten = Session("quinten", root, runtime_root)
    xaver = Session("xaver", root, runtime_root)
    claude = Session("claude", root, runtime_root)
    sessions = [quinten, xaver, claude]

    try:
        step("three sessions come up")
        for s in sessions:
            if not check(f"{s.name}'s compositor is up", s.wait_up(), True):
                return 1

        room = wait_for(lambda: quinten.room(), timeout=20)
        check("quinten's workspace has a room to invite anyone into",
              bool(room), True, str(room))

        # ---- the two invitations, back to back ---------------------
        step(f"quinten invites {FIRST_INVITED}")
        first_token = invite_from_the_panel(quinten, FIRST_INVITED)
        if not first_token:
            return 1

        step(f"and invites {SECOND_INVITED} before {FIRST_INVITED} has redeemed anything")
        # The precondition the whole run rests on. If the first guest had
        # already arrived this would be the serialised case that always
        # worked, and the run would prove nothing.
        check(f"{FIRST_INVITED} has not arrived yet",
              len(quinten.roster()) == 0, True,
              f"roster holds {[c.get('name') for c in quinten.roster()]}")
        second_token = invite_from_the_panel(quinten, SECOND_INVITED)
        if not second_token:
            return 1
        check("the two invitations are different tokens",
              first_token != second_token, True)

        # ---- redeemed in the order that used to break it -----------
        step(f"{SECOND_INVITED} — invited second — joins first")
        if not check(f"{SECOND_INVITED} joined the room",
                     join_with(claude, second_token), True):
            return 1

        step(f"and only then does {FIRST_INVITED} join")
        if not check(f"{FIRST_INVITED} joined the room",
                     join_with(xaver, first_token), True):
            return 1

        # ---- what quinten's own screen says ------------------------
        step("quinten's roster is read back")
        open_collaborators(quinten)
        names = wait_for(
            lambda: (sorted(c.get("name", "") for c in quinten.roster())
                     if len(quinten.roster()) >= 2 else None),
            timeout=60)
        roster = quinten.roster()
        print(f"  roster: {json.dumps(roster)}")
        if names is None:
            gap("both guests did not reach quinten's roster inside the "
                "timeout, so the names could not be read off it")
            return 1

        check("both people are in the room", len(roster) == 2, True,
              f"{len(roster)} rows")
        # The symptom, asserted on directly: a row carrying the fallback
        # is somebody the host typed a name for showing up as a number.
        fallback = [c.get("name") for c in roster
                    if str(c.get("name", "")).startswith("peer@")]
        check("nobody the host named is on screen as a bare handle",
              not fallback, True, f"fell back to {fallback}")
        check("and the two rows are the two names quinten typed",
              names == sorted([FIRST_INVITED, SECOND_INVITED]), True,
              f"reads {names}")

        shot = os.path.join(out, "both-invited-both-named.png")
        try:
            quinten.snapshot(shot)
            print(f"  shot: {shot}")
        except Exception as exc:  # noqa: BLE001
            gap(f"could not read quinten's screen back ({exc})")
    finally:
        for s in sessions:
            s.stop()
        if not args.keep:
            shutil.rmtree(root, ignore_errors=True)
            shutil.rmtree(runtime_root, ignore_errors=True)
        else:
            print(f"\nrun directory kept at {root}")

    print()
    if gaps:
        print(f"{len(gaps)} gap(s): " + "; ".join(gaps))
    if failures:
        print(f"FAILED: {len(failures)} — " + "; ".join(failures))
        return 1
    print("PASS — two invitations out at once, redeemed out of order, "
          "and both people are called what they were invited as")
    return 0


if __name__ == "__main__":
    sys.exit(main())
