#!/usr/bin/env python3
"""A workspace is shared: what one member puts in the room, the others have.

A room *is* a workspace (`plan/collaboration.md` §4), and what a room
holds is its **record index** (`plan/rooms.md`) — the references to the
documents in it. This proves that index replicates between the room's
members, in both directions, and that a person's own view of the room
does not travel with it.

Two real compositors, separate identities, separate homes, one room.
Every claim is read back from the session that did **not** make the
change, through the production search path: the records provider matches
a query against the corpus each session knows about and labels each row
with the room it lives in, so a session returning "In this room" for a
record it never referenced is a session whose index genuinely holds it.

What must NOT travel is asserted too, and asserted the hard way — one
session is made to change its layout and focus while the other's is
compared byte for byte.

Usage:
  ./room-index-proof.py [--keep]

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
import uuid

REPO = pathlib.Path(__file__).resolve().parents[2]
SHELL = REPO / "target" / "debug" / "shell"

# The provider that owns the shell's own chrome actions, and the one that
# owns records.
ACTIONS = "actions"
RECORDS = "records"

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
            # Without its own device key a session's peer-sync turns itself
            # off, and the only visible symptom is that `peer.invite` is
            # never exported — which reads as a capability refusal rather
            # than a missing prerequisite.
            "QOS_PEER_KEY_PATH": os.path.join(self.dir, "keys", "peer-key"),
            # Distinct identities. Without this every session falls back to
            # $USER and all of them are the same person, which would make
            # any claim about "someone else's session" meaningless.
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
            start_new_session=True,
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

    def tree(self) -> dict:
        return self.call({"op": "state_dump"})["state"]["workspace_tree"]

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

    def snapshot(self, path: str) -> dict:
        return self.call({"op": "snapshot", "path": path})

    # -- the two things this script asserts about --------------------
    def holds_record(self, record_id: str, needle: str) -> str | None:
        """Does *this* session's index put `record_id` in the room it is in?

        Asked through the records search provider, which labels each row
        with where the record lives. Returns the provenance line, so the
        caller can tell "here" from "in some other room of mine".
        """
        for entry in self.enumerate(needle):
            if entry.get("provider") == RECORDS and entry.get("id") == record_id:
                return entry.get("subtitle") or ""
        return None

    def view_state(self) -> dict:
        """Everything about this session that is a fact about *the person*.

        Which workspace they are on, which strip they scrolled to, which
        group has their focus, and which tab is in front of them in every
        group. None of it is the room's, so none of it may move when a
        peer does something.
        """
        tree = self.tree()
        tabs = {}
        for ws in tree.get("workspaces", []):
            for strip in ws.get("strips", []):
                for group in strip.get("window_groups", []):
                    tabs[str(group.get("id"))] = group.get("active_tab")
        return {
            "active": tree.get("active"),
            "active_strip": tree.get("active_strip"),
            "active_group": tree.get("active_group"),
            "active_tab_per_group": tabs,
        }

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


def a_document(shared: str, who: str) -> tuple[str, str]:
    """A record naming a real file, and the token that finds it.

    A file record is the case the design calls the unification — "a file
    is a record whose identity is a path" — so it is the honest thing to
    prove with. The file is made for real: a reference to a document that
    does not exist would be a weaker claim than it looks.
    """
    token = f"{who}-note-{uuid.uuid4().hex[:8]}"
    path = os.path.join(shared, f"{token}.md")
    with open(path, "w") as fh:
        fh.write(f"# {token}\n\nwritten by {who}\n")
    return f"file:{path}", token


def bring(sess: Session, record_id: str) -> dict:
    return sess.invoke(ACTIONS, "record.bring",
                       {"id": record_id, "kind": "document"})


def can_bring(sess: Session) -> bool:
    """Is `record.bring` exported to this session at all?

    Enumeration is the gate's surface: an action a session may not
    perform is not shown to it. So this is the same question as "may
    they put a record in this room", asked without performing anything.
    """
    return any(
        a.get("provider") == ACTIONS and a.get("id") == "record.bring"
        for a in sess.enumerate("")
    )


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", action="store_true", help="leave sessions running")
    args = ap.parse_args()

    if not SHELL.exists():
        print(f"FAIL: compositor not built at {SHELL}", file=sys.stderr)
        print("      run: cargo build -p shell --bin shell", file=sys.stderr)
        return 1

    root = tempfile.mkdtemp(prefix="qos-room-index-")
    shared = os.path.join(root, "documents")
    os.makedirs(shared, exist_ok=True)
    sessions: list[Session] = []
    try:
        print("\n[1] two real sessions, isolated from this machine and each other")
        quinten = Session("quinten", root); sessions.append(quinten)
        xaver = Session("xaver", root); sessions.append(xaver)
        for s in sessions:
            if not check(f"{s.name} came up", s.wait_up(),
                         "" if os.path.exists(s.socket) else f"see {s.log_path}"):
                return 1

        print("\n[2] one room")
        exported = {f"{a['provider']}:{a['id']}" for a in quinten.enumerate("")}
        if not check("peer-sync is on (peer.invite is exported)",
                     f"{ACTIONS}:peer.invite" in exported):
            print("        peer-sync is off. Check the session log for "
                  "'device key load failed':")
            print(f"        {quinten.log_path}")
            return 1
        if not check("the room-index action is exported",
                     f"{ACTIONS}:record.bring" in exported):
            return 1

        token = mint_invite(quinten)
        if not check("quinten minted an invite", bool(token)):
            return 1
        if not check("xaver joined the room", join(xaver, token)):
            return 1
        gap("joining has no semantic op — the invite token still had to be typed")

        # The view each person has of the room, before anybody does
        # anything. Every later claim about what did *not* move is
        # measured against this.
        before = {s.name: s.view_state() for s in sessions}

        print("\n[3] quinten puts a document in the room")
        q_id, q_tok = a_document(shared, "quinten")
        check("nobody has it beforehand",
              xaver.holds_record(q_id, q_tok) is None
              and quinten.holds_record(q_id, q_tok) is None)
        resp = bring(quinten, q_id)
        if not check("the action was accepted", "err" not in resp,
                     json.dumps(resp.get("err", {}))[:160]):
            return 1
        check("quinten's own session holds it",
              bool(wait_for(lambda: quinten.holds_record(q_id, q_tok), timeout=20)))

        print("\n[4] xaver's session is asked about its own copy of the room")
        got = wait_for(lambda: xaver.holds_record(q_id, q_tok), timeout=45)
        check("xaver's room holds the document quinten brought", bool(got),
              got if got else "NOT REPLICATED after 45s")
        check("and his session says it is in this room, not somewhere else",
              bool(got) and "this room" in got.lower(), got or "")

        print("\n[5] the other direction — and what a guest is allowed to do")
        # Putting a record in a room is an act, so it is gated like one.
        # A guest who has only been admitted is not yet an editor.
        check("a guest cannot put a record in the room unasked",
              not can_bring(xaver),
              "record.bring is not exported to him")
        refused = bring(xaver, "file:/docs/should-not-land.md")
        check("and invoking it anyway is refused, not silently ignored",
              "err" in refused,
              json.dumps(refused.get("err", {}))[:120])

        # The owner makes him an editor. This is the room's governance,
        # so it travels the path that already existed for it.
        granted = quinten.call({
            "op": "grant", "subject": "person:xaver",
            "scope": "room", "verb": "write",
        })
        check("quinten grants xaver write in the room", "err" not in granted,
              json.dumps(granted.get("answer", granted.get("err", {})))[:120])
        check("the grant reaches xaver's own session",
              bool(wait_for(lambda: can_bring(xaver), timeout=45)),
              "record.bring is now exported to him")

        x_id, x_tok = a_document(shared, "xaver")
        resp = bring(xaver, x_id)
        check("xaver's action was accepted", "err" not in resp,
              json.dumps(resp.get("err", {}))[:160])
        got = wait_for(lambda: quinten.holds_record(x_id, x_tok), timeout=45)
        check("quinten's room holds the document xaver brought", bool(got),
              got if got else "NOT REPLICATED after 45s")

        print("\n[6] both at once — neither loses")
        q2_id, q2_tok = a_document(shared, "quinten-同time")
        x2_id, x2_tok = a_document(shared, "xaver-同time")
        # Issued back to back with no wait between them, so the two
        # references are in flight against each other.
        bring(quinten, q2_id)
        bring(xaver, x2_id)
        for who, sess in (("quinten", quinten), ("xaver", xaver)):
            both = wait_for(
                lambda s=sess: (s.holds_record(q2_id, q2_tok) is not None)
                and (s.holds_record(x2_id, x2_tok) is not None),
                timeout=60)
            check(f"{who}'s room holds both simultaneous documents", bool(both),
                  "" if both else "one of the two is missing")

        print("\n[7] the view does not follow")
        # Move xaver's desk: a new window takes focus and changes which
        # tab is in front. None of that is a fact about the room.
        moved = xaver.invoke(ACTIONS, "window.new.terminal")
        check("xaver opened a window on his own desk", "err" not in moved,
              json.dumps(moved.get("err", {}))[:120])
        time.sleep(6)  # two sync intervals, so anything that would travel has
        after_q = quinten.view_state()
        check("quinten's active workspace did not move",
              after_q["active"] == before["quinten"]["active"],
              f'{before["quinten"]["active"]} -> {after_q["active"]}')
        check("quinten's scrolled-to strip did not move",
              after_q["active_strip"] == before["quinten"]["active_strip"])
        check("quinten's focused group did not move",
              after_q["active_group"] == before["quinten"]["active_group"])
        check("the tab in front of quinten did not change",
              after_q["active_tab_per_group"]
              == before["quinten"]["active_tab_per_group"])
        after_x = xaver.view_state()
        check("and xaver's own view did change, so the check can fail",
              after_x != before["xaver"],
              "his layout moved, as it should")
        # The layout itself is his alone: quinten must not have grown a
        # window because xaver opened one.
        q_groups = len(after_q["active_tab_per_group"])
        check("quinten did not gain xaver's window",
              q_groups == len(before["quinten"]["active_tab_per_group"]),
              f"{len(before['quinten']['active_tab_per_group'])} -> {q_groups}")

        print("\n[8] a picture of xaver's session with quinten's document in it")
        shot = os.path.join(root, "xaver-room.png")
        xaver.key("Escape")
        time.sleep(0.3)
        # Super+K is the palette. The record row it lists is the proof
        # rendered rather than merely reported.
        xaver.call({"op": "key", "key": "super+k"})
        time.sleep(0.6)
        xaver.type_text(q_tok)
        time.sleep(1.2)
        xaver.snapshot(shot)
        ok = wait_for(lambda: os.path.exists(shot) and os.path.getsize(shot) > 5000,
                      timeout=20)
        check("a frame was captured from xaver's session", bool(ok),
              f"{shot} ({os.path.getsize(shot)} bytes)" if ok else "no PNG")
        if ok:
            keep = REPO / "target" / "room-index-proof"
            keep.mkdir(parents=True, exist_ok=True)
            shutil.copy(shot, keep / "xaver-room.png")
            print(f"        kept at {keep / 'xaver-room.png'}")

        print()
        if failures:
            print(f"FAILED: {len(failures)} claim(s): {', '.join(failures)}")
            return 1
        print("Every claim held, read back from the session that did not make "
              "the change.")
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
