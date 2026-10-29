#!/usr/bin/env python3
"""tilolang apps end to end, as real and independent Tilo apps.

Builds the notes, chat, todo and today packages with `tilolang build`, installs
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
  hosted-here the session's own workspace is hosted by this device: no app waits
             on a host, each shows its own state as first opened (first-*.png)
  snapshot   a PNG of the nested session per app, in OUT
  native     native Notes (so.tilo.notes), installed beside them, launches and
             presents a frame (native-notes.png)
  activation "Show this window" answers for tilolang Notes as for native Notes
  read       the door's `read` of tilolang Notes lists the room's records, and
             the notebook's read lists its notes by the ids a read takes
  idle       each app untouched for 10 s, native Notes included: one that commits no
             frame wakes no thread at all; one that animates commits one frame per
             pose and no more
  host-join  a local site (the room service and its workspace host, as
             nested-pair.sh runs it) and a session of its own whose community it
             is: the session joins the site's Lounge and it reaches "synced"; the
             time from dialling the host to the first sync is recorded
  host-again the same session started again, holding the Lounge already: it syncs
             again, and the time it takes is recorded
  spinner    started a third time with the site's processes stopped: the Lounge is
             connecting, Today (empty) says so with a spinner that turns one
             committed frame per pose, until the dial gives up
  unsynced   then the Lounge's host cannot be reached: native Notes says so, keeps
             its own action, and Today draws nothing more (unsynced-*.png)
  reconnect  the site's processes go on: the Lounge syncs again by itself
  host       in the synced Lounge every app is told so and shows the room
             (synced-*.png), native Notes' "New note" lands, and each launch's time
             to its first frame is recorded
  host-edit  tilolang Notes' "New note" lands in the Lounge too; so does a press
             of its button in a second window of it, which that window makes
             itself (activated by its test id), and the app started again
  host-undo  tilolang Notes' Undo, which its host publishes as every app's is,
             takes the person's last step in the Lounge back: History lists it
             undone
  routes     the broker's route access, as the attested launch of an app: the
             collaboration probe (crates/dev/collab-probe) is installed as
             Today, whose surface declares that it reads Notes' kinds, and as
             Todo, which declares no read. Today joins Notes' route read-only
             and every write there is refused; Todo cannot join it at all.
             This runs in a second start of the same throwaway session, with
             the probe packages installed in place of Today and Todo.

Usage:
  scripts/headless-examples/tilolang-e2e.py [--no-build] [--out DIR] [--keep] [--host-only]

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
import signal
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SESSION = REPO / "dist/dev/nested-session.sh"
BIN = REPO / "target/nested-session/bin"
APPS = ["notes", "chat", "tasks", "today"]
# The native app installed beside them, to hold the tilolang apps to what it does.
NATIVE = "so.tilo.notes"
# Opened last in the local site's Lounge, to time a first launch in a synced room.
NATIVE_CHAT = "so.tilo.chat"
PROBE_RUNTIME = "0.1.0+probe"
# The routes the probe tries as each app: Today declares that it reads Notes'
# kinds; Todo declares no read.
PROBE_ROUTES = {
    "today": ["so.tilo.lang.notes", "so.tilo.lang.today"],
    "todo": ["so.tilo.lang.notes"],
}


SITE = REPO / "crates/hosting/tilo-room-service/dev/run.py"
# The apps the local site's Lounge lets write: its defaults and the tilolang apps, as an
# operator names the apps of their site (TILO_ROOMS_AUTHOR_APPS).
SITE_AUTHORS = ["so.tilo.chat", "so.tilo.notes", "so.tilo.canvas"]


def app_id(name: str) -> str:
    return f"so.tilo.lang.{name}"


class Failed(Exception):
    pass


def run(argv, **kwargs) -> str:
    result = subprocess.run(argv, text=True, capture_output=True, **kwargs)
    if result.returncode != 0:
        raise Failed(f"{' '.join(map(str, argv))} failed ({result.returncode}): {result.stderr.strip()[-2000:]}")
    return result.stdout


def until(what: str, check, deadline: float = 60.0, step: float = 0.25):
    end = time.monotonic() + deadline
    while time.monotonic() < end:
        value = check()
        if value:
            return value
        time.sleep(step)
    raise Failed(f"timed out waiting for {what}")


class Session:
    """One headless nested session and the environment its tools act in."""

    def __init__(self, state: Path, ready: dict):
        self.pid = 0
        self.state = state
        self.control = ready["control"]
        self.runtime = ready["runtime"]

    def env(self) -> dict:
        state = self.state
        env = dict(os.environ)
        env.update(
            HOME=str(state / "home"),
            XDG_CONFIG_HOME=str(state / "config"),
            XDG_DATA_HOME=str(state / "data"),
            XDG_STATE_HOME=str(state / "state"),
            XDG_CACHE_HOME=str(state / "cache"),
            TILO_IDENTITY_ROOT=str(state / "identity"),
            TILO_APPS_DATA_DIR=str(state / "apps"),
            TILO_RUNTIME_BUNDLE_DIR=str(state / "runtimes"),
            TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE=str(state / "keys/runtime-publishers.txt"),
            TILO_TEST_CONTROL=self.control,
            XDG_RUNTIME_DIR=self.runtime,
        )
        env.pop("TILO_APPS_SYSTEM_DIR", None)
        return env

    def control_cmd(self, *args: str) -> str:
        return run([BIN / "tilo-control", *args], env=self.env())

    def control_json(self, *args: str):
        return json.loads(self.control_cmd(*args))

    def actions(self, query: str = "") -> list:
        # As an agent or a script lists them: with the actions listed for agents alone
        # ("Show this window"), which a person's palette leaves out.
        return self.control_json("actions", "list", query, "--json", "--programmatic").get("actions", [])

    def invoke(self, provider: str, ident: str, args: dict | None = None) -> dict:
        argv = ["actions", "invoke", provider, ident, "--wait"]
        if args is not None:
            argv += ["--args-json", json.dumps(args)]
        return self.control_json(*argv)

    def windows(self) -> list:
        return self.control_json("windows", "--json")

    def log(self) -> str:
        return (self.state / "logs/shell.log").read_text(errors="replace")

    def room_sync(self, room: str):
        """Where the workspace of `room` stands with its host, as its apps are told: None
        for a room no host is waited on, else "connecting", "unreachable", "synced", ..."""
        for entry in self.control_json("state-dump")["chrome"]["room_sync"]:
            if entry["room"] == room:
                return entry
        raise Failed(f"no workspace holds room {room}")

    def holds(self, room: str) -> bool:
        tree = self.control_json("state-dump")["workspace_tree"]
        return any(str(w.get("room")) == room for w in tree.get("workspaces", []))

    def active_room(self) -> str:
        tree = self.control_json("state-dump")["workspace_tree"]
        return next(str(w["room"]) for w in tree["workspaces"] if w["id"] == tree["active"])


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
    out, todo = [], [pid]
    while todo:
        for child in children(todo.pop()):
            out.append(child)
            todo.append(child)
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


def anon_kib(pid: int) -> int | None:
    """The anonymous memory `pid` holds, in KiB."""
    try:
        status = Path(f"/proc/{pid}/status").read_text()
    except OSError:
        return None
    for line in status.splitlines():
        if line.startswith("RssAnon:"):
            return int(line.split()[1])
    return None


def host_pids(session_pid: int) -> list[int]:
    """The tilolang hosts of the session."""
    return [p for p in descendants(session_pid) if exe(p).endswith("/bin/tilolang-host")]


def launch_timings(session: Session) -> dict:
    """Where hosted launches spent their time: the shell's handover of each instance and the
    host's opening of it, by stage, in µs."""
    return {
        "handed_over": log_lines(session, "tilolang instance handed over"),
        "opened": log_lines(session, "tilolang instance opened"),
    }


