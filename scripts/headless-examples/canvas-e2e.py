#!/usr/bin/env python3
"""The harness of the canvas arrangement: a headless nested session with Board, Todo and Chat
open over sample data, driven through the test control socket.

It prepares nothing itself: the nested session is built once (dist/dev/nested-session.sh
--prepare-only) and every start reuses that build (--no-build). Each start is headless and
renders on lavapipe alone (--software): it never opens this machine's GPU and never touches
the live desktop. Every session it starts is stopped again, its process groups and all.

What it sets up:

  apps       the tilolang apps Board, Todo and Chat (apps/tl/*), built with `tilolang build`
             and installed with --install-tapp, and native Tasks from the session's build
  seed       each tilolang app's room holds what its sample scenario shows ("a release
             board", "a saturday list", "a busy workspace"), made through the apps' public
             actions, as a person or an agent would make it (see seed_* below)
  windows    one window each of Board, Todo and Chat, each presenting a frame
  frames     every start records the compositor's frame loop: TILO_FRAME_TRACE (one row per
             tick) and TILO_FRAME_SUMMARY (written when the session stops cleanly), as
             OUT/frames-<tag>.csv and .json, with a viewer attached (`watch`) so headless
             frames rasterize

--smoke starts a fresh session, seeds it, opens the three windows, snapshots it, restarts it
on the same state without --fresh-desktop and checks the three windows came back. The verdict
is OUT/summary.json; the exit status is non-zero when a step fails. canvas_steps() is where the
canvas's own steps go.

Usage:
  scripts/headless-examples/canvas-e2e.py --smoke [--out DIR] [--state DIR] [--build]
  scripts/headless-examples/canvas-e2e.py --canvas [--out DIR] [--state DIR] [--shots DIR]
  scripts/headless-examples/canvas-e2e.py --start --keep [--out DIR] [--state DIR]

--start starts a fresh session, opens the windows and seeds it, prints its ready JSON (control
socket, runtime directory, the script's pid) and, with --keep, leaves it running: stop it with
`kill -TERM -<pid>`. Without --build the session's build (target/nested-session, or
TILO_NESTED_SESSION_BUILD) and target/debug/tilolang must exist. --build makes both with
dist/dev/nested-session-build.sh, which stages what the session needs (a dbus-daemon, the
build's timings) into target/nested-session.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SESSION_SH = REPO / "dist/dev/nested-session.sh"

# The tilolang apps, by their directory under apps/tl, and the native apps installed beside them.
TL_APPS = ["board", "todo", "chat"]
NATIVE_APPS = "tasks agent"


def _load_e2e():
    """scripts/headless-examples/tilolang-e2e.py, whose session helpers this reuses."""
    spec = importlib.util.spec_from_file_location("tilolang_e2e", Path(__file__).with_name("tilolang-e2e.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


e2e = _load_e2e()
Failed = e2e.Failed
run = e2e.run
until = e2e.until
app_id = e2e.app_id


# --- the session -----------------------------------------------------------------------------

class CanvasSession:
    """One headless nested session on a state directory, started and stopped as often as a
    test needs: `start(fresh)`, `stop()`, `restart()`; driven with `control(...)`."""

    def __init__(self, state: Path, out: Path, tapps: list[Path], native: str = NATIVE_APPS):
        self.state = state.resolve()
        self.out = out.resolve()
        self.tapps = tapps
        self.native = native
        self.proc: subprocess.Popen | None = None
        self.ready: dict | None = None
        self.starts = 0
        self.tag = ""
        self.frames: dict[str, dict] = {}

    # -- lifecycle --

    def start(self, fresh: bool = True) -> "CanvasSession":
        """Start the session on its state; `fresh` forgets the saved desktop (workspaces and
        windows, so their rooms too), else it comes back as it was left."""
        if self.proc is not None:
            raise Failed("the session is running already")
        self.starts += 1
        self.tag = f"{self.starts}-{'fresh' if fresh else 'again'}"
        ready = self.out / f"ready-{self.tag}.json"
        ready.unlink(missing_ok=True)
        argv = [SESSION_SH, "--no-build", "--headless", "--software", "--apps", self.native,
                "--state", self.state, "--ready", ready]
        if fresh:
            argv.append("--fresh-desktop")
        for tapp in self.tapps:
            argv += ["--install-tapp", tapp]
        trace = self.out / f"frames-{self.tag}.csv"
        summary = self.out / f"frames-{self.tag}.json"
        self.frames[self.tag] = {"trace": str(trace), "summary": str(summary)}
        env = {
            **os.environ,
            "RUST_LOG": os.environ.get("RUST_LOG", "warn,tilo::launch_latency=info"),
            "TILO_FRAME_TRACE": str(trace),
            "TILO_FRAME_SUMMARY": str(summary),
        }
        print(f"==> starting the headless session ({self.tag}, lavapipe only)", file=sys.stderr)
        log = open(self.out / f"session-{self.tag}.log", "w")
        self.proc = subprocess.Popen(argv, stdout=log, stderr=subprocess.STDOUT,
                                     start_new_session=True, env=env)
        until("the session", lambda: ready.is_file() or self.proc.poll() is not None, 300, 0.25)
        if self.proc.poll() is not None:
            code = self.proc.returncode
            self.proc = None
            raise Failed(f"the session ended ({code}); see {self.out / f'session-{self.tag}.log'}")
        self.ready = json.loads(ready.read_text())
        # A viewer is looking: headless frames are rasterized, so the frame trace measures
        # painting and not layout alone.
        self.control({"op": "watch", "watching": True})
        # Apps open in the Home workspace once this device's native owner holds its room.
        until("the Home workspace to be native", lambda: (self.state / "native-workspace").is_file(), 120, 0.2)
        return self

    def stop(self) -> None:
        """Stop the session: SIGTERM to the script's process group, whose cleanup stops the
        shell's, the bus's and the services' groups. The shell exits cleanly, so the frame
        summary is written."""
        proc, self.proc = self.proc, None
        if proc is None:
            return
        if proc.poll() is None:
            try:
                os.killpg(proc.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                proc.wait(60)
            except subprocess.TimeoutExpired:
                os.killpg(proc.pid, signal.SIGKILL)
                proc.wait(10)
        log = self.state / "logs/shell.log"
        if log.is_file():
            shutil.copy(log, self.out / f"shell-{self.tag}.log")
        self.ready = None

    def restart(self) -> "CanvasSession":
        """Stop and start again on the same state, without --fresh-desktop: the workspaces, their
        windows and their layout survive."""
        self.stop()
        return self.start(fresh=False)

    def running(self) -> bool:
        return self.proc is not None and self.proc.poll() is None

    # -- control --

    def control(self, payload: dict | str, timeout: float = 30.0) -> dict:
        """Send one request to the test control socket and answer its reply, whose one key names
        its shape (`ok`, `state`, `answer`, `foreign_frames`, ...); an `err` reply raises. `payload` is a dict or its JSON text. See
        crates/session/shell/src/shell/test_control.rs for the ops."""
        if isinstance(payload, str):
            payload = json.loads(payload)
        if self.ready is None:
            raise Failed("the session is not running")
        wait = payload.get("wait_ms", 0) / 1000.0
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
            s.settimeout(timeout + wait)
            s.connect(self.ready["control"])
            s.sendall(json.dumps(payload).encode() + b"\n")
            buf = b""
            while b"\n" not in buf:
                chunk = s.recv(1 << 20)
                if not chunk:
                    raise Failed(f"the control socket closed mid-reply to {payload.get('op')}")
                buf += chunk
        reply = json.loads(buf.partition(b"\n")[0])
        if "err" in reply:
            raise Failed(f"{payload.get('op')} refused: {json.dumps(reply['err'])[:2000]}")
        return reply

    def snapshot(self, path: Path) -> Path:
        path = Path(path).resolve()
        path.parent.mkdir(parents=True, exist_ok=True)
        self.control({"op": "snapshot", "path": str(path)})
        until(f"the snapshot {path.name}", lambda: path.is_file() and path.stat().st_size > 0, 30, 0.1)
        return path

    def windows(self) -> list[dict]:
        return self.control({"op": "windows"})["answer"]["windows"]

    def state_dump(self) -> dict:
        return self.control({"op": "state_dump"})["state"]

    def foreign_frames(self) -> list[dict]:
        return self.control({"op": "foreign_frames"})["foreign_frames"]["frames"]

    def presented(self, app: str) -> bool:
        return any(f["app_id"] == app and f["presented"] and f["pixels_ready"] for f in self.foreign_frames())

    def click_at(self, x: float, y: float) -> dict:
        return self.control({"op": "click_at", "x": x, "y": y})

    def right_click_at(self, x: float, y: float) -> None:
        self.control({"op": "pointer_move", "x": x, "y": y})
        self.control({"op": "pointer_button", "pressed": True, "button": "right"})
        self.control({"op": "pointer_button", "pressed": False, "button": "right"})

    def type(self, text: str) -> dict:
        return self.control({"op": "type", "text": text})

    def key(self, key: str) -> dict:
        return self.control({"op": "key", "key": key})

    # -- actions --

    def actions(self, query: str = "") -> list[dict]:
        """The actions listed for `query`, with those listed for agents alone, once the listing
        has settled."""
        found, cursor = [], None
        while True:
            request = {"op": "actions_enumerate", "query": query, "include_programmatic": True,
                       "wait_ms": 5000, "limit": 1000}
            if cursor:
                request["cursor"] = cursor
            answer = self.control(request)["answer"]
            found += answer["actions"]
            cursor = answer.get("next_cursor")
            if not cursor:
                return found

    def invoke(self, provider: str, ident: str, args: dict | None = None) -> dict:
        """Invoke an action by its address and answer its settled receipt."""
        payload = {"op": "actions_invoke", "provider": provider, "id": ident, "wait_ms": 20000}
        if args:
            payload["args"] = args
        return self.control(payload)["answer"]


def start(state: Path, fresh: bool, out: Path | None = None, tapps: list[Path] | None = None,
          native: str = NATIVE_APPS) -> CanvasSession:
    """Start a session on `state` (see CanvasSession.start)."""
    out = Path(out or state.parent / "out")
    out.mkdir(parents=True, exist_ok=True)
    return CanvasSession(state, out, tapps or [], native).start(fresh)


# --- building --------------------------------------------------------------------------------

def prepare(state: Path, build: bool) -> None:
    """Install the session's build into `state`; with `build`, build it first."""
    if build:
        print("==> building the nested session", file=sys.stderr)
        run([REPO / "dist/dev/nested-session-build.sh"], cwd=REPO)
    print("==> preparing the session", file=sys.stderr)
    run([SESSION_SH, "--no-build", "--prepare-only", "--apps", NATIVE_APPS, "--state", state])


