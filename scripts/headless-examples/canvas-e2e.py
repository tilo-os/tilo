#!/usr/bin/env python3
"""The canvas: what a person types on it, and where their windows stand on it.

One headless nested session whose whole screen is the tilolang shell (`--shell screen`), with
the tree's own apps installed: the shell itself, Board and Chat built from `apps/tl` into
signed packages, and Notes, Chat, Tasks and the agent from the session's build. Nothing here
starts an app binary directly and nothing touches this machine's desktop: lavapipe alone, a
private runtime directory, a throwaway identity. Every session it starts is stopped again, its
process groups and all.

What it drives, as a person does:

  style    the desk shows the canvas: the bar's style menu, then Frames, pressed where they are
           drawn
  windows  Board and Chat open onto the canvas and take free space on it
  ask      a sentence typed at the canvas entry is the person's send to the workspace's agent:
           Chat opens their conversation with it as a place on the canvas, and no Tasks window
           opens
  again    a second ask lands in the conversation the first opened: that window is raised and
           no second window of the same conversation stands
  placed   every window stands at its own place on the workspace's canvas, none over another,
           and each presents a frame
  frames   panning, zooming and typing keep the loop inside a frame's budget, read from the
           frame trace of the start (`TILO_FRAME_TRACE`, `TILO_FRAME_SUMMARY`)
  restart  on the same state again, without --fresh-desktop: every place is where it was

The verdict is OUT/canvas-summary.json; the exit status is non-zero when a step fails. The
session's build is `target/nested-session`, or TILO_NESTED_SESSION_BUILD (`--build` makes it
through dist/dev/nested-session-build.sh, or through TILO_NESTED_SESSION_BUILD_EXEC where this
machine has no toolchain). Nothing runs it on a schedule: it takes a session of its own, so a
person starts it.

Usage:
  scripts/headless-examples/canvas-e2e.py --canvas [--out DIR] [--state DIR] [--shots DIR]
  scripts/headless-examples/canvas-e2e.py --start --keep [--out DIR] [--state DIR]

--start starts a fresh session, shows the canvas and opens the apps, prints its ready JSON
(control socket, runtime directory, the script's pid) and, with --keep, leaves it running: stop
it with `kill -TERM -<pid>`.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]

# The tilolang apps as the apps' own packages name them (their `app` declaration), by their
# directory under apps/tl: the shell is the session's screen, and Board is a window it draws on
# the canvas.
TL_APPS = ["shell", "board"]
# The apps the session's build installs beside them (`--apps`): Chat is where the canvas' ask
# lands, the agent answers it there, and Tasks is installed and closed — an ask must not open
# its window.
NATIVE_APPS = "chat tasks agent"
# The style whose desk is the canvas, and the canvas' own entry, by the names the desk's
# controls answer to.
CANVAS = "Frames"
CANVAS_ENTRY = "Find something, or ask"


sys.path.insert(0, str(Path(__file__).resolve().parent))
# The session this drives and the tools that act in it: the one harness every
# check of a desktop with the tree's apps starts (scripts/headless-examples/real_apps.py).
import real_apps  # noqa: E402
from real_apps import Failed, run, until  # noqa: E402


def app_id(name: str) -> str:
    """The application id of the tree's tilolang app `name`: its package's own."""
    return f"so.tilo.lang.{name}"


# --- the session -----------------------------------------------------------------------------

class CanvasSession(real_apps.Session):
    """One headless nested session on a state directory, started and stopped as often as a
    test needs, with the frame trace of each start recorded (see real_apps.Session)."""

    def __init__(self, state: Path, out: Path, tapps: list[Path], native: str = NATIVE_APPS):
        super().__init__(state)
        self.out = Path(out).resolve()
        self.tapps = tapps
        self.native = native
        self.starts = 0
        self.frames: dict[str, dict] = {}

    def start(self, fresh: bool = True) -> "CanvasSession":
        """Start the session on its state, the tilolang shell as its whole screen; `fresh`
        forgets the saved desktop (workspaces and windows, so their places too), else it comes
        back as it was left."""
        self.starts += 1
        tag = f"{self.starts}-{'fresh' if fresh else 'again'}"
        trace, summary = self.out / f"frames-{tag}.csv", self.out / f"frames-{tag}.json"
        self.frames[tag] = {"trace": str(trace), "summary": str(summary)}
        super().start(apps=self.native, fresh=fresh, out=self.out, tag=tag, tapps=self.tapps,
                      session_args=["--shell", "screen"],
                      env={"TILO_FRAME_TRACE": str(trace), "TILO_FRAME_SUMMARY": str(summary)})
        return self

    def stop(self) -> None:
        """Stop the session: SIGTERM to the script's process group, whose cleanup stops the
        shell's, the bus's and the services' groups. The shell exits cleanly, so the frame
        summary is written."""
        tag = self.tag
        super().stop()
        log = self.state / "logs/session.log"
        if tag and log.is_file():
            shutil.copy(log, self.out / f"shell-{tag}.log")

    def restart(self) -> "CanvasSession":
        """Stop and start again on the same state, without --fresh-desktop: the workspaces, their
        windows and their places survive."""
        self.stop()
        return self.start(fresh=False)


def start(state: Path, fresh: bool, out: Path | None = None, tapps: list[Path] | None = None,
          native: str = NATIVE_APPS) -> CanvasSession:
    """Start a session on `state` (see CanvasSession.start)."""
    out = Path(out or state.parent / "out")
    out.mkdir(parents=True, exist_ok=True)
    return CanvasSession(state, out, tapps or [], native).start(fresh)


# --- building --------------------------------------------------------------------------------

def prepare(state: Path, build: bool) -> None:
    """Install the session's build into `state`; with `build`, build it first."""
    real_apps.prepare(state, apps=NATIVE_APPS, built=build,
                      remote=os.environ.get("TILO_NESTED_SESSION_BUILD_EXEC"))


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


