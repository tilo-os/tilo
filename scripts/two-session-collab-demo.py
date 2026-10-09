#!/usr/bin/env python3
"""Two Tilo sessions collaborating in one workspace, end to end.

Starts two compositors — separate state directories, separate
Iroh endpoint keys, separate user identities, separate control sockets —
and drives them through the flow a pair of users would take:

  1. the host mints an invite from the command palette (`/invite`),
  2. the peer pastes it into the palette's connect prompt (`/connect`),
  3. a change the host makes to the room shows up in the peer's own
     state,
  4. the host grants the peer a capability; the peer's own action gate
     starts permitting the action,
  5. the host revokes it; the peer's own action gate stops,
  6. the peer tries to administer the room and is refused.

Every claim is read back from the *other* session's `state_dump` or
from its own `check_action`, never from the side that made the change.

    scripts/two-session-collab-demo.py
    scripts/two-session-collab-demo.py --out /tmp/demo --keep

Requires `cargo build -p shell --bin shell` to have run. No display.
"""
from __future__ import annotations

import argparse
import json
import re
import os
import shutil
import signal
import socket
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# The app the demonstration grants over, and the action it gates. Both
# are arbitrary names — what matters is that the peer's answer about
# them comes from the replicated capability list.
DEMO_APP = "org.tilo.Notes"
DEMO_ACTION = "edit"
DEMO_RESOURCE = "notes.notebook"
# A path the filesystem capability covers. Two capabilities are granted
# rather than one because they answer different questions and only one of
# them is expressible in the Permissions tab: `Capability::from_capability`
# maps filesystem / device / network scopes into the portal-shaped
# descriptor the tab renders, and nothing else. So the workspace-scoped
# grant is the one the action gate reads, and the path-scoped grant is the
# one that shows up on screen with a Revoke button next to it.
DEMO_PATH = "/var/workspace/shared"

# How long to wait for a room change to cross. Replication is pulled on
# a fixed cadence, so this is that cadence plus room for a slow frame.
SYNC_TIMEOUT_S = 25.0


# --------------------------------------------------------------------
# control socket
# --------------------------------------------------------------------