def window_marks(state: Path) -> dict:
    """Each hosted window's own startup marks (ms after its process began, the launcher's
    before it), by the file it wrote: when its patch connection and first tree arrived and
    what its first frame drew."""
    out = {}
    for path in state.glob("**/startup-*.json"):
        try:
            data = json.loads(path.read_text())
        except (OSError, ValueError):
            continue
        marks = {e["name"]: round(e["ns"] / 1e6, 1) for e in data.get("events", [])}
        if not any(name.startswith("tilolang-") for name in marks):
            continue
        origin = data.get("origin_epoch_us", 0)
        launcher = {e["name"]: round((e["epoch_us"] - origin) / 1000, 1) for e in data.get("launcher", [])}
        keep = ("tilolang-", "first-frame", "render-worker-first", "gpu-device-request-done",
                "window-render-context-done")
        out[str(path.relative_to(state))] = {
            "launcher": launcher,
            "marks": {k: v for k, v in marks.items() if k.startswith(keep)},
        }
    return out


# --- checks ---------------------------------------------------------------------

def check_registry(session: Session, names=APPS) -> dict:
    # A cold query starts the index's refresh and may answer before it has
    # every application (docs/headless-testing.md): ask again until it does.
    found = {}
    for name in names:
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
    def presented():
        frames = session.control_json("raw", '{"op":"foreign_frames"}')["foreign_frames"]["frames"]
        shown = {f["app_id"] for f in frames if f["presented"] and f["pixels_ready"]}
        return all(app_id(n) in shown for n in APPS)
    until("a presented frame of every app", presented, 30)
    return {
        app_id(n): {"group": windows[app_id(n)]["group"], "tab": windows[app_id(n)]["tab"]}
        for n in APPS
    }


def check_processes(session: Session, session_pid: int) -> dict:
    tree = descendants(session_pid)
    shell = next(p for p in tree if exe(p).endswith("/bin/shell") and "__render-service" not in cmdline(p))
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


def own_action(session: Session, name: str, title: str, kind: str):
    for action in session.actions(title):
        if (action.get("application") == app_id(name) and action.get("title") == title
                and action.get("operation_kind") == kind and action["id"].startswith(app_id(name))):
            return action
    return None


def history_total(session: Session, name: str) -> int:
    history = until(f"{app_id(name)}'s history action", lambda: own_action(session, name, "History", "View"))
    answer = session.invoke("app-actions", history["id"])
    try:
        return answer["receipt"]["update"]["output"]["value"]["total"]
    except (KeyError, TypeError):
        raise Failed(f"{app_id(name)}'s history answered without a total: {json.dumps(answer)}") from None


def check_actions(session: Session) -> dict:
    out = {}
    news = {"notes": "New note", "chat": "New chat", "todo": "New item"}
    for name in APPS:
        published = until(
            f"{app_id(name)}'s actions",
            lambda: [a for a in session.actions(app_id(name)) if a.get("application") == app_id(name)],
        )
        out[app_id(name)] = {"published": len(published)}
    for name, title in news.items():
        action = until(f"{app_id(name)}'s {title!r}", lambda: own_action(session, name, title, "Document"))
        before = history_total(session, name)
        state = session.invoke("app-actions", action["id"])["receipt"]["update"]["state"]
        after = until(f"{title!r} in {app_id(name)}'s history",
                      lambda: (lambda t: t if t > before else None)(history_total(session, name)), 15)
        out[app_id(name)].update(invoked=title, state=state, history=[before, after])
    return out


