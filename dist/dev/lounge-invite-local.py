#!/usr/bin/env python3
"""The fresh-device Lounge test against a local site run the way eu1 runs the
Lounge: invite-only, governed by its relay, from this tree's build.

  1. inviter   session A joins the site while it is open, the site restarts
               taking people by invitation only (TILO_ROOMS_ADMISSION=invite,
               as eu1 sets it), and A is still let in as a member
  2. history   A posts a Chat message and creates a note before anyone else
               joins
  3. invite    A issues an invitation with `invitation.issue`
  4. join      fresh session B, whose door refuses it, redeems that invitation
               through the join field
  5. prejoin   B shows A's earlier message and note (each its own result)
  6. chat      B posts, A reads it; A answers, B reads it
  7. roster    each roster holds the other

Every result is printed as `ok` or `FAIL`. Exits non-zero when any failed.
"""
from __future__ import annotations

import importlib.util
import json
import os
import pathlib
import subprocess
import sys
import time

REPO = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("sda", REPO / "dist/dev/site-door-acceptance.py")
sda = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sda)
PAIR = sda.PAIR
Failed, wait_for = sda.Failed, sda.wait_for
NOTES = "so.tilo.notes"
TOKEN = sys.argv[1] if len(sys.argv) > 1 else time.strftime("%H%M%S")


class Site(sda.LocalSite):
    """sda's local site with its door's admission chosen, restartable in
    place: the same room, state and ports."""

    base = 0

    def start(self, admission: str = "open", fresh: bool = True):
        build = pathlib.Path(os.environ.get("TILO_NESTED_SESSION_BUILD", REPO / "target/nested-session"))
        self.dir.mkdir(parents=True, exist_ok=True)
        ready = self.dir / "ready.json"
        ready.unlink(missing_ok=True)
        self.base = self.base or sda.free_port_base()
        base = self.base
        command = [sys.executable, "-I", str(sda.RUN_SITE), "--scratch", str(self.dir),
                   "--name", "lounge", "--shown-name", "Lounge", "--home-app", "so.tilo.chat",
                   "--relay-bin", str(build / "bin/tilo-relay"),
                   "--room-service-bin", str(build / "bin/tilo-room-service"),
                   "--web-port", str(base), "--api-port", str(base + 1), "--port-base", str(base + 10),
                   "--ready-file", str(ready), "--no-bundle"]
        if fresh:
            command.append("--fresh")
        env = dict(os.environ, TILO_ROOMS_ADMISSION=admission,
                   TILO_RELAY_LOG=os.environ.get("TILO_RELAY_LOG", "info,tilo_relay=debug,tilo_relay_session=debug"),
                   TILO_ROOMS_GUEST_JOINS_PER_10_MIN="1000", TILO_ROOMS_GUEST_JOINS_PER_DAY="100000")
        log = open(self.dir / "site.log", "a")
        self.process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT,
                                        start_new_session=True, env=env)

        def answered():
            if self.process.poll() is not None:
                raise Failed(f"the local site exited with {self.process.returncode} (see {self.dir / 'site.log'})")
            try:
                return json.loads(ready.read_text())
            except (OSError, ValueError):
                return None
        up = wait_for("the local site to answer", answered, sda.SITE_START)
        self.site, self.room = up["site"], up["room"]
        print(f"the local site is up at {self.site} (room {self.room}, admission {admission})", flush=True)


class Running(sda.Running):
    def start(self, fresh: bool, build: bool):
        command = [str(sda.SESSION), "--state", str(self.state), "--headless", "--community", self.site,
                   "--apps", "chat notes"]
        if fresh:
            command.append("--reset-identity")
        if not build:
            command.append("--no-build")
        log = open(self.out / f"{self.name}.log", "a")
        self.process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT,
                                        start_new_session=True, env=dict(os.environ, XKB_DEFAULT_LAYOUT="us"))