def build_tl_apps(state: Path, out: Path, build: bool) -> list[Path]:
    """`tilolang build` each of TL_APPS against the session's runtime: their signed packages."""
    tool = REPO / "target/debug/tilolang"
    if build:
        print("==> building the tilolang tool", file=sys.stderr)
        run(["cargo", "build", "-p", "tilolang-cli", "--bin", "tilolang"], cwd=REPO)
    if not tool.is_file():
        raise Failed(f"no tilolang tool at {tool} (cargo build -p tilolang-cli --bin tilolang)")
    runtime = (state / "runtimes/.version").read_text().strip()
    tapps = []
    for name in TL_APPS:
        print(f"==> tilolang build {name}", file=sys.stderr)
        run([tool, "build", REPO / "apps/tl" / name, "--search", REPO / "apps/tl",
             "--out", out / "build", "--tilo-runtime", f"tilo-runtime/{runtime}"])
        tapp = out / "build" / f"{name}.tapp"
        if not tapp.is_file():
            raise Failed(f"tilolang build made no {tapp}")
        tapps.append(tapp)
    return tapps


# --- windows ---------------------------------------------------------------------------------

def open_apps(session: CanvasSession, apps: list[str]) -> dict:
    """Open one window of each of `apps` (that has none yet) and wait until each presents a
    frame: their windows by app id."""
    for app in apps:
        until(f"{app} among the applications", lambda: any(
            a.get("provider") == "applications" and a.get("id") == app for a in session.actions(app)), 60, 0.5)
    have = {w.get("app_id") for w in session.windows()}
    for app in apps:
        if app not in have:
            session.invoke("applications", app)
    windows = until("a window of every app", lambda: (lambda ws: ws if all(a in ws for a in apps) else None)(
        {w.get("app_id"): w for w in session.windows()}), 60)
    until("a presented frame of every app", lambda: all(session.presented(a) for a in apps), 60)
    return {a: windows[a] for a in apps}