def value_of(answer: dict):
    try:
        return answer["receipt"]["update"]["output"]["value"]
    except (KeyError, TypeError):
        raise Failed(f"an answer without a value: {json.dumps(answer)[:2000]}") from None


def app_action(session: Session, app: str, title: str):
    """The first action called `title` that the window of `app` publishes. Its application is
    `app` exactly: so.tilo.notes never matches so.tilo.lang.notes. (An app out of focus lists
    its actions under directory ids, which do not start with the app's id.)"""
    for action in session.actions(title):
        if action.get("application") == app and action.get("title") == title:
            return action
    return None


def check_native(session: Session, out: Path) -> dict:
    """Native Notes launches beside the tilolang apps and presents a frame."""
    session.invoke("applications", NATIVE)
    windows = until(f"{NATIVE}'s window",
                    lambda: {w.get("app_id"): w for w in session.windows() if w.get("app_id") == NATIVE})
    def presented():
        frames = session.control_json("raw", '{"op":"foreign_frames"}')["foreign_frames"]["frames"]
        return any(f["app_id"] == NATIVE and f["presented"] and f["pixels_ready"] for f in frames)
    until(f"a presented frame of {NATIVE}", presented, 30)
    # Connected: its window publishes its actions.
    until(f"{NATIVE}'s actions", lambda: app_action(session, NATIVE, "Show this window"), 60)
    w = windows[NATIVE]
    session.invoke("desktop-windows", f"tab.focus:{w['group']}:{w['tab']}")
    time.sleep(0.5)
    path = out / "native-notes.png"
    session.control_cmd("snapshot", str(path))
    return {"group": w["group"], "tab": w["tab"], "snapshot": str(path)}


def check_activation(session: Session) -> dict:
    """"Show this window" answers for a tilolang app as it does for the native app."""
    out = {}
    for app in (NATIVE, app_id("notes")):
        action = until(f"{app}'s 'Show this window'", lambda: app_action(session, app, "Show this window"))
        out[app] = value_of(session.invoke("app-actions", action["id"]))
    if out[app_id("notes")] != out[NATIVE]:
        raise Failed(f"'Show this window' answers differently from native Notes: {out}")
    return out


def check_read(session: Session) -> dict:
    """An agent or a script reads what a tilolang app holds through the door's `read`, as it
    reads a native app's content: the room's records, then one of them by its id."""
    action = until("tilolang Notes' 'Read the content'",
                   lambda: app_action(session, app_id("notes"), "Read the content"))
    room = value_of(session.invoke("app-actions", action["id"]))
    # Notes' root record, whatever its type is called: the one record of the app's own kinds.
    prefix = f"tl.{app_id('notes')}."
    notebook = next((r for r in room.get("records", []) if r.get("kind", "").startswith(prefix)), None)
    if notebook is None:
        raise Failed(f"the room's read lists no record of Notes: {json.dumps(room)[:2000]}")
    kind = notebook["kind"]
    read = value_of(session.invoke("app-actions", action["id"], {"item": notebook["item"]}))
    # The collection of notes, whatever the root calls it: its field listing Notes' notes.
    notes = next((f for f in read.get("fields", {}).values() if isinstance(f, dict)
                  and any(i.get("type") == "Note" for i in f.get("items", []))), {})
    if read.get("kind") != kind or not notes.get("items"):
        raise Failed(f"Notes' root record's read holds no notes: {json.dumps(read)[:2000]}")
    native = until("native Notes' 'Read the content'", lambda: app_action(session, NATIVE, "Read the content"))
    return {
        "records": len(room["records"]),
        "notebook": {"label": read.get("label"), "notes": notes.get("count")},
        "native": sorted(value_of(session.invoke("app-actions", native["id"]))),
    }


def snapshots(session: Session, windows: dict, out: Path, prefix: str = "") -> dict:
    shots = {}
    for name in APPS:
        w = windows[app_id(name)]
        session.invoke("desktop-windows", f"tab.focus:{w['group']}:{w['tab']}")
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
    """Every app, native Notes among them, left untouched for 10 s. An app that commits no frame
    is settled, and every thread of it sleeps: zero context switches. An app that commits frames
    is animating, which in this session is only the connecting spinner of an app whose room never
    syncs and that holds nothing to show (Today; native Notes): one frame per pose of the spinner
    and no more, with its context switches per frame recorded."""
    time.sleep(3)
    pids = {app: info["pid"] for app, info in processes["apps"].items()}
    native = native_pid(session_pid)
    if native is None:
        raise Failed("native Notes is not running to be held to the same idle")
    pids[NATIVE] = native
    before = {app: switches(pid) for app, pid in pids.items()}
    seqs = {app: frame_seq(session, app) for app in pids}
    start = time.monotonic()
    time.sleep(seconds)
    elapsed = time.monotonic() - start
    poses = elapsed * INDICATOR_POSES_PER_SECOND
    out, problems = {}, []
    for app, pid in pids.items():
        after = switches(pid)
        frames = frame_seq(session, app) - seqs[app]
        woke = {t: after[t] - before[app].get(t, 0) for t in after if after[t] != before[app].get(t, 0)}
        total = sum(woke.values())
        out[app] = {"frames": frames, "switches": total, "threads": woke}
        if frames == 0 and woke:
            problems.append(f"{app} drew nothing and woke: {woke}")
        elif frames > poses + 1:
            problems.append(f"{app} committed {frames} frames in {elapsed:.1f} s, more than its "
                            f"spinner's {poses:.0f} poses")
        if frames:
            out[app]["switches_per_frame"] = round(total / frames, 1)
    if problems:
        raise Failed(f"{'; '.join(problems)} ({json.dumps(out)})")
    return out


