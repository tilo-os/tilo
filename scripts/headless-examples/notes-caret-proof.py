#!/usr/bin/env python3
"""A field a person types into in an app tells the desk where its caret is, with
no input method running, Super+V opens the clipboard under that caret, and what
the person copied in the app is in it and pastes elsewhere.

A session starts on a scratch home with the tree's own Notes, the person opens
Notes, makes a page with Ctrl+N and types into its body. Everything is read off
the desk's own state, as the paste pane and the presence carets read it:

  caret    the desk holds a caret once the page's name is typed, follows it
           down into the body, and moves right as more is typed
  paste    Super+V, pressed the moment the caret moves, opens the clipboard pane
           under that caret's line, its first frame as tall as its page
  closed   Escape closes the pane and the caret stays where it was
  copy     what Ctrl+A, Ctrl+C took in the page is in the pane, pastes into the
           desk's search with Ctrl+V, and a copy the session's own action makes
           joins it in the same history
  choose   a copy clicked in the pane is pasted at the page's caret and the pane
           goes; opened over the desk's search, the pane leaves the search up,
           takes the keys typed to find a copy, and Enter pastes it there

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

from PIL import Image

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


# `intent.accent_ink`, the colour a caret is drawn in.
CARET = bytes((129, 168, 255))


def bars_in(path, rect) -> list[list[int]]:
    """The caret bars in the PNG at `path` inside `rect`, `[x0, y0, x1, y1]` each: the caret's ink,
    a line tall and as wide as the pen. Only a field that holds the keyboard paints one."""
    x0, y0, x1, y1 = (int(value) for value in rect)
    image = Image.open(path).convert("RGB")
    crop = image.crop((max(0, x0), max(0, y0), max(x0 + 8, x1), max(y0 + 8, y1)))
    width, height = crop.size
    data = crop.tobytes()
    found: dict[tuple[int, int, int], list[int]] = {}
    at = data.find(CARET)
    while at != -1:
        if at % 3 == 0:
            pixel = at // 3
            x, y = pixel % width, pixel // width
            top = y
            while top > 0 and data[((top - 1) * width + x) * 3:][:3] == CARET:
                top -= 1
            bottom = y + 1
            while bottom < height and data[(bottom * width + x) * 3:][:3] == CARET:
                bottom += 1
            if 10 <= bottom - top <= 44:
                found[(x, top, bottom)] = [x0 + x, y0 + top, x0 + x + 1, y0 + bottom]
        at = data.find(CARET, at + 3)
    # A caret is as wide as the pen: the columns of one bar are one caret.
    bars: list[list[int]] = []
    for bar in sorted(found.values(), key=lambda bar: (bar[1], bar[0])):
        if bars and bars[-1][1] == bar[1] and bars[-1][3] == bar[3] and bars[-1][2] == bar[0]:
            bars[-1][2] = bar[2]
        else:
            bars.append(bar)
    return [bar for bar in bars if 1 <= bar[2] - bar[0] <= 4]


def carets_on_desk(session, area, frames: int = 6) -> list[list[int]]:
    """Every caret bar the desk shows in `area` across `frames` fresh frames, so each caret is seen
    on either side of its blink: only a field that holds the keyboard paints one."""
    bars: list[list[int]] = []
    for frame in range(frames):
        path = SHOTS / f"desk-{frame}.png"
        session.snapshot(path)
        bars += [bar for bar in bars_in(path, area) if bar not in bars]
    return bars


def only_caret_in(session, rect, area) -> tuple[bool, list[list[int]]]:
    """Whether the desk shows a caret in `area` and every one there is inside `rect`, and those
    seen."""
    seen = carets_on_desk(session, area)
    return bool(seen) and all(rect[0] <= bar[0] and bar[2] <= rect[2] and rect[1] <= bar[1]
                              and bar[3] <= rect[3] for bar in seen), seen


def around(rect, margin: float = 8.0) -> list[float]:
    """`rect` with `margin` on every side, where a caret of a field that moved slightly still is."""
    return [rect[0] - margin, rect[1] - margin, rect[2] + margin, rect[3] + margin]


def keeps_a_record() -> bool:
    """Whether Notes keeps its notebook record in the session's state yet: once
    it does, its window takes the keyboard."""
    return any(NOTES in str(path) for path in STATE.rglob("*.automerge"))


def query(session) -> str:
    """What the desk's search field holds."""
    return session.control({"op": "search_rows"})["answer"]["query"]


