#!/usr/bin/env python3
"""Devices that switch between their own workspaces and the Lounge: the
acceptance of one device hosting its own workspaces beside a community's.

Against a local site run the way eu1 runs the Lounge (invite-only, governed
by its room host, from this tree's build: lounge-invite-local.py's site),
with three nested headless sessions of fresh identities and every app the
nested session ships:

  own          A and B each make a workspace of their own (workspace.create),
               hosted by their own device: its room synced, not the site's,
               and never on the other device
  join         B, refused by the door, joins with A's invitation
  apps         every app the session installs is listed in A's own workspace
               and in the Lounge alike, and each opens there into a window of
               that workspace
  shared       the Lounge's layout is personal: a window A shares on purpose
               shows on B under A's tab, as B's own window of the same app,
               and none of A's other windows; it goes when A stops sharing
  data         in A's own workspace and in the Lounge A writes a Chat message,
               a note, a task, a sheet and a document paragraph: each Lounge
               item reaches B, no own item ever shows in the Lounge or on B
  presence     in the Lounge each roster holds the other and A's pointer
               reaches B at the same place on a window A shares; in their own
               workspace neither shows the other, nor a pointer from the Lounge
  switch       A and B switch own -> Lounge -> own -> Lounge four times each
               through the workspace-select action the overview and palette
               take; after each switch the workspace in front, its windows
               (and that they are on screen) and the app list are right, and
               the switch took no longer than SWITCH_BOUND
  cold-open    fresh device C joins later with a second invitation and finds
               the Lounge's message, note, task and sheet, and no own item
  end          no error notification on A or B, every workspace still there
               with every window it had, every room synced

Every result is printed as `ok` or `FAIL`, with timings. Exits non-zero when
any failed.
"""
from __future__ import annotations

import importlib.util
import collections
import json
import os
import pathlib
import re
import sys
import time

REPO = pathlib.Path(__file__).resolve().parents[2]


def load(name: str, path: pathlib.Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


lil = load("lounge_invite_local", REPO / "dist/dev/lounge-invite-local.py")
sda, PAIR = lil.sda, lil.PAIR
Failed, wait_for, Session = PAIR.Failed, PAIR.wait_for, PAIR.Session
CHAT, NOTES = PAIR.CHAT, lil.NOTES
TASKS, SHEETS, WORDS = "so.tilo.tasks", "so.tilo.sheets", "so.tilo.word-processor"
TOKEN = sys.argv[1] if len(sys.argv) > 1 else time.strftime("%H%M%S")
# Longest a switch between two workspaces this device holds may take, from
# the action to the workspace in front with its windows on screen and its
# app list answered: a local change of the desktop, no network in it.
SWITCH_BOUND = 2.0
# Longest an app may take to open into a window of the workspace in front.
OPEN_BOUND = 60.0
# Longest an item written on one device may take to show on another.
SYNC_BOUND = 120.0
SWITCHES = 4
# Where the sessions' states live (main sets it beside the outputs).
WORK = REPO / "target"


class Running(sda.Running):
    """A nested headless session with every app the nested session ships."""

    def start(self, fresh: bool, build: bool):
        import subprocess
        command = [str(sda.SESSION), "--state", str(self.state), "--headless", "--community", self.site]
        if fresh:
            command.append("--reset-identity")
        if not build:
            command.append("--no-build")
        log = open(self.out / f"{self.name}.log", "a")
        self.process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT,
                                        start_new_session=True, env=dict(os.environ, XKB_DEFAULT_LAYOUT="us"))

    def shipped(self) -> dict:
        """The apps this session installed that open windows, id to name
        (its plan; services such as Agent and Web Search open none)."""
        import tomllib
        plan = json.loads((self.state / "plan.json").read_text())
        out = {}
        for row in plan["catalog"]:
            manifest = tomllib.loads((REPO / row["manifest"]).read_text())
            if manifest.get("package", {}).get("kind", "app") == "app":
                out[row["id"]] = row["name"]
        return out


# --- Reads --------------------------------------------------------------------

def tree(session: Session) -> dict:
    return session.state()["workspace_tree"]


def workspace(session: Session, state: dict | None = None) -> dict | None:
    t = (state or session.state())["workspace_tree"]
    return next((w for w in t.get("workspaces", []) if str(w.get("room")) == session.room), None)


def tabs(session: Session, state: dict | None = None) -> dict:
    """The tabs of `session`'s room's workspace, tab id to app id."""
    w = workspace(session, state) or {}
    out = {}
    for strip in w.get("strips", []):
        for group in strip.get("window_groups", []):
            for tab in group.get("tabs", []):
                app = next((body.get("app_id") for body in (tab.get("tile") or {}).values()
                            if isinstance(body, dict) and body.get("app_id")), None)
                out[tab["id"]] = app
    return out


def app_list(session: Session) -> set:
    return {row["id"] for row in session.catalogue() if row.get("provider") == "applications"}


def errors(session: Session) -> list:
    return [n for n in session.state()["chrome"].get("notifications", [])
            if n.get("severity") == "Error"]