def frame_seq(session: Session, app: str) -> int:
    frames = session.control_json("raw", '{"op":"foreign_frames"}')["foreign_frames"]["frames"]
    return max((f.get("frame_seq") or 0 for f in frames if f["app_id"] == app), default=0)


# The poses a second of a looping indicator (ui_core::animation::INDICATOR_STEP).
INDICATOR_POSES_PER_SECOND = 30


ANSI = re.compile(r"\x1b\[[0-9;]*m")


def log_lines(session: Session, pattern: str) -> list[dict]:
    """The shell log's tracing lines matching `pattern`, each as its `key=value` fields."""
    out = []
    for line in ANSI.sub("", session.log()).splitlines():
        if re.search(pattern, line):
            out.append(dict(re.findall(r"(\w+)=(\S+)", line)))
    return out


def sync_times(session: Session) -> list[float]:
    """How long each room waited from dialling its host to its first sync, in ms."""
    return [round(float(f["connect_to_sync_ms"]), 1)
            for f in log_lines(session, "room synced with its host")]


def first_frames(session: Session) -> dict:
    """Per package, the milliseconds from asking for the launch to its first presented frame."""
    return {f["package_id"]: round(int(f["request_to_first_present_us"]) / 1000, 1)
            for f in log_lines(session, "application first frame presented")}


def presented(session: Session, app: str) -> bool:
    frames = session.control_json("raw", '{"op":"foreign_frames"}')["foreign_frames"]["frames"]
    return any(f["app_id"] == app and f["presented"] and f["pixels_ready"] for f in frames)


def check_hosted_here(session: Session, windows: dict, out: Path) -> dict:
    """The session's own workspace is hosted by this device: its apps wait on no other host
    (the room syncs with nothing, or with this device's own workspace host, which is there
    already), and each shows its own state at once."""
    room = session.active_room()
    told = session.room_sync(room)
    if told["state"] not in (None, "synced"):
        raise Failed(f"the session's own workspace waits on a host: {told}")
    return {"room": room, "state": told["state"],
            "snapshots": snapshots(session, windows, out, "first-")}


def open_in_workspace(session: Session, apps: list[str], workspace: str) -> dict:
    """Open `apps` in the workspace in front, called `workspace`, until each presents a frame.
    An app with a window there already keeps it: each opens once."""
    already = {w.get("app_id") for w in session.windows() if w.get("workspace") == workspace}
    for app in apps:
        if app not in already:
            session.invoke("applications", app)
    def opened():
        windows = {w.get("app_id"): w for w in session.windows()}
        return windows if all(a in windows for a in apps) else None
    windows = until("every app's window", opened)
    elsewhere = {a: w.get("workspace") for a, w in windows.items() if a in apps and w.get("workspace") != workspace}
    if elsewhere:
        raise Failed(f"apps opened outside {workspace}: {elsewhere}")
    until("a presented frame of every app", lambda: all(presented(session, a) for a in apps), 60)
    return {a: {"group": windows[a]["group"], "tab": windows[a]["tab"]} for a in apps}


def shots_of(session: Session, windows: dict, out: Path, prefix: str) -> dict:
    shots = {}
    for app, w in windows.items():
        session.invoke("desktop-windows", f"tab.focus:{w['group']}:{w['tab']}")
        time.sleep(0.5)
        name = {NATIVE: "native-notes", NATIVE_CHAT: "native-chat"}.get(app, app.rsplit(".", 1)[-1])
        path = out / f"{prefix}{name}.png"
        session.control_cmd("snapshot", str(path))
        shots[app] = str(path)
    return shots


def check_joined(session: Session, room: str) -> dict:
    """A fresh session joins the local site's Lounge through its door, as a computer joins
    tilo.coffee's, and the room syncs with its host."""
    until("the session to join the Lounge", lambda: session.holds(room), 120, 0.1)
    until("the Lounge to sync with its host",
          lambda: session.room_sync(room)["state"] == "synced", 60, 0.1)
    if session.active_room() != room:
        session.invoke("actions", f"workspace.select.{room}")
        until("the Lounge in front", lambda: session.active_room() == room, 15)
    return {"room": room, "connect_to_sync_ms": sync_times(session)}


def check_host_again(session: Session, room: str) -> dict:
    """A session that holds the Lounge already syncs with its host again on start."""
    until("the Lounge restored", lambda: session.holds(room), 60, 0.1)
    until("the Lounge to sync again", lambda: session.room_sync(room)["state"] == "synced", 60, 0.1)
    return {"connect_to_sync_ms": sync_times(session)}


def history_now(session: Session, app: str):
    """How many versions `app`'s History lists now; None while it does not answer."""
    action = next((a for a in session.actions("History") if a.get("application") == app
                   and a.get("title") == "History" and a.get("operation_kind") == "View"), None)
    try:
        return action and value_of(session.invoke("app-actions", action["id"]))["total"]
    except Failed:
        return None