def call(sock_path: str, payload: dict, timeout: float = 30.0) -> dict:
    """Send one command, read one reply."""
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
    """One running compositor, addressed by its control socket."""

    def __init__(self, name: str, user: str, run_dir: str, sock: str, binary: str,
                 size: str, runtime_dir: str):
        self.name = name
        self.user = user
        self.dir = run_dir
        self.sock = sock
        self.binary = binary
        self.size = size
        # Each session needs its own XDG_RUNTIME_DIR: the compositor
        # binds its collab broker at <runtime>/tilo/services/collab.sock,
        # and two sessions sharing one runtime dir are two sessions
        # contending for one socket. On a real machine that is a
        # per-user directory and the question does not arise; here it
        # has to be arranged. Keep it short — sun_path is ~108 bytes.
        self.runtime_dir = runtime_dir
        self.proc: subprocess.Popen | None = None
        self.log_path = os.path.join(run_dir, "compositor.log")
        self._log = None

    @property
    def collab_sock(self) -> str:
        """Where this session's broker should be listening."""
        return os.path.join(self.runtime_dir, "tilo", "services", "collab.sock")

    # -- lifecycle ---------------------------------------------------

    def start(self) -> None:
        os.makedirs(os.path.join(self.dir, "config"), exist_ok=True)
        os.makedirs(os.path.join(self.dir, "keys"), exist_ok=True)
        os.makedirs(self.runtime_dir, mode=0o700, exist_ok=True)
        env = dict(os.environ)
        # No display, and nothing inherited that would make the two
        # sessions share state.
        env.pop("DISPLAY", None)
        env.pop("WAYLAND_DISPLAY", None)
        # Nothing may point the toolkit at a broker other than this
        # session's own — the point of the exercise is that a session
        # provides one without being told where it is.
        env.pop("QUICK_COLLAB_SOCK", None)
        env.pop("QUICK_COLLAB_CAPNP_SOCK", None)
        env.update(
            XDG_RUNTIME_DIR=self.runtime_dir,
            XDG_CONFIG_HOME=os.path.join(self.dir, "config"),
            XDG_DATA_HOME=os.path.join(self.dir, "data"),
            XDG_CACHE_HOME=os.path.join(self.dir, "cache"),
            TILO_USER_ID=self.user,
            TILO_PEER_KEY_PATH=os.path.join(self.dir, "keys", "peer-key"),
            TILO_FS_ACL_PATH=os.path.join(self.dir, "config", "fs-capabilities.json"),
            TILO_TEST_CONTROL=self.sock,
            TILO_HEADLESS_SIZE=self.size,
            RUST_LOG=env.get("RUST_LOG", "info"),
        )
        self._log = open(self.log_path, "w")
        self.proc = subprocess.Popen(
            [self.binary, "--backend", "headless", "--enable-peer-sync"],
            cwd=REPO, env=env, stdout=self._log, stderr=subprocess.STDOUT,
        )

    def wait_ready(self, timeout: float = 90.0) -> None:
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.proc and self.proc.poll() is not None:
                raise RuntimeError(
                    f"{self.name} exited before binding its control socket; "
                    f"see {self.log_path}"
                )
            if os.path.exists(self.sock):
                # Bound is not the same as answering.
                try:
                    self.state()
                    return
                except (OSError, RuntimeError):
                    pass
            time.sleep(0.25)
        raise RuntimeError(f"{self.name} never answered on {self.sock}")

    def stop(self) -> None:
        if self.proc and self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
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
        active = tree["active"]
        for ws in tree["workspaces"]:
            if ws["id"] == active:
                return ws
        raise RuntimeError(f"{self.name}: no active workspace in the tree")

    def key(self, name: str) -> None:
        ok(call(self.sock, {"op": "key", "key": name}))

    def type_text(self, text: str) -> None:
        # Long tokens go in chunks: one command per few hundred
        # characters keeps each request comfortably under the socket's
        # line cap while still arriving as ordinary key events.
        for i in range(0, len(text), 200):
            ok(call(self.sock, {"op": "type", "text": text[i:i + 200]}))

    def modifiers(self, ctrl=False, shift=False, alt=False, logo=False) -> None:
        ok(call(self.sock, {"op": "modifiers", "ctrl": ctrl, "shift": shift,
                            "alt": alt, "logo": logo}))

    def snapshot(self, path: str) -> str:
        os.makedirs(os.path.dirname(path), exist_ok=True)
        ok(call(self.sock, {"op": "snapshot", "path": path}))
        return path

    def settle(self, seconds: float = 0.6) -> None:
        """Let the compositor paint.

        Rects for chrome affordances are published by the draw pass, so
        a click by name issued in the same breath as the state change
        that reveals it would miss.
        """
        time.sleep(seconds)

    def palette(self) -> dict:
        return self.chrome()["palette"]

    def open_palette(self, machine_wide: bool = True) -> None:
        """Open the command palette.

        Super+Space searches the whole machine; Ctrl+K narrows to the
        focused app. Which one to use depends on the row wanted: the
        shell's own actions only appear under Super+Space, while the
        peer-sync rows are offered under both and win the ranking under
        Ctrl+K, where filenames merely containing "invite" are not
        competing for the top row.
        """
        self.modifiers(logo=machine_wide, ctrl=not machine_wide)
        self.key("Space" if machine_wide else "k")
        self.modifiers()
        self.settle(0.4)

    def run_palette_command(self, query: str, expect_id: str | None = None,
                            machine_wide: bool = True) -> str | None:
        """Open the palette, type `query`, and activate the top row.

        Returns the id of the row that was activated, so the caller can
        assert it ran the command it meant to rather than whatever the
        search happened to rank first.
        """
        self.open_palette(machine_wide)
        self.type_text(query)
        self.settle(0.7)
        selected = self.palette()["selected_id"]
        if expect_id is not None and selected != expect_id:
            rows = [r["id"] for r in self.palette()["rows"]][:6]
            raise RuntimeError(
                f"{self.name}: palette query {query!r} selected {selected!r}, "
                f"expected {expect_id!r} (rows: {rows})")
        self.key("Enter")
        self.settle()
        return selected

    def open_permissions_tab(self) -> bool:
        """Open workspace settings on its Permissions tab.

        Returns whether the tab was reachable. The pill always paints —
        a host has to be able to reach the tab that grants before
        anything has been granted — so a `False` here means the modal
        itself did not open.
        """
        if not self.chrome()["overlays"]["ws_settings"]:
            self.run_palette_command("workspace settings", expect_id="result:0")
        if not self.chrome()["overlays"]["ws_settings"]:
            return False
        self.settle()
        targets = {t["name"] for t in ok(call(self.sock, {"op": "list_targets"}))["targets"]}
        if "ws_settings_tab[permissions]" not in targets:
            return False
        self.click("ws_settings_tab[permissions]")
        return self.chrome()["ws_settings_tab"] == "Permissions"

    def close_ws_settings(self) -> None:
        while self.chrome()["overlays"]["ws_settings"]:
            self.key("Escape")
            self.settle(0.3)

    def revoke_rects(self) -> list[str]:
        return sorted(t["name"]
                      for t in ok(call(self.sock, {"op": "list_targets"}))["targets"]
                      if t["name"].startswith("permissions_revoke["))

    def toggle_overview(self) -> None:
        self.modifiers(ctrl=True)
        self.key("Tab")
        self.modifiers()
        self.settle()

    def click(self, name: str) -> None:
        ok(call(self.sock, {"op": "click_named", "name": name}))
        self.settle(0.4)

    def grant(self, subject: str, scope: str, verb: str,
              cap_id: str | None = None) -> dict:
        payload = {"op": "grant", "subject": subject, "scope": scope, "verb": verb}
        if cap_id:
            payload["id"] = cap_id
        return call(self.sock, payload)

    def revoke(self, cap_id: str) -> dict:
        return call(self.sock, {"op": "revoke", "id": cap_id})

    def check_action(self, actor: str, app: str = DEMO_APP,
                     action: str = DEMO_ACTION, verb: str = "write",
                     resource: str = DEMO_RESOURCE) -> dict:
        return call(self.sock, {"op": "check_action", "actor": actor, "app": app,
                                "resource": resource, "action": action,
                                "verb": verb})


