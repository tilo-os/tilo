#!/usr/bin/env python3
"""Two people in one community workspace, end to end: the presence acceptance.

Starts dist/dev/nested-pair.sh headless (a local site with its Lounge, and two
nested sessions with two throwaway identities that join it through the site's
door as two computers join tilo.coffee's Lounge), or attaches to a pair
already running (--attach READY, the file `nested-pair.sh --ready` writes).
Each session then does what a person there would: goes to the Lounge and
chooses to show where they are (a public workspace starts at "that I'm
here", which shares no pointer). Through each session's own control socket
and the actions people and agents use, it checks:

  roster    each session's roster shows the other person, arrived
  layout    each session shows the Lounge windows the other opened
  document  a message A posts in the Lounge's Chat is read on B
  pointer   A's pointer over a Chat window reaches B, on B's tile for the
            same tab and at the same offset from that tile's corner
  follow    B follows A: B's view goes to the window A is in, and goes with
            A when A moves to another window
  chat      what A types in cursor chat (Super+/ over a shared window) shows
            beside A's pointer on B

A check that cannot observe its subject fails; nothing is skipped. Every
check runs and prints one line; the exit status is non-zero when any failed.
State dumps and snapshots of both sessions are kept in --out.
"""
from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import signal
import socket
import subprocess
import sys
import tempfile
import time

REPO = pathlib.Path(__file__).resolve().parents[2]
CHAT = "so.tilo.chat"
# How long one check may take to converge between the two sessions. The
# host relays within milliseconds; launching an app takes seconds.
SETTLE = 60.0
START = 900.0


class Failed(Exception):
    pass


def call(sock: str, payload: dict, timeout: float = 15.0) -> dict:
    wait = payload.get("wait_ms", 0) / 1000.0
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
        s.settimeout(timeout + wait)
        s.connect(sock)
        s.sendall(json.dumps(payload).encode() + b"\n")
        buf = b""
        while b"\n" not in buf:
            chunk = s.recv(1 << 20)
            if not chunk:
                raise Failed("the session closed the control connection mid-reply")
            buf += chunk
    return json.loads(buf.partition(b"\n")[0])


def ok(sock: str, payload: dict) -> dict:
    response = call(sock, payload)
    if "err" in response:
        raise Failed(f"{payload['op']} refused: {response['err']}")
    return response


def wait_for(what: str, probe, timeout: float = SETTLE):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            value = probe()
        except (OSError, ValueError) as error:
            value, last = None, error
        if value:
            return value
        time.sleep(0.2)
    raise Failed(f"timed out waiting for {what}" + (f" (last error: {last})" if last else ""))


class Session:
    def __init__(self, name: str, control: str, room: str):
        self.name, self.control, self.room = name, control, room

    def state(self) -> dict:
        return ok(self.control, {"op": "state_dump"})["state"]

    def lounge(self, state: dict | None = None) -> dict:
        tree = (state or self.state())["workspace_tree"]
        workspace = next((w for w in tree.get("workspaces", []) if str(w.get("room")) == self.room), None)
        if workspace is None:
            raise Failed(f"{self.name} no longer holds the Lounge")
        return workspace

    def catalogue(self, query: str = "") -> list:
        rows, cursor = [], None
        while True:
            request = {"op": "actions_enumerate", "include_programmatic": True, "query": query,
                       "limit": 500, "detail": "compact"}
            if cursor:
                request["cursor"] = cursor
            answer = ok(self.control, request)["answer"]
            rows += answer["actions"]
            cursor = answer.get("next_cursor")
            if not cursor:
                return rows

    def action(self, predicate, what: str, query: str = "") -> dict:
        return wait_for(f"{self.name} to offer {what}",
                        lambda: next((row for row in self.catalogue(query) if predicate(row)), None))

    def invoke(self, row: dict, args: dict | None = None):
        """Invoke `row` and wait for it to end; its output value."""
        response = ok(self.control, {"op": "actions_invoke", "provider": row["provider"],
                                     "id": row["id"], "args": args or {}, "wait_ms": 20000})
        receipt = response["answer"].get("receipt", {})
        update = receipt.get("update") or {}
        if update.get("state") != "succeeded":
            raise Failed(f"{self.name}: {row['id'].split(chr(31))[-1]} ended {update.get('state')}: "
                         f"{json.dumps(update)[:400]}")
        return (update.get("output") or {}).get("value")

    def shell_action(self, action_id: str, args: dict | None = None):
        row = self.action(lambda row: row.get("provider") == "actions" and row.get("id") == action_id,
                          action_id, action_id)
        return self.invoke(row, args)

    def chat(self, operation: str, args: dict | None = None):
        row = self.action(lambda row: row.get("application") == CHAT and row.get("room") == self.room
                          and row.get("operation") == operation,
                          f"Chat's {operation} in the Lounge")
        return self.invoke(row, args)

    def tiles(self) -> dict:
        """Each shown tab's tile rectangle [x, y, w, h], by tab id."""
        state = self.state()
        workspace = self.lounge(state)
        targets = {row["name"]: row["rect"] for row in ok(self.control, {"op": "list_targets"})["targets"]}
        out = {}
        for strip_index, strip in enumerate(workspace.get("strips", [])):
            for group_index, group in enumerate(strip.get("window_groups", [])):
                suffix = f"/GroupRow[{strip_index}]/WindowGroup[{group_index}]/TileHost[0]"
                rect = next((rect for name, rect in targets.items() if name.endswith(suffix)), None)
                if rect is not None and group.get("active_tab") is not None:
                    out[group["active_tab"]] = rect
        return out

    def snapshot(self, out: pathlib.Path, label: str):
        try:
            call(self.control, {"op": "snapshot", "path": str(out / f"{self.name}-{label}.png")}, 30)
            (out / f"{self.name}-{label}.json").write_text(json.dumps(self.state(), indent=1))
        except (OSError, Failed, ValueError) as error:
            print(f"   ({self.name}: no {label} snapshot: {error})", file=sys.stderr)