# --- seeding ---------------------------------------------------------------------------------

# A scenario runs only in captures: its hidden action may act as named people at given times
# (`as mia at ...`), which nothing a live room offers may do. So the live room gets the same
# records through what every app publishes to people and agents: its declared actions, and the
# room's typed field edit (`apply` with `edit-text`, what a bound text input sends) for a field
# no declared action sets. The door stamps each change with the session's person; nothing here
# names an author or a time.

TODO_ITEMS = [  # "a saturday list": title, done, notes (due dates have no action to set them)
    ("Take the bins out", True, ""),
    ("Buy oat milk and eggs", False, ""),
    ("Call the landlord about the boiler", False, "Ask whether Tuesday morning works for the engineer."),
    ("Water the plants", True, ""),
    ("Return the library books", False, ""),
    ("Book a haircut", False, ""),
    ("Pay the electricity bill", True, ""),
    ("Fix the bike's back brake", False, ""),
    ("Reply to Mia about Sunday lunch", False, ""),
    ("Clean the oven", False, ""),
    ("Pick up the parcel from the post office", False, "Bring an ID; it closes at one."),
    ("Change the bed sheets", True, ""),
    ("Order a birthday present for Sam", False, ""),
    ("Back up the laptop", False, ""),
]

BOARD_COLUMNS = ["To do", "Doing", "Done"]  # "a release board"