# --- the desk --------------------------------------------------------------------------------

def control_rect(session: CanvasSession, name: str, deadline: float = 30.0) -> list[float]:
    """Where the desk draws the control `name` now, once it stands still across two frames: a
    press aimed at a place a control has left misses it."""
    def standing():
        first = next((t.get("rect") for t in session.targets() if t.get("name") == name), None)
        if first is None:
            return None
        time.sleep(0.1)
        second = next((t.get("rect") for t in session.targets() if t.get("name") == name), None)
        return second if first == second else None

    rect = until(f"the control {name!r} to stand still", standing, deadline)
    return rect


def press(session: CanvasSession, name: str) -> dict:
    """Press the control `name` where it is drawn, as the person's pointer does."""
    x, y, w, h = control_rect(session, name)
    return session.click_at(x + w / 2, y + h / 2)


def show_canvas(session: CanvasSession) -> dict:
    """Show the desk as the canvas: the bar's style menu, then Frames, as a person switches."""
    menu = next((t["name"] for t in session.targets()
                 if str(t.get("name", "")).startswith("Showing ")), None)
    if menu is None:
        raise Failed(f"the bar offers no style menu: "
                     f"{[t.get('name') for t in session.targets()][:20]}")
    pressed = {"menu": menu}
    press(session, menu)
    rect = control_rect(session, CANVAS)
    pressed[CANVAS] = rect
    press(session, CANVAS)
    until("the canvas' entry", lambda: any(
        t.get("name") == CANVAS_ENTRY for t in session.targets()), 30)
    return pressed


# --- windows and places ----------------------------------------------------------------------

def canvas_places(session: CanvasSession) -> dict[str, dict]:
    """Where each window group of the desk stands on its workspace's canvas, by group: the
    arrangement's own rows, as the desk publishes them (`arrangement.places` of each workspace,
    a place being where its top-leading corner is and how wide and tall it stands there)."""
    tree = session.state_dump().get("workspace_tree") or {}
    places: dict[str, dict] = {}
    for workspace in tree.get("workspaces") or []:
        for group, place in (workspace.get("arrangement") or {}).get("places", {}).items():
            places[str(group)] = place
    return places


def placed(session: CanvasSession) -> dict[str, dict]:
    """Every open window with the place it stands at on the canvas, by its app: what a person
    sees, the window and where on the canvas it is."""
    rows = {}
    for window in placed_rows(session):
        rows[window.get("app_id")] = window
    return rows


def placed_rows(session: CanvasSession) -> list[dict]:
    """Every open window with the place it stands at, one row by window: the two windows of one
    app — an app and the conversation an ask opened in it — are two rows, each at its own
    place."""
    places = canvas_places(session)
    return [{**window, "place": places.get(str(window.get("group")))}
            for window in session.windows()]