def arrive(session: Session):
    """Go to the Lounge and show where you are, as a person there does."""
    workspace = session.lounge()
    # Offered for the workspaces one is not in.
    if session.state()["workspace_tree"].get("active") != workspace["id"]:
        session.shell_action(f"workspace.select.{workspace['id']}")
    session.shell_action("presence.visibility.set", {"visibility": "where"})
    wait_for(f"{session.name} in the Lounge",
             lambda: session.state()["workspace_tree"].get("active") == workspace["id"])


def other_person(viewer: Session) -> dict | None:
    """The roster row of the other person, once they arrived: a person
    (no agent), not just invited, editing (the room's host owns it, a
    watching page only views)."""
    rows = viewer.state()["peers"].get("roster") or []
    here = [row for row in rows if not row.get("invited") and not row.get("is_agent")
            and row.get("role") == "Editor"]
    return here[0] if len(here) == 1 else None


def wiggle(session: Session, x: float, y: float):
    """Move the pointer to (x, y) as a hand does, in a few steps."""
    for step in range(4, -1, -1):
        ok(session.control, {"op": "pointer_move", "x": x - step * 6, "y": y})
        time.sleep(0.05)


def chat_tabs(session: Session) -> set:
    """The tabs of the Chat windows in `session`'s Lounge."""
    return {tab["id"] for strip in session.lounge().get("strips", [])
            for group in strip.get("window_groups", []) for tab in group.get("tabs", [])
            if any(isinstance(body, dict) and body.get("app_id") == CHAT
                   for body in (tab.get("tile") or {}).values())}


def chat_window(session: Session, among: set | None = None) -> tuple[int, list]:
    """The tab of a Chat window shown in the Lounge (one of `among`, when
    given), and its tile."""
    def find():
        tiles = session.tiles()
        for tab in sorted(chat_tabs(session)):
            if tab in tiles and (among is None or tab in among):
                return tab, tiles[tab]
        return None
    return wait_for(f"{session.name}: a Chat window shown in the Lounge", find)


def shared_chat_window(a: Session, b: Session) -> tuple[int, list, list]:
    """A Chat window both sessions show: its tab and its tile on each."""
    def find():
        both = chat_tabs(a) & chat_tabs(b)
        tiles_a, tiles_b = a.tiles(), b.tiles()
        for tab in sorted(both):
            if tab in tiles_a and tab in tiles_b:
                return tab, tiles_a[tab], tiles_b[tab]
        return None
    return wait_for("a Chat window both sessions show", find)


def check_layout(a: Session, b: Session):
    """The Lounge's windows are everyone's: each session shows the windows
    the other opened."""
    def same():
        tabs = lambda session: {tab["id"] for strip in session.lounge().get("strips", [])
                                for group in strip.get("window_groups", [])
                                for tab in group.get("tabs", [])}
        mine, theirs = tabs(a), tabs(b)
        return (mine, theirs) if mine == theirs else None
    try:
        mine, _ = wait_for("both sessions to show the same windows of the Lounge", same, 30)
    except Failed:
        raise Failed(f"A shows tabs {sorted(chat_tabs(a))}, B shows {sorted(chat_tabs(b))}")
    print(f"ok layout: both show the Lounge's {len(mine)} window(s)")


def check_roster(a: Session, b: Session) -> dict:
    # Pointers say someone is here; so does taking part in a document.
    x, y, _, _ = chat_window(a)[1]
    wiggle(a, x + 40, y + 40)
    x, y, _, _ = chat_window(b)[1]
    wiggle(b, x + 40, y + 40)
    seen_by_b = wait_for("B's roster to show A, arrived", lambda: other_person(b))
    seen_by_a = wait_for("A's roster to show B, arrived", lambda: other_person(a))
    print(f"ok roster: A sees {seen_by_a['label']!r} ({seen_by_a['id']}), "
          f"B sees {seen_by_b['label']!r} ({seen_by_b['id']})")
    return seen_by_b