def digest(path: pathlib.Path, words: tuple[str, ...], limit: int = 250):
    """The lines of `path` mentioning any of `words`, a run of the same
    message folded into its first line and a count: t keeps no outputs of
    a failed run, so what explains it is printed."""
    import re
    try:
        lines = path.read_text(errors="replace").splitlines()
    except OSError:
        print(f"--- {path}: missing", flush=True)
        return
    strip = re.compile(r"\x1b\[[0-9;]*m")
    kept, last, repeats = [], None, 0
    for line in lines:
        line = strip.sub("", line)
        if not any(word in line.lower() for word in words):
            continue
        message = re.sub(r"^\S+Z\s+", "", line)
        message = re.sub(r"\d+", "N", message)
        if message == last:
            repeats += 1
            continue
        if repeats:
            kept.append(f"      (and {repeats} more like it)")
        kept.append(line[:600])
        last, repeats = message, 0
    if repeats:
        kept.append(f"      (and {repeats} more like it)")
    print(f"--- {path}: {len(kept)} digest lines (last {min(limit, len(kept))})", flush=True)
    for line in kept[-limit:]:
        print(f"    {line}", flush=True)


SHELL_WORDS = ("native", "dial", "https", "enrol", "invit", "join", "peer-sync", "peer_sync",
               "room_sync", "connect", "refus", "error", "panic", "door", "admit")
SITE_WORDS = ("session", "hello", "join", "enrol", "invit", "admit", "refus", "error", "warn",
              "governed", "room", "member", "listen", "iroh")


def roster(session) -> list:
    return session.state().get("peers", {}).get("roster", [])


def notes_rows(session) -> list:
    return [row for row in session.catalogue()
            if row.get("application") == NOTES and row.get("room") == session.room]


def listed_note(session, title: str, page: str = ""):
    """A row naming the note titled `title` (or minted as `page`) in the room.

    Notes publishes each note as a resource of its own, labelled by its
    title (`notes.notebook/pages/<id>`); its entries also answer a search by
    title through the app-content provider, whose row id is app, room,
    record and key joined by U+001F.
    """
    for row in notes_rows(session):
        text = json.dumps(row)
        if title in text or (page and page in text):
            return row
    for row in session.catalogue(title):
        parts = str(row.get("id", "")).split("\x1f")
        if (row.get("provider") == "app-content" and len(parts) == 4 and parts[0] == NOTES
                and parts[1] == session.room and title in str(row.get("title", ""))):
            return row
    return None


def describe_notes(session, title: str) -> str:
    rows = notes_rows(session)
    resources = sorted({f"{r.get('resource')}={r.get('resource_label')!r}" for r in rows})
    searched = [{k: r.get(k) for k in ("provider", "id", "title", "subtitle")}
                for r in session.catalogue(title)]
    return (f"{len(rows)} Notes rows in the room, resources {json.dumps(resources)[:1500]}; "
            f"a search for the title answers {json.dumps(searched)[:1500]}")


