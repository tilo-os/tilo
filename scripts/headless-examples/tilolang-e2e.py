#!/usr/bin/env python3
"""tilolang apps end to end, as real and independent Tilo apps.

Builds the notes, chat, tasks and today packages with `tilolang build`, installs
them into a headless nested Tilo session (dist/dev/nested-session.sh) that
renders on lavapipe and never opens this machine's GPU (`--software`), and
checks through the semantic control surface (tilo-control) what an installed
app gets on a real system:

  registry   each package is an application under its own id, name and category
  launch     each runs its own process from the tilolang runtime bundle, in its
             own sandbox (user, mount and pid namespaces of its own), and the
             compositor admits its window under its own app id: a sandboxed
             client reaches the compositor only through verified launch custody
  gpu        the shell, its render service and every app load no Vulkan driver
             but lavapipe
  actions    each app publishes its actions; a "New ..." action changes the
             app's record (its history gains the change)
  hosts      each app's window holds none of its data (TILO_TILOLANG_HOSTED): its
             instance runs in a tilolang host, and the apps of one publisher in one
             workspace share one host process, handed every window's instance
  hosted-here the session's own workspace is one this device holds, so no app waits
             on a host: each shows its own state as first opened (first-*.png)
  snapshot   a PNG of the nested session per app, in OUT
  windows    a second window of Tasks, opened as a person opens one from search (its row
             with Ctrl+Enter), is a second window of the same process and its instance runs
             in the same host; a task typed into either window shows in the other at once, and
             each window names the controls of its own content where it stands
  titles     each tab is named by the page its window shows, not by its app: the two windows of
             Tasks read Today, the place both stand on, not Tasks, the app's name, so a person
             reads which page a window is at without opening it (titles-window-*.png)
  today      Today's Tasks card reads Tasks' own record: the tasks typed into Tasks' Today
             show in Today's window, which holds none of them itself (today-*.png); one is
             marked done by its box, a checkbox Tasks' window names ("Mark ... done"), another
             from its row's menu, pressed by name as a tool presses it, and Today's "Morning
             cleanup", pressed by name, runs on them and asks the person before changing Tasks
             and Notes
  places     a window grouped beside another, too narrow for Tasks' sidebar, still reaches its
             places through its own toggle, pressed by name as a tool presses a control, though
             another window goes by that name too: Inbox chosen there, the tabs read Today and
             Inbox (places-*.png)
  reopened   a task Tasks holds before the session stops shows in Today's card after it starts
             again and Today opens: Today reads what Tasks' record already held, not only what
             arrives while it is open (today-reopened.png)
  record-open a task no window shows opens from search: Enter on its row takes a window of
             Tasks to it, the task as its page, its tab named by it; Ctrl+Enter opens a window
             of its own beside, named by the task (record-open*.png)
  native     native Notes (so.tilo.notes), installed beside them, launches and
             presents a frame (native-notes.png)
  activation "Show this window" answers for tilolang Notes as for native Notes
  read       the door's `read` of tilolang Notes lists the workspace's records, and
             the notebook's read lists its notes by the ids a read takes
  idle       each app untouched for 10 s, native Notes and the tilolang host that
             runs the apps' instances included: every thread of them sleeps
  routes     the broker's route access, as the attested launch of an app: the
             collaboration probe (crates/dev/collab-probe) is installed as Today
             (which declares that it reads Notes' kinds) and as Tasks (which
             declares no read), in a second start of the same throwaway session.
             Tasks cannot join Notes' route at all; Today joins it to read, and
             its write is refused.

Usage:
  scripts/headless-examples/tilolang-e2e.py [--no-build] [--out DIR] [--keep]

Without --no-build it builds the nested session remotely (nested-session.sh:
with cargo), and the tilolang tool and the probe with cargo. Each run
uses a fresh throwaway session state in OUT/state and removes its compositor,
runtime directory and processes at the end; --keep leaves the session running
(stop it with `kill $(cat OUT/state/session.pid)`). The verdict is OUT/summary.json; the
exit status is non-zero when a check fails. Never touches the live desktop.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import struct
import subprocess
import sys
import time
import zlib
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).resolve().parent))
# The session this checks and the tools that act in it: one harness for every
# check of a desktop with the tree's apps (scripts/headless-examples/real_apps.py).
from real_apps import (BUILD, Failed, Session, prepare, run,  # noqa: E402
                       start_session, stop_session, until)


def app_id(name: str) -> str:
    """The application id of the tilolang package `name` (its directory under apps/tl)."""
    return f"so.tilo.lang.{name}"


BIN = BUILD / "bin"
DESK_RESOURCE = "desktop:desk"
APPS = ["notes", "chat", "tasks", "today"]
# The native app installed beside them, to hold the tilolang apps to what it does.
NATIVE = "so.tilo.notes"
PROBE_RUNTIME = "0.1.0+probe"
# The routes the probe tries as each app: Today declares that it reads Notes'
# kinds; Tasks declares no read.
PROBE_ROUTES = {
    "today": ["so.tilo.lang.notes", "so.tilo.lang.today"],
    "tasks": ["so.tilo.lang.notes"],
}


# --- /proc --------------------------------------------------------------------

def children(pid: int) -> list[int]:
    found = []
    for task in Path(f"/proc/{pid}/task").glob("*"):
        try:
            found += [int(c) for c in (task / "children").read_text().split()]
        except OSError:
            pass
    return found


def descendants(pid: int) -> list[int]:
    out, pending = [], [pid]
    while pending:
        for child in children(pending.pop()):
            out.append(child)
            pending.append(child)
    return out


def environ(pid: int) -> dict:
    try:
        raw = Path(f"/proc/{pid}/environ").read_bytes()
    except OSError:
        return {}
    pairs = (item.split(b"=", 1) for item in raw.split(b"\0") if b"=" in item)
    return {k.decode(errors="replace"): v.decode(errors="replace") for k, v in pairs}


def exe(pid: int) -> str:
    try:
        return os.readlink(f"/proc/{pid}/exe")
    except OSError:
        return ""


def cmdline(pid: int) -> list[str]:
    try:
        return Path(f"/proc/{pid}/cmdline").read_bytes().decode(errors="replace").split("\0")
    except OSError:
        return []


def namespaces(pid: int) -> dict:
    out = {}
    for ns in ("user", "mnt", "pid"):
        try:
            out[ns] = os.readlink(f"/proc/{pid}/ns/{ns}")
        except OSError:
            out[ns] = None
    return out


def vulkan_drivers(pid: int) -> list[str] | None:
    """The Vulkan drivers mapped into `pid`, or None when its maps are not
    readable (a non-dumpable process; `sudo -n` is tried once)."""
    try:
        maps = Path(f"/proc/{pid}/maps").read_text()
    except OSError:
        result = subprocess.run(["sudo", "-n", "cat", f"/proc/{pid}/maps"], text=True, capture_output=True)
        if result.returncode != 0:
            return None
        maps = result.stdout
    return sorted({m for m in re.findall(r"libvulkan_[a-z_]+\.so", maps)})


def switches(pid: int) -> dict[str, int]:
    """Context switches of each thread of `pid`, by `tid comm`."""
    out = {}
    for task in Path(f"/proc/{pid}/task").glob("*"):
        try:
            comm = (task / "comm").read_text().strip()
            total = sum(
                int(line.split()[1])
                for line in (task / "status").read_text().splitlines()
                if line.startswith(("voluntary_ctxt_switches", "nonvoluntary_ctxt_switches"))
            )
            out[f"{task.name} {comm}"] = total
        except OSError:
            pass
    return out


def host_pids(session_pid: int) -> list[int]:
    """The tilolang hosts of the session."""
    return [p for p in descendants(session_pid) if exe(p).endswith("/bin/tilolang-host")]


# --- checks ---------------------------------------------------------------------

def check_registry(session: Session) -> dict:
    # A cold query starts the index's refresh and may answer before it has
    # every application (docs/headless-testing.md): ask again until it does.
    found = {}
    for name in APPS:
        hits = until(f"{app_id(name)} among the session's applications", lambda: [
            a for a in session.actions(app_id(name))
            if a.get("provider") == "applications" and a.get("id") == app_id(name)
        ], 30, 0.5)
        found[app_id(name)] = {"title": hits[0]["title"], "category": hits[0].get("subtitle")}
    return found


def launch_all(session: Session) -> dict:
    for name in APPS:
        session.invoke("applications", app_id(name))
    def admitted():
        windows = {w.get("app_id"): w for w in session.windows()}
        return windows if all(app_id(n) in windows for n in APPS) else None
    windows = until("every app's window", admitted)
    until("every app answering with its own actions",
          lambda: all(session.answering(app_id(n)) for n in APPS), 30)
    return {
        app_id(n): {"group": windows[app_id(n)]["group"], "tab": windows[app_id(n)]["tab"]}
        for n in APPS
    }


def check_processes(session: Session, session_pid: int) -> dict:
    tree = descendants(session_pid)
    shell = next(p for p in tree if exe(p).endswith("/bin/tilo-session") and "__render-service" not in cmdline(p))
    render = [p for p in tree if "__render-service" in cmdline(p)]
    shell_ns = namespaces(shell)
    out = {"shell": {"pid": shell, "vulkan": vulkan_drivers(shell)}, "apps": {}}
    out["render_service"] = [{"pid": p, "vulkan": vulkan_drivers(p)} for p in render]
    for name in APPS:
        procs = [p for p in tree if environ(p).get("TILO_PACKAGE_ID") == app_id(name)
                 and exe(p).endswith("/bin/tilolang-app")]
        if len(procs) != 1:
            raise Failed(f"{app_id(name)} runs in {len(procs)} tilolang runtime processes")
        pid = procs[0]
        env = environ(pid)
        ns = namespaces(pid)
        own = {k: ns[k] is not None and ns[k] != shell_ns[k] for k in ns}
        if not all(own.values()):
            raise Failed(f"{app_id(name)} shares a namespace with the shell: {ns} vs {shell_ns}")
        out["apps"][app_id(name)] = {
            "pid": pid,
            "exe": exe(pid),
            "module": env.get("TILO_APP_MODULE"),
            "icon": env.get("TILO_APP_ICON"),
            "own_namespaces": own,
            "vulkan": vulkan_drivers(pid),
        }
    for who in [out["shell"], *out["render_service"], *out["apps"].values()]:
        drivers = who["vulkan"]
        if drivers and any(d != "libvulkan_lvp.so" for d in drivers):
            raise Failed(f"pid {who['pid']} loaded a GPU's Vulkan driver: {drivers}")
    return out


def check_hosts(session: Session, processes: dict, session_pid: int) -> dict:
    """Every app's window is its window alone, and one host of the workspace and the apps'
    publisher runs every window's instance."""
    hosts = until("the workspace's tilolang host", lambda: host_pids(session_pid) or None, 30)
    if len(hosts) != 1:
        raise Failed(f"the apps of one publisher in one workspace run in {len(hosts)} hosts")
    for app, seen in processes["apps"].items():
        if environ(seen["pid"]).get("TILO_TILOLANG_HOSTED") != "1":
            raise Failed(f"{app}'s window holds its own data: it was not launched as a hosted window")
    def handed():
        lines = log_lines(session, "tilolang instance handed over")
        apps = {line.get("package_id") for line in lines}
        return lines if all(app in apps for app in processes["apps"]) else None
    lines = until("every window's instance handed to the host", handed, 30)
    return {"host": hosts[0], "handed": len(lines)}


