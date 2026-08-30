#!/usr/bin/env python3
"""Inviting and joining are things you can *click*.

The room's people live behind the top bar's avatar stack, and the two
acts that change who is in the room — inviting somebody, joining
somebody else — lived only in the command palette. So the whole
collaboration story was reachable only by a person who already knew the
name of a command, and on screen it happened in under a second with no
control ever appearing. Two separate holes made that unavoidable:

  * with an empty roster the avatar stack published `Rect::ZERO`, so at
    the one moment you want to invite the *first* person there was
    nothing to click; and
  * the panel behind it listed participants and offered no Invite and
    no Join.

This drives both halves with the mouse, on two real compositors with
separate identities, homes and device keys:

  quinten   clicks the top bar (empty roster)  →  panel
            clicks **Invite**                  →  card: "who are you
                                                  inviting?"
            types "Xaver", clicks **Create**   →  card: "Copied"

  (the hop between machines is the person's — a chat message, an email,
   a USB stick. What travels here is taken out of quinten's *clipboard*,
   never out of his chrome state, so what crosses is what the button
   actually produced.)

  xaver     clicks the top bar                 →  panel
            clicks **Join**                    →  prompt
            clicks **Paste**, clicks **Join**  →  in the room

  quinten   sees **Xaver** in his own roster, not a peer id

Two negative claims carry the weight:

  1. The token is never typed. The only thing this script types is a
     person's name; the token is copied on one machine and pasted on
     the other, so a paste that does nothing leaves the prompt empty
     and fails the run rather than quietly falling back to typing a
     value the harness already had in a variable.
  2. This script never invokes `peer.invite` / `peer.invite.named` /
     `peer.connect` through the action API. Every step is `click_named`
     or a key, so a control that is not painted, not hit-testable or
     not wired fails the run — where an invocation would have proved
     only the third of those.

Usage:
  ./collab-panel-proof.py [--keep] [--out DIR]

  cargo build -p shell

Exits non-zero if any claim fails.
"""
from __future__ import annotations

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

# The real user's home, captured before any session overrides `HOME`.
REAL_HOME = os.path.expanduser("~")

# What quinten types into the invite composer. Deliberately not the
# guest session's own `QOS_USER_ID`: a name that matched what the guest
# calls itself could have been read off the guest's own presence frames,
# and a name a peer announces about itself is exactly what this room
# refuses to believe. This one exists nowhere but in quinten's typing.
INVITED_AS = "Xaver"

failures: list[str] = []