def room_sync(session: Session) -> dict:
    return {row["room"]: row.get("state") for row in session.state()["chrome"].get("room_sync", [])}


def app_rows(session: Session, app: str) -> list:
    return [row for row in session.catalogue() if row.get("application") == app and row.get("room") == session.room]


def content_room(row: dict) -> str | None:
    """The room a row of a room's content is in: its `room`, or for an
    app-content entry the room its id names (app, room, record, key)."""
    if row.get("provider") == "app-content":
        parts = str(row.get("id", "")).split("\x1f")
        return parts[1] if len(parts) == 4 else None
    return row.get("room")


def holding(session: Session, token: str) -> list:
    """Rows of any room's content on `session`'s device that mention
    `token`, by the room they are in: its catalogue and its search."""
    rows = session.catalogue() + session.catalogue(token)
    return [(content_room(row), row) for row in rows
            if content_room(row) and token in json.dumps(row, ensure_ascii=False)]


def holds(session: Session, token: str) -> dict | None:
    """A row of `session`'s room's content that mentions `token`."""
    return next((row for room, row in holding(session, token) if room == session.room), None)


# --- Acts ---------------------------------------------------------------------

def select(session: Session, expected_tabs: dict | None = None, expected_apps: set | None = None) -> float:
    """Switch `session`'s device to its room's workspace through the
    workspace-select action; the seconds until it is in front with its
    windows on screen and (when given) its tabs and app list as expected."""
    start = time.monotonic()
    state = session.state()
    w = workspace(session, state)
    if w is None:
        raise Failed(f"{session.name} holds no workspace of room {session.room}")
    if state["workspace_tree"].get("active") != w["id"]:
        session.invoke({"provider": "actions", "id": f"workspace.select.{session.room}"})

    def settled():
        state = session.state()
        if state["workspace_tree"].get("active") != w["id"]:
            return None
        if expected_tabs is not None and tabs(session, state) != expected_tabs:
            return None
        shown = PAIR.active_tab(session)
        if shown is not None and shown not in session.tiles():
            return None
        if expected_apps is not None and app_list(session) != expected_apps:
            return None
        return True
    try:
        wait_for(f"{session.name}: workspace {w.get('name')!r} in front", settled, 60)
    except Failed:
        state = session.state()
        raise Failed(f"{session.name}: after selecting {w.get('name')!r} the active workspace is "
                     f"{state['workspace_tree'].get('active')} (wanted {w['id']}), tabs "
                     f"{tabs(session, state)} (wanted {expected_tabs}), app list differs by "
                     f"{sorted(app_list(session) ^ expected_apps) if expected_apps else '-'}")
    return time.monotonic() - start


SETTINGS = "so.tilo.settings"


def settings_elsewhere(session: Session) -> tuple[dict, int] | None:
    """The workspace and tab of the device's one Settings window, when it is
    in a workspace other than `session`'s."""
    state = session.state()
    for w in state["workspace_tree"].get("workspaces", []):
        if str(w["room"]) == session.room:
            continue
        view = Session(session.name, session.control, str(w["room"]))
        tab = next((tab for tab, app in tabs(view, state).items() if app == SETTINGS), None)
        if tab is not None:
            return w, tab
    return None


def open_app(session: Session, app: str) -> tuple[int, float]:
    """Open `app` in `session`'s workspace (in front) as the launcher does:
    the tab it opened into, and the seconds it took.

    Settings is the device's control panel, one window per machine: opened
    again from another workspace it brings its window to front where it is.
    That is checked, and the workspace is selected again after."""
    before = set(tabs(session))
    start = time.monotonic()
    elsewhere = settings_elsewhere(session) if app == SETTINGS else None
    if elsewhere is not None:
        w, tab = elsewhere
        row = session.action(lambda row: row.get("provider") == "applications" and row.get("id") == app,
                             f"{app} to open")
        session.invoke(row)
        view = Session(session.name, session.control, str(w["room"]))
        try:
            wait_for(f"{session.name}: the one Settings window in front in {w.get('name')!r}",
                     lambda: PAIR.active_tab(view) == tab, OPEN_BOUND)
        except Failed:
            state = session.state()
            t = state["workspace_tree"]
            raise Failed(f"{session.name}: opening Settings again from {workspace(session, state).get('name')!r} "
                         f"left workspace {t.get('active')} in front with tab {PAIR.active_tab(session)} "
                         f"(Settings is tab {tab} in {w.get('name')!r} = {w['id']}); its windows "
                         f"{[r for r in PAIR.ok(session.control, {'op': 'windows'})['answer']['windows'] if r.get('app_id') == SETTINGS]}")
        seconds = time.monotonic() - start
        select(session)
        return tab, seconds
    row = session.action(lambda row: row.get("provider") == "applications" and row.get("id") == app,
                         f"{app} to open")
    session.invoke(row)

    def opened():
        now = tabs(session)
        fresh = [tab for tab, a in now.items() if a == app and tab not in before]
        if fresh:
            return fresh[0]
        shown = PAIR.active_tab(session)
        return shown if shown is not None and now.get(shown) == app else None
    try:
        tab = wait_for(f"{session.name}: {app} in a window of the workspace", opened, OPEN_BOUND)
    except Failed:
        state = session.state()
        t = state["workspace_tree"]
        where = {}
        for w in t.get("workspaces", []):
            view = Session(session.name, session.control, str(w["room"]))
            where[w.get("name")] = sorted(a for a in tabs(view, state).values() if a == app)
        rows = [r for r in PAIR.ok(session.control, {"op": "windows"})["answer"]["windows"] if r.get("app_id") == app]
        raise Failed(f"{session.name}: {app} opened no window in {workspace(session, state).get('name')!r} "
                     f"(its windows: {rows}); "
                     f"active workspace {t.get('active')}, its windows by workspace {where}, "
                     f"notifications {state['chrome'].get('notifications')}")
    seconds = time.monotonic() - start
    # The place a launch holds is a tab too: the app runs once its own
    # window, mapped by its process, stands there.
    # A refused open says so in a notice that lasts seconds: watched while
    # waiting, so the refusal is reported in the words the person read.
    noticed = []

    def running():
        state = session.state()
        noticed.extend(n for n in state["chrome"].get("notifications") or [] if n not in noticed)
        return tile_kind(session, tab, state) == "Foreign" or None
    try:
        wait_for(f"{session.name}: {app} running in its window", running, OPEN_BOUND)
    except Failed:
        raise Failed(f"{session.name}: {app} has a tab ({tile_kind(session, tab)}) but is not running; "
                     f"notices while it opened {noticed}")
    return tab, seconds