def png_rgb(path: Path) -> tuple[int, int, int, bytes]:
    """A snapshot's width, height, channels and decoded pixels."""
    data = path.read_bytes()
    pos, width, height, channels, packed = 8, 0, 0, 0, bytearray()
    while pos + 12 <= len(data):
        length = struct.unpack(">I", data[pos:pos + 4])[0]
        kind, payload = data[pos + 4:pos + 8], data[pos + 8:pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            width, height, depth, colour = struct.unpack(">IIBB", payload[:10])
            channels = {2: 3, 6: 4}.get(colour, 0) if depth == 8 else 0
        elif kind == b"IDAT":
            packed += payload
    if not channels:
        raise Failed(f"{path} is no 8-bit RGB(A) PNG")
    raw, stride = zlib.decompress(bytes(packed)), width * channels
    out, above, offset = bytearray(), bytearray(stride), 0
    for _ in range(height):
        kind, row = raw[offset], bytearray(raw[offset + 1:offset + 1 + stride])
        offset += stride + 1
        for i in range(stride):
            left = row[i - channels] if i >= channels else 0
            corner = above[i - channels] if i >= channels else 0
            if kind == 1:
                row[i] = (row[i] + left) & 0xff
            elif kind == 2:
                row[i] = (row[i] + above[i]) & 0xff
            elif kind == 3:
                row[i] = (row[i] + (left + above[i]) // 2) & 0xff
            elif kind == 4:
                p = left + above[i] - corner
                pa, pb, pc = abs(p - left), abs(p - above[i]), abs(p - corner)
                row[i] = (row[i] + (left if pa <= pb and pa <= pc else above[i] if pb <= pc else corner)) & 0xff
        out += row
        above = row
    return width, height, channels, bytes(out)


def window_pixels(shot: tuple[int, int, int, bytes], rect: list[int]) -> bytes:
    """The pixels of `rect`, `[x, y, width, height]` on the desktop, in the snapshot `shot`."""
    width, height, channels, pixels = shot
    x, y, w, h = rect
    x0, y0, x1, y1 = max(x, 0), max(y, 0), min(x + w, width), min(y + h, height)
    if x1 - x0 < 64 or y1 - y0 < 64:
        raise Failed(f"a window at {rect} is not on the {width}x{height} desktop to be seen")
    return b"".join(pixels[(row * width + x0) * channels:(row * width + x1) * channels]
                    for row in range(y0, y1))


def search_beside(session: Session, title: str, beside: bool = True) -> None:
    """Open what is called `title` in a window of its own, as a person does: search
    (Super+Space), its name, and Ctrl+Enter on its row; without `beside`, Enter opens it
    where its app shows it."""
    session.control({"op": "key_down", "key": "Super"})
    session.control({"op": "key", "key": "Space"})
    session.control({"op": "key_up", "key": "Super"})
    session.control({"op": "type", "text": title})
    def settled():
        answer = session.control({"op": "search_rows"})["answer"]
        if answer["query"] != title or answer["searching"]:
            return None
        rows = answer["rows"]
        index = next((i for i, row in enumerate(rows) if row["title"] == title
                      and "window" not in (row.get("kind") or "").lower()), None)
        return (index, rows) if index is not None else None
    index, rows = until(f"search's row for {title!r}", settled, 30)
    for _ in range(index):
        session.control({"op": "key", "key": "ArrowDown"})
    if not beside:
        session.control({"op": "key", "key": "Enter"})
        return
    session.control({"op": "modifiers", "ctrl": True})
    try:
        session.control({"op": "key", "key": "Enter"})
    finally:
        session.control({"op": "modifiers", "ctrl": False})


def check_windows(session: Session, processes: dict, session_pid: int, out: Path) -> dict:
    """A second window of Tasks, opened from search as a person opens one, is a window of the
    process Tasks' first window is, and its instance runs in the host the first one's does. A
    task typed into the first window shows in the second at once, and each window names the
    controls of its own content: a tool or an assistive technology pressing one presses the
    window it stands in."""
    tasks = app_id("tasks")
    first = [w for w in session.windows() if w.get("app_id") == tasks]
    if len(first) != 1:
        raise Failed(f"{tasks} shows {len(first)} windows before a second is asked for")
    hosts = host_pids(session_pid)
    def handed() -> int:
        return len([line for line in log_lines(session, "tilolang instance handed over")
                    if line.get("package_id") == tasks])
    before_handed = handed()
    search_beside(session, "Tasks")
    windows = until(f"a second window of {tasks}", lambda: (lambda shown: shown if len(shown) == 2
                    else None)([w for w in session.windows() if w.get("app_id") == tasks]), 30)
    until("the second window's instance handed to the host", lambda: handed() > before_handed, 30)
    procs = [p for p in descendants(session_pid) if environ(p).get("TILO_PACKAGE_ID") == tasks
             and exe(p).endswith("/bin/tilolang-app")]
    if procs != [processes["apps"][tasks]["pid"]]:
        raise Failed(f"{tasks}'s second window started another process: {procs}")
    if host_pids(session_pid) != hosts:
        raise Failed(f"{tasks}'s second window's instance runs elsewhere: hosts {hosts} became "
                     f"{host_pids(session_pid)}")
    def shot(name: str | None) -> list[bytes]:
        """Each window in front in turn, as it shows then: the strip shows one at a time. Without a
        `name` nothing is captured: the pass only brings each window forward."""
        seen = []
        for i, window in enumerate(windows):
            focus(session, window)
            time.sleep(1.0)
            if name is None:
                continue
            rect = next(w["rect"] for w in session.windows() if w.get("tab") == window["tab"])
            path = out / f"tasks-window-{i}-{name}.png"
            session.control_cmd("snapshot", str(path))
            seen.append(window_pixels(png_rgb(path), rect))
        return seen
    # A window brought forward stands where the strip's pan puts it, and the strip already holds the
    # window it has just opened in view: this pass settles where each stands, so the two captures
    # below compare what the windows show and not where the strip settled.
    shot(None)
    before = shot("before")
    if shot("still") != before:
        raise Failed(f"{tasks}'s windows change while nothing happens: nothing could tell an edit")
    # Typed into the first window's own field, as a person adds a task there.
    focus(session, windows[0])
    time.sleep(0.5)
    session.control({"op": "type", "text": "Water the plants"})
    session.control({"op": "key", "key": "Enter"})
    after = until("the task in both of Tasks' windows", lambda: (lambda now: now if all(
        a != b for a, b in zip(now, before)) else None)(shot("after")), 30, 1.0)
    # Then into the second: the workspace's committed read of the first task reached both
    # instances, and each still sends what its person does next.
    focus(session, windows[1])
    time.sleep(0.5)
    session.control({"op": "type", "text": "Call the plumber"})
    session.control({"op": "key", "key": "Enter"})
    until("the second task in both of Tasks' windows", lambda: (lambda now: now if all(
        a != b for a, b in zip(now, after)) else None)(shot("next")), 30, 1.0)
    # What each window offers by name, listed while it is the one in front: the desk names the
    # controls of the window an app drew, so a window whose instance is not a tab of the desk's
    # names nothing of its content at all.
    typed = ["Water the plants", "Call the plumber"]
    listed = {}
    for window in windows:
        names = window_controls(session, window)
        listed[str(window["tab"])] = names
        missing = [title for title in typed if title not in names]
        if missing:
            raise Failed(f"{tasks}'s window {window['tab']} names no control of its own: "
                         f"{missing} not among the controls it lists: {names}")
    return {"windows": [{"group": w["group"], "tab": w["tab"]} for w in windows],
            "controls": listed,
            "process": procs[0], "host": hosts[0], "changed": len(after)}


def check_titles(session: Session, out: Path) -> dict:
    """Each tab is named by what its window shows, not by its app: the two windows of Tasks read
    Today, the place both stand on, where both read Tasks, the app's name, before. A person reads
    which page a window is at without opening it, and the app's own glyph says which app it is."""
    tasks = app_id("tasks")
    found = [w for w in session.windows() if w.get("app_id") == tasks]
    if len(found) != 2:
        raise Failed(f"{tasks} shows {len(found)} windows, not the two this check reads")
    titles = {w["tab"]: w.get("title") for w in found}
    if set(titles.values()) != {"Today"}:
        raise Failed(f"{tasks}'s tabs read {titles}: a tab names its app, not the page its window "
                     f"shows (both windows stand on Tasks' Today)")
    # Both tabs as the strip shows them, each window in front in turn: titles-*.png.
    for index, (tab, title) in enumerate(titles.items()):
        window = next(w for w in found if w.get("tab") == tab)
        focus(session, window)
        time.sleep(1.0)
        session.control_cmd("snapshot", str(out / f"titles-window-{index}.png"))
    return {"titles": {str(tab): title for tab, title in titles.items()},
            "page": "Today", "app_name": "Tasks"}


def press_in(session: Session, window: dict, name: str) -> None:
    """Press the control `name` `window` draws, by name, as a tool or an agent presses a control:
    the press reaches the window in front, and never another window's control of that name."""
    focus(session, window)
    until(f"window {window['tab']} in front", lambda: any(
        shown.get("tab") == window["tab"] and shown.get("active")
        for shown in session.windows()), 30)
    until(f"{name!r} among the controls of window {window['tab']}",
          lambda: name in window_controls(session, window), 30)
    session.click_named(name)


def tasks_titles(session: Session) -> list[str]:
    """What the tabs of Tasks' windows read, in the strip's order."""
    return sorted(w.get("title") for w in session.windows() if w.get("app_id") == app_id("tasks"))


def check_record_open(session: Session, out: Path) -> dict:
    """A task no window shows opens from search. With both windows of Tasks on Today, Enter on an
    Inbox task's row opens it where Tasks shows it: a window of Tasks goes to the Inbox, shows the
    task as its page and comes forward, its tab named by the task, and no window is added.
    Ctrl+Enter on the row then opens a window of its own beside it, as Ctrl+Enter opens an app's,
    showing the task's page, in front and named by the task."""
    tasks = app_id("tasks")
    title = "Book the ferry"
    def tasks_windows() -> list[dict]:
        return [w for w in session.windows() if w.get("app_id") == tasks]
    before = tasks_windows()
    if [w.get("title") for w in before] != ["Today"] * len(before):
        raise Failed(f"Tasks' windows do not all stand on Today: {before}")
    made = until("New task", lambda: app_action(session, tasks, "New task", "Document"))
    invoke_found(session, made, {"title": title})
    search_beside(session, title, beside=False)
    shown = until(f"a window of Tasks named {title!r}", lambda: next(
        (w for w in tasks_windows() if w.get("title") == title and w.get("active")), None), 30)
    if len(tasks_windows()) != len(before):
        raise Failed(f"Enter opened another window: {tasks_windows()}")
    time.sleep(1.0)
    session.control_cmd("snapshot", str(out / "record-open-in-place.png"))
    search_beside(session, title)
    def opened_beside() -> dict | None:
        new = [w for w in tasks_windows() if w["tab"] not in {b["tab"] for b in before}]
        return new[0] if len(new) == 1 and new[0].get("title") == title else None
    beside = until(f"a window of its own named {title!r}", opened_beside, 30)
    if not beside.get("active"):
        raise Failed(f"the window opened for the task is not in front: {beside}")
    time.sleep(1.0)
    session.control_cmd("snapshot", str(out / "record-open-beside.png"))
    # The task's page stands in the window: the controls of its fields, which its row has not.
    names: list[str] = []
    def shows_task() -> bool:
        names[:] = window_controls(session, beside)
        return "Add date" in names
    try:
        until(f"the window named {title!r} showing the task's page", shows_task, 30)
    except Failed:
        raise Failed(f"the window named {title!r} shows no task's page: {names}") from None
    return {"in_place": {"tab": shown["tab"], "title": shown.get("title")},
            "beside": {"tab": beside["tab"], "title": beside.get("title")},
            "titles": tasks_titles(session)}


def check_places(session: Session, out: Path) -> dict:
    """A window grouped beside another is too narrow for Tasks' sidebar, and still reaches its
    places: its own sidebar toggle shows them — pressed by name, as a tool presses a control, even
    though another window goes by that name too — and Inbox chosen there takes the window to the
    Inbox, so the two tabs read Today and Inbox."""
    tasks = app_id("tasks")
    window = next(w for w in session.windows() if w.get("app_id") == tasks)
    press_in(session, window, "Show sidebar")
    time.sleep(1.0)
    session.control_cmd("snapshot", str(out / "places-sidebar.png"))
    press_in(session, window, "Inbox")
    titles = until("the tabs to read Today and Inbox",
                   lambda: (read := tasks_titles(session)) == ["Inbox", "Today"] and read, 30)
    time.sleep(1.0)
    session.control_cmd("snapshot", str(out / "places-inbox.png"))
    return {"titles": titles}


def window_controls(session: Session, window: dict) -> list[str]:
    """The controls the desk names in `window`, with the window in front: what its app drew of its
    own content — its rows, its lists — named where the window stands."""
    focus(session, window)
    until(f"window {window['tab']} in front", lambda: any(
        shown.get("tab") == window["tab"] and shown.get("active")
        for shown in session.windows()), 30)
    time.sleep(0.5)
    rect = next((shown["rect"] for shown in session.windows()
                 if shown.get("tab") == window["tab"]), None)
    if rect is None:
        raise Failed(f"window {window['tab']} is not drawn on the desk to be read")
    x, y, width, height = rect
    return [target["name"] for target in session.targets()
            if x <= target["rect"][0] < x + width and y <= target["rect"][1] < y + height]


def today_shown(session: Session, out: Path, name: str) -> bytes:
    """Today's window in front, as it shows now, captured as today-`name`.png: its pixels."""
    today = app_id("today")
    window = next(w for w in session.windows() if w.get("app_id") == today)
    focus(session, window)
    time.sleep(1.0)
    rect = next(w["rect"] for w in session.windows() if w.get("tab") == window["tab"])
    path = out / f"today-{name}.png"
    session.control_cmd("snapshot", str(path))
    return window_pixels(png_rgb(path), rect)


def hold_today(session: Session, out: Path, held: list[bytes]) -> dict:
    """Capture Today's window before Tasks holds a task, into `held`."""
    held.append(today_shown(session, out, "before"))
    return {"before": str(out / "today-before.png")}


def named(session: Session) -> list[str]:
    """The controls the desk names now, by the names a tool presses them by."""
    return [target["name"] for target in session.targets()]


def check_today(session: Session, out: Path, before: bytes) -> dict:
    """Today's Tasks card, which held nothing before Tasks held a task, shows the tasks typed
    into Tasks' Today since: Today reads them from Tasks' own record, live, and its window names
    them as its own controls. Two of them are finished in Tasks: one by its box, the other from its
    row's menu, opened where the row stands and run by the command a tool presses by name, both
    standing in Tasks' history. Its "Morning cleanup" is a control of that window too, pressed here
    as a tool or an assistive technology presses it: with tasks done in Tasks it clears them and
    files "Morning: 2 done" in Notes, or asks the person's leave to change those two apps, which
    this check denies: an app's change of another app is made only with the person's leave
    (`docs/architecture/apps.md`)."""
    today = app_id("today")
    held = ["Water the plants", "Call the plumber"]
    # What Today's window offers by name: the card's rows, read off Tasks' record, and the
    # cleanup its header shows.
    focus(session, next(w for w in session.windows() if w.get("app_id") == today))
    until("Tasks' tasks in Today's card", lambda: (lambda shown: shown if all(
        title in shown for title in held) else None)(named(session)), 30)
    session.wait_target("Morning cleanup")
    done = mark_done(session, held[0])
    menu = menu_complete(session, held[1])
    # The tasks the two checks above finished: what the cleanup counts.
    finished = 2
    note = f"Morning: {finished} done"
    named_before = named(session)
    # The cleanup, pressed on Today's window: the app runs it, which either files its note in
    # Notes and clears the tasks done, or asks the person's leave for the two apps it changes.
    session.click_named("Morning cleanup")

    def ran():
        shown = named(session)
        if note in shown:
            return "answered"
        return "consent" if "Allow once" in shown else None
    how = until("Today's cleanup to run", ran, 30)
    denied = 0
    if how == "consent":
        # Each question the cleanup asks is denied, as a person who gives no leave answers it.
        quiet = time.monotonic() + 3
        while time.monotonic() < quiet:
            if "Allow once" in named(session):
                session.click_named("Deny")
                denied += 1
                quiet = time.monotonic() + 3
            time.sleep(0.3)
        for title in (title for title in held if title in named_before):
            if title not in named(session):
                raise Failed(f"{title!r} left Tasks without the note Morning cleanup files")
        if note in named(session):
            raise Failed("the note is named without the change it describes")
    # What the person sees of it, as the pictures the desk drew before and after.
    after = today_shown(session, out, "after")
    width = 4  # a snapshot holds RGBA pixels
    moved = sum(1 for i in range(0, min(len(after), len(before)), width)
                if after[i:i + width] != before[i:i + width]) / max(1, len(before) // width)
    return {"before": str(out / "today-before.png"), "after": str(out / "today-after.png"),
            "note": note if how == "answered" else None,
            "asked_consent": how == "consent", "denied": denied, "controls": len(named_before),
            "moved": round(moved, 3), "done": done, "menu": menu}


# The task Tasks holds when the session stops, which Today's card lists after it starts again.
KEPT_TASK = "Book the dentist"


def keep_task(session: Session) -> dict:
    """Type a task into the window of Tasks on Today, due there and not done, and see it list it:
    what Tasks' record holds when the session stops."""
    tasks = app_id("tasks")
    window = next(w for w in session.windows()
                  if w.get("app_id") == tasks and w.get("title") == "Today")
    focus(session, window)
    time.sleep(0.5)
    session.control({"op": "type", "text": KEPT_TASK})
    session.control({"op": "key", "key": "Enter"})
    until(f"{KEPT_TASK!r} in Tasks' window", lambda: KEPT_TASK in window_controls(session, window), 30)
    return {"task": KEPT_TASK}


def check_reopened(session: Session, out: Path) -> dict:
    """The session started again on the state the first start left, and Today opened in it:
    Today's Tasks card lists the task Tasks' record held before the stop. Today reads what that
    record already holds when it opens, not only what arrives while it is open."""
    today = app_id("today")
    window = session.launch(today)
    until(f"{KEPT_TASK!r} in Today's card", lambda: KEPT_TASK in window_controls(session, window), 60)
    today_shown(session, out, "reopened")
    return {"task": KEPT_TASK, "window": {"group": window["group"], "tab": window["tab"]},
            "shot": str(out / "today-reopened.png")}


def mark_done(session: Session, title: str) -> dict:
    """Mark the task `title` done by its box in Tasks' window, pressed by name as a screen reader
    or an agent presses it: the box is a checkbox of its own, named for what it does. The workspace
    takes the change, so it stands in Tasks' history; a change it refused would leave none."""
    tasks = app_id("tasks")
    focus(session, next(w for w in session.windows() if w.get("app_id") == tasks))
    box = f"Mark {title} done"
    session.wait_target(box)
    before = history_total(session, "tasks")
    refused = len([line for line in session.log().splitlines()
                   if "a change the workspace refuses" in line and tasks in line])
    session.click_named(box)
    after = until(f"{title!r} done in Tasks' history",
                  lambda: (lambda t: t if t > before else None)(history_total(session, "tasks")), 15)
    if len([line for line in session.log().splitlines()
            if "a change the workspace refuses" in line and tasks in line]) > refused:
        raise Failed(f"the workspace refused {title!r} marked done")
    return {"box": box, "history": [before, after]}


def menu_complete(session: Session, title: str) -> dict:
    """A task's row menu, opened where the row stands, and the command in it a tool presses by
    name as a screen reader presses it: Complete marks the task done, the menu closes on it, and
    the change stands in Tasks' history — the workspace took it. Nothing of the window may name
    Complete before the menu opens, so the press reaches the menu's command and no button of a
    page that says the same word."""
    tasks = app_id("tasks")
    focus(session, next(w for w in session.windows() if w.get("app_id") == tasks))
    session.wait_target(title)
    shown = named(session)
    if "Complete" in shown:
        raise Failed(f"the window names Complete before {title!r}'s menu opens: {shown}")
    session.click_named(title, verb="show-context-menu")
    session.wait_target("Complete")
    before = history_total(session, "tasks")
    session.click_named("Complete")
    after = until(f"{title!r} completed from its menu in Tasks' history",
                  lambda: (lambda t: t if t > before else None)(history_total(session, "tasks")), 15)
    if any(target["name"] == "Complete" for target in session.targets()):
        raise Failed(f"{title!r}'s menu stood open after Complete")
    return {"task": title, "command": "Complete", "history": [before, after]}


def invoke_found(session: Session, found: dict, args: dict | None = None) -> dict:
    """Invoke the action the listing printed, as the tools do: its own provider and id."""
    return session.invoke(found["provider"], found["id"], args)


def history_total(session: Session, name: str) -> int:
    history = until(f"{app_id(name)}'s history action",
                    lambda: app_action(session, app_id(name), "History", "View"))
    answer = invoke_found(session, history)
    try:
        return answer["receipt"]["update"]["output"]["value"]["total"]
    except (KeyError, TypeError):
        raise Failed(f"{app_id(name)}'s history answered without a total: {json.dumps(answer)}") from None


def check_actions(session: Session) -> dict:
    out = {}
    news = {"notes": "New note", "chat": "New channel", "tasks": "New task"}
    for name in APPS:
        published = until(
            f"{app_id(name)}'s actions",
            lambda: [a for a in session.actions(app_id(name)) if a.get("application") == app_id(name)],
        )
        out[app_id(name)] = {"published": len(published)}
    for name, title in news.items():
        action = until(f"{app_id(name)}'s {title!r}",
                       lambda: app_action(session, app_id(name), title, "Document"))
        before = history_total(session, name)
        state = invoke_found(session, action)["receipt"]["update"]["state"]
        after = until(f"{title!r} in {app_id(name)}'s history",
                      lambda: (lambda t: t if t > before else None)(history_total(session, name)), 15)
        out[app_id(name)].update(invoked=title, state=state, history=[before, after])
    return out


def value_of(answer: dict):
    try:
        return answer["receipt"]["update"]["output"]["value"]
    except (KeyError, TypeError):
        raise Failed(f"an answer without a value: {json.dumps(answer)[:2000]}") from None


def app_action(session: Session, app: str, title: str, kind: str | None = None):
    """The first action called `title` that the window of `app` publishes, of the kind its effect
    has when `kind` names one (`View`, `Document`). Its application is `app` exactly: so.tilo.notes
    never matches so.tilo.lang.notes."""
    for action in session.actions(title):
        if (action.get("application") == app and action.get("title") == title
                and kind in (None, action.get("operation_kind"))):
            return action
    return None


def focus(session: Session, window: dict) -> None:
    """Bring the tab of `window` to the front, as the desk's own verb does."""
    session.invoke("so.tilo.session", f"{DESK_RESOURCE}\u001ffocus", {"tab": str(window["tab"])})


def check_native(session: Session, out: Path) -> dict:
    """Native Notes launches beside the tilolang apps and presents a frame."""
    session.invoke("applications", NATIVE)
    windows = until(f"{NATIVE}'s window",
                    lambda: {w.get("app_id"): w for w in session.windows() if w.get("app_id") == NATIVE})
    session.answering(NATIVE, 30)
    # Connected: its window publishes its actions.
    until(f"{NATIVE}'s actions", lambda: app_action(session, NATIVE, "Show this window"), 60)
    w = windows[NATIVE]
    focus(session, w)
    time.sleep(0.5)
    path = out / "native-notes.png"
    session.control_cmd("snapshot", str(path))
    return {"group": w["group"], "tab": w["tab"], "snapshot": str(path)}


def check_activation(session: Session) -> dict:
    """"Show this window" answers for a tilolang app as it does for the native app."""
    out = {}
    for app in (NATIVE, app_id("notes")):
        action = until(f"{app}'s 'Show this window'", lambda: app_action(session, app, "Show this window"))
        out[app] = value_of(invoke_found(session, action))
    if out[app_id("notes")] != out[NATIVE]:
        raise Failed(f"'Show this window' answers differently from native Notes: {out}")
    return out


def check_read(session: Session) -> dict:
    """An agent or a script reads what a tilolang app holds through the door's `read`, as it
    reads a native app's content: the workspace's records, then one of them by its id."""
    action = until("tilolang Notes' 'Read the content'",
                   lambda: app_action(session, app_id("notes"), "Read the content"))
    answer = value_of(invoke_found(session, action))
    # Notes' root record, whatever its type is called: the one record of the app's own kinds.
    prefix = f"tl.{app_id('notes')}."
    notebook = next((r for r in answer.get("records", []) if r.get("kind", "").startswith(prefix)), None)
    if notebook is None:
        raise Failed(f"the workspace's read lists no record of Notes: {json.dumps(answer)[:2000]}")
    kind = notebook["kind"]
    read = value_of(invoke_found(session, action, {"item": notebook["item"]}))
    # The collection of notes, whatever the root calls it: its field listing Notes' notes.
    notes = next((f for f in read.get("fields", {}).values() if isinstance(f, dict)
                  and any(i.get("type") == "Note" for i in f.get("items", []))), {})
    if read.get("kind") != kind or not notes.get("items"):
        raise Failed(f"Notes' root record's read holds no notes: {json.dumps(read)[:2000]}")
    native = until("native Notes' 'Read the content'", lambda: app_action(session, NATIVE, "Read the content"))
    return {
        "records": len(answer["records"]),
        "notebook": {"label": read.get("label"), "notes": notes.get("count")},
        "native": sorted(value_of(invoke_found(session, native))),
    }


def snapshots(session: Session, windows: dict, out: Path, prefix: str = "") -> dict:
    shots = {}
    for name in APPS:
        w = windows[app_id(name)]
        focus(session, w)
        time.sleep(0.5)
        path = out / f"{prefix}{name}.png"
        session.control_cmd("snapshot", str(path))
        shots[app_id(name)] = str(path)
    return shots


def native_pid(session_pid: int) -> int | None:
    """The process of native Notes, when it runs."""
    return next((p for p in descendants(session_pid)
                 if environ(p).get("TILO_PACKAGE_ID") == NATIVE and Path(exe(p)).name == "notes"), None)


def check_idle(session: Session, processes: dict, session_pid: int, seconds: float = 10.0) -> dict:
    """Every app, native Notes and the tilolang host that runs the apps' instances, left untouched
    for 10 s: every thread of them sleeps. Nothing animates here and this session reports no
    per-window frame count, so a thread that woke woke for work nobody asked for."""
    time.sleep(3)
    pids = {app: info["pid"] for app, info in processes["apps"].items()}
    native = native_pid(session_pid)
    if native is None:
        raise Failed("native Notes is not running to be held to the same idle")
    pids[NATIVE] = native
    hosts = host_pids(session_pid)
    if not hosts:
        raise Failed("no tilolang host runs the apps' instances to be held to the same idle")
    pids.update({f"host {host}": host for host in hosts})
    before = {who: switches(pid) for who, pid in pids.items()}
    time.sleep(seconds)
    out, problems = {}, []
    for who, pid in pids.items():
        after = switches(pid)
        woke = {t: after[t] - before[who].get(t, 0) for t in after if after[t] != before[who].get(t, 0)}
        out[who] = {"switches": sum(woke.values()), "threads": woke}
        if woke:
            problems.append(f"{who} woke with nothing to do: {woke}")
    if problems:
        raise Failed(f"{'; '.join(problems)} ({json.dumps(out)})")
    return out


ANSI = re.compile(r"\x1b\[[0-9;]*m")


def log_lines(session: Session, pattern: str) -> list[dict]:
    """The session log's tracing lines matching `pattern`, each as its `key=value` fields."""
    out = []
    for line in ANSI.sub("", session.log()).splitlines():
        if re.search(pattern, line):
            out.append(dict(re.findall(r"(\w+)=(\S+)", line)))
    return out


def check_hosted_here(session: Session, windows: dict, out: Path) -> dict:
    """The session's own workspace is hosted by this device: this device holds it (the desk's
    dump names it among the workspaces kept here, not as a replica still on its way from
    another device), so its apps wait on no host and each shows its own state at once."""
    state = session.state_dump()
    held = {row["workspace"] for row in state.get("held_workspaces") or []}
    waiting = [w["name"] or w["room"] for w in state["workspace_tree"]["workspaces"]
               if w["id"] not in held]
    if waiting:
        raise Failed(f"the session's own workspace is held by another device, so its apps wait "
                     f"on that host: {waiting}")
    return {"held_workspaces": state["held_workspaces"],
            "snapshots": snapshots(session, windows, out, "first-")}


def probe_packages(state: Path, out: Path) -> list[Path]:
    """The probe as Today (whose surface declares that it reads Notes' kinds)
    and as Tasks (which declares no read): each package is the app's own, with
    the probe as its entrypoint and the routes to try in its payload. Bakes the
    probe's runtime bundle into the session, signed by its runtime publisher."""
    probe = REPO / "target/debug/tilo-collab-probe"
    if not probe.is_file():
        raise Failed(f"no probe at {probe} (cargo build -p collab-probe)")
    bundle = state / "runtimes" / PROBE_RUNTIME
    shutil.rmtree(bundle, ignore_errors=True)
    run([BIN / "tilo-runtime-bundle", "build", "--version", PROBE_RUNTIME, "--out", bundle,
         "--include-bin", probe, "--identity", state / "keys/publisher.json"])
    identity = Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local/state")) / "tilolang/publisher.json"
    tapps = []
    for name, routes in PROBE_ROUTES.items():
        staged = out / f"probe-{name}"
        shutil.rmtree(staged, ignore_errors=True)
        shutil.copytree(out / "build" / name / "payload", staged / "payload")
        (staged / "payload/probe-routes").write_text("\n".join(routes) + "\n")
        manifest = (out / "build" / name / "manifest.toml").read_text()
        # The probe runs as an app launched with that app's own custody, as any app's process
        # is: its entrypoint names the probe's runtime, not tilolang's, so the session starts it
        # as an app process rather than handing a tilolang host its instance.
        manifest, n = re.subn(r'(?m)^optional-runtimes = \["([^"]*)"\]',
                              f'optional-runtimes = ["\\1", "probe/{PROBE_RUNTIME}"]', manifest)
        manifest, m = re.subn(r'(?ms)^\[entrypoint\]\nruntime = "[^"]*"\nbin = "[^"]*"',
                              f'[entrypoint]\nruntime = "probe/{PROBE_RUNTIME}"\nbin = "bin/tilo-collab-probe"',
                              manifest)
        if n != 1 or m != 1:
            raise Failed(f"cannot point {name}'s manifest at the probe")
        (staged / "manifest.toml").write_text(manifest)
        tapp = staged / f"{name}.tapp"
        run([BIN / "tilo-app-package", "pack", "--manifest", staged / "manifest.toml",
             "--payload", staged / "payload", "--identity", identity, "--output", tapp])
        tapps.append(tapp)
    return tapps


def open_probes(session: Session) -> None:
    """The probe packages up, each with a window: one the desk already shows is left to it (a
    workspace's record says what was open in it), the rest opened as a person opens an app."""
    def up():
        windows = {window.get("app_id") for window in session.windows()}
        missing = [name for name in PROBE_ROUTES if app_id(name) not in windows]
        for name in missing:
            opened = next((action for action in session.actions(app_id(name))
                           if action.get("provider") == "applications"
                           and action.get("id") == app_id(name)), None)
            if opened is not None:
                session.invoke("applications", opened["id"])
        return windows if not missing else None
    until("a window of each probe package", up, 120)


def check_routes(session: Session) -> dict:
    """Launch the probe as Today and as Tasks and read what the broker let
    each do on the routes it tried."""
    open_probes(session)
    pattern = re.compile(r"\[app (\S+) stdout\] probe: ROUTE (\S+) (.*)")
    # Each probe's answer on each route it tries: a log line said twice is one answer.
    wanted = {(app_id(who), route) for who, routes in PROBE_ROUTES.items() for route in routes}
    def answered():
        lines = [m.groups() for m in pattern.finditer(session.log())]
        return lines if wanted <= {(who, route) for who, route, _ in lines} else None
    lines = until("the probes' route answers", answered, 60)
    result = {f"{who} -> {route}": answer for who, route, answer in lines}
    today = result.get(f"{app_id('today')} -> {app_id('notes')}", "")
    tasks = result.get(f"{app_id('tasks')} -> {app_id('notes')}", "")
    if not (today.startswith("JOINED") and "WRITE-REFUSED" in today and "changes nothing" in today):
        raise Failed(f"Today's declared read of Notes was not a read-only join: {today!r}")
    if not tasks.startswith("JOIN-REFUSED"):
        raise Failed(f"Tasks, which declares no read of Notes, joined its route: {tasks!r}")
    return result


def run_checks(summary: dict, checks) -> bool:
    for name, check in checks:
        print(f"==> {name}", file=sys.stderr)
        try:
            summary["checks"][name] = check()
            print(json.dumps({name: summary["checks"][name]}, indent=2))
        except Failed as error:
            summary["failed"].append(name)
            summary["checks"][name] = {"failed": str(error)}
            print(f"FAIL {name}: {error}", file=sys.stderr)
            if name in ("launch", "processes"):
                return False
    return True


def apps_phases(summary: dict, state: Path, out: Path, running: dict) -> bool:
    """The apps in a session of their own, then the route checks in a second start of it."""
    proc, session = start_session(state, out, [out / "build" / f"{n}.tapp" for n in APPS], "apps", apps="notes")
    running["proc"] = proc
    pid = proc.pid
    # Today's window before Tasks holds a task, which its card is held to after.
    today_before: list[bytes] = []
    ok = run_checks(summary, [
        ("registry", lambda: check_registry(session)),
        ("launch", lambda: launch_all(session)),
        ("processes", lambda: check_processes(session, pid)),
        ("hosts", lambda: check_hosts(session, summary["checks"]["processes"], pid)),
        # Before anything is in them: the session's own workspace is this device's to
        # host, so no app waits on a host and each shows its own state.
        ("hosted-here", lambda: check_hosted_here(session, summary["checks"]["launch"], out)),
        ("actions", lambda: check_actions(session)),
        ("snapshots", lambda: snapshots(session, summary["checks"]["launch"], out)),
        ("native", lambda: check_native(session, out)),
        ("activation", lambda: check_activation(session)),
        ("read", lambda: check_read(session)),
        # Before the checks that type into the desk: every app's threads are held to sleeping
        # on a desk no other check has just touched.
        ("idle", lambda: check_idle(session, summary["checks"]["processes"], pid)),
        ("today-before", lambda: hold_today(session, out, today_before)),
        ("windows", lambda: check_windows(session, summary["checks"]["processes"], pid, out)),
        ("titles", lambda: check_titles(session, out)),
        ("today", lambda: check_today(session, out, today_before[0])),
        ("record-open", lambda: check_record_open(session, out)),
        ("kept", lambda: keep_task(session)),
        ("places", lambda: check_places(session, out)),
    ])
    stop_session(proc)
    running["proc"] = None
    # The next start writes its own session log over this one.
    shutil.copy(state / "logs/session.log", out / "shell-apps.log")
    if not ok:
        return False
    if "kept" not in summary["failed"]:
        # The same session started again, with what Tasks held when it stopped, and Today opened.
        proc, session = start_session(state, out, [out / "build" / f"{n}.tapp" for n in APPS],
                                      "reopened", apps="notes")
        running["proc"] = proc
        run_checks(summary, [("reopened", lambda: check_reopened(session, out))])
        stop_session(proc)
        running["proc"] = None
        shutil.copy(state / "logs/session.log", out / "shell-reopened.log")
    # Route access, with the probe installed as Today and as Tasks. The probes first: until
    # a package names the probe's runtime, the collection after each install reclaims it.
    tapps = [*probe_packages(state, out), out / "build/notes.tapp"]
    # Only the packages this check is about: the probe as Today and Tasks, and Notes.
    proc, session = start_session(state, out, tapps, "routes", apps="")
    running["proc"] = proc
    run_checks(summary, [("routes", lambda: check_routes(session))])
    stop_session(proc)
    running["proc"] = None
    shutil.copy(state / "logs/session.log", out / "shell-routes.log")
    return True


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--out", type=Path, default=REPO / "target/tilolang-e2e")
    parser.add_argument("--keep", action="store_true")
    args = parser.parse_args()
    out = args.out.resolve()
    shutil.rmtree(out, ignore_errors=True)
    out.mkdir(parents=True)
    state = out / "state"
    tool = REPO / "target/debug/tilolang"
    os.environ.setdefault("TMPDIR", str(Path.home() / "tmp"))
    Path(os.environ["TMPDIR"]).mkdir(parents=True, exist_ok=True)

    print("==> preparing the session", file=sys.stderr)
    prepare(state, apps="notes", built=not args.no_build,
            remote=os.environ.get("TILO_NESTED_SESSION_BUILD_EXEC"))
    if not args.no_build:
        print("==> building the tilolang tool and the probe", file=sys.stderr)
        run(["cargo", "build", "-p", "tilolang-cli", "--bin", "tilolang",
             "-p", "collab-probe", "--bin", "tilo-collab-probe",
             "-p", "tilolang-host", "--example", "components"], cwd=REPO)
    runtime = (state / "runtimes/.version").read_text().strip()
    print("==> tilolang build", file=sys.stderr)
    run([tool, "build", REPO / "apps/tl/today", "--search", REPO / "apps/tl", "--out", out / "build",
         "--tilo-runtime", f"tilo-runtime/{runtime}"])

    summary: dict = {"checks": {}, "failed": []}
    # The session running now, stopped at the end whatever happens.
    running: dict = {"proc": None}
    try:
        apps_phases(summary, state, out, running)
    except Failed as error:
        summary["failed"].append("session")
        summary["error"] = str(error)
        print(f"FAIL {error}", file=sys.stderr)
    finally:
        (out / "summary.json").write_text(json.dumps(summary, indent=2))
        if not args.keep:
            stop_session(running["proc"])
    print(f"==> {'FAILED: ' + ', '.join(summary['failed']) if summary['failed'] else 'every check passed'}"
          f" (summary: {out / 'summary.json'})", file=sys.stderr)
    return 1 if summary["failed"] else 0


if __name__ == "__main__":
    sys.exit(main())