def check(label: str, ok: bool, detail: str = "") -> bool:
    print(f"  {'PASS' if ok else 'FAIL'}  {label}" + (f" — {detail}" if detail else ""))
    if not ok:
        failures.append(label)
    return bool(ok)


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

    A stale binary produces a verdict about code that is not the code
    under test, and it produces it silently — the run looks exactly like
    a run that worked.
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
        # Short paths. `sun_path` is 108 bytes and both the control
        # socket and the Wayland socket live under directories named
        # here — a run rooted somewhere verbose fails at bind with an
        # error about the path rather than about the length.
        self.socket = os.path.join(runtime_root, f"{name[0]}.sock")
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
            # The app payload + runtime bundle are the machine's, not the
            # session's — they are read-only inputs both sessions share.
            "QOS_APPS_DATA_DIR": os.path.join(REAL_HOME, ".local/share/qos-apps"),
            "QOS_RUNTIME_BUNDLE_DIR": os.path.join(REAL_HOME, ".local/share/qos-runtime"),
            # Without its own device key a session's peer-sync turns
            # itself off, and the only visible symptom is that the
            # invite controls never appear.
            "QOS_PEER_KEY_PATH": os.path.join(self.dir, "keys", "peer-key"),
            "QOS_USER_ID": name,
            "NIRIOS_HEADLESS_SIZE": "1600x1000",
            # The host is de_DE and bwrap does not clear the
            # environment, so without this the chrome renders in German.
            "LC_ALL": "en_US.UTF-8",
            "LANG": "en_US.UTF-8",
        }
        # Never open a window on the user's display. `--backend headless`
        # is not enough on its own: winit falls back to X11 when only
        # WAYLAND_DISPLAY is cleared.
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

    def overlays(self) -> dict:
        return self.chrome().get("overlays", {}) or {}

    def peer_sync(self) -> dict:
        return self.chrome().get("peer_sync", {}) or {}

    def peers(self) -> dict:
        """Pointers and roster, as this session's own frame holds them.

        `roster` is built from the function that paints the
        collaborators panel, so asserting on it is asserting on the
        rows a person would see rather than on a field behind them.
        """
        return self.state().get("peers") or {}

    def targets(self) -> list:
        return self.call({"op": "list_targets"})["targets"]

    def named_rect(self, name: str) -> list | None:
        """The rect a name resolves to *right now*, or None.

        `register_chrome_rects` drops zero-area entries, and this drops
        any that slipped through, so a name that does not resolve means
        "not on screen" — which is exactly the claim each of these steps
        needs to be able to make.
        """
        for t in self.targets():
            if t.get("name") == name:
                rect = t.get("rect")
                if rect and rect[2] > 0 and rect[3] > 0:
                    return rect
        return None

    def click_named(self, name: str) -> dict:
        return self.call({"op": "click_named", "name": name})

    def type_text(self, text: str) -> dict:
        """Type `text`, one press+release per character.

        Used for exactly one thing: a person's name. The token is never
        typed — see the negative claims in the module docstring.
        """
        return self.call({"op": "type", "text": text})

    def clipboard_read(self) -> str | None:
        return self.call({"op": "clipboard_paste"})["clipboard"]["text"]

    def clipboard_write(self, text: str):
        self.call({"op": "clipboard_copy", "text": text})

    def snapshot(self, path: str) -> dict:
        return self.call({"op": "snapshot", "path": path}, timeout=180)

    def settle(self, seconds: float = 0.6):
        time.sleep(seconds)

    def stop(self):
        try:
            self.proc.terminate()
            self.proc.wait(timeout=10)
        except Exception:
            try:
                self.proc.kill()
            except Exception:
                pass
        try:
            self.log.close()
        except Exception:
            pass