# --------------------------------------------------------------------
# reporting
# --------------------------------------------------------------------

FAILURES: list[str] = []


def step(n: str, text: str) -> None:
    print(f"\n=== {n}  {text}", flush=True)


def claim(description: str, actual, expected) -> None:
    good = actual == expected
    mark = "PASS" if good else "FAIL"
    print(f"  [{mark}] {description}\n         expected {expected!r}\n"
          f"         observed {actual!r}", flush=True)
    if not good:
        FAILURES.append(description)


def note(text: str) -> None:
    print(f"  ...  {text}", flush=True)


def wait_for(fn, timeout: float = SYNC_TIMEOUT_S, interval: float = 0.5):
    """Poll `fn` until it returns a truthy value, or give up."""
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        last = fn()
        if last:
            return last
        time.sleep(interval)
    return last


# --------------------------------------------------------------------
# the demonstration
# --------------------------------------------------------------------


def broker_answers(path: str) -> bool:
    """Is a broker actually listening at `path`?

    Existence is not the claim worth making — a stale socket file from
    a session that died survives on disk and connects to nothing. What
    an app needs is a listener that accepts, which is exactly what the
    launcher does on its behalf, so do the same thing here.
    """
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(5.0)
    try:
        s.connect(path)
        return True
    except OSError:
        return False
    finally:
        s.close()