def new_note_in(session: Session, app: str, args: dict | None = None) -> list[int]:
    """Invoke `app`'s "New note" edit and wait for it in the app's history: [before, after]."""
    # Out of focus, an app lists its actions under directory ids, which move on as the
    # listing changes: find the edit by its application, title and kind, and ask again
    # when the id listed a moment ago has moved on.
    def listed():
        for query in ("New note", "notes"):
            for action in session.actions(query):
                if (action.get("application") == app and action.get("title") == "New note"
                        and action.get("operation_kind") == "Document"):
                    return action
        return None

    def history():
        return history_now(session, app)

    until(f"{app}'s 'New note'", listed)
    before = until(f"{app}'s history", lambda: (lambda t: None if t is None else (t,))(history()))[0]
    refusals: list[str] = []

    def invoked():
        action = listed()
        if action is None:
            return None
        try:
            return session.invoke("app-actions", action["id"], args)
        except Failed as error:
            refusals.append(str(error)[-600:])
            # A refusal that names the edit, not a stale id, is the answer.
            return None if "unknown_action" in str(error) else {"refused": str(error)[-600:]}

    try:
        answer = until(f"{app}'s 'New note' answered", invoked, 30)
    except Failed:
        raise Failed(f"{app}'s 'New note' was never answered: {refusals[-1:]}") from None
    if "refused" in answer:
        raise Failed(f"{app}'s 'New note' failed: {answer['refused']}")
    after = until(f"'New note' in {app}'s history",
                  lambda: (lambda t: t if t is not None and t > before else None)(history()), 15)
    return [before, after]


def check_host(session: Session, room: str, out: Path) -> dict:
    """In the Lounge, synced with its host: every app is told the room is synced and shows
    it, not "Connecting…"; the Lounge lets each write; native Notes' "New note" changes the
    room; and native Chat, opened now, reaches its first frame in a synced room."""
    told = session.room_sync(room)
    if told["state"] != "synced":
        raise Failed(f"the Lounge is not synced: {told}")
    apps = [app_id(n) for n in APPS]
    missing = [a for a in [*apps, NATIVE] if a not in told["app_authors"]]
    if missing:
        raise Failed(f"the Lounge does not let these apps write: {missing}")
    windows = open_in_workspace(session, [*apps, NATIVE, NATIVE_CHAT], "Lounge")
    shots = shots_of(session, windows, out, "synced-")
    return {
        "launch_to_first_frame_ms": first_frames(session),
        "snapshots": shots,
        "native_new_note": new_note_in(session, NATIVE, {"title": "From the Lounge"}),
    }


def windows_of(session: Session, app: str, workspace: str) -> list:
    return [w for w in session.windows() if w.get("app_id") == app and w.get("workspace") == workspace]


COMPONENTS = REPO / "target/debug/examples/components"


def test_id_of(package: str, kind: str, label: str) -> str:
    """The test id of the `kind` component saying `label` in the first window of the tilolang
    package `package` (apps/tl/<package>), as its instance builds it: the key the compiler gave
    it, read from the same compiler (tilolang-host's `components` example)."""
    for line in run([COMPONENTS, REPO / "apps/tl" / package]).splitlines():
        component = json.loads(line)
        if component["kind"] == kind and component["label"] == label:
            return component["key"]
    raise Failed(f"apps/tl/{package}'s first window shows no {kind} saying {label!r}")


def activate(session: Session, test_id: str) -> None:
    """The focused window activates its control `test_id`, as assistive technology would."""
    session.control_cmd("raw", json.dumps({"op": "activate_test_id", "test_id": test_id}))


def check_host_edit(session: Session, pids_of) -> dict:
    """tilolang Notes' "New note" changes the Lounge, as native Notes' does; so does a press of
    its button in a second window of the app beside the first, made by that window itself;
    and the app started again edits too."""
    notes = app_id("notes")
    out = {"history": new_note_in(session, notes)}
    # A second window: the first duplicated, as Super+T does. The host's memory before and after
    # is what one more instance costs it.
    time.sleep(2)
    hosts = host_pids(session.pid)
    out["host_anon_before"] = anon_kib(hosts[0]) if len(hosts) == 1 else None
    first = windows_of(session, notes, "Lounge")[0]
    session.invoke("desktop-windows", f"tab.focus:{first['group']}:{first['tab']}")
    session.invoke("actions", "tab.duplicate")
    until("a second window of Notes", lambda: len(windows_of(session, notes, "Lounge")) == 2, 30)
    until("a frame of the second window", lambda: sum(
        1 for f in session.control_json("raw", '{"op":"foreign_frames"}')["foreign_frames"]["frames"]
        if f["app_id"] == notes and f["presented"] and f["pixels_ready"]) == 2, 30)
    hosts = host_pids(session.pid)
    if len(hosts) != 1:
        raise Failed(f"the Lounge's tilolang apps run in {len(hosts)} hosts, not one: {hosts}")
    out["host_anon_kib"] = [out.pop("host_anon_before"), anon_kib(hosts[0])]
    # The second window, in front, presses its own "New note".
    second = next(w for w in windows_of(session, notes, "Lounge")
                  if (w["group"], w["tab"]) != (first["group"], first["tab"]))
    session.invoke("desktop-windows", f"tab.focus:{second['group']}:{second['tab']}")
    before = until("Notes' history", lambda: (lambda t: None if t is None else (t,))(history_now(session, notes)))[0]
    activate(session, test_id_of("notes", "ui.Button", "New note"))
    after = until("the second window's 'New note' in Notes' history",
                  lambda: (lambda t: t if t is not None and t > before else None)(history_now(session, notes)), 15)
    out["second_window"] = [before, after]
    if len(windows_of(session, notes, "Lounge")) != 2:
        raise Failed("a window of Notes closed after its second window edited")
    # Started again: every window closed, the app gone, and opened anew.
    for w in windows_of(session, notes, "Lounge"):
        session.invoke("desktop-windows", f"tab.close:{w['group']}:{w['tab']}")
    until("Notes closed", lambda: not windows_of(session, notes, "Lounge") and not pids_of(notes), 30)
    open_in_workspace(session, [notes], "Lounge")
    out["relaunched"] = new_note_in(session, notes)
    return out