CHAT = [  # "a busy workspace": each conversation and what was said in it, in order
    ("Weekend plans", ["Is everyone still up for the walk on Saturday?",
                       "The long loop, round the reservoir and back over the hill.",
                       "I can bring sandwiches. The forecast says sun until three.",
                       "Count me in.",
                       "Let's meet at the station at ten.",
                       "Platform four, by the coffee stand."]),
    ("Release checklist", ["Docs still need the new screenshots.", "I'll take the screenshots after lunch."]),
    ("Flat hunting", ["The one on Elm Street has a balcony.",
                      "Viewing is Saturday at eleven, bring the deposit papers."]),
    ("Book club: Piranesi", ["Halfway through. The statues are wonderful."]),
    ("Design review", ["The sidebar collapses too early at large text.", "Agreed, I'll look at the thresholds."]),
    ("Sam's birthday", ["Concert tickets or a kitchen knife?"]),
    ("Garden allotment", ["The tomatoes need water by Thursday."]),
    ("Running group", ["Long run on Sunday, fifteen kilometres."]),
    ("Importer bugs", ["The importer drops tables with merged cells."]),
    ("Recipes", ["The lentil soup needs more lemon than you think."]),
    ("Interview loop", ["Ask about the last project they were proud of.", "I'll take the system design round."]),
    ("Holiday in August", []),
]


def app_ids(name: str) -> dict[str, str]:
    """apps/tl/NAME/app.ids: each declaration's stable id, by its name."""
    ids = {}
    for line in (REPO / "apps/tl" / name / "app.ids").read_text().splitlines():
        if "=" in line and not line.startswith("#"):
            key, _, value = line.rpartition("=")
            ids.setdefault(key.strip(), value.strip())
    return ids


class AppActions:
    """The published actions of one tilolang app in the session, by the ids its app.ids names."""

    def __init__(self, session: CanvasSession, name: str, root_type: str):
        self.session = session
        self.app = app_id(name)
        self.ids = app_ids(name)
        self.root = f"root-{self.ids[root_type]}"
        self.root_kind = self.kind(root_type)

    def kind(self, type_name: str) -> str:
        return f"tl.{self.app}.{type_name}"

    def listed(self, operation: str, key: str | None = None) -> dict | None:
        for action in self.session.actions(""):
            identity = action.get("semantic_identity") or {}
            if (action.get("application") == self.app and action.get("operation") == operation
                    and (key is None or identity.get("key") == key)):
                return action
        return None

    def keys(self, type_name: str) -> list[str]:
        """The published resources of `type_name`, as `tl:<app>:<id>` keys, in listing order."""
        kind, seen = self.kind(type_name), []
        for action in self.session.actions(""):
            key = (action.get("semantic_identity") or {}).get("key")
            if action.get("application") == self.app and (action.get("semantic_identity") or {}).get(
                    "kind") == kind and key not in seen:
                seen.append(key)
        return seen

    def invoke(self, operation: str, args: dict | None = None, key: str | None = None) -> dict:
        """Invoke the action `operation` (on the resource `key`, else on the app's root or room),
        listing it again while an id listed a moment ago has moved on."""
        last = None
        for _ in range(20):
            action = self.listed(operation, key)
            if action is None:
                last = f"{self.app} lists no action {operation} on {key or 'its root'}"
                time.sleep(0.5)
                continue
            try:
                update = self.session.invoke(action["provider"], action["id"], args)["receipt"]["update"]
            except Failed as error:
                if "list it again" in str(error) or "unknown_action" in str(error):
                    last = str(error)
                    time.sleep(0.25)
                    continue
                raise
            if update.get("state") != "succeeded":
                raise Failed(f"{self.app} {operation} on {key or 'its root'}: {json.dumps(update)[:1000]}")
            return update
        raise Failed(last or f"{self.app} {operation} never answered")

    def created(self, type_name: str, before: list[str], operation: str, args: dict | None = None,
                key: str | None = None) -> str:
        """Invoke a creating action and answer the key of the one resource it made."""
        self.invoke(operation, args, key)
        new = until(f"the {type_name} {operation} made",
                    lambda: [k for k in self.keys(type_name) if k not in before], 15, 0.25)
        if len(new) != 1:
            raise Failed(f"{operation} made {len(new)} {type_name}s: {new}")
        return new[0]

    def ref(self, type_name: str, key: str, label: str = "") -> dict:
        """A resource argument, as the door takes a picked resource."""
        return {"app": self.app, "kind": self.kind(type_name), "id": key, "label": label}

    def edit_item_text(self, collection: str, key: str, field: str, text: str) -> None:
        """Set the text field `field` of the item `key` of the root's `collection`: the typed edit
        a bound text input sends, applied through the room's `apply`."""
        item = key.rsplit(":", 1)[-1]
        action = {
            "app": self.app, "kind": self.root_kind, "action_id": "edit-text",
            "args": {"field": self.ids[field], "value": text},
            # Who invoked is the door's to record, not this value's.
            "authorship": {"invoker": "", "module": ""},
            "edits": [{"op": "edit", "id": self.root, "edit": {
                "path": [{"field": self.ids[collection]}, {"item": item}, {"field": self.ids[field]}],
                "op": "replace", "text": text}}],
        }
        self.invoke("apply", {"action": action})