def channel_reader(session: Session) -> tuple[dict, str]:
    """Chat's `read-messages` of a text channel in the Lounge, and the
    channel's id, which the bound resource names
    (`tilo-ui-bound:v1:<host>:<lifetime>:<hex of chat.workspace/channels/ID>`)."""
    def find():
        for row in session.catalogue():
            if (row.get("application") == CHAT and row.get("room") == session.room
                    and row.get("operation") == "read-messages"):
                logical = bytes.fromhex(row["resource"].rsplit(":", 1)[-1]).decode()
                match = re.fullmatch(r"chat\.workspace/channels/(.+)", logical)
                if match:
                    return row, match.group(1)
        return None
    return wait_for(f"{session.name}: a channel of the Lounge's Chat to read", find)


def check_document(a: Session, b: Session):
    text = f"Hello from A at {time.strftime('%H.%M.%S')}"
    _, channel = channel_reader(a)
    try:
        a.chat("post", {"channel": channel, "text": text})
        how = "through Chat's post"
    except Failed as refused:
        # As a person does: type it into the message field and press Enter.
        print(f"   (A's post action was refused: {refused}; typing it instead)")
        _, (ax, ay, aw, ah) = chat_window(a)
        ok(a.control, {"op": "click_at", "x": ax + aw * 0.6, "y": ay + ah - 46})
        ok(a.control, {"op": "type", "text": text + "\n"})
        how = "typed into Chat"

    def read():
        row, _ = channel_reader(b)
        page = b.invoke(row, {"pageSize": 32})
        messages = ((page or {}).get("content") or {}).get("messages") or []
        return any(m.get("text") == text for m in messages)
    wait_for("A's message to be readable on B", read)
    print(f"ok document: B reads {text!r} ({how}) in channel {channel}")


def check_pointer(a: Session, b: Session, a_on_b: dict):
    tab, (ax, ay, aw, ah), (bx, by, _, _) = shared_chat_window(a, b)
    offset = (round(min(220.0, aw / 3)), round(min(160.0, ah / 3)))
    wiggle(a, ax + offset[0], ay + offset[1])
    expected = (bx + offset[0], by + offset[1])

    def arrived():
        for row in b.state()["peers"].get("pointers") or []:
            if (row.get("user") == a_on_b["id"] and abs(row["x"] - expected[0]) <= 1
                    and abs(row["y"] - expected[1]) <= 1):
                return row
        return None
    try:
        row = wait_for(f"A's pointer on B at {expected}", arrived, 20)
    except Failed:
        seen = [(row.get("user"), row.get("x"), row.get("y")) for row in
                b.state()["peers"].get("pointers") or []]
        raise Failed(f"A's pointer is not on B's tile of tab {tab} at {expected}; B shows {seen}")
    print(f"ok pointer: on A at {offset} from tab {tab}'s corner, on B at ({row['x']:.0f}, {row['y']:.0f}),"
          f" the same offset from B's tile, named {row.get('name')!r}")


def active_tab(session: Session) -> int | None:
    state = session.state()
    tree = state["workspace_tree"]
    workspace = session.lounge(state)
    if tree.get("active") != workspace["id"]:
        return None
    strip_id = (tree.get("active_strip") or {}).get(str(workspace["id"]))
    for strip in workspace.get("strips", []):
        if strip["id"] == strip_id:
            groups = strip.get("window_groups", [])
            active = (tree.get("active_group") or {}).get(str(strip_id))
            group = next((g for g in groups if g["id"] == active), groups[0] if groups else None)
            return group.get("active_tab") if group else None
    return None


def check_follow(a: Session, b: Session, a_on_b: dict):
    wait_for("A to be followable on B", lambda: any(
        row.get("id") == a_on_b["id"] and row.get("followable")
        for row in b.state()["peers"].get("roster") or []))
    # A is in a window B shows too (the Lounge's windows are everyone's).
    tab, (ax, ay, aw, ah), _ = shared_chat_window(a, b)
    ok(a.control, {"op": "click_at", "x": ax + 56, "y": ay + 72})
    where = wait_for(f"A in tab {tab}", lambda: active_tab(a) == tab and tab)
    b.shell_action("presence.follow", {"collaborator": a_on_b["id"]})
    wait_for("B to follow A", lambda: (b.state()["chrome"].get("presence") or {}).get("following")
             == a_on_b["id"])
    wait_for(f"B's view at A's window (tab {where})", lambda: active_tab(b) == where)
    # A goes to another window of the Lounge: the next one, or a new one.
    others = [tab["id"] for strip in a.lounge().get("strips", [])
              for group in strip.get("window_groups", []) for tab in group.get("tabs", [])
              if tab["id"] != where]
    if others:
        a.shell_action("strip.focus-next")
    else:
        a.chat("open-workspace")
    moved = wait_for("A in another window", lambda: (t := active_tab(a)) is not None and t != where and t)
    try:
        wait_for(f"B's view to go with A to tab {moved}", lambda: active_tab(b) == moved, 30)
    finally:
        b.shell_action("presence.follow.stop")
    print(f"ok follow: B's view went to A's window (tab {where}) and with A to tab {moved}")