def check_host_idle(session: Session, seconds: float = 10.0) -> dict:
    """The tilolang host, its instances untouched for 10 s, sleeps: what each of its threads
    woke is recorded, and it must not wake more than once a second."""
    time.sleep(3)
    hosts = host_pids(session.pid)
    if len(hosts) != 1:
        raise Failed(f"expected one tilolang host, found {hosts}")
    before = switches(hosts[0])
    time.sleep(seconds)
    after = switches(hosts[0])
    woke = {t: after[t] - before.get(t, 0) for t in after if after[t] != before.get(t, 0)}
    total = sum(woke.values())
    out = {"pid": hosts[0], "seconds": seconds, "switches": total, "threads": woke,
           "anon_kib": anon_kib(hosts[0])}
    if total > seconds:
        raise Failed(f"the idle tilolang host woke {total} times in {seconds:.0f} s: {json.dumps(out)}")
    return out


def check_host_undo(session: Session) -> dict:
    """In the Lounge, tilolang Notes' Undo, published by its host as every app publishes it,
    takes back the person's last step as a step of its own: History lists that step undone."""
    notes = app_id("notes")
    edit = new_note_in(session, notes)
    undo = until("Notes' Undo", lambda: next(
        (a for a in session.actions("Undo") if a.get("application") == notes and a.get("title") == "Undo"),
        None))
    answer = value_of(session.invoke("app-actions", undo["id"]))
    undone = answer.get("undone") or []
    if len(undone) != 1:
        raise Failed(f"Notes' Undo took back {len(undone)} steps, not the last one: {answer}")
    change = undone[0]["change"]
    history = next(a for a in session.actions("History") if a.get("application") == notes
                   and a.get("title") == "History" and a.get("operation_kind") == "View")
    versions = value_of(session.invoke("app-actions", history["id"]))["versions"]
    listed = next((v for v in versions if v.get("change") == change), None)
    if not listed or not listed.get("undone"):
        raise Failed(f"History does not list {change} undone: {listed}")
    return {"history": edit, "undone": change, "now": answer.get("now")}


def check_spinner(session: Session, room: str, pid_of) -> dict:
    """The Lounge's host is stopped: its room is connecting, and Today, empty and in front,
    says so with a spinner that turns one committed frame per pose, none between, until the
    dial gives up and the room says its host cannot be reached."""
    today = app_id("today")
    until("the Lounge restored", lambda: session.holds(room), 60, 0.1)
    check_registry(session, ["today"])
    # The room is dialled once this device's native owner has opened, which an app in
    # the room needs to open too.
    until("the Lounge dialled", lambda: session.room_sync(room)["state"] is not None, 60, 0.1)
    windows = open_in_workspace(session, [today], "Lounge")
    w = windows[today]
    session.invoke("desktop-windows", f"tab.focus:{w['group']}:{w['tab']}")
    state = session.room_sync(room)["state"]
    if state != "connecting":
        raise Failed(f"with its host stopped, the Lounge is {state!r}, not connecting")
    pid = pid_of(today)
    before, seq, start = switches(pid), frame_seq(session, today), time.monotonic()
    until("the dial to give up", lambda: session.room_sync(room)["state"] != "connecting", 60, 0.1)
    frames = frame_seq(session, today) - seq
    elapsed = time.monotonic() - start
    after = switches(pid)
    woke = sum(after[t] - before.get(t, 0) for t in after)
    poses = elapsed * INDICATOR_POSES_PER_SECOND
    out = {"seconds": round(elapsed, 1), "frames": frames, "poses": round(poses), "switches": woke,
           "then": session.room_sync(room)["state"]}
    if frames == 0:
        raise Failed(f"the connecting spinner stood still: {out}")
    # One pose a frame; a frame whose clock reading straddles two poses may land late by one.
    if frames > poses + 1:
        raise Failed(f"the spinner committed more frames than it has poses: {out}")
    if out["then"] != "unreachable":
        raise Failed(f"a host that does not answer is not said to be out of reach: {out}")
    return out


def check_unsynced(session: Session, room: str, out: Path, pid_of) -> dict:
    """The Lounge's host cannot be reached: each app says so in place of its own empty state,
    keeps its own action to work on what this device has, and, with nothing on its way,
    draws nothing more."""
    apps = [app_id(n) for n in APPS]
    windows = open_in_workspace(session, [*apps, NATIVE], "Lounge")
    told = session.room_sync(room)
    if told["state"] != "unreachable":
        raise Failed(f"the Lounge should say its host cannot be reached: {told}")
    shots = shots_of(session, windows, out, "unsynced-")
    time.sleep(1)
    today = app_id("today")
    pid = pid_of(today)
    before, seq = switches(pid), frame_seq(session, today)
    time.sleep(5)
    frames = frame_seq(session, today) - seq
    woke = {t: c - before.get(t, 0) for t, c in switches(pid).items() if c != before.get(t, 0)}
    if frames or woke:
        raise Failed(f"Today, with nothing on its way, drew {frames} frames and woke {woke}")
    return {"state": told["state"], "snapshots": shots}


def check_reconnect(session: Session, room: str) -> dict:
    """The host is back: the Lounge syncs by itself, with no one asking."""
    until("the Lounge to sync once its host is back",
          lambda: session.room_sync(room)["state"] == "synced", 60, 0.1)
    return {"state": "synced"}


