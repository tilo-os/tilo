#!/usr/bin/env python3
"""A field a person types into in an app tells the desk where its caret is, with
no input method running, and Super+V opens the clipboard under that caret.

A session starts on a scratch home with the tree's own Notes, the person opens
Notes, makes a page with Ctrl+N and types into its body. Everything is read off
the desk's own state, as the paste pane and the presence carets read it:

  caret    the desk holds a caret once the page's name is typed, follows it
           down into the body, and moves right as more is typed
  paste    Super+V, pressed the moment the caret moves, opens the clipboard pane
           under that caret's line, its first frame as tall as its page
  closed   Escape closes the pane and the caret stays where it was

It needs the session and the tree's apps built (dist/dev/nested-session-build.sh,
or `TILO_NESTED_SESSION_BUILD` naming where they were).

Usage:
  ./notes-caret-proof.py [--build]
"""
import json
import os
import pathlib
import sys
import tempfile
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import real_apps  # noqa: E402
from real_apps import Failed, until  # noqa: E402

NOTES = "so.tilo.notes"
APPS = "notes"
STATE = pathlib.Path(os.environ.get("TILO_NOTES_PROOF_STATE") or tempfile.mkdtemp(
    prefix="tilo-notes-caret-"))
SHOTS = pathlib.Path(os.environ.get("TILO_NOTES_PROOF_SHOTS")
                     or tempfile.mkdtemp(prefix="tilo-notes-caret-shots-"))
PANE = "quick-settings"

failures: list[str] = []


def check(label: str, ok: bool, detail: str = "") -> bool:
    print(f"  {'PASS' if ok else 'FAIL'}  {label}" + (f" — {detail}" if detail else ""))
    if not ok:
        failures.append(label)
    return ok


def chrome(session) -> dict:
    return session.state_dump()["chrome"]


def caret(session) -> list | None:
    """The caret of the focused field, `[x0, y0, x1, y1]` on the desk."""
    return chrome(session).get("caret")


def pane(session) -> list | None:
    """Where the clipboard pane is drawn, `[x0, y0, x1, y1]`, while it is open: the size its app
    asked for, which is shorter than the room when its page is."""
    return (chrome(session).get("surfaces") or {}).get(PANE)


def room(session) -> list | None:
    """The room the stack gave the clipboard pane, `[x0, y0, x1, y1]`."""
    return (chrome(session).get("places") or {}).get(PANE)


def wait_for(label: str, read, deadline: float = 10.0):
    try:
        return until(label, read, deadline, 0.1)
    except Failed:
        return None


def settled(read, pause: float = 0.3):
    """What `read` answers once it answers the same twice `pause` apart: the
    pane slides in, and where it rests is where it was placed."""
    first = read()
    time.sleep(pause)
    return first if first and read() == first else None


def keeps_a_record() -> bool:
    """Whether Notes keeps its notebook record in the session's state yet: once
    it does, its window takes the keyboard."""
    return any(NOTES in str(path) for path in STATE.rglob("*.automerge"))


def notes_window(session) -> dict | None:
    return next((window for window in session.windows()
                 if window.get("app_id") == NOTES and window.get("active")), None)


def main() -> int:
    SHOTS.mkdir(parents=True, exist_ok=True)
    if "--build" in sys.argv:
        real_apps.build(apps=APPS)
    real_apps.prepare(STATE, apps=APPS, built=False)
    print(f"==> state {STATE}")

    session = real_apps.Session(STATE)
    try:
        session.start(apps=APPS, fresh=True, out=SHOTS)
        session.launch("notes")
        session.answering("notes", deadline=180)
        check("Notes keeps a record of the workspace",
              wait_for("the Notes record", keeps_a_record, 30) is not None)
        window = wait_for("the Notes window in front", lambda: notes_window(session), 30)
        if not check("the Notes window is the one in front", window is not None):
            return 1
        check("no field has the keyboard before a page is made", caret(session) is None,
              json.dumps(caret(session)))

        # A page, its name, then its body: what a person types.
        session.chord("Control", "n")
        session.type("Garden plan")
        title = wait_for("the title's caret", lambda: caret(session))
        check("typing into the page's name reports its caret", title is not None)
        if title is None:
            session.snapshot(SHOTS / "01-no-caret.png")
            return 1
        print(f"  title caret {title}, window {json.dumps(window.get('rect'))}")
        session.key("Enter")
        session.type("Water the beds")
        first = wait_for("the body's caret", lambda: (now := caret(session)) and
                         now[1] > title[1] and now)
        check("typing into the body moves the caret down to it", first is not None,
              json.dumps(caret(session)))
        first = first or caret(session) or title
        width, height = first[2] - first[0], first[3] - first[1]
        check("the caret is a thin bar the height of a line", 0 <= width <= 4 and 10 <= height <= 40,
              json.dumps(first))
        session.type(" before noon")
        moved = wait_for("the caret moving", lambda: (now := caret(session)) and
                         now[0] > first[0] and now)
        check("the caret moves right as more is typed", moved is not None,
              json.dumps(caret(session)))
        at = moved or first

        # The caret back at the line's end, then Super+V at once: the pane is placed at the
        # caret the app's last redraw moved, not the one the desk knew before it.
        session.chord("Control", "a")
        session.chord("Control", "c")
        session.key("End")
        session.chord("Super", "v")
        # The first frame the person sees is the size the panel fits, never the room it was
        # given: the desk holds the pane back until its app has drawn that frame.
        opened = wait_for("the clipboard pane, drawn", lambda: pane(session))
        end = wait_for("the caret at rest", lambda: settled(lambda: caret(session))) or at
        shot = session.snapshot(SHOTS / "02-super-v-at-caret.png")
        if check("Super+V opens the clipboard pane", opened is not None):
            span = room(session) or opened
            check("the pane's first frame is as tall as its page, not its room",
                  40 <= opened[3] - opened[1] <= 0.55 * (span[3] - span[1]),
                  f"pane {json.dumps(opened)}, room {json.dumps(span)}")
            check("the pane opens under the caret's line", end[3] <= opened[1] <= end[3] + 24,
                  f"caret {json.dumps(end)}, pane {json.dumps(opened)}")
            # Its room starts at the caret's column, unless the window runs out before a pane
            # that wide fits beside it: then it takes the room from the window's edge.
            check("the pane starts at the caret's column, inside its room",
                  span[0] - 1 <= opened[0] <= end[0] + 24,
                  f"caret {json.dumps(end)}, pane {json.dumps(opened)}, room {json.dumps(span)}")
        print(f"  shot {shot}")

        session.key("Escape")
        closed = wait_for("the pane closing", lambda: pane(session) is None)
        check("Escape closes the pane", closed is not None)
        check("the caret is still reported after the pane closes", caret(session) is not None,
              json.dumps(caret(session)))
    finally:
        session.stop()

    print(f"==> shots in {SHOTS}")
    print("PASS" if not failures else f"FAIL ({len(failures)}): {', '.join(failures)}")
    return 0 if not failures else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Failed as error:
        print(f"FAIL — {error}")
        sys.exit(1)
    except Exception as error:  # noqa: BLE001 - a proof that crashed is a failure
        print(f"FAIL — {type(error).__name__}: {error}")
        sys.exit(1)