def main() -> int:
    os.environ.setdefault("RUST_LOG", "info,shell::shell::network=debug,shell::network=debug,"
                          "shell::shell::peer_sync_overlay=debug,shell::shell::overlay_host=debug,"
                          "tilo_joined_room=debug,tilo_relay_session=debug,tilo_relay=debug,tilo_room_service=debug,"
                          "channel_crypto=debug,shell::shell::mls_vault=debug,shell::shell::channel_policy=debug,"
                          "shell::shell::broker_services=debug,shell::network::actor=debug")
    # Under /target on t, which keeps it; under the tree's target/ elsewhere.
    base = pathlib.Path("/target") if pathlib.Path("/target").is_dir() else REPO / "target"
    out = (base / "lounge-invite-local" / time.strftime("%Y%m%d-%H%M%S")).resolve()
    out.mkdir(parents=True, exist_ok=True)
    site = Site(out, None)
    results: list[tuple[str, bool]] = []

    def check(name: str, passed: bool, detail: str):
        print(f"{'ok  ' if passed else 'FAIL'} {name}: {detail}", flush=True)
        results.append((name, passed))

    a = b = None
    try:
        site.start("open")
        url = site.site if site.site.endswith("/") else site.site + "/"
        a = Running("a", out / "a", url, out)
        a.start(fresh=True, build=False)
        session_a = a.joined()
        room = session_a.room
        print(f"A joined the open site: room {room}", flush=True)
        site.stop()
        site.start("invite", fresh=False)
        check("inviter-room", site.room == room, f"the restarted site names {site.room}, A joined {room}")
        session_a = wait_for("A to hold the room again after the site restarted",
                             lambda: session_a if sda.installed(session_a) else None, sda.START)

        # 2. History from before B joins.
        _, channel = PAIR.channel_reader(session_a)
        earlier = f"before {TOKEN}: written before the fresh device joined"
        session_a.chat("post", {"channel": channel, "text": earlier})
        print("A posted:", earlier, flush=True)
        note_title = f"Note before {TOKEN}"
        minted = ""
        # Notes offers its actions once it runs in the room: open it there.
        session_a.shell_action(f"workspace.select.{room}")
        session_a.invoke(session_a.action(lambda row: row.get("provider") == "applications"
                                          and row.get("id") == NOTES, "Notes to open", "Notes"))
        try:
            note_rows = wait_for("A's Notes to offer new-note in the room",
                                 lambda: (rows := notes_rows(session_a)) and any(
                                     row.get("operation") == "new-note" for row in rows) and rows, 300)
        except Failed as e:
            print(e, "; A's Notes rows:", sorted({row.get("operation") for row in notes_rows(session_a)}),
                  flush=True)
            session_a.snapshot(out, "no-new-note")
            note_rows = []
        print("A's Notes operations:", sorted({row.get("operation") for row in note_rows}), flush=True)
        new_note = next((row for row in note_rows if row.get("operation") == "new-note"), None)
        if new_note is None:
            check("note-created", False, "A's Notes offers no new-note in the room")
        else:
            value = session_a.invoke(new_note, {"title": note_title})
            check("note-created", value is not PAIR.Stale, f"new-note answered {json.dumps(value)[:200]}")
            minted = (value or {}).get("page", "") if isinstance(value, dict) else ""
            try:
                row = wait_for("A's catalogue to list the note",
                               lambda: listed_note(session_a, note_title, minted), 120)
                check("note-listed", True, f"A lists it: {json.dumps(row)[:300]}")
            except Failed as e:
                check("note-listed", False, f"{e}; {describe_notes(session_a, note_title)}")
                session_a.snapshot(out, "note-not-listed")

        # 3. The invitation.
        try:
            session_a.shell_action(f"workspace.select.{room}")
        except Failed as e:
            print("selecting the room on A:", e, flush=True)
        issued = session_a.shell_action("invitation.issue", {
            "role": "editor", "expires_in_seconds": 21600, "invitee_name": "Fresh device", "confirm": True})
        invite = (issued or {}).get("invite")
        check("invite", bool(invite), f"invitation.issue answered {'an invite' if invite else issued}")
        if not invite:
            raise Failed("no invitation to redeem")

        # 4. B, refused by the door, redeems it.
        b = Running("b", out / "b", url, out)
        b.start(fresh=True, build=False)
        control_b = sda.control_of(b.state)
        wait_for("B's control socket", lambda: os.path.exists(control_b) or None, sda.START)
        probe = PAIR.Session("b", control_b, room)
        wait_for("B to answer", lambda: probe.state() or None, 300)
        probe.shell_action("peer.connect")
        time.sleep(2)
        probe.snapshot(out, "sheet-open")
        PAIR.ok(control_b, {"op": "type", "text": invite})
        time.sleep(1)
        probe.snapshot(out, "typed")
        PAIR.ok(control_b, {"op": "key", "key": "Enter"})

        def held():
            tree = probe.state()["workspace_tree"]
            joining = {str(r) for r in tree.get("joining_rooms", []) or []}
            return probe if sda.workspaces_of(probe) and room not in joining and any(
                row.get("room") == room for row in probe.catalogue()) else None

        session_b = None
        waited = 0
        for label, until in (("entered-5s", 5), ("entered-60s", 60), ("entered-180s", 180), ("after-join", 900)):
            try:
                session_b = wait_for(f"the room to install on B ({label})", held, until - waited)
            except Failed:
                pass
            waited = until
            probe.snapshot(out, label)
            if session_b is not None:
                break
        held_now = sda.workspaces_of(probe)
        check("join", session_b is not None and len(held_now) == 1,
              f"B holds {[(w['id'], w['room']) for w in held_now]} of room {room}")
        if session_b is None:
            # Where B's join stands: its join card, what it is joining.
            try:
                state = probe.state()
                print("B join card:", json.dumps(state["chrome"].get("peer_sync")), flush=True)
                print("B joining:", state["workspace_tree"].get("joining_rooms"), flush=True)
            except (OSError, Failed, ValueError, KeyError) as e:
                print("B's join state is unreadable:", e, flush=True)
            raise Failed("B never held the room")
        try:
            session_b.shell_action(f"workspace.select.{room}")
        except Failed as e:
            print("selecting the room on B:", e, flush=True)

        def has_app(session, app):
            return any(row.get("application") == app and row.get("room") == room for row in session.catalogue())

        def open_app(session, app, name):
            session.invoke(session.action(lambda row: row.get("provider") == "applications"
                                          and row.get("id") == app, f"{name} to open", name))

        # The room's windows are everyone's: A's Chat should come to B by itself.
        try:
            wait_for("Chat in the room on B", lambda: has_app(session_b, PAIR.CHAT) or None, 120)
            print("B: Chat came with the room", flush=True)
        except Failed:
            print("B: no Chat in the room after 120 s; opening it", flush=True)
            open_app(session_b, PAIR.CHAT, "Chat")
        time.sleep(20)
        session_b.snapshot(out, "chat-open")

        # 5. What the room held before B joined.
        try:
            wait_for("A's earlier message on B", lambda: PAIR.reads_message(session_b, channel, earlier) or None, 600)
            check("prejoin-chat", True, f"B reads {earlier!r}")
        except Failed as e:
            check("prejoin-chat", False, str(e))
        if not notes_rows(session_b):
            try:
                wait_for("Notes in the room on B", lambda: notes_rows(session_b) or None, 60)
            except Failed:
                print("B: no Notes in the room after 60 s; opening it", flush=True)
                open_app(session_b, NOTES, "Notes")
                time.sleep(20)
                session_b.snapshot(out, "notes-open")
        try:
            row = wait_for("A's earlier note on B",
                           lambda: listed_note(session_b, note_title, minted), 600)
            check("prejoin-note", True, f"B's Notes lists {note_title!r}: {row.get('id', '')[-80:]}")
        except Failed as e:
            check("prejoin-note", False, f"{e}; {describe_notes(session_b, note_title)}")

        # 6. Chat both ways.
        hello = f"hello from a fresh device {TOKEN}"
        session_b.chat("post", {"channel": channel, "text": hello})
        try:
            wait_for("B's message on A", lambda: PAIR.reads_message(session_a, channel, hello) or None, 600)
            check("chat-b-to-a", True, f"A reads {hello!r}")
        except Failed as e:
            check("chat-b-to-a", False, str(e))
        reply = f"reply {TOKEN} from the inviter"
        session_a.chat("post", {"channel": channel, "text": reply})
        try:
            wait_for("A's reply on B", lambda: PAIR.reads_message(session_b, channel, reply) or None, 600)
            check("chat-a-to-b", True, f"B reads {reply!r}")
        except Failed as e:
            check("chat-a-to-b", False, str(e))

        # 7. Rosters.
        roster_a, roster_b = roster(session_a), roster(session_b)
        print("A roster:", json.dumps(roster_a)[:800], flush=True)
        print("B roster:", json.dumps(roster_b)[:800], flush=True)
        check("roster", len(roster_a) >= 2 and len(roster_b) >= 2,
              f"A lists {len(roster_a)}, B lists {len(roster_b)}")
        for session in (session_a, session_b):
            session.snapshot(out, "end")
    except (Failed, OSError) as failure:
        check("setup", False, repr(failure))
        for running in (a, b):
            if running is None:
                continue
            sda.tail(out / f"{running.name}.log", 10)
    finally:
        for running in (a, b):
            if running is not None:
                running.stop()
        if not all(passed for _, passed in results):
            for running in (a, b):
                if running is not None:
                    digest(running.state / "logs/shell.log", SHELL_WORDS)
            digest(site.dir / "site.log", SITE_WORDS, 400)
        site.stop()
    failed = [name for name, passed in results if not passed]
    print(("RESULT ok" if not failed else f"RESULT failed: {', '.join(failed)}") + f" ({out})", flush=True)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