def probe_packages(state: Path, out: Path) -> list[Path]:
    """The probe as Today (whose surface declares that it reads Notes' kinds)
    and as Todo (which declares no read): each package is the app's own, with
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


def check_routes(session: Session) -> dict:
    """Launch the probe as Today and as Todo and read what the broker let
    each do on the routes it tried."""
    # As in the first start, the applications are listed before any opens:
    # an app opened in the session's first moments, before its broker has
    # bound, is refused ("native app authorship broker unavailable").
    check_registry(session, [*PROBE_ROUTES, "notes"])
    for name in PROBE_ROUTES:
        session.invoke("applications", app_id(name))
    pattern = re.compile(r"\[app (\S+) stdout\] probe: ROUTE (\S+) (.*)")
    def answered():
        lines = [m.groups() for m in pattern.finditer(session.log())]
        return lines if len(lines) >= sum(map(len, PROBE_ROUTES.values())) else None
    lines = until("the probes' route answers", answered, 60)
    result = {f"{who} -> {route}": answer for who, route, answer in lines}
    today = result.get(f"{app_id('today')} -> {app_id('notes')}", "")
    todo = result.get(f"{app_id('todo')} -> {app_id('notes')}", "")
    if not (today.startswith("JOINED") and "WRITE-REFUSED" in today and "changes nothing" in today):
        raise Failed(f"Today's declared read of Notes was not a read-only join: {today!r}")
    if not todo.startswith("JOIN-REFUSED"):
        raise Failed(f"Todo, which declares no read of Notes, joined its route: {todo!r}")
    return result


def free_port_base() -> int:
    """A base whose ports the local site takes (see nested-pair.sh --ports) are all free."""
    import random
    import socket
    for _ in range(64):
        base = random.randrange(20000, 40000, 100)
        try:
            for port in (base, base + 1, base + 10, base + 74):
                for kind in (socket.SOCK_STREAM, socket.SOCK_DGRAM):
                    with socket.socket(socket.AF_INET, kind) as probe:
                        probe.bind(("127.0.0.1", port))
        except OSError:
            continue
        return base
    raise Failed("no free ports for the local site")


class Site:
    """A local site as nested-pair.sh runs it: the room service in persistent mode and its
    workspace host, the site's entry naming the host's room as its home workspace, the
    Lounge. Its processes can be stopped and let go on, as a host that stops answering."""

    def __init__(self, out: Path):
        self.dir = out / "site"
        self.dir.mkdir(parents=True, exist_ok=True)
        ready = self.dir / "ready.json"
        base = free_port_base()
        argv = [sys.executable, "-I", SITE, "--scratch", self.dir, "--name", "lounge",
                "--shown-name", "Lounge", "--no-bundle", "--relay-bin", BIN / "tilo-relay",
                "--room-service-bin", BIN / "tilo-room-service", "--web-port", str(base),
                "--api-port", str(base + 1), "--port-base", str(base + 10), "--ready-file", ready]
        authors = ",".join([*SITE_AUTHORS, *(app_id(n) for n in APPS)])
        env = {**os.environ, "TILO_ROOMS_AUTHOR_APPS": authors,
               "TILO_ROOMS_GUEST_JOINS_PER_10_MIN": "1000", "TILO_ROOMS_GUEST_JOINS_PER_DAY": "100000"}
        print("==> starting the local site", file=sys.stderr)
        self.proc = subprocess.Popen(argv, stdout=open(self.dir / "site.log", "w"), stderr=subprocess.STDOUT,
                                     start_new_session=True, env=env)
        until("the local site", lambda: ready.is_file() or self.proc.poll() is not None, 300, 0.25)
        if self.proc.poll() is not None:
            raise Failed(f"the local site ended ({self.proc.returncode}); see {self.dir / 'site.log'}")
        answer = json.loads(ready.read_text())
        self.url, self.room = answer["site"], answer["room"]

    def processes(self) -> list[int]:
        # run.py starts the room service in a session of its own, and the service its
        # workspace host: the whole tree, not one process group.
        return [self.proc.pid, *descendants(self.proc.pid)]

    def signal(self, number: int) -> None:
        for pid in self.processes():
            try:
                os.kill(pid, number)
            except ProcessLookupError:
                pass

    def pause(self) -> None:
        self.signal(signal.SIGSTOP)

    def resume(self) -> None:
        self.signal(signal.SIGCONT)

    def stop(self) -> None:
        if self.proc.poll() is None:
            self.resume()
            os.killpg(self.proc.pid, signal.SIGTERM)
            self.proc.wait(60)


def start_session(state: Path, out: Path, tapps: list[Path], tag: str, native: str = "",
                  community: str | None = None, fresh: bool = True):
    ready = out / f"ready-{tag}.json"
    argv = [SESSION, "--no-build", "--headless", "--software", "--apps", native,
            "--state", state, "--ready", ready]
    if fresh:
        argv.append("--fresh-desktop")
    if community:
        argv += ["--community", community]
    for tapp in tapps:
        argv += ["--install-tapp", tapp]
    print(f"==> starting the headless session ({tag}, lavapipe only)", file=sys.stderr)
    log = open(out / f"session-{tag}.log", "w")
    proc = subprocess.Popen(argv, stdout=log, stderr=subprocess.STDOUT, start_new_session=True,
                            env={**os.environ, "RUST_LOG": os.environ.get(
        "RUST_LOG", "warn,tilo::launch_latency=info,tilo::room_sync=info")})
    until("the session", lambda: ready.is_file() or proc.poll() is not None, 300, 0.5)
    if proc.poll() is not None:
        raise Failed(f"the session ended ({proc.returncode}); see {out / f'session-{tag}.log'}")
    return proc, Session(state, json.loads(ready.read_text()))


def stop_session(proc) -> None:
    if proc is not None and proc.poll() is None:
        os.kill(proc.pid, signal.SIGTERM)
        proc.wait(60)


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
    proc, session = start_session(state, out, [out / "build" / f"{n}.tapp" for n in APPS], "apps",
                                  native="notes")
    running["proc"] = proc
    pid = proc.pid
    ok = run_checks(summary, [
        ("registry", lambda: check_registry(session)),
        ("launch", lambda: launch_all(session)),
        ("processes", lambda: check_processes(session, pid)),
        # Before anything is in them: the session's own workspace is this device's to
        # host, so no app waits on a host and each shows its own state.
        ("hosted-here", lambda: check_hosted_here(session, summary["checks"]["launch"], out)),
        ("actions", lambda: check_actions(session)),
        ("snapshots", lambda: snapshots(session, summary["checks"]["launch"], out)),
        ("native", lambda: check_native(session, out)),
        ("activation", lambda: check_activation(session)),
        ("read", lambda: check_read(session)),
        ("idle", lambda: check_idle(session, summary["checks"]["processes"], pid)),
    ])
    stop_session(proc)
    running["proc"] = None
    # The second start writes its own shell log over this one.
    shutil.copy(state / "logs/shell.log", out / "shell-apps.log")
    if not ok:
        return False
    # Route access, with the probe installed as Today and as Todo. The probes first: until
    # a package names the probe's runtime, the collection after each install reclaims it.
    tapps = [*probe_packages(state, out), out / "build/notes.tapp"]
    proc, session = start_session(state, out, tapps, "routes")
    running["proc"] = proc
    run_checks(summary, [("routes", lambda: check_routes(session))])
    stop_session(proc)
    running["proc"] = None
    shutil.copy(state / "logs/shell.log", out / "shell-routes.log")
    return True


def host_phases(summary: dict, out: Path, running: dict) -> None:
    """A workspace with a host: a local site, and a session of its own in its Lounge,
    started three times: fresh, again, and with the site's processes stopped."""
    site = Site(out)
    running["site"] = site
    host_state = out / "host-state"
    run([SESSION, "--no-build", "--prepare-only", "--apps", "notes chat", "--state", host_state])
    tapps = [out / "build" / f"{n}.tapp" for n in APPS]

    def start(tag: str, fresh: bool) -> Session:
        proc, session = start_session(host_state, out, tapps, tag, native="notes chat",
                                      community=site.url, fresh=fresh)
        running["proc"] = proc
        session.pid = proc.pid
        return session

    def stop(tag: str) -> None:
        stop_session(running["proc"])
        running["proc"] = None
        shutil.copy(host_state / "logs/shell.log", out / f"shell-{tag}.log")

    def pids_of(app: str) -> list[int]:
        return [p for p in descendants(running["proc"].pid)
                if environ(p).get("TILO_PACKAGE_ID") == app and exe(p).endswith("/bin/tilolang-app")]

    def pid_of(app: str) -> int:
        return pids_of(app)[0]

    # A fresh session joins the Lounge; started again it holds it already. No app opens
    # in either, so every app opens in the third start, where each is first opened.
    session = start("host-join", True)
    run_checks(summary, [("host-join", lambda: check_joined(session, site.room))])
    stop("host-join")
    session = start("host-again", False)
    run_checks(summary, [("host-again", lambda: check_host_again(session, site.room))])
    stop("host-again")
    # The host stops answering: its processes are stopped, not gone, so a dial waits
    # for an answer until it gives up.
    site.pause()
    session = start("host", False)
    run_checks(summary, [
        ("spinner", lambda: check_spinner(session, site.room, pid_of)),
        ("unsynced", lambda: check_unsynced(session, site.room, out, pid_of)),
    ])
    site.resume()
    run_checks(summary, [
        ("reconnect", lambda: check_reconnect(session, site.room)),
        ("host", lambda: check_host(session, site.room, out)),
        ("host-edit", lambda: check_host_edit(session, pids_of)),
        ("host-undo", lambda: check_host_undo(session)),
        ("host-idle", lambda: check_host_idle(session)),
    ])
    summary["checks"].setdefault("host", {})["launch_timings"] = launch_timings(session)
    summary["checks"]["host"]["window_marks"] = window_marks(session.state)
    stop("host")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--out", type=Path, default=REPO / "target/tilolang-e2e")
    parser.add_argument("--keep", action="store_true")
    parser.add_argument("--host-only", action="store_true",
                        help="run only the checks of a workspace with a host (host to reconnect)")
    args = parser.parse_args()
    out = args.out.resolve()
    shutil.rmtree(out, ignore_errors=True)
    out.mkdir(parents=True)
    state = out / "state"
    tool = REPO / "target/debug/tilolang"
    os.environ.setdefault("TMPDIR", str(Path.home() / "tmp"))
    Path(os.environ["TMPDIR"]).mkdir(parents=True, exist_ok=True)

    build = [] if not args.no_build else ["--no-build"]
    print("==> preparing the session", file=sys.stderr)
    run([SESSION, *build, "--prepare-only", "--apps", "notes", "--state", state])
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
    # The session running now, and the local site, stopped at the end whatever happens.
    running: dict = {"proc": None, "site": None}
    try:
        ok = args.host_only or apps_phases(summary, state, out, running)
        if ok:
            host_phases(summary, out, running)
    except Failed as error:
        summary["failed"].append("session")
        summary["error"] = str(error)
        print(f"FAIL {error}", file=sys.stderr)
    finally:
        (out / "summary.json").write_text(json.dumps(summary, indent=2))
        if not args.keep:
            stop_session(running["proc"])
            if running["site"] is not None:
                running["site"].stop()
    print(f"==> {'FAILED: ' + ', '.join(summary['failed']) if summary['failed'] else 'every check passed'}"
          f" (summary: {out / 'summary.json'})", file=sys.stderr)
    return 1 if summary["failed"] else 0


if __name__ == "__main__":
    sys.exit(main())