def expect(app: str, got: dict, **want) -> dict:
    """`got`, once each count in `want` is what it says."""
    wrong = {k: (got[k], v) for k, v in want.items() if got[k] != v}
    if wrong:
        raise Failed(f"{app} holds other counts than its scenario (got, want): {wrong}")
    return got


def seed_todo(session: CanvasSession) -> dict:
    todo = AppActions(session, "todo", "TodoList")
    todo.invoke(todo.ids["rename"], {"name": "Saturday"})
    for title, done, notes in TODO_ITEMS:
        key = todo.created("Todo", todo.keys("Todo"), todo.ids["add"], {"title": title})
        if done:
            todo.invoke(todo.ids["check"], {"done": True}, key)
        if notes:
            todo.edit_item_text("TodoList.items", key, "Todo.notes", notes)
    return expect("Todo", {"list": "Saturday", "items": len(todo.keys("Todo")),
                           "done": sum(1 for _, done, _ in TODO_ITEMS if done)}, items=len(TODO_ITEMS))


def seed_chat(session: CanvasSession) -> dict:
    chat = AppActions(session, "chat", "Conversations")
    for title, said in CHAT:
        key = chat.created("Conversation", chat.keys("Conversation"), chat.ids["start"], {"title": title})
        for text in said:
            chat.invoke(chat.ids["post"], {"text": text, "block": ""}, key)
    return expect("Chat", {"conversations": len(chat.keys("Conversation")),
                           "messages": sum(len(s) for _, s in CHAT)}, conversations=len(CHAT))


BOARD_CARDS = [  # "a release board": column, title, notes
    ("To do", "Write the release notes", "Lead with the sync fix, then the new board."),
    ("To do", "Screenshots for the docs", ""),
    ("To do", "Translate the onboarding", "German and French first."),
    ("To do", "Check the importer on large tables", ""),
    ("Doing", "Fix the sidebar at large text", "It collapses too early above 150%."),
    ("Doing", "Review the sharing dialog", ""),
    ("Done", "Bump the version", ""),
    ("Done", "Merge the offline queue", "Landed on Tuesday."),
    ("Done", "Plan the next cycle", ""),
]


def seed_board(session: CanvasSession) -> dict:
    """The release board: its three columns and the cards in each."""
    board = AppActions(session, "board", "Board")
    columns = {}
    for title in BOARD_COLUMNS:
        key = board.created("Column", board.keys("Column"), board.ids["new_column"])
        board.invoke(board.ids["rename_column"], {"title": title}, key)
        columns[title] = key
    for column, title, notes in BOARD_CARDS:
        key = board.created("Card", board.keys("Card"), board.ids["new_card"],
                            {"col": board.ref("Column", columns[column], column)})
        board.edit_item_text("Board.cards", key, "Card.title", title)
        if notes:
            board.edit_item_text("Board.cards", key, "Card.notes", notes)
    return expect("Board", {"columns": len(board.keys("Column")), "cards": len(board.keys("Card"))},
                  columns=len(BOARD_COLUMNS), cards=len(BOARD_CARDS))


def seed(session: CanvasSession) -> dict:
    """Fill each tilolang app's room with its scenario's data, through the apps' public actions."""
    return {"todo": seed_todo(session), "chat": seed_chat(session), "board": seed_board(session)}


# --- canvas ----------------------------------------------------------------------------------

ASK_FREE = "show me this week's release work"
ASK_CARD = "what blocks this card from being done?"
TASKS = "so.tilo.tasks"


def canvas_state(session: CanvasSession) -> dict:
    """The canvas as the shell shows it (state_dump's chrome.canvas)."""
    return session.state_dump()["chrome"].get("canvas") or {}


def canvas_windows(session: CanvasSession) -> dict[int, dict]:
    """Every window on the canvas by its group: its place (canvas pixels) and screen rect, with
    the app its front tab shows."""
    apps = {w.get("group"): w.get("app_id") for w in session.windows()}
    return {w["group"]: {**w, "app_id": apps.get(w["group"])} for w in canvas_state(session).get("windows", [])}


def overlaps(a: list[float], b: list[float]) -> bool:
    return a[0] < b[0] + b[2] and b[0] < a[0] + a[2] and a[1] < b[1] + b[3] and b[1] < a[1] + a[3]


def no_overlaps(windows: dict[int, dict]) -> bool:
    placed = [w["place"] for w in windows.values() if w.get("place")]
    return all(not overlaps(a, b) for i, a in enumerate(placed) for b in placed[i + 1:])


def trace_tail(session: CanvasSession) -> int:
    """The start_us of the last tick the frame trace holds now."""
    trace = Path(session.frames[session.tag]["trace"])
    lines = trace.read_text().strip().splitlines() if trace.is_file() else []
    for line in reversed(lines):
        first = line.split(",", 2)
        if len(first) > 1 and first[1].isdigit():
            return int(first[1])
    return 0