def searching(session) -> bool:
    """Whether the desk's search is open."""
    return any(shown for slot, shown in (chrome(session).get("overlays") or {}).items()
               if "search" in slot)


# Where the pane's copies stand in it: under its search field and the line counting them, one
# row each, and Clear history under the last.
FIRST_COPY, COPY_ROW, CLEAR_ROW = 76, 52, 36


def copy_at(session, index: int) -> tuple[float, float]:
    """A point on the text of the pane's `index`th copy, clear of its pin and remove."""
    at = pane(session)
    return at[0] + 120, at[1] + FIRST_COPY + COPY_ROW * index + COPY_ROW / 2


def page_holds(text: str) -> bool:
    """Whether the page's record, as the session keeps it on disk, holds `text`."""
    return any(text.encode() in path.read_bytes()
               for path in STATE.rglob("*.automerge") if NOTES in str(path))


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
        # The window's rect, where its page, the search and the pane over it draw their carets.
        x, y, w, h = window["rect"]
        area = [x, y, x + w, y + h]
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
        span = room(session) or opened
        shot = session.snapshot(SHOTS / "02-super-v-at-caret.png")
        print(f"  shot {shot}")
        # The pane holds the keyboard, so the page's field under it keeps its text and gives up its
        # caret: the pane's search is the only caret the desk shows.
        pane_at = pane(session) or [0, 0, 0, 0]
        one, seen = only_caret_in(session, pane_at, area)
        check("the pane's search is the only caret over the page's field", one,
              f"carets {json.dumps(seen)}, pane {json.dumps(pane_at)}, page caret {json.dumps(at)}")
        session.key("Escape")
        closed = wait_for("the pane closing", lambda: pane(session) is None)
        check("Escape closes the pane", closed is not None)
        # The pane holds the keyboard while it is up, so the page's caret is read once it has it
        # back: the caret the pane was placed at.
        end = wait_for("the caret at rest", lambda: settled(lambda: caret(session)))
        check("the caret is reported again after the pane closes", end is not None,
              json.dumps(caret(session)))
        end = end or at
        if check("Super+V opens the clipboard pane", opened is not None):
            span = span or opened
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

        # What was copied in the page pastes into another app's field: the desk's search.
        session.chord("Super", "Space")
        # The search holds the keys over the page, which is left: the search's caret is the one
        # caret the desk shows, and the page's comes back with the keys.
        over_page = wait_for("the search's caret over the page",
                             lambda: settled(lambda: caret(session)))
        one, seen = only_caret_in(session, around(over_page or [0, 0, 0, 0]), area)
        check("the search over the page shows the desk's one caret", one,
              f"carets {json.dumps(seen)}, search caret {json.dumps(over_page)}")
        session.chord("Control", "v")
        pasted = wait_for("the paste in the search", lambda: query(session) or None)
        session.snapshot(SHOTS / "03-pasted-in-search.png")
        check("Ctrl+V pastes the page's copy into the desk's search",
              pasted == "Water the beds before noon", repr(pasted))
        # Escape empties the query, then closes the search.
        session.key("Escape")
        session.key("Escape")
        check("Escape closes the search", wait_for("the search closing",
                                                   lambda: not searching(session)) is not None,
              json.dumps(chrome(session).get("overlays")))

        # The session's own copy goes on the clipboard and into the history the page's copy is
        # in: it pastes as the page's did, and the pane lists both.
        session.invoke("so.tilo.session", "session:clipboard\x1fcopy", {"text": "the autumn release"})
        session.chord("Super", "Space")
        session.chord("Control", "v")
        pasted = wait_for("the session's copy in the search",
                          lambda: (now := query(session)) and "autumn" in now and now)
        check("Ctrl+V pastes the session's copy into the desk's search",
              pasted == "the autumn release", repr(query(session)))
        session.key("Escape")
        session.key("Escape")
        wait_for("the search closing", lambda: not searching(session))
        session.chord("Super", "v")
        wait_for("the clipboard pane", lambda: settled(lambda: pane(session)))
        # The pane grows as the history's reading brings its rows: two copies and Clear history.
        tall = FIRST_COPY + 2 * COPY_ROW + CLEAR_ROW
        grown = wait_for("the pane grown to both copies",
                         lambda: (now := pane(session)) and now[3] - now[1] >= tall and now)
        print(f"  shot {session.snapshot(SHOTS / '04-both-copies.png')}")
        if not check("the pane is tall enough for both copies and Clear history",
                     grown is not None, json.dumps(pane(session))):
            return 1

        # A copy clicked in the pane is pasted where the caret was, and the pane goes.
        session.click_at(*copy_at(session, 0))
        check("a click on a copy closes the pane",
              wait_for("the pane closing", lambda: pane(session) is None) is not None)
        # The page draws the paste, its caret past the text it took, and keeps it in its record.
        check("the page's caret moves past the pasted copy",
              wait_for("the paste landing", lambda: (now := caret(session)) and
                       now[0] > end[0] + 40 and now) is not None, json.dumps(caret(session)))
        session.snapshot(SHOTS / "05-clicked-into-page.png")
        check("the clicked copy is pasted into the page",
              wait_for("the paste in the page's record",
                       lambda: page_holds("the autumn release") or None) is not None)

        # Opened over the desk's search, the pane leaves it up, and Enter pastes the copy that
        # matches what was typed into it.
        session.chord("Super", "Space")
        wait_for("the search open", lambda: searching(session) or None)
        # Where the desk's own search draws its caret while it is the one typed into.
        search_caret = wait_for("the search's caret", lambda: settled(lambda: caret(session)))
        session.chord("Super", "v")
        wait_for("the clipboard pane over the search", lambda: settled(lambda: pane(session)))
        check("the search stays open under the pane", searching(session),
              json.dumps(chrome(session).get("overlays")))
        # What is typed now searches the pane's copies: the pane has the keyboard, not the search.
        session.type("beds")
        check("the search under the pane keeps what it held", query(session) == "",
              repr(query(session)))
        session.snapshot(SHOTS / "06-pane-over-search.png")
        # Only the surface holding the keyboard paints a caret: the pane's search, not the desk's
        # search it opened over (which keeps its text and takes its caret back with the pane gone).
        pane_at = pane(session) or [0, 0, 0, 0]
        one, seen = only_caret_in(session, pane_at, area)
        check("the pane's search is the one caret the desk shows, not the search under it", one,
              f"carets {json.dumps(seen)}, pane {json.dumps(pane_at)}, "
              f"search caret {json.dumps(search_caret)}")
        session.key("Enter")
        check("Enter closes the pane",
              wait_for("the pane closing", lambda: pane(session) is None) is not None)
        pasted = wait_for("the paste in the search", lambda: query(session) or None)
        session.snapshot(SHOTS / "07-entered-into-search.png")
        check("Enter pastes the copy that matches what was typed, not the newest",
              pasted == "Water the beds before noon", repr(pasted))
        # The search under it takes the keyboard, and its caret, back.
        back = wait_for("the search's caret again", lambda: settled(lambda: caret(session)))
        one, seen = only_caret_in(session, around(back or [0, 0, 0, 0]), area)
        check("the search under the pane shows the desk's one caret again", one,
              f"carets {json.dumps(seen)}, search caret {json.dumps(back)}")
        session.type("!")
        check("the search has the keyboard back",
              wait_for("the search typed into", lambda: query(session).endswith("!") or None)
              is not None, repr(query(session)))
        session.key("Escape")
        session.key("Escape")
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