def check_cursor_chat(a: Session, b: Session, a_on_b: dict):
    text = "hi from A"
    _, (ax, ay, aw, ah), _ = shared_chat_window(a, b)
    # Super+/ opens cursor chat whatever has the keyboard: Chat keeps text
    # focus in its message field, where a bare "/" is typing.
    wiggle(a, ax + aw * 0.6, ay + ah * 0.45)
    ok(a.control, {"op": "modifiers", "ctrl": False, "shift": False, "alt": False, "logo": True})
    try:
        ok(a.control, {"op": "type", "text": "/"})
    finally:
        ok(a.control, {"op": "modifiers", "ctrl": False, "shift": False, "alt": False, "logo": False})
    ok(a.control, {"op": "type", "text": text})

    def said():
        for row in b.state()["peers"].get("pointers") or []:
            if row.get("user") == a_on_b["id"] and row.get("chat") == text:
                return row
        return None
    wait_for("A's cursor chat beside A's pointer on B", said, 20)
    ok(a.control, {"op": "key", "key": "Enter"})
    print(f"ok chat: B shows {text!r} beside A's pointer")


def start_pair(args, out: pathlib.Path) -> tuple[subprocess.Popen | None, dict]:
    if args.attach:
        return None, json.loads(pathlib.Path(args.attach).read_text())
    ready = out / "ready.json"
    command = [str(REPO / "dist/dev/nested-pair.sh"), "--headless", "--fresh", "--fresh-site",
               "--apps", "chat notes", "--state", str(out / "pair"), "--ready", str(ready),
               "--ports", str(args.ports)]
    if args.no_build:
        command.append("--no-build")
    log = open(out / "pair.log", "w")
    # Keys are typed as a US keyboard makes them (Super+/ opens cursor chat);
    # the sessions take the machine's layout from here, not the host's.
    env = dict(os.environ, XKB_DEFAULT_LAYOUT="us")
    pair = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT, start_new_session=True,
                            env=env)

    def up():
        if pair.poll() is not None:
            raise Failed(f"nested-pair.sh exited with {pair.returncode} (see {out / 'pair.log'})")
        return ready.is_file()
    wait_for("both sessions to join the Lounge", up, START)
    return pair, json.loads(ready.read_text())


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--attach", help="the ready file of a pair already running (nested-pair.sh --ready)")
    parser.add_argument("--no-build", action="store_true", help="start the pair from the last build")
    parser.add_argument("--ports", type=int, default=8780, help="the local site's ports (nested-pair.sh --ports)")
    parser.add_argument("--out", type=pathlib.Path, help="where state dumps, snapshots and logs go")
    args = parser.parse_args()
    out = (args.out or pathlib.Path(tempfile.mkdtemp(prefix="tilo-nested-pair-check-"))).resolve()
    out.mkdir(parents=True, exist_ok=True)
    pair = None
    failed = []
    try:
        pair, ready = start_pair(args, out)
        a = Session("a", ready["a"], ready["room"])
        b = Session("b", ready["b"], ready["room"])
        print(f"ok joined: both sessions hold the Lounge ({ready['room']}) at {ready['site']}")
        for session in (a, b):
            arrive(session)
        a_on_b = check_roster(a, b)
        for name, check in (("layout", check_layout), ("document", check_document),
                            ("pointer", check_pointer),
                            ("follow", check_follow), ("chat", check_cursor_chat)):
            try:
                if name in ("layout", "document"):
                    check(a, b)
                else:
                    check(a, b, a_on_b)
            except Failed as failure:
                failed.append(name)
                print(f"FAILED {name}: {failure}")
                for session in (a, b):
                    session.snapshot(out, f"failed-{name}")
        for session in (a, b):
            session.snapshot(out, "end")
    except Failed as failure:
        failed.append("setup")
        print(f"FAILED: {failure}")
    finally:
        if pair is not None and pair.poll() is None:
            os.killpg(pair.pid, signal.SIGTERM)
            try:
                pair.wait(timeout=90)
            except subprocess.TimeoutExpired:
                os.killpg(pair.pid, signal.SIGKILL)
    print(("all presence checks passed" if not failed else f"failed: {', '.join(failed)}") + f" ({out})")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