def frame_window(session: CanvasSession, since_us: int, until_us: int, name: str) -> dict:
    """What the frame loop did between two trace marks: how many frames it painted, how long
    each painted frame's work took (input, pump, layout, scene, submit), and the period between
    painted frames, against the 60 Hz the headless output runs at."""
    trace = Path(session.frames[session.tag]["trace"])
    header, *rows = trace.read_text().strip().splitlines()
    cols = header.split(",")
    at = {c: i for i, c in enumerate(cols)}
    work, starts = [], []
    for row in rows:
        v = row.split(",")
        if len(v) != len(cols) or not v[at["start_us"]].isdigit():
            continue
        start = int(v[at["start_us"]])
        if not (since_us < start <= until_us) or v[at["painted"]] != "1":
            continue
        starts.append(start)
        work.append(sum(int(v[at[c]] or 0) for c in ("input_us", "pump_us", "layout_us", "scene_us", "submit_us")))
    work.sort()
    periods = sorted(b - a for a, b in zip(starts, starts[1:]))

    def pct(values, p):
        return values[min(len(values) - 1, int(len(values) * p))] / 1000.0 if values else None

    seconds = (until_us - since_us) / 1e6 if until_us > since_us else 0
    return {
        "what": name,
        "painted_frames": len(work),
        "seconds": round(seconds, 2),
        "painted_fps": round(len(work) / seconds, 1) if seconds else None,
        "frame_work_ms": {"p50": pct(work, 0.5), "p95": pct(work, 0.95), "p99": pct(work, 0.99),
                          "max": work[-1] / 1000.0 if work else None},
        "frame_period_ms": {"p50": pct(periods, 0.5), "p95": pct(periods, 0.95)},
        "budget_ms": round(1000 / 60, 2),
    }


def measure(session: CanvasSession, name: str, drive) -> dict:
    """Frame times while `drive()` runs, read from the session's frame trace."""
    session.control({"op": "pointer_move", "x": 2, "y": 400})
    time.sleep(0.3)
    since = trace_tail(session)
    drive()
    time.sleep(0.4)
    session.control({"op": "pointer_move", "x": 3, "y": 400})
    time.sleep(0.3)
    return frame_window(session, since, trace_tail(session), name)