def tile_kind(session: Session, tab: int, state: dict | None = None) -> str | None:
    """What `tab` of `session`'s room's workspace holds: `Foreign` once the
    app's own window stands in it, `LocalApp` while its launch holds it."""
    w = workspace(session, state) or {}
    for strip in w.get("strips", []):
        for group in strip.get("window_groups", []):
            for t in group.get("tabs", []):
                if t["id"] == tab:
                    return next(iter(t.get("tile") or {}), None)
    return None


def act(session: Session, app: str, operation: str, args: dict, timeout: float = 300):
    """Invoke `app`'s `operation` in `session`'s room, looked up again while
    the app republishes its surface."""
    def done():
        row = next((row for row in app_rows(session, app) if row.get("operation") == operation), None)
        if row is None:
            return None
        value = session.invoke(row, args)
        return None if value is PAIR.Stale else (value,)
    try:
        return wait_for(f"{session.name}: {app}'s {operation} in the room", done, timeout)[0]
    except Failed:
        ops = sorted({str(row.get("operation")) for row in app_rows(session, app)})
        raise Failed(f"{session.name}: {app} offers no {operation} in room {session.room}; it offers {ops}")


def join_with_invite(name: str, out: pathlib.Path, url: str, room: str, invite: str) -> tuple[Running, Session]:
    """A fresh device, refused by the door, that redeems `invite` through
    the join field as a person does."""
    running = Running(name, WORK / name, url, out)
    running.start(fresh=True, build=False)
    control = sda.control_of(running.state)
    wait_for(f"{name}'s control socket", lambda: os.path.exists(control) or None, sda.START)
    probe = Session(name, control, room)
    wait_for(f"{name} to answer", lambda: probe.state() or None, 300)
    probe.shell_action("peer.connect")
    time.sleep(2)
    PAIR.ok(control, {"op": "type", "text": invite})
    time.sleep(1)
    PAIR.ok(control, {"op": "key", "key": "Enter"})

    def held():
        t = probe.state()["workspace_tree"]
        joining = {str(r) for r in t.get("joining_rooms", []) or []}
        return probe if sda.workspaces_of(probe) and room not in joining and sda.installed(probe) else None
    try:
        wait_for(f"the Lounge to install on {name}", held, 900)
    except Failed:
        state = probe.state()
        print(f"{name} join card:", json.dumps(state["chrome"].get("peer_sync")), flush=True)
        print(f"{name} joining:", state["workspace_tree"].get("joining_rooms"), flush=True)
        probe.snapshot(out, "join-failed")
        rows = probe.catalogue()
        print(f"{name} workspaces of the Lounge: {len(sda.workspaces_of(probe))}; catalogue rows by room "
              f"{dict(collections.Counter(str(row.get('room')) for row in rows))}; apps in the Lounge "
              f"{sorted({str(row.get('application')) for row in rows if row.get('room') == room})}", flush=True)
        focused(running.state / "logs/shell.log", APP_WORDS, 300)
        raise Failed(f"{name} never held the Lounge")
    return running, probe


def make_own(lounge: Session, name: str) -> Session:
    """A workspace of the device's own, made as the overview's new-workspace
    row makes one; the view of it."""
    known = {str(w["room"]) for w in tree(lounge).get("workspaces", [])}
    lounge.shell_action("workspace.create", {"name": name})
    made = wait_for(f"{lounge.name}: workspace {name!r}", lambda: next(
        (w for w in tree(lounge).get("workspaces", []) if str(w["room"]) not in known and w.get("name") == name),
        None), 60)
    return Session(lounge.name, lounge.control, str(made["room"]))