def chat_windows(session: CanvasSession) -> list[dict]:
    """The windows of Chat the desk stands: a conversation is one of them."""
    return [row for row in session.windows() if row.get("app_id") == "so.tilo.chat"]


def overlaps(a: list[float], b: list[float]) -> bool:
    return a[0] < b[0] + b[2] and b[0] < a[0] + a[2] and a[1] < b[1] + b[3] and b[1] < a[1] + a[3]


def no_overlaps(rows: list[dict]) -> bool:
    """No two windows stand over one another on the canvas."""
    at = [row["place"] for row in rows if row.get("place")]
    return all(not overlaps([p["x"], p["y"], p["width"], p["height"]],
                            [q["x"], q["y"], q["width"], q["height"]])
               for at_index, p in enumerate(at) for q in at[at_index + 1:])


# --- windows ---------------------------------------------------------------------------------

def open_apps(session: CanvasSession, apps: list[str]) -> dict:
    """Open one window of each of `apps` (that has none yet) and wait until each presents a
    frame: their windows by app id."""
    for app in apps:
        until(f"{app} among the applications", lambda: any(
            a.get("provider") == "applications" and a.get("id") == app
            for a in session.actions(app)), 60, 0.5)
    have = {w.get("app_id") for w in session.windows()}
    for app in apps:
        if app not in have:
            session.invoke("applications", app)
    windows = until("a window of every app", lambda: (lambda ws: ws if all(a in ws for a in apps) else None)(
        {w.get("app_id"): w for w in session.windows()}), 60)
    until("every app answering with its own actions",
          lambda: all(session.answering(a) for a in apps), 60)
    return {a: windows[a] for a in apps}


# --- the canvas' own actions -----------------------------------------------------------------

def type_on_canvas(session: CanvasSession, text: str) -> None:
    """Type `text` at the canvas' entry and submit it, as the person does: the entry is where
    they point on the canvas, and Enter asks it."""
    press(session, CANVAS_ENTRY)
    time.sleep(0.3)
    session.type(text)
    time.sleep(0.4)
    session.key("Enter")


# --- frames ----------------------------------------------------------------------------------

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


# --- the canvas, end to end ------------------------------------------------------------------

ASK = "what is on my task list?"
# The second ask lands in the conversation the first opened: one window shows it, and the
# person's lens is on it, never a second window of the same conversation.
ASK_AGAIN = "and what is due today?"

# How long the windows must stand still for the second ask to have landed: Chat opens the
# conversation's window as soon as the send reaches it, so a second window would show by then.
# That the send itself lands is the session's own test (`session/tests/canvas_ask.rs`: a second
# ask goes to the agent the first made); what only a live session shows is the window it opens.
SECOND_ASK_SETTLES = 10.0