def canvas_run(session: CanvasSession, out: Path, shots: Path) -> dict:
    """The canvas end to end, in a fresh session:

    1. the workspace shows its canvas, empty; a click on it puts the caret there (shot 1),
       typing goes to it (shot 2), and Enter asks the workspace's agent: the ask's window (its
       Tasks issue, which the agent works on) opens where the caret was (shot 3);
    2. Board, Todo and Chat open over their sample data and take free space without moving the
       answer (shot 4);
    3. a sentence typed into the menu of Board (right-click, type, Enter) asks about it, and its
       answer opens beside Board;
    4. panning, zooming and typing are measured from the frame trace;
    5. a restart keeps every window's place."""
    steps: dict = {}
    shots.mkdir(parents=True, exist_ok=True)
    session.start(fresh=True)
    # An agent of the workspace's own takes the asks; the Tasks issue waits for it.
    steps["agent"] = session.invoke("actions", "agent.invite", {"name": "Claude", "preset": "contributor"})
    # Adding an agent opens its access review in the workspace settings; close it.
    session.key("Escape")
    time.sleep(1.0)
    session.invoke("actions", "workspace.show-canvas")
    state = until("the canvas", lambda: (lambda s: s if s.get("shown") and s.get("frame") else None)(
        canvas_state(session)), 30)
    fx, fy, fw, fh = state["frame"]
    caret_at = (fx + fw * 0.30, fy + fh * 0.28)
    session.click_at(*caret_at)
    caret = until("a caret where the canvas was clicked", lambda: canvas_state(session).get("caret"), 10)
    session.snapshot(shots / "01-empty-canvas-caret.png")
    session.type(ASK_FREE)
    until("the sentence at the caret", lambda: (canvas_state(session).get("caret") or {}).get("text") == ASK_FREE, 10)
    session.snapshot(shots / "02-typing.png")
    session.key("Enter")
    answer = until("the answer's window at the caret", lambda: next(
        (w for w in canvas_windows(session).values()
         if w.get("app_id") == TASKS and w.get("place")
         and abs(w["place"][0] - caret["at"][0]) < 1 and abs(w["place"][1] - caret["at"][1]) < 1), None), 90)
    until("the answer's window presents", lambda: session.presented(TASKS), 60)
    time.sleep(1.0)
    session.snapshot(shots / "03-answer-arriving.png")
    steps["free_space_ask"] = {"caret": caret, "answer_window": answer}

    apps = [app_id(n) for n in TL_APPS]
    open_apps(session, apps)
    steps["seed"] = seed(session)
    windows = until("every window placed, none over another", lambda: (lambda ws: ws if all(
        any(w.get("app_id") == a and w.get("place") for w in ws.values()) for a in apps) and no_overlaps(ws)
        else None)(canvas_windows(session)), 30)
    kept = windows[answer["group"]]["place"]
    if kept != answer["place"]:
        raise Failed(f"the answer moved when the apps opened: {answer['place']} -> {kept}")
    time.sleep(1.5)
    session.snapshot(shots / "04-populated-canvas.png")
    steps["populated"] = {str(g): {"app": w["app_id"], "place": w["place"]} for g, w in windows.items()}

    # Right-click Board and type: the menu takes the sentence, and its answer opens beside Board.
    board = next(w for w in windows.values() if w.get("app_id") == app_id("board"))
    bx, by, bw, bh = board["screen"]
    tasks_before = {g for g, w in windows.items() if w.get("app_id") == TASKS}
    session.right_click_at(bx + bw * 0.25, by + bh * 0.35)
    time.sleep(0.6)
    session.snapshot(shots / "05-board-menu.png")
    session.type(ASK_CARD)
    time.sleep(0.4)
    session.snapshot(shots / "06-board-menu-typed.png")
    session.key("Enter")
    beside = until("the menu's answer beside Board", lambda: next(
        (w for g, w in canvas_windows(session).items()
         if w.get("app_id") == TASKS and g not in tasks_before and w.get("place")), None), 90)
    bp = board["place"]
    gap = beside["place"][0] - (bp[0] + bp[2])
    steps["board_menu_ask"] = {"board": bp, "answer_window": beside["place"], "gap_right_of_board": gap}
    time.sleep(1.5)
    session.snapshot(shots / "07-answer-beside-board.png")

    # Frame times: panning (wheel), zooming (Ctrl+wheel) and typing at a caret, on empty canvas.
    empty = (fx + fw - 40, fy + fh - 40)

    def pan():
        session.control({"op": "pointer_move", "x": empty[0], "y": empty[1]})
        for i in range(60):
            session.control({"op": "scroll", "dx": 0.0 if i % 2 else 1.0, "dy": -1.0 if i < 30 else 1.0})
            time.sleep(1 / 60)

    def zoom():
        session.control({"op": "pointer_move", "x": empty[0], "y": empty[1]})
        session.control({"op": "modifiers", "ctrl": True, "shift": False, "alt": False, "logo": False})
        for i in range(40):
            session.control({"op": "scroll", "dx": 0.0, "dy": -1.0 if i < 20 else 1.0})
            time.sleep(1 / 60)
        session.control({"op": "modifiers", "ctrl": False, "shift": False, "alt": False, "logo": False})

    def typing():
        session.click_at(*empty)
        for ch in "release notes for friday":
            session.type(ch)
            time.sleep(1 / 30)
        session.key("Escape")

    steps["frames"] = [measure(session, "pan", pan), measure(session, "zoom", zoom),
                       measure(session, "type", typing)]

    before = {g: w["place"] for g, w in canvas_windows(session).items() if w.get("place")}
    session.restart()
    after = until("the canvas after a restart, every window placed", lambda: (lambda s, ws: ws if s.get("shown") and all(
        g in ws and ws[g].get("place") for g in before) else None)(canvas_state(session), canvas_windows(session)), 90)
    moved = {g: (before[g], after[g]["place"]) for g in before if after[g]["place"] != before[g]}
    if moved:
        raise Failed(f"windows moved across the restart: {moved}")
    time.sleep(2.0)
    session.snapshot(shots / "08-after-restart.png")
    steps["restart"] = {"places_kept": len(before)}
    return steps


def canvas_steps(session: CanvasSession, out: Path) -> dict:
    """The canvas's own steps in the smoke session (the full run is --canvas)."""
    session.invoke("actions", "workspace.show-canvas")
    state = until("the canvas", lambda: (lambda s: s if s.get("shown") else None)(canvas_state(session)), 30)
    return {"shown": state.get("shown"), "windows": len(state.get("windows", []))}


# --- smoke -----------------------------------------------------------------------------------

def smoke(session: CanvasSession, out: Path) -> dict:
    summary: dict = {"steps": {}, "failed": []}

    def step(name, fn):
        print(f"==> {name}", file=sys.stderr)
        t0 = time.monotonic()
        try:
            summary["steps"][name] = fn()
        except Failed as error:
            summary["failed"].append(name)
            summary["steps"][name] = {"failed": str(error)}
            print(f"FAIL {name}: {error}", file=sys.stderr)
            raise
        finally:
            summary.setdefault("seconds", {})[name] = round(time.monotonic() - t0, 1)

    apps = [app_id(n) for n in TL_APPS]
    try:
        step("start", lambda: session.start(fresh=True) and {"ready": session.ready})
        step("windows", lambda: {a: {k: w.get(k) for k in ("workspace", "group", "tab", "title")}
                                 for a, w in open_apps(session, apps).items()})
        step("seed", lambda: seed(session))
        step("snapshot", lambda: {"path": str(session.snapshot(out / "smoke.png"))})
        step("canvas", lambda: canvas_steps(session, out))
        step("restart", lambda: session.restart() and {"ready": session.ready})
        step("windows-again", lambda: check_windows_back(session, summary["steps"]["windows"]))
        step("snapshot-again", lambda: {"path": str(session.snapshot(out / "smoke-again.png"))})
    except Failed:
        pass
    finally:
        session.stop()
    summary["frames"] = frame_summaries(session)
    return summary


