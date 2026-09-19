#!/usr/bin/env python3
"""An agent edits a shared document, and the other people see it.

The text is read back from sessions that did **not** write it.

Three compositors with separate identities and homes share one room.
The third invokes `actions/text.insert` by address with arguments and no
synthetic input at all — no click, no keystroke. Then ada's and
bob's own sessions are asked whether that text is in their copy of the
document.

The read-back goes through the production search path rather than a
back door: the text providers match a query against each tab's document
body (`crates/session/shell/src/shell/search/providers/text_app.rs`), so a session that
returns a hit for a token it never typed has that token in its document.

Usage:
  ./agent-collab-proof.py [--keep]

Exits non-zero if any claim fails.
"""
import argparse
import json
import os
import pathlib
import re
import shutil
import socket
import subprocess

from reaper import die_with_parent
import sys
import tempfile
import time
import uuid

REPO = pathlib.Path(__file__).resolve().parents[2]
SHELL = REPO / "target" / "debug" / "shell"

# The provider that owns the shell's own chrome actions.
ACTIONS = "actions"
# A token no seeded document could contain, so a hit proves replication
# rather than coincidence.
TOKEN = f"agent-wrote-this-{uuid.uuid4().hex[:8]}"

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


def wait_for(pred, timeout=20.0, poll=0.3):
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
        # Every home an app or the shell might persist into. `TILO_STATE_PATH`
        # alone moves only the workspace tree; records live under
        # XDG_DATA_HOME, and a relative XDG_CONFIG_HOME is ignored outright
        # by `dirs::config_dir`, so all of these are absolute.
        env = {
            **os.environ,
            "TILO_TEST_CONTROL": self.socket,
            "TILO_STATE_PATH": os.path.join(self.dir, "config", "state.json"),
            "XDG_DATA_HOME": os.path.join(self.dir, "data"),
            "XDG_CACHE_HOME": os.path.join(self.dir, "cache"),
            "XDG_STATE_HOME": os.path.join(self.dir, "state"),
            "XDG_CONFIG_HOME": os.path.join(self.dir, "config"),
            "XDG_RUNTIME_DIR": os.path.join(self.dir, "run"),
            # Each session needs its own device key. Without this the key
            # falls back to a system path the session cannot write, and
            # peer-sync turns itself off — quietly, from this script's
            # point of view: the only visible symptom is that the peer.*
            # actions are never exported, which reads as a capability
            # refusal rather than a missing prerequisite.
            "TILO_PEER_KEY_PATH": os.path.join(self.dir, "keys", "peer-key"),
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

    # -- convenience -------------------------------------------------
    def chrome(self) -> dict:
        return self.call({"op": "state_dump"})["state"]["chrome"]

    def key(self, k: str):
        self.call({"op": "key", "key": k})

    def type_text(self, t: str):
        self.call({"op": "type", "text": t})

    def enumerate(self, query: str = "") -> list:
        r = self.call({"op": "actions_enumerate", "query": query})
        return r.get("answer", {}).get("actions", []) if "answer" in r else []

    def invoke(self, provider: str, ident: str, args: dict | None = None) -> dict:
        payload = {"op": "actions_invoke", "provider": provider, "id": ident}
        if args is not None:
            payload["args"] = args
        return self.call(payload)

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
    """Join the room `token` names.

    The prompt is opened by address; the token itself still has to be
    typed, because the compositor has no semantic join op. That is
    recorded as a GAP rather than glossed.
    """
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


def body_carries_token(sess: Session, token: str) -> bool:
    """Does this session's own document contain `token`?

    Asked through the search providers, which match against each tab's
    document body. The session is answering about its own state.
    """
    for entry in sess.enumerate(token):
        if entry.get("provider") in ("notes", "editor"):
            return True
    return False


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", action="store_true", help="leave sessions running")
    args = ap.parse_args()

    if not SHELL.exists():
        print(f"FAIL: compositor not built at {SHELL}", file=sys.stderr)
        print("      run: cargo build -p shell --bin shell", file=sys.stderr)
        return 1

    root = tempfile.mkdtemp(prefix="tilo-agent-collab-")
    sessions: list[Session] = []
    try:
        print("\n[1] three real sessions, isolated from this machine and each other")
        ada = Session("ada", root); sessions.append(ada)
        bob = Session("bob", root); sessions.append(bob)
        agent = Session("agent", root); sessions.append(agent)
        for s in sessions:
            if not check(f"{s.name} came up", s.wait_up(),
                         f"see {s.log_path}" if not os.path.exists(s.socket) else ""):
                return 1

        print("\n[2] one room")
        # Peer-sync silently disables itself when the device key is
        # unreadable, and the only symptom downstream is a missing
        # action. Check the prerequisite explicitly so a failure here
        # names its own cause instead of looking like a refusal.
        exported = {f"{a['provider']}:{a['id']}" for a in ada.enumerate("")}
        if not check("peer-sync is on (peer.invite is exported)",
                     f"{ACTIONS}:peer.invite" in exported):
            print("        peer-sync is off. Check the session log for "
                  "'device key load failed':")
            print(f"        {ada.log_path}")
            return 1

        token = mint_invite(ada)
        if not check("ada minted an invite", bool(token)):
            return 1
        for guest in (bob, agent):
            check(f"{guest.name} joined the room", join(guest, token))
        gap("joining has no semantic op — the invite token still had to be typed")

        room = ada.chrome().get("peer_sync", {})
        print(f"        host status: {room.get('status')!r}")

        print("\n[3] the agent edits the document, by address, with arguments")
        # Nothing is typed into the agent. The address plus the argument
        # is the entire input.
        before_self = body_carries_token(agent, TOKEN)
        check("the token is nowhere beforehand", not before_self)

        acts = {f"{a['provider']}:{a['id']}" for a in agent.enumerate("")}
        has_insert = f"{ACTIONS}:text.insert" in acts
        if not check("the agent can see the edit action", has_insert,
                     "actions:text.insert" if has_insert else "NOT EXPORTED"):
            print("        (the agent is a guest; if this is a capability refusal, "
                  "that is the gate working, not a bug)")
            return 1

        resp = agent.invoke(ACTIONS, "text.insert", {"text": TOKEN})
        if "err" in resp:
            check("the edit was accepted", False, json.dumps(resp["err"])[:160])
            return 1
        check("the edit was accepted", True, json.dumps(resp.get("answer", {}))[:90])

        landed = wait_for(lambda: body_carries_token(agent, TOKEN), timeout=15)
        check("the text is in the agent's own document", bool(landed))

        print("\n[4] the other two sessions are asked about their own copies")
        for peer in (ada, bob):
            got = wait_for(lambda p=peer: body_carries_token(p, TOKEN), timeout=45)
            check(f"{peer.name}'s document carries the agent's text", bool(got),
                  "replicated" if got else "NOT REPLICATED after 45s")

        print("\n[5] presence")
        pres = ada.chrome().get("collab") or {}
        # Presence shape is not asserted blindly: report what is there and
        # only claim what can be read.
        print(f"        ada's collab view: {json.dumps(pres)[:200]}")
        if not pres:
            gap("no presence payload is visible in chrome state — the agent's "
                "caret/authorship could not be verified from here")

        print()
        if failures:
            print(f"FAILED: {len(failures)} claim(s): {', '.join(failures)}")
            return 1
        print("Every claim held, read back from the sessions that did not write.")
        if gaps:
            print(f"{len(gaps)} thing(s) this run could not verify:")
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
