#!/usr/bin/env python3
"""A workspace is shared: what one member puts in the room, the others have.

A room *is* a workspace (`plan/collaboration.md` §4), and what a room
holds is its **record index** (`plan/rooms.md`) — the references to the
documents in it. This checks that the index replicates between the room's
members, in both directions, and that a person's own view of the room
does not travel with it.

Two compositors with separate identities and homes share one room.
Every claim is read back from the session that did **not** make the
change, through the production search path: the records provider matches
a query against the corpus each session knows about and labels each row
with the room it lives in, so a session returning "In this room" for a
record it never referenced has that record in its index.

Both halves are asserted: a window one person opens must appear for the
other, and the other must stay on its current tab, strip and focus.

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

from reaper import die_with_parent
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
            "TILO_TEST_CONTROL": self.socket,
            "TILO_STATE_PATH": os.path.join(self.dir, "config", "state.json"),
            "XDG_DATA_HOME": os.path.join(self.dir, "data"),
            "XDG_CACHE_HOME": os.path.join(self.dir, "cache"),
            "XDG_STATE_HOME": os.path.join(self.dir, "state"),
            "XDG_CONFIG_HOME": os.path.join(self.dir, "config"),
            "XDG_RUNTIME_DIR": os.path.join(self.dir, "run"),
            # Without its own device key a session's peer-sync turns itself
            # off, and the only visible symptom is that `peer.invite` is
            # never exported — which reads as a capability refusal rather
            # than a missing prerequisite.
            "TILO_PEER_KEY_PATH": os.path.join(self.dir, "keys", "peer-key"),
            # Distinct identities. Without this every session falls back to
            # $USER and all of them are the same person, which would make
            # any claim about "someone else's session" meaningless.
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

    def groups_here(self) -> dict:
        """Every window group in the workspace this session is in.

        Keyed by group id — which is salted per peer, so a group that
        appears under the *same* id on two machines is the same window,
        not a lookalike.
        """
        tree = self.tree()
        active = tree.get("active")
        out = {}
        for ws in tree.get("workspaces", []):
            if ws.get("id") != active:
                continue
            for strip in ws.get("strips", []):
                for group in strip.get("window_groups", []):
                    out[str(group.get("id"))] = {
                        "active_tab": group.get("active_tab"),
                        "tabs": [t.get("title") for t in group.get("tabs", [])],
                        "tab_ids": [str(t.get("id")) for t in group.get("tabs", [])],
                        "strip": str(strip.get("id")),
                    }
        return out

    def tabs_here(self) -> dict:
        """Every tab in the workspace this session is in, by tab id.

        Ids are salted per peer, so a tab that appears under the *same*
        id on two machines is the same window, not a lookalike.
        """
        out = {}
        for gid, g in self.groups_here().items():
            for t in g["tab_ids"]:
                out[t] = {"group": gid, "strip": g["strip"]}
        return out

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

    root = tempfile.mkdtemp(prefix="tilo-room-index-")
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
        # Workspace scope, not room scope. `Scope::Room` covers only
        # room-shaped resources (`Scope::matches_resource`), so a
        # room-scoped grant authorises nothing a person *does* here —
        # `Scope::Workspace` is the spelling that means "an editor in
        # this workspace", and it is what the roster reads as membership.
        granted = quinten.call({
            "op": "grant", "subject": "person:xaver",
            "scope": "workspace", "verb": "write",
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
        q2_id, q2_tok = a_document(shared, "quinten-atonce")
        x2_id, x2_tok = a_document(shared, "xaver-atonce")
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

        print("\n[7] quinten opens a window — it opens for xaver, and does "
              "not move him")
        # Put xaver somewhere specific first, so "he did not move" is a
        # claim about a place he chose rather than about a default.
        xaver.invoke(ACTIONS, "tab.new.terminal.here")
        time.sleep(1.5)
        before_groups = xaver.groups_here()
        before_view = xaver.view_state()
        check("xaver is on a tab he opened himself",
              bool(before_view["active_group"]),
              f"focused group {before_view['active_group']}")

        q_before = set(quinten.tabs_here())
        opened = quinten.invoke(ACTIONS, "window.new.terminal")
        check("quinten opened a window", "err" not in opened,
              json.dumps(opened.get("err", {}))[:120])
        # Keyed on the *tab*, not the group: the placement rule tabs into
        # the focused group when it is already tabbed, so a new window
        # does not always mean a new group.
        fresh = wait_for(lambda: (set(quinten.tabs_here()) - q_before) or None,
                         timeout=20)
        if not check("quinten's own session shows the new window", bool(fresh)):
            return 1
        tid = sorted(fresh)[0]

        # The claim. Read back from xaver, by the id quinten minted.
        landed = wait_for(lambda: tid in xaver.tabs_here(), timeout=60)
        check("the window quinten opened is open for xaver too", bool(landed),
              f"tab {tid}" if landed else "NOT REPLICATED after 60s")
        if landed:
            where = xaver.tabs_here()[tid]
            titles = xaver.groups_here()[where["group"]]["tabs"]
            check("and it arrived with what is in it", bool(titles),
                  json.dumps(titles))

        # …and the view did not follow.
        after_view = xaver.view_state()
        after_groups = xaver.groups_here()
        check("xaver's active workspace did not move",
              after_view["active"] == before_view["active"],
              f'{before_view["active"]} -> {after_view["active"]}')
        check("xaver's scrolled-to strip did not move",
              after_view["active_strip"] == before_view["active_strip"])
        check("xaver's focus did not move",
              after_view["active_group"] == before_view["active_group"])
        for g, was in before_groups.items():
            if g in after_groups:
                check(f"the tab in front of xaver in group {g[:6]}… did not change",
                      after_groups[g]["active_tab"] == was["active_tab"],
                      f'{was["active_tab"]} -> {after_groups[g]["active_tab"]}')
        # The arriving window is not what he is looking at: his focused
        # group is unchanged above, and it is his own.
        if landed:
            arrived_in = xaver.tabs_here()[tid]["group"]
            focused = {str(v) for v in (after_view["active_group"] or {}).values()}
            check("xaver is still looking at his own window, not the new one",
                  arrived_in not in focused,
                  f"arrived in {arrived_in[:6]}…, focused {sorted(f[:6] for f in focused)}")

        print("\n[7b] and the other direction")
        x_before = set(xaver.tabs_here())
        xaver.invoke(ACTIONS, "window.new.terminal")
        x_new = wait_for(lambda: (set(xaver.tabs_here()) - x_before) or None,
                         timeout=20)
        if check("xaver opened a window", bool(x_new)):
            xtid = sorted(x_new)[0]
            check("the window xaver opened is open for quinten too",
                  bool(wait_for(lambda: xtid in quinten.tabs_here(), timeout=60)),
                  f"tab {xtid}")

        print("\n[8] a picture of xaver's session with quinten's document in it")
        shot = os.path.join(root, "xaver-room.png")
        xaver.key("Escape")
        time.sleep(0.3)
        # Super+K is the palette. The modifier is held across the
        # keystroke the way hardware holds it — the `key` op is a
        # press+release pair, which would drop Super before the `k`
        # arrived.
        xaver.call({"op": "key_down", "key": "Super"})
        xaver.call({"op": "key", "key": "k"})
        xaver.call({"op": "key_up", "key": "Super"})
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