# What explains an app that offers nothing, or a device that never joins.
APP_WORDS = ("collab", "seal", "encrypt", "key", "refus", "error", "panic", "warn", "so.tilo.chat",
             "so.tilo.tasks", "so.tilo.word-processor", "launch", "join", "enrol", "admit", "replay",
             "ready", "subscri", "diag::launch", "diag::replay", "accepted wayland toplevel", "workspace host session", "session ended",
             "session closed", "redial", "dial")
# Said many times a second by a room without a voice carrier: not the story.
NOISE = ("voice", "TIMING", "native participation", "NameLost")


def focused(path: pathlib.Path, words: tuple[str, ...], limit: int):
    """The last `limit` lines of `path` that mention any of `words`, less
    the noise, each run of one message folded: t keeps no outputs of a
    failed run, so what explains it is printed."""
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
        lower = line.lower()
        if any(word.lower() in lower for word in NOISE) or not any(word in lower for word in words):
            continue
        message = re.sub(r"\d+", "N", re.sub(r"^\S+Z\s+", "", line))
        if message == last:
            repeats += 1
            continue
        if repeats:
            kept.append(f"      (and {repeats} more like it)")
        kept.append(line[:700])
        last, repeats = message, 0
    print(f"--- {path}: {len(kept)} lines (last {min(limit, len(kept))})", flush=True)
    for line in kept[-limit:]:
        print(f"    {line}", flush=True)


def keep(source: pathlib.Path, into: pathlib.Path):
    """`source`, compressed, among the outputs."""
    import gzip
    import shutil
    try:
        with open(source, "rb") as src, gzip.open(into, "wb") as dst:
            shutil.copyfileobj(src, dst)
    except OSError as error:
        print(f"   (no copy of {source}: {error})", flush=True)