def click(s: Session, name: str, what: str) -> bool:
    """Click a named control, and fail the run when it is not there."""
    rect = s.named_rect(name)
    if not rect:
        check(f"{s.name} can click {what}", False, f"`{name}` resolves to nothing")
        return False
    resp = s.click_named(name)
    if "err" in resp:
        check(f"{s.name} can click {what}", False, json.dumps(resp["err"])[:140])
        return False
    check(f"{s.name} clicks {what}", True, f"at {rect}")
    s.settle()
    return True


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", action="store_true",
                    help="leave the session roots on disk for inspection")
    ap.add_argument("--out", default=None, help="where the PNGs go")
    args = ap.parse_args()

    shots = args.out or os.path.join(tempfile.gettempdir(), "collab-panel")
    os.makedirs(shots, exist_ok=True)

    stale = freshness(SHELL, [
        REPO / "shell" / "src" / "shell" / "overlay_host.rs",
        REPO / "shell" / "src" / "shell" / "topbar_chrome.rs",
        REPO / "shell" / "src" / "compositor" / "frame.rs",
        # The roster: who the panel lists, and the state dump that
        # reports it. A binary older than either answers the last
        # question this script asks with code that is not under test.
        REPO / "shell" / "src" / "shell" / "peer_presence.rs",
        REPO / "shell" / "src" / "compositor" / "native.rs",
        # Where the name goes and when: the composer parks it against
        # the room, and the arrival of the peer that redeems the token
        # is what writes it into the room document. A binary older than
        # either answers the last question this script asks with code
        # that is not under test.
        REPO / "shell" / "src" / "shell" / "network.rs",
        REPO / "shell" / "src" / "shell" / "chrome_state.rs",
    ])
    if stale:
        print(f"REFUSING: {stale}\n  build first:  cargo build -p shell")
        return 2

    root = tempfile.mkdtemp(prefix="qinv-")
    runtime_root = tempfile.mkdtemp(prefix="/tmp/qs-")
    quinten = Session("quinten", root, runtime_root)
    xaver = Session("xaver", root, runtime_root)
    try:
        step("two compositors, separate identities and device keys")
        for s in (quinten, xaver):
            if not check(f"{s.name}'s session came up", s.wait_up()):
                print(open(s.log_path).read()[-3000:])
                return 1

        step("quinten's room is empty, and the top bar still offers a way in")
        roster = quinten.state()["workspace_tree"].get("collaborators") or []
        check("nobody is in the room yet", not roster, f"{len(roster)} collaborators")
        stack = quinten.named_rect("topbar_avatar_stack")
        if not check("the avatar stack is on screen anyway", bool(stack),
                     "an empty roster used to publish Rect::ZERO"
                     if not stack else f"at {stack}"):
            return 1
        quinten.snapshot(os.path.join(shots, "01-empty-roster-topbar.png"))

        step("he clicks it")
        if not click(quinten, "topbar_avatar_stack", "the avatar stack"):
            return 1
        check("the collaborators panel opened",
              bool(quinten.overlays().get("collaborators")))
        quinten.snapshot(os.path.join(shots, "02-quinten-panel.png"))

        step("and the panel offers Invite")
        invite_rect = quinten.named_rect("collab_invite")
        if not check("there is an Invite button to press", bool(invite_rect),
                     "the panel listed people and offered no way to add one"
                     if not invite_rect else f"at {invite_rect}"):
            return 1
        check("and a Join button beside it", bool(quinten.named_rect("collab_join")))
        if not click(quinten, "collab_invite", "Invite"):
            return 1

        step("and it asks who, before it mints anything")
        # The whole reason this step exists. A button that minted an
        # anonymous token left the host's own roster reading
        # `peer@12191268537764683811` once the guest arrived, because
        # nothing in the flow had ever been told a name — and a name is
        # governance here, so it cannot be supplied by the person
        # arriving.
        if not check("the card asks who is being invited",
                     wait_for(lambda: quinten.peer_sync().get("invite_name")
                              is not None, timeout=15) is not None,
                     repr(quinten.peer_sync().get("invite_name"))):
            return 1
        check("and nothing was minted before it was answered",
              quinten.peer_sync().get("invite_token") is None,
              repr(quinten.peer_sync().get("invite_token")))
        quinten.type_text(INVITED_AS)
        quinten.settle()
        check(f"the field took the name {INVITED_AS!r}",
              quinten.peer_sync().get("invite_name") == INVITED_AS,
              repr(quinten.peer_sync().get("invite_name")))
        quinten.snapshot(os.path.join(shots, "03-quinten-naming.png"))

        step("he presses Create invite")
        if not click(quinten, "peer_sync_create", "Create invite"):
            return 1

        token = wait_for(lambda: quinten.peer_sync().get("invite_token"), timeout=30)
        if not check("a token was minted", bool(token), f"{len(token or '')} chars"):
            return 1
        check(f"and the card says whose it is — {INVITED_AS!r}",
              quinten.peer_sync().get("invite_for") == INVITED_AS,
              repr(quinten.peer_sync().get("invite_for")))
        status = quinten.peer_sync().get("status") or ""
        check("and that it was copied, where he is looking",
              "copied" in status.lower(), repr(status))
        copied = wait_for(lambda: quinten.clipboard_read(), timeout=10)
        check("the token really is on his system clipboard",
              copied == token,
              "clipboard is empty" if not copied
              else ("matches the minted token" if copied == token
                    else f"clipboard holds something else ({len(copied)} chars)"))
        quinten.snapshot(os.path.join(shots, "04-quinten-invite-copied.png"))
        if not copied:
            return 1

        # Before anybody redeems it: whoever the room has named is
        # listed, and listed as *invited*. Naming happens when the token
        # is minted, so a roster that treated a name as an arrival would
        # claim xaver was here on the strength of a token still sitting
        # on quinten's clipboard — and joining would then change nothing
        # on screen, which is the whole beat.
        #
        # Whether there is anything to look at here depends on which
        # route the name came in by. The palette's `peer.invite.named`
        # is told a peer id, so it can write the name into the room
        # document at mint time and the seat appears immediately. The
        # panel cannot: the person it is inviting has no identity this
        # machine has ever seen until they redeem, so the name waits on
        # the inviting machine and is written the moment they do. Either
        # way nobody is listed as *present* before redeeming, which is
        # the claim — stated out loud rather than passed over quietly,
        # because a check that cannot tell "held" from "never ran" is
        # not a check.
        pending = [r for r in ((quinten.peers() or {}).get("roster") or [])
                   if not r.get("is_agent")]
        if pending:
            check("a peer the room has named but who has not redeemed "
                  "reads as invited, not present",
                  all(r.get("invited") is True for r in pending),
                  json.dumps(pending)[:200])
        else:
            print("  n/a   no row yet — the panel holds the name until "
                  "the invitee redeems, because until then there is no "
                  "peer identity to attach it to")

        step("the token travels — by hand, off quinten's clipboard")
        # Deliberately `copied`, not `token`: what crosses is what the
        # button produced, so a copy that silently truncated fails the
        # join rather than passing on a value it never wrote.
        xaver.clipboard_write(copied)
        check("it is on xaver's clipboard now", xaver.clipboard_read() == copied)

        step("xaver clicks his top bar too")
        if not click(xaver, "topbar_avatar_stack", "the avatar stack"):
            return 1
        check("his collaborators panel opened",
              bool(xaver.overlays().get("collaborators")))
        xaver.snapshot(os.path.join(shots, "05-xaver-panel.png"))

        step("and clicks Join")
        if not click(xaver, "collab_join", "Join"):
            return 1
        if not check("the connect prompt opened",
                     wait_for(lambda: xaver.peer_sync().get("connect_input")
                              is not None, timeout=15) is not None):
            return 1
        xaver.snapshot(os.path.join(shots, "06-xaver-prompt.png"))

        step("he pastes with the card's own Paste button")
        if not click(xaver, "peer_sync_paste", "Paste"):
            return 1
        pasted = xaver.peer_sync().get("connect_input")
        check("the prompt filled itself from the clipboard",
              pasted == token,
              "prompt is still empty — the paste did nothing" if not pasted
              else ("holds the whole token" if pasted == token
                    else f"holds {len(pasted)} chars, token is {len(token)}"))
        xaver.snapshot(os.path.join(shots, "07-xaver-pasted.png"))
        if pasted != token:
            return 1

        step("and presses Join on the card")
        if not click(xaver, "peer_sync_join", "Join"):
            return 1
        joined = wait_for(
            lambda: (xaver.peer_sync().get("status") or "").startswith("Joined"),
            timeout=45)
        check("xaver joined quinten's room", bool(joined),
              repr(xaver.peer_sync().get("status")))
        xaver.snapshot(os.path.join(shots, "08-xaver-joined.png"))

        step("and quinten sees him")
        pointers = wait_for(
            lambda: ((quinten.state().get("peers") or {}).get("pointers") or None),
            timeout=60)
        check("xaver's presence reaches quinten's session", bool(pointers),
              json.dumps(pointers)[:200] if pointers else "no presence arrived")
        if not quinten.overlays().get("collaborators"):
            click(quinten, "topbar_avatar_stack", "the avatar stack again")
        quinten.snapshot(os.path.join(shots, "09-quinten-roster.png"))

        # And the panel lists him.
        #
        # Read from `peers()["roster"]`, which is built from
        # `collaborator_roster_rows` — the same function that paints the
        # rows in 09-quinten-roster.png. Asserting the painted list
        # rather than the field behind it is the difference between "the
        # data arrived" and "the surface shows it", and only the second
        # is the claim this script makes.
        #
        # And it reads the name quinten gave, because the panel's
        # Invite asked for one. That name never crossed the wire: it
        # stayed on quinten's machine until xaver redeemed the token,
        # and was written into the room document then, by the room's
        # own authority, on the channel `apply_room_changes` refuses
        # from everybody else. The guest supplied nothing but a signed
        # token.
        roster = wait_for(
            lambda: ((quinten.peers() or {}).get("roster") or None), timeout=60)
        check("the panel lists the person who joined", bool(roster),
              json.dumps(roster)[:200] if roster
              else "the roster is empty after somebody demonstrably arrived")
        people = [r for r in (roster or []) if not r.get("is_agent")]
        check("and lists him as a person, not an agent", len(people) == 1,
              f"{len(people)} people in {json.dumps(roster)[:160]}")
        if people:
            label = people[0].get("label") or ""
            # Never blank, and never a name nobody chose. Which of the
            # two it is depends on whether the Invite button minted a
            # named token — so the claim is stated as the invariant that
            # holds either way, rather than pinned to today's answer:
            # the label is his peer identity, or it is exactly a name
            # this room's own document records for a peer.
            recorded = set()
            for room in (quinten.state()["workspace_tree"]
                         .get("room_peer_names") or {}).values():
                recorded.update((room or {}).values())
            check("under a label that is his identity or a name the room "
                  "recorded — never blank, never invented",
                  bool(label) and (label.startswith("peer@")
                                   or label in recorded),
                  f"{label!r}; the room records {sorted(recorded)}")
            # And which of the two it is, now that Invite asks. This is
            # the claim the script exists for: the host's own roster
            # reads a person, not `peer@12191268537764683811`.
            check(f"and that label is the name quinten typed — {INVITED_AS!r}",
                  label == INVITED_AS,
                  f"{label!r}; the room records {sorted(recorded)}")
            # And as somebody who *arrived*, not somebody who was asked.
            # The roster lists both, and draws them differently: a peer
            # the room has named and who has not redeemed anything reads
            # as `invited`, dimmed. Xaver redeemed his, so his row is a
            # participant — and that flip is the visible half of joining.
            check("and as a participant, not a pending invitation",
                  people[0].get("invited") is False,
                  json.dumps(people[0])[:160])
        # The negative half: quinten must not be in his own list of
        # other people. He names himself when he mints an invite, so
        # that his guests read a name rather than a number, and reading
        # that back would put the person holding the machine in it.
        check("and quinten is not listed as a peer of his own",
              not any((r.get("label") or "") == quinten.name
                      for r in (roster or [])),
              json.dumps(roster)[:160])

        step("and xaver sees quinten — by name, and without a pointer")
        # The other direction, and not the same claim. Quinten is named
        # in the room document: `issue_invite` records the inviter's own
        # name there, on the authority-checked channel, so that a guest
        # reads a person rather than a number. That name replicated to
        # xaver when he joined, which is why his side can say "quinten"
        # while quinten's side can only say `peer@…` for him.
        #
        # No pointer is waited on here on purpose. A session whose
        # pointer has never been anywhere publishes nothing, so a roster
        # that needed one would leave somebody who joined and sat still
        # out of the room they had just joined.
        if not xaver.overlays().get("collaborators"):
            click(xaver, "topbar_avatar_stack", "the avatar stack again")
        guest_roster = wait_for(
            lambda: ((xaver.peers() or {}).get("roster") or None), timeout=60)
        check("xaver's panel lists the host of the room he joined",
              bool(guest_roster),
              json.dumps(guest_roster)[:200] if guest_roster
              else "the guest's roster is empty inside the room he joined")
        check("under the name the room gave him, not a number",
              any((r.get("label") or "") == quinten.name
                  for r in (guest_roster or [])),
              json.dumps(guest_roster)[:200])
        check("and with the role the room's own capability list records",
              any((r.get("label") or "") == quinten.name
                  and r.get("role") == "owner"
                  for r in (guest_roster or [])),
              json.dumps(guest_roster)[:200])
        # Quinten is a participant on xaver's screen, not an invitation.
        # He is in the room he governs, and xaver learned it the way a
        # guest does: quinten answered his request for the room's
        # changes, under a peer id the transport authenticated.
        check("as somebody who is in the room, not somebody pending",
              all(r.get("invited") is False for r in (guest_roster or [])
                  if (r.get("label") or "") == quinten.name),
              json.dumps(guest_roster)[:200])
        xaver.snapshot(os.path.join(shots, "10-xaver-roster.png"))

        step("the negative claims")
        check("no token was ever typed — only copied and pasted", True,
              "the one `type` op in this script carries a person's name "
              "into the composer; the token crossed by clipboard or the "
              "join above failed")
        check("no invite was made through the action API", True,
              "every step above is `click_named`, so an unpainted or "
              "unwired control fails the run")

        print(f"\nshots in {shots}")
        for name in sorted(os.listdir(shots)):
            print(f"  {os.path.join(shots, name)}")
        print()
        if failures:
            print(f"FAILED: {len(failures)}")
            for f in failures:
                print(f"  - {f}")
            return 1
        print("all claims hold")
        return 0
    finally:
        xaver.stop()
        quinten.stop()
        if args.keep:
            print(f"session roots kept at {root}")
        else:
            shutil.rmtree(root, ignore_errors=True)
            shutil.rmtree(runtime_root, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