def check_windows_back(session: CanvasSession, before: dict) -> dict:
    """After a restart without --fresh-desktop, each window is back in its workspace, and each
    app presents a frame again."""
    windows = until("the windows restored", lambda: (lambda ws: ws if all(a in ws for a in before) else None)(
        {w.get("app_id"): w for w in session.windows()}), 60)
    moved = {a: (before[a]["workspace"], windows[a].get("workspace"))
             for a in before if windows[a].get("workspace") != before[a]["workspace"]}
    if moved:
        raise Failed(f"windows came back in other workspaces: {moved}")
    tree = session.state_dump()["workspace_tree"]
    restored = {"windows": {a: {k: windows[a].get(k) for k in ("workspace", "group", "tab", "title")}
                            for a in before},
                "workspaces": len(tree.get("workspaces", []))}
    try:
        until("a presented frame of every restored app", lambda: all(session.presented(a) for a in before), 60)
    except Failed:
        missing = [a for a in before if not session.presented(a)]
        log = (session.state / "logs/shell.log").read_text(errors="replace").splitlines()
        said = [line[-300:] for line in log
                if any(f"[app {a} stderr] error" in line or f"[app {a} stderr] Error" in line for a in missing)]
        raise Failed(f"the windows came back ({json.dumps(restored)}) but these apps present no frame: "
                     f"{missing}; they said: {said[-6:]}") from None
    return restored


def frame_summaries(session: CanvasSession) -> dict:
    out = {}
    for tag, paths in session.frames.items():
        summary = Path(paths["summary"])
        out[tag] = {**paths, "written": summary.is_file()}
        if summary.is_file():
            try:
                out[tag]["summary_json"] = json.loads(summary.read_text())
            except json.JSONDecodeError:
                pass
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--smoke", action="store_true", help="run the smoke sequence")
    parser.add_argument("--canvas", action="store_true", help="run the canvas end to end (canvas_run)")
    parser.add_argument("--shots", type=Path, help="where --canvas writes its screenshots (default OUT/shots)")
    parser.add_argument("--out", type=Path, default=REPO / "target/canvas-e2e")
    parser.add_argument("--state", type=Path, help="the session's state (default OUT/state)")
    parser.add_argument("--build", action="store_true",
                        help="build the nested session and the tilolang tool first (remotely)")
    parser.add_argument("--keep", action="store_true", help="leave the session --start started running")
    parser.add_argument("--start", action="store_true",
                        help="start a fresh session, seed it and open the windows, then print its ready JSON")
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    state = (args.state or out / "state").resolve()
    os.environ.setdefault("TMPDIR", str(Path.home() / "tmp"))
    Path(os.environ["TMPDIR"]).mkdir(parents=True, exist_ok=True)

    t0 = time.monotonic()
    prepare(state, args.build)
    tapps = build_tl_apps(state, out, args.build)
    session = CanvasSession(state, out, tapps)
    if args.smoke:
        summary = smoke(session, out)
        summary["total_seconds"] = round(time.monotonic() - t0, 1)
        (out / "summary.json").write_text(json.dumps(summary, indent=2))
        verdict = "FAILED: " + ", ".join(summary["failed"]) if summary["failed"] else "smoke passed"
        print(f"==> {verdict} (summary: {out / 'summary.json'})", file=sys.stderr)
        return 1 if summary["failed"] else 0
    if args.canvas:
        summary: dict = {}
        try:
            summary["steps"] = canvas_run(session, out, (args.shots or out / "shots").resolve())
            summary["passed"] = True
        except Failed as error:
            summary["passed"] = False
            summary["failed"] = str(error)
            print(f"FAIL canvas: {error}", file=sys.stderr)
        finally:
            session.stop()
        summary["frame_summaries"] = frame_summaries(session)
        summary["total_seconds"] = round(time.monotonic() - t0, 1)
        (out / "canvas-summary.json").write_text(json.dumps(summary, indent=2))
        print(f"==> canvas {'passed' if summary['passed'] else 'FAILED'} (summary: {out / 'canvas-summary.json'})",
              file=sys.stderr)
        return 0 if summary["passed"] else 1
    if args.start:
        try:
            session.start(fresh=True)
            open_apps(session, [app_id(n) for n in TL_APPS])
            seed(session)
            print(json.dumps({**session.ready, "pid": session.proc.pid}))
        finally:
            if not args.keep:
                session.stop()
        return 0
    parser.print_help()
    return 2


if __name__ == "__main__":
    sys.exit(main())