def main() -> int:
    os.environ.setdefault("RUST_LOG", "info,tilo_joined_room=debug,shell::shell::broker_services=debug,"
                          "shell::network::actor=debug,shell::shell::network=debug")
    os.environ.setdefault("TILO_RELAY_LOG", "info")
    base = pathlib.Path("/target") if pathlib.Path("/target").is_dir() else REPO / "target"
    out = (base / "lounge-switch-local" / time.strftime("%Y%m%d-%H%M%S")).resolve()
    out.mkdir(parents=True, exist_ok=True)
    # The sessions' whole states (apps, runtimes) stay beside the outputs,
    # not in them: a failure keeps the outputs, and their logs are copied in.
    global WORK
    WORK = out.parent.parent / "lounge-switch-states" / out.name
    WORK.mkdir(parents=True, exist_ok=True)
    site = lil.Site(WORK, None)
    results: list[tuple[str, bool]] = []
    timings: dict[str, list[float]] = {}
    began = time.monotonic()

    def check(name: str, passed: bool, detail: str):
        print(f"{'ok  ' if passed else 'FAIL'} {name}: {detail}", flush=True)
        results.append((name, passed))

    def timed(kind: str, seconds: float):
        timings.setdefault(kind, []).append(seconds)

    def attempt(name: str, body):
        """Run one check's body; a Failed it raises is that check's FAIL."""
        try:
            body()
        except Failed as failure:
            check(name, False, str(failure))

    def phase(what: str):
        print(f"--- {what} ({time.monotonic() - began:.0f} s in)", flush=True)

    a = b = c = None
    try:
        # --- The Lounge, invite-only, with A a member of it --------------------
        phase("site and A")
        site.start("open")
        url = site.site if site.site.endswith("/") else site.site + "/"
        a = Running("a", WORK / "a", url, out)
        a.start(fresh=True, build=False)
        la = a.joined()
        room = la.room
        site.stop()
        site.start("invite", fresh=False)
        la = wait_for("A to hold the Lounge again after the site restarted",
                      lambda: la if sda.installed(la) else None, sda.START)
        shipped = a.shipped()
        print(f"the session ships {len(shipped)} apps: {sorted(shipped)}", flush=True)

        # --- A's own workspace --------------------------------------------------
        phase("A's own workspace")
        oa = make_own(la, "Own of A")
        try:
            wait_for("A's own room synced with its host", lambda: room_sync(oa).get(oa.room) == "synced", 120)
            check("own-a", oa.room != room, f"A's own workspace is room {oa.room}, synced with A's own host")
        except Failed:
            check("own-a", False, f"A's own room {oa.room} stands {room_sync(oa).get(oa.room)!r}")
        print("A's workspaces:", [(w["id"], w.get("name"), w["room"]) for w in tree(la)["workspaces"]], flush=True)

        # --- Every app, in A's own workspace and in the Lounge ------------------
        phase("apps in A's own workspace")
        select(oa)
        apps_own = app_list(oa)
        missing = sorted(set(shipped) - apps_own)
        check("apps-listed-own", not missing,
              f"A's own workspace lists {len(apps_own)} apps" + (f"; missing {missing}" if missing else ""))
        for app in sorted(shipped):
            try:
                tab, seconds = open_app(oa, app)
                timed("open", seconds)
                check(f"open-own/{shipped[app]}", True, f"tab {tab} in A's own workspace in {seconds:.1f} s")
            except Failed as failure:
                check(f"open-own/{shipped[app]}", False, str(failure))
                oa.snapshot(out, f"open-own-{shipped[app]}")
        phase("apps in the Lounge on A")
        select(la)
        apps_lounge = app_list(la)
        check("apps-same", apps_lounge == apps_own,
              f"the Lounge lists {len(apps_lounge)} apps, A's own workspace {len(apps_own)}"
              + (f"; they differ by {sorted(apps_lounge ^ apps_own)}" if apps_lounge != apps_own else ""))
        for app in sorted(shipped):
            try:
                tab, seconds = open_app(la, app)
                timed("open", seconds)
                check(f"open-lounge/{shipped[app]}", True, f"tab {tab} in the Lounge in {seconds:.1f} s")
            except Failed as failure:
                check(f"open-lounge/{shipped[app]}", False, str(failure))
                la.snapshot(out, f"open-lounge-{shipped[app]}")
        own_apps = {app for app in tabs(oa).values()}
        check("windows-own-kept", set(shipped) <= own_apps,
              f"A's own workspace holds windows of {len(own_apps)} apps after opening the Lounge's")

        # --- Data in A's own workspace and in the Lounge ------------------------
        items: dict[str, dict[str, str]] = {"own": {}, "lounge": {}}

        def write(view: Session, where: str):
            select(view)
            label = "Own" if where == "own" else "Lounge"
            text = f"{label} message {TOKEN}"
            try:
                _, channel = PAIR.channel_reader(view)
            except Failed as failure:
                offered = {app: sorted({str(row.get("operation")) for row in app_rows(view, app)})
                           for app in (CHAT, NOTES, TASKS, SHEETS, WORDS)}
                def logical(row):
                    try:
                        return bytes.fromhex(str(row.get("resource", "")).rsplit(":", 1)[-1]).decode()[:60]
                    except ValueError:
                        return str(row.get("resource"))[:60]
                chat_rows = sorted({f"{row.get('operation')}@{logical(row)}" for row in app_rows(view, CHAT)
                                    if row.get("operation") in ("read-messages", "post", "create-channel", "read")})
                rows = view.catalogue()
                by_app = dict(collections.Counter(
                    f"{row.get('application')}@{str(row.get('room'))[:8]}" for row in rows
                    if row.get("application") in (CHAT, NOTES, TASKS, SHEETS, WORDS)))
                check(f"data-{where}/chat", False, f"{failure}; Chat offers {chat_rows}; in room {view.room} "
                      f"the apps offer {offered}; rooms {room_sync(view)}; the device's catalogue holds "
                      f"{len(rows)} rows (complete: {getattr(view, 'last_completion', None)}), these apps' by app@room {by_app}")
                view.snapshot(out, f"no-channel-{where}")
                return
            try:
                view.chat("post", {"channel": channel, "text": text})
            except Failed as failure:
                ops = sorted({str(row.get("operation")) for row in app_rows(view, CHAT)})
                check(f"data-{where}/chat", False, f"{failure}; Chat offers {ops} in room {view.room}")
            else:
                items[where]["chat"] = text
            items[where]["channel"] = channel
            for app, operation, key, args in (
                    (NOTES, "new-note", "note", lambda t: {"title": t}),
                    (TASKS, "create", "task", lambda t: {"title": t}),
                    (SHEETS, "new-sheet", "sheet", lambda t: {"name": t}),
                    (WORDS, "append-paragraph", "document", lambda t: {"text": t})):
                token = f"{label} {key} {TOKEN}"
                answer = None
                try:
                    answer = act(view, app, operation, args(token))
                    items[where][key] = token
                    row = wait_for(f"A to list {token!r}", lambda: holds(view, token), SYNC_BOUND)
                    check(f"data-{where}/{key}", True, f"A lists {token!r}: {str(row.get('id'))[-60:]}")
                except Failed as failure:
                    listed = [str(row.get("title") or row.get("label") or row.get("id"))[:60]
                              for row in view.catalogue() if content_room(row) == view.room][:12]
                    check(f"data-{where}/{key}", False, f"{failure}; {operation} answered "
                          f"{json.dumps(answer, ensure_ascii=False, default=str)[:300]}; the room lists {listed}")

        phase("data in A's own workspace")
        write(oa, "own")
        phase("data in the Lounge on A")
        write(la, "lounge")

        # --- B joins -----------------------------------------------------------------
        phase("B joins")
        select(la)
        issued = la.shell_action("invitation.issue", {
            "role": "editor", "expires_in_seconds": 21600, "invitee_name": "Second device", "confirm": True})
        invite = (issued or {}).get("invite")
        if not invite:
            raise Failed(f"invitation.issue answered {issued}")
        b, lb = join_with_invite("b", out, url, room, invite)
        check("join-b", True, f"B holds the Lounge {room}")
        ob = make_own(lb, "Own of B")
        try:
            wait_for("B's own room synced with its host", lambda: room_sync(ob).get(ob.room) == "synced", 120)
            check("own-b", True, f"B's own workspace is room {ob.room}, synced")
        except Failed:
            check("own-b", False, f"B's own room {ob.room} stands {room_sync(ob).get(ob.room)!r}")
        rooms_a = {str(w["room"]) for w in tree(la)["workspaces"]}
        rooms_b = {str(w["room"]) for w in tree(lb)["workspaces"]}
        check("own-private", oa.room not in rooms_b and ob.room not in rooms_a,
              f"A holds {sorted(rooms_a)}, B holds {sorted(rooms_b)}")
        select(ob)
        open_app(ob, NOTES)
        b_note = f"B own note {TOKEN}"
        attempt("data-own-b/note", lambda: (act(ob, NOTES, "new-note", {"title": b_note}),
                                            wait_for("B to list its own note", lambda: holds(ob, b_note), SYNC_BOUND),
                                            check("data-own-b/note", True, f"B lists {b_note!r}")))

        # --- The Lounge's windows and data on B ------------------------------------
        phase("the Lounge on B")
        # The Lounge's layout is personal: each device keeps its own windows.
        # A shares one of them on purpose; B shows it as A's tab, and none of
        # A's other windows, and loses it once A stops sharing. A Tilo app's
        # window is shared as the same app on the same record, marked with
        # A's presence: B's own window of that app stands in A's tab
        # (`Foreign`), where an outside window would be A's stream of it
        # (`HostedWindow`).
        def own_windows() -> dict:
            """A's own windows in the Lounge, tab to app."""
            return {t: app for t, app in tabs(la).items() if tile_kind(la, t) == "Foreign"}

        def window_to_share(own: dict) -> int:
            tab = next((t for t, app in own.items() if app == NOTES), next(iter(own), None))
            if tab is None:
                raise Failed(f"A has no window of its own in the Lounge to share: {tabs(la)}")
            return tab

        def window_action(session: Session, verb: str, tab: int):
            group = next((g["id"] for s in workspace(session)["strips"] for g in s["window_groups"]
                          if any(t["id"] == tab for t in g["tabs"])), None)
            address = f"tab.{verb}:{group}:{tab}"
            if group is None or session.invoke({"provider": "desktop-windows", "id": address}) is PAIR.Stale:
                raise Failed(f"{session.name} offers no {address} (its tabs {tabs(session)})")

        def tile_of(session: Session, tab: int) -> dict | None:
            for strip in (workspace(session) or {}).get("strips", []):
                for group in strip.get("window_groups", []):
                    for t in group.get("tabs", []):
                        if t["id"] == tab:
                            return t.get("tile")
            return None

        def share_with_b(tab: int, app: str) -> float:
            """A shares its window `tab`; the seconds until B's own window of
            `app` stands in A's tab."""
            select(la)
            window_action(la, "share", tab)
            select(lb)
            start = time.monotonic()
            try:
                wait_for(f"B to show A's shared {app} window as its own {app} in A's tab",
                         lambda: tabs(lb).get(tab) == app and tile_kind(lb, tab) == "Foreign",
                         SYNC_BOUND)
            except Failed:
                raise Failed(f"A shared tab {tab} ({app}); B shows {tabs(lb)}, A's tab there holding "
                             f"{json.dumps(tile_of(lb, tab), default=str)[:300]}")
            return time.monotonic() - start

        def shared_windows():
            select(la)
            own = own_windows()
            tab = window_to_share(own)
            app = own[tab]
            took = share_with_b(tab, app)
            timed("sync", took)
            leaked = sorted(t for t in tabs(lb) if t in own and t != tab)
            if leaked:
                raise Failed(f"B shows A's windows {leaked}, which A did not share")
            check("shared-windows", True, f"B shows A's shared {app} window as A's tab, its own {app} "
                  f"in it {took:.1f} s after A shared it, and none of A's other {len(own) - 1} windows")
            select(la)
            window_action(la, "unshare", tab)
            select(lb)
            try:
                wait_for("B to drop A's tab once A stopped sharing", lambda: tab not in tabs(lb), SYNC_BOUND)
            except Failed:
                raise Failed(f"A stopped sharing tab {tab}; B still shows {tabs(lb)}")
            check("shared-windows/stopped", True, f"B dropped A's {app} tab once A stopped sharing it")
        attempt("shared-windows", shared_windows)
        select(lb)
        lounge_items = items["lounge"]
        if "chat" in lounge_items:
            attempt("sync/chat", lambda: (
                wait_for("A's Lounge message on B", lambda: PAIR.reads_message(
                    lb, lounge_items["channel"], lounge_items["chat"]), SYNC_BOUND),
                check("sync/chat", True, f"B reads {lounge_items['chat']!r}")))
        for key in ("note", "task", "sheet", "document"):
            if key not in lounge_items:
                continue
            token = lounge_items[key]
            attempt(f"sync/{key}", lambda: (
                wait_for(f"B to list {token!r}", lambda: holds(lb, token), SYNC_BOUND),
                check(f"sync/{key}", True, f"B lists {token!r} in the Lounge")))

        phase("live edits in the Lounge")
        for key, writer, reader, app, operation, arg in (
                ("live-task", la, lb, TASKS, "create", "title"),
                ("live-note", lb, la, NOTES, "new-note", "title")):
            token = f"Lounge {key} {TOKEN} by {writer.name}"

            def live():
                act(writer, app, operation, {arg: token})
                start = time.monotonic()
                wait_for(f"{reader.name} to list {token!r}", lambda: holds(reader, token), SYNC_BOUND)
                timed("sync", time.monotonic() - start)
                lounge_items[key] = token
                check(f"live/{key}", True, f"{reader.name} lists {token!r} written on {writer.name} "
                      f"{time.monotonic() - start:.1f} s later")
            attempt(f"live/{key}", live)

        def leaks(session: Session, token: str, allowed: str | None) -> list:
            return [(r, str(row.get("id"))[-60:]) for r, row in holding(session, token) if r != allowed]
        for key, token in items["own"].items():
            if key == "channel":
                continue
            found = leaks(la, token, oa.room) + leaks(lb, token, None)
            check(f"private/{key}", not found,
                  f"A's own {key} {token!r} is in no other room on A nor anywhere on B"
                  + (f"; found {found}" if found else ""))
        found = leaks(la, b_note, None) + leaks(lb, b_note, ob.room)
        check("private/b-note", not found, f"B's own note is nowhere on A" + (f"; found {found}" if found else ""))

        # --- Presence --------------------------------------------------------------
        phase("presence")
        b_on_a = a_on_b = None
        try:
            for session in (la, lb):
                PAIR.arrive(session)
            a_on_b = PAIR.check_roster(la, lb)
            b_on_a = PAIR.other_person(la)
            check("presence-roster", True, f"A sees {b_on_a and b_on_a['label']!r}, B sees {a_on_b['label']!r}")
        except Failed as failure:
            check("presence-roster", False, str(failure))
        # In a personal layout no window is both devices' own: a pointer
        # crosses only inside a window shared on purpose, which B shows under
        # A's tab. A shares one, points into it, and B draws A's pointer at
        # the same place on its window of it.
        def presence_pointer():
            select(la)
            own = own_windows()
            tab = window_to_share(own)
            app = own[tab]
            try:
                share_with_b(tab, app)
                window_action(la, "focus", tab)
                window_action(lb, "focus", tab)
                tile_a = wait_for(f"A's shared {app} window on A's screen", lambda: la.tiles().get(tab))
                tile_b = wait_for(f"A's shared {app} window on B's screen", lambda: lb.tiles().get(tab))
                PAIR.pointer_reaches(la, lb, a_on_b, tab, tile_a, tile_b)
                scope = next((row.get("room") for row in lb.state()["peers"].get("pointers") or []
                              if row.get("user") == a_on_b["id"]), None)
                check("presence-pointer", True, f"A's pointer reaches B on A's shared {app} window "
                      f"(tab {tab}) in scope {scope}")
            finally:
                try:
                    select(la)
                    window_action(la, "unshare", tab)
                except Failed as failure:
                    print(f"   (A could not stop sharing tab {tab}: {failure})", flush=True)
        if a_on_b is not None:
            attempt("presence-pointer", presence_pointer)

        def presence_in_own(viewer: Session, other: dict | None, mover: Session, label: str):
            select(viewer)
            # The other one moves about the Lounge meanwhile.
            x, y, w, h = PAIR.chat_window(mover)[1]
            PAIR.wiggle(mover, x + w / 2, y + h / 2)
            time.sleep(3)
            state = viewer.state()
            roster = [row for row in state["peers"].get("roster") or []
                      if other is not None and row.get("id") == other["id"]]
            pointers = [row for row in state["peers"].get("pointers") or []
                        if row.get("room") == viewer.room]
            check(f"presence-own/{label}", not roster and not pointers,
                  f"{label}'s own workspace shows no one from the Lounge"
                  + (f"; roster {roster}" if roster else "") + (f"; pointers {pointers}" if pointers else ""))
        attempt("presence-own/a", lambda: presence_in_own(oa, b_on_a, lb, "a"))
        attempt("presence-own/b", lambda: presence_in_own(ob, a_on_b, la, "b"))
        for session in (la, lb):
            select(session)

        # --- Switching back and forth ---------------------------------------------
        phase("switching")
        expected = {}
        for view in (oa, la, ob, lb):
            expected[(view.name, view.room)] = (tabs(view), app_list(view))
        stuck = []
        for i in range(SWITCHES):
            for own, lounge in ((oa, la), (ob, lb)):
                for view in (own, lounge):
                    want_tabs, want_apps = expected[(view.name, view.room)]
                    try:
                        seconds = select(view, want_tabs, want_apps)
                        timed("switch", seconds)
                        print(f"   switch {i + 1}: {view.name} to {'own' if view is own else 'Lounge'} "
                              f"in {seconds:.2f} s", flush=True)
                        if seconds > SWITCH_BOUND:
                            stuck.append(f"{view.name} {i + 1} {'own' if view is own else 'Lounge'}: {seconds:.2f} s")
                    except Failed as failure:
                        stuck.append(str(failure))
                        view.snapshot(out, f"switch-{i}")
                    if errors(view):
                        stuck.append(f"{view.name}: error notifications {errors(view)}")
        switches = timings.get("switch", [])
        check("switch", not stuck and len(switches) == SWITCHES * 4,
              f"{len(switches)} switches, slowest {max(switches, default=0):.2f} s, "
              f"median {sorted(switches)[len(switches) // 2] if switches else 0:.2f} s (bound {SWITCH_BOUND} s)"
              + (f"; {stuck}" if stuck else ""))

        # --- C opens the Lounge cold -----------------------------------------------
        phase("C joins cold")
        select(la)
        issued = la.shell_action("invitation.issue", {
            "role": "editor", "expires_in_seconds": 21600, "invitee_name": "Third device", "confirm": True})
        invite = (issued or {}).get("invite")
        if not invite:
            raise Failed(f"the second invitation.issue answered {issued}")
        c, lc = join_with_invite("c", out, url, room, invite)
        check("join-c", True, f"C holds the Lounge {room}")
        select(lc)
        if "chat" in lounge_items:
            attempt("cold/chat", lambda: (
                wait_for("the Lounge's message on C", lambda: PAIR.reads_message(
                    lc, lounge_items["channel"], lounge_items["chat"]), SYNC_BOUND * 3),
                check("cold/chat", True, f"C reads {lounge_items['chat']!r}")))
        for key, token in lounge_items.items():
            if key in ("chat", "channel"):
                continue
            attempt(f"cold/{key}", lambda: (
                wait_for(f"C to list {token!r}", lambda: holds(lc, token), SYNC_BOUND * 3),
                check(f"cold/{key}", True, f"C lists {token!r}")))
        found = [(key, leaks(lc, token, None)) for key, token in items["own"].items() if key != "channel"]
        found = [entry for entry in found if entry[1]] + ([("b-note", leaks(lc, b_note, None))]
                                                          if leaks(lc, b_note, None) else [])
        check("cold/private", not found, "C finds no own item of A or B" + (f"; found {found}" if found else ""))

        # --- After it all ------------------------------------------------------------
        phase("end")
        for own, lounge in ((oa, la), (ob, lb)):
            problems = []
            if errors(lounge):
                problems.append(f"error notifications {errors(lounge)}")
            for view in (own, lounge):
                want_tabs, _ = expected[(view.name, view.room)]
                have = tabs(view)
                lost = sorted(set(want_tabs) - set(have))
                if lost:
                    problems.append(f"room {view.room} lost tabs {lost}")
            unsynced = {r: s for r, s in room_sync(lounge).items() if s != "synced"}
            if unsynced:
                problems.append(f"rooms not synced {unsynced}")
            check(f"end/{lounge.name}", not problems, "no errors, every window kept, every room synced"
                  + (f"; {problems}" if problems else ""))
        for session in (oa, la, ob, lb, lc):
            session.snapshot(out, f"end-{'lounge' if session.room == room else 'own'}")
    except (Failed, OSError) as failure:
        check("setup", False, repr(failure))
        for running in (a, b, c):
            if running is not None:
                sda.tail(out / f"{running.name}.log", 10)
    finally:
        for running in (a, b, c):
            if running is not None:
                running.stop()
                keep(running.state / "logs/shell.log", out / f"{running.name}-shell.log.gz")
        if not all(passed for _, passed in results):
            for running in (a, b, c):
                if running is not None:
                    focused(running.state / "logs/shell.log", APP_WORDS, 2500)
                    # How the session ended, if it did, and its last words
                    # unfiltered: a shell that went quiet says nothing the
                    # filter keeps.
                    process = getattr(running, "process", None)
                    print(f"--- {running.name}'s session: "
                          f"{'still running' if process is None or process.poll() is None else f'exited {process.poll()}'}",
                          flush=True)
                    try:
                        tail = (running.state / "logs/shell.log").read_text(errors="replace").splitlines()[-40:]
                    except OSError:
                        tail = []
                    for line in tail:
                        print(f"    | {line[:400]}", flush=True)
            focused(site.dir / "site.log", tuple(lil.SITE_WORDS) + ("diag::",), 300)
        site.stop()
        keep(site.dir / "site.log", out / "site.log.gz")
    for kind, values in sorted(timings.items()):
        print(f"timing {kind}: {len(values)} x, slowest {max(values):.2f} s, total {sum(values):.1f} s", flush=True)
    print(f"took {time.monotonic() - began:.0f} s", flush=True)
    failed = [name for name, passed in results if not passed]
    print(("RESULT ok" if not failed else f"RESULT failed: {', '.join(failed)}") + f" ({out})", flush=True)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