def run(host: Session, peer: Session, shots: str) -> None:
    step("0.", "each session runs a collaboration broker of its own")
    # This is what a real login was missing: the broker only ever
    # started in the hosted entry point, behind an environment variable
    # the OS image never set, so apps asked for the collab service,
    # found nothing, and ran alone without saying so.
    for s in (host, peer):
        chrome = s.chrome()
        claim(f"{s.name} session reports collaboration available",
              chrome["collab"]["available"], True)
        claim(f"{s.name} session reports no reason it would be unavailable",
              chrome["collab"]["unavailable_reason"], None)
        # The compositor says where it bound; check it is the path the
        # launcher computes, and that something answers there.
        claim(f"{s.name} broker bound the launcher's service path",
              chrome["collab"]["socket"], s.collab_sock)
        claim(f"{s.name} broker accepts a connection",
              broker_answers(s.collab_sock), True)
    claim("the two sessions have separate brokers",
          host.collab_sock != peer.collab_sock, True)

    step("1.", "two sessions, each with its own identity and state")
    host_chrome, peer_chrome = host.chrome(), peer.chrome()
    claim("host session's user", host_chrome["local_user_id"], host.user)
    claim("peer session's user", peer_chrome["local_user_id"], peer.user)
    claim("they are different processes",
          host.proc.pid != peer.proc.pid, True)
    note(f"host pid {host.proc.pid} on {host.sock}")
    note(f"peer pid {peer.proc.pid} on {peer.sock}")
    note(f"host peer-id {peer_id_of(host)}")
    note(f"peer peer-id {peer_id_of(peer)}")
    claim("the two sessions have distinct Iroh identities",
          peer_id_of(host) != peer_id_of(peer), True)

    step("2.", "the host creates a workspace and makes it active")
    before = len(host.tree()["workspaces"])
    host.toggle_overview()
    claim("the overview opened", host.chrome()["overlays"]["overview"], True)
    host.click("add_workspace")
    after_tree = host.tree()
    claim("host workspace count grew", len(after_tree["workspaces"]), before + 1)
    host.click(f"overview_card[{before}]")
    claim("the overview closed on selection",
          host.chrome()["overlays"]["overview"], False)
    host_ws = host.active_workspace()
    claim("the new workspace is the active one",
          host_ws["id"], after_tree["workspaces"][before]["id"])
    note(f"host active workspace: {host_ws['name']} (id {host_ws['id']}, "
         f"room {host_ws['room']})")
    claim("the workspace it just made has an owner", host_ws["owner"], host.user)
    claim("and the host may administer it",
          host.chrome()["may_administer_active"], True)

    step("3.", "the host mints an invite from the command palette")
    host.run_palette_command("invite peer", expect_id="result:0")
    token = wait_for(lambda: host.chrome()["peer_sync"]["invite_token"], timeout=20)
    claim("the palette produced an invite token", bool(token), True)
    if not token:
        raise SystemExit("no token: the rest of the demonstration cannot run")
    note(f"token is {len(token)} chars; status: "
         f"{host.chrome()['peer_sync']['status']}")
    host.snapshot(os.path.join(shots, "01-host-invite-token.png"))
    # Dismiss the card: it is a modal, and it sits over the middle of
    # the screen where the settings modal's fields are.
    host.key("Escape")
    host.settle()
    claim("the invite card dismisses",
          host.chrome()["peer_sync"]["invite_token"], None)

    step("4.", "the peer joins through the palette's connect prompt")
    peer_ws_before = peer.active_workspace()
    note(f"peer active workspace before: {peer_ws_before['name']} "
         f"(room {peer_ws_before['room']})")
    claim("peer starts in a different room",
          peer_ws_before["room"] != host_ws["room"], True)
    peer.run_palette_command("connect to peer", expect_id="result:0")
    opened = wait_for(
        lambda: peer.chrome()["peer_sync"]["connect_input"] is not None, timeout=15)
    claim("the connect prompt opened", bool(opened), True)
    peer.snapshot(os.path.join(shots, "02-peer-connect-prompt.png"))
    peer.type_text(token)
    typed = peer.chrome()["peer_sync"]["connect_input"]
    claim("the token arrived in the prompt intact", typed, token)
    peer.key("Enter")
    status = wait_for(
        lambda: (peer.chrome()["peer_sync"]["status"] or "").startswith("Joined"),
        timeout=15)
    note(f"peer status: {peer.chrome()['peer_sync']['status']}")
    claim("the peer joined a room", bool(status), True)
    peer.key("Escape")
    peer_ws = peer.active_workspace()
    claim("the peer is now in the host's room", peer_ws["room"], host_ws["room"])
    # Joining also records who administers the room. The value comes from
    # the invite's signature, so the peer knows the room's owner and
    # capabilities may only ever arrive from that peer — including not
    # from another guest of the same room.
    claim("the peer records the inviter as the room's governance authority",
          peer.tree().get("room_authority", {}).get(peer_ws["room"]),
          {"Peer": f"iroh:{peer_id_of(host)}"})
    claim("the host still governs the room it made",
          host.tree().get("room_authority", {}).get(host_ws["room"], "Local"),
          "Local")
    peer.snapshot(os.path.join(shots, "03-peer-joined.png"))

    step("5.", "a change the host makes to the room reaches the peer")
    marker = f"Shared room {int(time.time())}"
    rename_active_workspace(host, marker)
    claim("host renamed its workspace", host.active_workspace()["name"], marker)
    arrived = wait_for(lambda: peer.active_workspace()["name"] == marker)
    claim("the peer's own copy carries the new name",
          peer.active_workspace()["name"], marker)
    # Ownership is part of the same replicated document, and it is what
    # decides who may administer. Read it off the peer.
    claim("the peer's copy records the host as owner",
          peer.active_workspace()["owner"], host.user)
    claim("the peer may not administer the room it joined",
          peer.chrome()["may_administer_active"], False)
    host.snapshot(os.path.join(shots, "04-host-after-rename.png"))
    peer.snapshot(os.path.join(shots, "05-peer-after-rename.png"))
    if not arrived:
        raise SystemExit("the rename never crossed; nothing downstream would mean anything")

    step("6.", "the peer cannot do the thing yet")
    before_grant = peer.check_action(f"app:{DEMO_APP}")
    note(f"peer check_action -> {json.dumps(before_grant)}")
    claim("the peer's own action gate denies it",
          before_grant.get("answer", {}).get("permitted"), False)

    step("7.", "the host grants the capability")
    cap_id = "demo-notes-write"
    fs_cap_id = "demo-notes-path"
    granted = host.grant(f"app:{DEMO_APP}", "workspace", "write", cap_id)
    note(f"host grant (workspace scope) -> {json.dumps(granted)}")
    claim("the grant was recorded", granted.get("answer", {}).get("granted"), cap_id)
    granted_fs = host.grant(f"app:{DEMO_APP}", f"fs:{DEMO_PATH}", "write", fs_cap_id)
    note(f"host grant (path scope) -> {json.dumps(granted_fs)}")
    claim("the path grant was recorded",
          granted_fs.get("answer", {}).get("granted"), fs_cap_id)

    has_cap = wait_for(
        lambda: {c["id"] for c in peer.active_workspace().get("capabilities") or []}
        >= {cap_id, fs_cap_id})
    claim("both capabilities are in the peer's own list", bool(has_cap), True)
    after_grant = peer.check_action(f"app:{DEMO_APP}")
    note(f"peer check_action -> {json.dumps(after_grant)}")
    claim("the peer's own action gate now permits it",
          after_grant.get("answer", {}).get("permitted"), True)

    # On screen, on the peer's machine: the Permissions tab lists the
    # grant that arrived over the wire. This is the one place the chrome
    # paints a replicated capability, so it is the screenshot that shows
    # the sync rather than merely showing a running desktop.
    claim("the peer's Permissions tab is reachable", peer.open_permissions_tab(), True)
    peer_rows = peer.revoke_rects()
    note(f"peer permissions rows: {peer_rows}")
    # Rows are addressable by the capability id they revoke, which is
    # the key `revoke_as` takes — so this asserts the peer's screen shows
    # a row for *this* capability, not merely one mentioning the app.
    claim("the peer's tab lists the replicated capability",
          f"permissions_revoke[{fs_cap_id}]" in peer_rows, True)
    peer.snapshot(os.path.join(shots, "07-peer-permissions-after-grant.png"))
    host.open_permissions_tab()
    host.snapshot(os.path.join(shots, "06-host-permissions-after-grant.png"))

    step("8.", "the peer tries to administer the room, and is refused")
    peer_grant = peer.grant("app:org.tilo.Rogue", "workspace", "admin")
    note(f"peer grant -> {json.dumps(peer_grant)}")
    claim("the peer's grant was refused", "err" in peer_grant, True)
    claim("refused for the right reason",
          peer_grant.get("err", {}).get("code"), "grant_refused")
    peer_revoke = peer.revoke(cap_id)
    note(f"peer revoke -> {json.dumps(peer_revoke)}")
    claim("the peer's revoke was refused", "err" in peer_revoke, True)
    still_there = any(c["id"] == cap_id
                      for c in peer.active_workspace().get("capabilities") or [])
    claim("and the capability the peer tried to revoke is still there",
          still_there, True)
    rogue_on_host = any(c.get("verb") == "Admin"
                        and "Rogue" in json.dumps(c.get("subject"))
                        for c in host.active_workspace().get("capabilities") or [])
    claim("nothing the peer asked for reached the host", rogue_on_host, False)
    peer.snapshot(os.path.join(shots, "07b-peer-revoke-disabled.png"))

    step("9.", "the host revokes, and the peer stops being able to")
    # The path-scoped grant is revoked by clicking the Revoke button in
    # the host's own Permissions tab — the affordance a user has.
    host_rows = host.revoke_rects()
    note(f"host permissions rows: {host_rows}")
    target = f"permissions_revoke[{fs_cap_id}]"
    claim("the host's tab offers a Revoke button for it",
          target in host_rows, True)
    if target not in host_rows:
        target = None
    if target:
        host.click(target)
        host.settle()
    claim("the path capability is gone from the host's list",
          all(c["id"] != fs_cap_id
              for c in host.active_workspace().get("capabilities") or []), True)
    fs_gone = wait_for(
        lambda: all(c["id"] != fs_cap_id
                    for c in peer.active_workspace().get("capabilities") or []))
    claim("and gone from the peer's own list too", bool(fs_gone), True)
    host.snapshot(os.path.join(shots, "08b-host-permissions-after-revoke.png"))
    host.close_ws_settings()

    # The workspace-scoped grant is revoked through
    # `Workspace::revoke_as` rather than by a click, to exercise the same
    # authority check from the other side of the API.
    revoked = host.revoke(cap_id)
    note(f"host revoke (workspace scope) -> {json.dumps(revoked)}")
    claim("the host's revoke succeeded",
          revoked.get("answer", {}).get("revoked"), True)
    gone = wait_for(
        lambda: all(c["id"] != cap_id
                    for c in peer.active_workspace().get("capabilities") or []))
    claim("the capability is gone from the peer's own list", bool(gone), True)
    after_revoke = peer.check_action(f"app:{DEMO_APP}")
    note(f"peer check_action -> {json.dumps(after_revoke)}")
    claim("the peer's own action gate denies it again",
          after_revoke.get("answer", {}).get("permitted"), False)
    # Snapshot the peer while its settings modal is still up: the row
    # that was there in shot 07 is gone, and the Permissions pill with
    # it, because the room no longer holds a grant the tab can express.
    peer.snapshot(os.path.join(shots, "09-peer-permissions-after-revoke.png"))
    note(f"peer permissions rows after revoke: {peer.revoke_rects()}")
    peer.close_ws_settings()
    host.snapshot(os.path.join(shots, "08-host-after-revoke.png"))