def canvas_run(session: CanvasSession, out: Path, shots: Path) -> dict:
    """The canvas end to end, in a fresh session:

    1. the desk shows the canvas: the person switches the bar's style to Frames, and the
       canvas' entry stands on it;
    2. Board and Chat open onto it, each stands at its own place on the canvas, none over
       another, and each presents a frame (shot 1);
    3. a sentence typed at the entry asks the workspace's agent: Chat opens their conversation
       with it as a place on the canvas, and no Tasks window opens (shot 2); a second ask lands
       in that same window, raised, and opens no second window of the conversation;
    4. panning, zooming and typing at the entry are measured from the frame trace;
    5. a restart keeps every place (shot 3).

    The words the send carried are the session's own test (`session/tests/canvas_ask.rs`: the
    entry's submit is `shell:arrangement#place-found`, and what it sends is `session:send-to`'s
    reading); what only a live session shows is here: the conversation the person reads the
    answer in stands on the canvas, at the free space beside them."""
    steps: dict = {}
    shots.mkdir(parents=True, exist_ok=True)
    session.start(fresh=True)
    steps["style"] = show_canvas(session)
    apps = [app_id("board"), "so.tilo.chat"]
    open_apps(session, apps)
    until("every window placed, none over another",
          lambda: (lambda rows: rows if all(row.get("place") for row in rows)
                   and no_overlaps(rows) else None)(placed_rows(session)), 30)
    until("every window to present its own frame",
          lambda: all(row.get("frames") for row in placed(session).values()), 30)
    time.sleep(1.5)
    session.snapshot(shots / "01-populated-canvas.png")
    before = placed(session)
    steps["populated"] = {app: {"place": row.get("place"), "rect": row.get("rect")}
                          for app, row in before.items()}

    # A sentence at the entry is the person's ask: it goes to the workspace's agent, and the
    # conversation it lands in stands on the canvas beside them.
    conversation_before = {row.get("group") for row in session.windows() if row.get("app_id") == "so.tilo.chat"}
    type_on_canvas(session, ASK)
    conversation = until("Chat to open the conversation the ask landed in", lambda: (
        lambda new: new[0] if new else None)(
        [row for row in session.windows()
         if row.get("app_id") == "so.tilo.chat" and row.get("group") not in conversation_before]), 90)
    stood = until("the conversation to stand on the canvas", lambda: next(
        (row for row in placed_rows(session)
         if row.get("group") == conversation.get("group")), {}).get("place"), 30)
    if any(row.get("app_id") == "so.tilo.tasks" for row in session.windows()):
        raise Failed("asking on the canvas opened a Tasks window")
    steps["ask"] = {"asked": ASK, "conversation": conversation.get("title"), "place": stood}
    time.sleep(1.5)
    session.snapshot(shots / "02-asked.png")

    # A second ask lands in the same conversation: the window it already shows is raised and
    # revealed, never a second window of the same conversation.
    standing = {row.get("group"): row for row in chat_windows(session)}
    type_on_canvas(session, ASK_AGAIN)
    since = time.monotonic()

    def the_same_windows():
        now = {row.get("group"): row for row in chat_windows(session)}
        if set(now) != set(standing):
            raise Failed(f"the second ask opened another window of the conversation: "
                         f"{sorted(set(now) - set(standing))}")
        if time.monotonic() - since < SECOND_ASK_SETTLES:
            return None
        return now

    after_ask = until("the second ask to land in the standing conversation", the_same_windows, 60)
    raised = [row for row in after_ask.values() if row.get("active")]
    if not raised:
        raise Failed(f"the second ask left the conversation's window behind: {after_ask}")
    steps["ask-again"] = {"asked": ASK_AGAIN, "windows": len(after_ask),
                          "conversation": raised[0].get("title")}

    # Frames: panning (wheel), zooming (Ctrl+wheel) and typing at the entry, on the canvas.
    empty = (40.0, 700.0)

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
        press(session, CANVAS_ENTRY)
        for ch in "release notes for friday":
            session.type(ch)
            time.sleep(1 / 30)
        session.key("Escape")

    steps["frames"] = [measure(session, "pan", pan), measure(session, "zoom", zoom),
                       measure(session, "type", typing)]

    before_restart = {group: place for group, place in canvas_places(session).items()}
    session.restart()
    after = until("the canvas after a restart, every place kept", lambda: (lambda places: places if
                 all(group in places and places[group] for group in before_restart) else None)(
        canvas_places(session)), 90)
    moved = {group: (before_restart[group], after[group]) for group in before_restart
             if after[group] != before_restart[group]}
    if moved:
        raise Failed(f"places moved across the restart: {moved}")
    time.sleep(2.0)
    session.snapshot(shots / "03-after-restart.png")
    steps["restart"] = {"places_kept": len(before_restart)}
    return steps


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
    parser.add_argument("--canvas", action="store_true", help="run the canvas end to end")
    parser.add_argument("--shots", type=Path, help="where --canvas writes its screenshots (default OUT/shots)")
    parser.add_argument("--out", type=Path, default=REPO / "target/canvas-e2e")
    parser.add_argument("--state", type=Path, help="the session's state (default OUT/state)")
    parser.add_argument("--build", action="store_true",
                        help="build the nested session and the tilolang tool first (remotely)")
    parser.add_argument("--keep", action="store_true", help="leave the session --start started running")
    parser.add_argument("--start", action="store_true",
                        help="start a fresh session, show the canvas and open the apps, then print its ready JSON")
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
            show_canvas(session)
            open_apps(session, [app_id("board"), "so.tilo.chat"])
            print(json.dumps({**session.ready, "pid": session.proc.pid}))
        finally:
            if not args.keep:
                session.stop()
        return 0
    parser.print_help()
    return 2


if __name__ == "__main__":
    sys.exit(main())