def peer_id_of(s: Session) -> str:
    """Pull the Iroh endpoint id the session logged at startup."""
    try:
        with open(s.log_path) as f:
            for line in f:
                # tracing paints its field names and separators, so the
                # escapes have to come off before the split.
                plain = re.sub(r"\x1b\[[0-9;]*m", "", line)
                if "local_peer_id=" in plain:
                    return plain.split("local_peer_id=")[1].split()[0].strip()
    except OSError:
        pass
    return "<unknown>"


def rename_active_workspace(s: Session, name: str) -> None:
    """Rename through the chrome's own name-edit path.

    Opens workspace settings, clicks the name field, replaces what is
    there and commits with Enter — the same sequence a user performs.
    """
    s.run_palette_command("workspace settings", expect_id="result:0")
    if not s.chrome()["overlays"]["ws_settings"]:
        raise RuntimeError(f"{s.name}: workspace settings did not open")
    s.click("ws_settings_name_input")
    if s.chrome()["editing_workspace_name"] is None:
        raise RuntimeError(f"{s.name}: the name field did not take focus")
    for _ in range(64):
        s.key("Backspace")
    s.type_text(name)
    s.key("Enter")
    s.settle()
    s.key("Escape")
    s.settle()


# --------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", default=os.path.join(REPO, "target", "two-session-demo"),
                    help="run directory for state, logs and screenshots")
    ap.add_argument("--shell", default=os.path.join(REPO, "target", "debug", "shell"),
                    help="path to the compositor binary")
    ap.add_argument("--size", default="1280x800", help="headless viewport")
    ap.add_argument("--keep", action="store_true",
                    help="keep the previous run directory instead of wiping it")
    args = ap.parse_args()

    if not os.path.exists(args.shell):
        print(f"no compositor at {args.shell}\n"
              f"build it first:  cargo build -p shell --bin shell", file=sys.stderr)
        return 2

    if not args.keep:
        shutil.rmtree(args.out, ignore_errors=True)
    shots = os.path.join(args.out, "shots")
    os.makedirs(shots, exist_ok=True)

    # Unix socket paths are capped near 108 bytes, so they go in the
    # runtime dir rather than next to the rest of the run.
    runtime = os.environ.get("XDG_RUNTIME_DIR") or "/tmp"
    host_sock = os.path.join(runtime, f"tilo-demo-host-{os.getpid()}.sock")
    peer_sock = os.path.join(runtime, f"tilo-demo-peer-{os.getpid()}.sock")
    # A private runtime dir per session, standing in for the per-user
    # one logind would give each of two real logins.
    host_runtime = os.path.join(runtime, f"qd-h{os.getpid()}")
    peer_runtime = os.path.join(runtime, f"qd-p{os.getpid()}")

    host = Session("host", "alice", os.path.join(args.out, "host"), host_sock,
                   args.shell, args.size, host_runtime)
    peer = Session("peer", "bob", os.path.join(args.out, "peer"), peer_sock,
                   args.shell, args.size, peer_runtime)

    print(f"run directory: {args.out}")
    print(f"screenshots:   {shots}")
    rc = 0
    try:
        for s in (host, peer):
            s.start()
        for s in (host, peer):
            s.wait_ready()
            print(f"{s.name} ready (pid {s.proc.pid}, user {s.user})")
        run(host, peer, shots)
    except Exception as exc:  # noqa: BLE001 — the report is the product
        FAILURES.append(f"{type(exc).__name__}: {exc}")
        import traceback
        traceback.print_exc()
    finally:
        for s in (host, peer):
            s.stop()
            for path in (s.sock,):
                try:
                    os.unlink(path)
                except FileNotFoundError:
                    pass

    print("\n" + "=" * 68)
    if FAILURES:
        rc = 1
        print(f"{len(FAILURES)} claim(s) did not hold:")
        for f in FAILURES:
            print(f"  - {f}")
    else:
        print("every claim held.")
    print(f"screenshots: {shots}")
    print(f"logs:        {host.log_path}\n             {peer.log_path}")
    return rc


if __name__ == "__main__":
    sys.exit(main())
