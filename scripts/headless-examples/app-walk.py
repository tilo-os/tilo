#!/usr/bin/env python3
"""An app's first hour, walked in the versions its step file names.

A Rust app under `apps/` is deleted only once the tilolang app under `apps/tl/`
does what a person did with it at least as well. What a person did is a walk of
their own: open the app, make something, find it again, look at it in the dark.
This walks that same first hour in each version the step file names — the Rust
one and the tilolang one before the change that retires the Rust app, the
tilolang one alone after it — in headless sessions of their own, and snapshots
every step, so the runs can be put side by side and judged.

One step file per app (`app-walks/<app>.json`) holds the steps, and both
versions are driven by it: the presses, typing, searches and theme changes are
the same, and only the control each version answers to differs, which the step
file names per version. The steps themselves are the app's own; a new app's
walk is a new file, not a new script.

    scripts/headless-examples/app-walk.py notes
    scripts/headless-examples/app-walk.py notes --only tl     # one version
    scripts/headless-examples/app-walk.py --list              # the walks there are

What each step says is OUT/walk.json: for each version, the steps in order,
each with its snapshot, how long the app took to answer, how many notes it
held afterwards, and — where a step expected a control — what the app offered
instead. A step that cannot be walked in a version is recorded as such (the
control is missing, the note never appeared, the file chooser asks something
else) and the walk goes on: what a person finds missing is the finding.

The Rust version is the session build's own app (`TILO_NESTED_APPS`,
dist/dev/nested-session-build.sh); the tilolang one is `tilolang build` of the
app's package against that session's runtime, installed beside it as a signed
package. Both run as the image runs them, through the session's launcher and
sandbox: nothing starts an app binary directly and nothing touches this
machine's desktop (lavapipe alone, a private runtime directory, a throwaway
identity).

It needs the session build and the `tilolang` tool, which the nested-session
build makes (or `--build` makes them first, through
TILO_NESTED_SESSION_BUILD_EXEC where this machine has no toolchain).

Usage:
  scripts/headless-examples/app-walk.py notes [--out DIR] [--state DIR]
                                      [--only rust|tl] [--build] [--keep]
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
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
# The session this walks and the tools that act in it: the one harness every
# check of a desktop with the tree's apps starts (scripts/headless-examples/real_apps.py).
import real_apps  # noqa: E402
from real_apps import BUILD, Failed, Session, prepare, run, until  # noqa: E402

WALKS = HERE / "app-walks"
# What a control's identity joins: the record kind it acts on, and the operation,
# in the app's own ids (`apps/tl/<app>/app.ids`). A control that runs an app's
# action and a field that edits a record publish it.
ID_SEP = "\x1f"
VARIANTS = ("rust", "tl")
# The shell's own verbs: a window is focused and closed by the tab it holds.
SHELL = "so.tilo.session"
SHELL_RESOURCE = "desktop:shell"
# The file chooser's own steps (`so.tilo.file-chooser`), which the person takes in
# its sheet: select the file, then use it.
FILE_CHOOSER = "so.tilo.file-chooser"
FILE_CHOOSER_STEP = f"{FILE_CHOOSER}.step"
FILE_CHOOSER_QUESTION = "portal.file-chooser"
FILE_CHOOSER_RESOURCE = "file-chooser.request"


def until_value(what: str, check, deadline: float = 30.0, step: float = 0.1):
    """`until`, with the deadline named per call: every step here is bounded by
    what a person would wait, and a control that never comes is a finding."""
    return until(what, check, deadline, step)


# --- what the app holds, read back ------------------------------------------------------------

def app_action(session: Session, app: str, operation: str, resource: str | None = None):
    """The action `operation` the window of `app` publishes: found by the name
    the app gives it, never by the words its title reads in. Its application is
    `app` exactly (`so.tilo.notes` never matches `so.tilo.notes`)."""
    for action in session.actions(""):
        if (action.get("application") == app and action.get("operation") == operation
                and resource in (None, action.get("resource"))):
            return action
    return None


def value_of(answer: dict):
    try:
        return answer["receipt"]["update"]["output"]["value"]
    except (KeyError, TypeError):
        raise Failed(f"an answer without a value: {json.dumps(answer)[:2000]}") from None


def read_back(session: Session, app: str) -> dict:
    """The app's own read — the one on the resource its "Show this window" stands
    on — as this person reads it."""
    own = until_value(f"{app}'s 'Show this window'",
                      lambda: app_action(session, app, "activate-window"), 60)["resource"]
    action = until_value(f"{app}'s read on {own}",
                         lambda: app_action(session, app, "read", own), 60)
    return value_of(session.invoke(action["provider"], action["id"]))


def notes_held(read: dict) -> int | None:
    """How many notes a read of the app's own record lists, where the read says:
    the records whose kind is a note, or the collection of notes it holds under
    `notes` or `pages`. An app whose own read is of its notebook alone (its
    notes kept in a part beside it) answers None: what it holds is not this."""
    if not isinstance(read, dict):
        return None
    records = read.get("records")
    if isinstance(records, list) and records:
        counted = [r for r in records if isinstance(r, dict)
                   and str(r.get("kind", "")).rsplit(".", 1)[-1] in ("Note", "Page")]
        if counted:
            return len(counted)
    found: list[int] = []

    def walk(value, key: str | None) -> None:
        if isinstance(value, dict):
            items, count = value.get("items"), value.get("count")
            if isinstance(items, list) and (
                    (key or "").lower() in ("notes", "pages")
                    or any(isinstance(item, dict)
                           and str(item.get("type", "")).rsplit(".", 1)[-1] in ("Note", "Page")
                           for item in items)):
                found.append(count if isinstance(count, int) else len(items))
            for inner, inner_value in value.items():
                walk(inner_value, inner)
        elif isinstance(value, list):
            for inner in value:
                walk(inner, key)

    walk(read, None)
    return found[0] if found else None


# --- the walk ---------------------------------------------------------------------------------

def pick(value, variant: str):
    """The value a step means in `variant`: a dict naming the versions differs
    per version, anything else is the same for both."""
    if isinstance(value, dict) and set(value) & set(VARIANTS):
        if variant not in value:
            raise Failed(f"the step names no {variant} value: {json.dumps(value)}")
        return value[variant]
    return value


def alternative(value, variant: str):
    """One of several controls a step tries, or None when this one is written
    for a version other than `variant`."""
    if isinstance(value, dict) and set(value) & set(VARIANTS) and variant not in value:
        return None
    return pick(value, variant)


def control(session: Session, selector) -> dict | None:
    """The control `selector` names: an identity (a record kind and its
    operation), a name the widget answers to, `{"prefix": "note-"}` for the
    first control whose name starts that way (a row the app names by its id),
    `{"doing": "dpj7scfe"}` for the control that edits the field or runs the
    action with that id, on whatever record the window shows it for — the end
    of an identity is stable where the record it stands on is not — or
    `{"menuitem": "Tag…"}` for the widest drawn control of that name: the row a
    menu draws for its command, not a button of the window that says the same
    words."""
    targets = session.targets()
    if isinstance(selector, dict) and "menuitem" in selector:
        found = [
            target
            for target in targets
            if target.get("name") == selector["menuitem"] and drawn(target)
        ]
        return max(found, key=lambda target: target["rect"][2]) if found else None
    for target in targets:
        identity = target.get("identity") or ""
        if isinstance(selector, dict):
            if "prefix" in selector:
                if str(target.get("name", "")).startswith(selector["prefix"]):
                    return target
            elif ID_SEP in identity and identity.rsplit(ID_SEP, 1)[-1] == selector["doing"]:
                return target
        elif ID_SEP in selector:
            if identity == selector:
                return target
        elif target.get("name") == selector:
            return target
    return None


def drawn(target: dict) -> bool:
    """Whether `target` is drawn anywhere: a control a window holds open but does
    not show — a menu's items while its menu is closed — stands nowhere, and a
    press aimed at it lands on nothing."""
    x, y, w, h = target["rect"]
    return w > 0 and h > 0 and x >= 0 and y >= 0


def row_of(session: Session, window: dict | None, name: str) -> dict | None:
    """The row `name` in the app window's own sidebar: a control with those words
    drawn in the window's leading column, below its header — never the desktop's
    tab of a window named by the same words."""
    if window is None or window.get("rect") is None:
        return None
    x, y, w, _h = window["rect"]
    for target in session.targets():
        if target.get("name") != name or not drawn(target):
            continue
        tx, ty, _tw, _th = target["rect"]
        if ty > y + 30 and tx < x + min(320.0, w / 2):
            return target
    return None


def press(session: Session, selector, verb: str = "activate") -> dict:
    """Press the control `selector` names, as a person's pointer does; `verb`
    opens its context menu where the control offers one."""
    target = control(session, selector)
    if target is None:
        raise Failed(f"no control named {selector!r} is offered")
    identity = target.get("identity") or ""
    if ID_SEP in identity:
        return session.click_doing(identity, verb)
    return session.click_named(target["name"], verb)


def hover(session: Session, selector: str, deadline: float = 20.0) -> dict:
    """Rest the pointer on the control `selector` names: what a row's own
    controls appear for."""
    target = until_value(f"the control {selector!r} to rest on",
                         lambda: control(session, selector), deadline)
    x, y, w, h = target["rect"]
    session.control({"op": "pointer_move", "x": x + w / 2, "y": y + h / 2})
    return target


class Walk:
    """One version's walk: its session, its steps and what each one found."""

    def __init__(self, spec: dict, variant: str, out: Path, state: Path,
                 discover: bool = False):
        self.spec = spec
        self.variant = variant
        self.v = spec[variant]
        self.out = out
        self.state = state
        # A walk being written: every step keeps the controls the version offered,
        # so the step file can name the one that is really there.
        self.discover = discover
        self.shots = out / "shots" / variant
        self.session = Session(state)
        self.steps: list[dict] = []
        self.window: dict | None = None
        self.appearance: dict | None = None

    def targets(self) -> list[dict]:
        """The controls the desktop offers now, as the walk records them where a
        step could not be walked: a name or an identity, and where it is."""
        return [{"name": t.get("name"), "identity": t.get("identity"), "rect": t.get("rect"),
                 "level": t.get("level")}
                for t in self.session.targets()]

    def actions(self, only_app: bool = False) -> list[dict]:
        """The actions the desktop lists now, in the words a person's palette
        reads: what a verb with no control still is. Every application's, unless
        `only_app` keeps this app's, so a step that could not be walked says
        what was published beside what it wanted."""
        return [{"title": a.get("title"), "operation": a.get("operation"),
                 "application": a.get("application"), "resource": a.get("resource")}
                for a in self.session.actions("")
                if not only_app or a.get("application") == self.v["id"]]

    def shot(self, name: str) -> str:
        path = self.shots / f"{name}.png"
        self.session.snapshot(path)
        return str(path)

    def window_of(self) -> dict | None:
        """The window of the app this walk opens, as the shell holds it."""
        ident = self.v["id"]
        found = next((w for w in self.session.windows() if w.get("app_id") == ident), None)
        if found is not None:
            self.window = found
        return found

    def launch(self, wait: bool = True, deadline: float = 120.0) -> None:
        """Open the app through the desktop's own launcher. Without `wait`, the
        walk goes on the moment the launcher takes it: what a person presses
        before the app is ready."""
        self.session.invoke("applications", self.v["id"])
        if wait:
            until_value(f"the window of {self.v['id']}", self.window_of, deadline)
            # The app's own process in the session's store, where the launcher
            # runs it from: a window without one is the shell's, not the app's.
            try:
                self.session.answering(self.v["id"], deadline)
            except Failed:
                pass

    def close_window(self) -> None:
        window = self.window_of()
        if window is None:
            return
        self.session.invoke(SHELL, f"{SHELL_RESOURCE}{ID_SEP}close-tab",
                            {"tab": str(window["tab"])})
        until_value(f"the window of {self.v['id']} to close",
                    lambda: self.window_of() is None, 30)

    def quit_app(self) -> None:
        """End the app, not only its window: the next launch is a cold start,
        which is what a person who starts a note the moment it opens meets."""
        window = self.window_of()
        if window is None:
            return
        self.session.invoke(SHELL, f"{SHELL_RESOURCE}{ID_SEP}force-quit-app",
                            {"tab": str(window["tab"])})
        until_value(f"the window of {self.v['id']} to go",
                    lambda: self.window_of() is None, 60)

    def set_dark(self, dark: bool) -> None:
        """Switch the desktop's appearance, through the action the person's
        Settings use: its provider and id as the desktop lists them."""
        if self.appearance is None:
            self.appearance = until_value(
                "the appearance action",
                lambda: next((a for a in self.session.actions("")
                              if a.get("operation") == "set-dark-mode"), None), 30)
        self.session.invoke(self.appearance["provider"], self.appearance["id"], {"on": dark})

    def theme_shot(self, name: str, dark: bool) -> str:
        """The state `name` again, in the theme named: the person switches it and
        looks at what they had."""
        before = (self.window_of() or {}).get("frames") or 0
        self.set_dark(dark)
        # A new theme reaches a window on the next frame it draws: wait for the
        # app's own frame count to move, then shoot what it drew.
        try:
            until_value(f"the app to draw again in the {'dark' if dark else 'light'} theme",
                        lambda: ((self.window_of() or {}).get("frames") or 0) > before,
                        20, 0.05)
        except Failed:
            pass
        return self.shot(f"{name}-{'dark' if dark else 'light'}")

    # -- the ops a step file may name ------------------------------------------------------

    def op(self, op: dict) -> dict:
        """One op of a step, as the step file writes it, and what it found."""
        (name, value), = op.items()
        handler = getattr(self, f"op_{name}", None)
        if handler is None:
            raise Failed(f"the step names an op this walk has no way to do: {name!r}")
        return handler(value) or {}

    def op_launch(self, value) -> dict:
        """Open the app, waiting for its window unless the value says not to."""
        wait = value if isinstance(value, bool) else value.get("wait", True)
        began = time.monotonic()
        self.launch(wait=wait, deadline=float(value.get("deadline", 120))
                    if isinstance(value, dict) else 120.0)
        waited = None
        if wait:
            waited = round((time.monotonic() - began) * 1000)
        return {"opened": self.v["id"], "waited_ms": waited}

    def op_press(self, value) -> dict:
        selector = pick(value, self.variant)
        found = control(self.session, selector)
        press(self.session, selector)
        return {"pressed": selector, "offered": found is not None}

    def op_early_press(self, value) -> dict:
        """Open the app and press its control the moment it is on screen: what a
        person does who starts a note right after opening Notes, before the app
        has caught up. A press that is taken makes a note; a press that is lost
        leaves the notebook as it was, and the count says which."""
        wanted = value.get("controls") or [value["control"]]
        offered = [s for s in map(lambda o: alternative(o, self.variant), wanted) if s]
        if value.get("close", True):
            # Closed the way a person closes a window, then opened again: the
            # launch is a cold one, and the window left behind is theirs.
            self.close_window()
        began = time.monotonic()
        self.session.invoke("applications", self.v["id"])

        def shown():
            if self.window_of() is None:
                return None
            for selector in offered:
                found = control(self.session, selector)
                if found is not None:
                    return found
            return None

        target = until_value(f"a control of {self.v['id']} on screen", shown,
                             float(value.get("deadline", 120)), 0.02)
        x, y, w, h = target["rect"]
        self.session.click_at(x + w / 2, y + h / 2)
        pressed = round((time.monotonic() - began) * 1000)
        until_value(f"the window of {self.v['id']}", self.window_of, 120)
        try:
            self.session.answering(self.v["id"], 120)
        except Failed:
            pass
        return {"pressed": target.get("identity") or target["name"],
                "pressed_ms_after_launch": pressed, "rect": target["rect"]}

    def op_press_any(self, value) -> dict:
        """Press the first of `selectors` the version offers, and say which: a
        step whose control one version draws and the other does not. Finding
        none is the finding, and the walk goes on."""
        offered = [s for s in map(lambda o: alternative(o, self.variant), value) if s]
        for selector in offered:
            if control(self.session, selector) is not None:
                press(self.session, selector)
                return {"pressed": selector}
        return {"pressed": None, "offered_none": offered}

    def op_offered(self, value) -> dict:
        """Which of `selectors` the version offers now, without pressing one."""
        offered = [s for s in map(lambda o: alternative(o, self.variant), value) if s]
        for selector in offered:
            if control(self.session, selector) is not None:
                return {"offered": selector}
        return {"offered": None, "offered_none": offered}

    def op_hover(self, value) -> dict:
        selector = pick(value, self.variant)
        hover(self.session, selector)
        return {"rested_on": selector}

    def op_click(self, value) -> dict:
        """Press at the middle of the control `selector` names: a field, or a
        control whose own press the walk cannot address by name."""
        selector = pick(value, self.variant)
        target = until_value(f"the control {selector!r}", lambda: control(self.session, selector))
        x, y, w, h = target["rect"]
        self.session.click_at(x + w / 2, y + h / 2)
        return {"clicked": selector, "rect": target["rect"]}

    def op_click_any(self, value) -> dict:
        """Click the first of `selectors` the version offers, and say which: a
        field takes the keyboard from a click where its action does not. A
        control a window holds but does not show — a menu's items while its menu
        is closed — stands nowhere, and is not one to click."""
        offered = [s for s in map(lambda o: alternative(o, self.variant), value) if s]
        for selector in offered:
            target = control(self.session, selector)
            if target is not None and drawn(target):
                x, y, w, h = target["rect"]
                self.session.click_at(x + w / 2, y + h / 2)
                return {"clicked": selector, "rect": target["rect"]}
        return {"clicked": None, "offered_none": offered}

    def op_select_row(self, value) -> dict:
        """Press the row `name` in the app window's own sidebar, where a person
        picks the note their window turns to."""
        name = pick(value, self.variant)
        target = until_value(f"the row {name!r} in the window",
                             lambda: row_of(self.session, self.window_of(), name), 30)
        x, y, w, h = target["rect"]
        self.session.click_at(x + w / 2, y + h / 2)
        return {"selected_row": name, "rect": target["rect"]}

    def op_row_menu(self, value) -> dict:
        """Open the menu of the row `name` in the app window's own sidebar, the
        way a person opens it: a second-button press on the row."""
        name = pick(value, self.variant)
        target = until_value(f"the row {name!r} in the window",
                             lambda: row_of(self.session, self.window_of(), name), 30)
        x, y, w, h = target["rect"]
        self.session.right_click_at(x + w / 2, y + h / 2)
        return {"menu_of_row": name, "rect": target["rect"]}

    def op_right_click(self, value) -> dict:
        """A second-button press where the control `selector` names is drawn: a
        row's or an area's own menu, opened the way a person opens it."""
        selector = pick(value, self.variant)
        target = until_value(f"the control {selector!r}", lambda: control(self.session, selector))
        x, y, w, h = target["rect"]
        self.session.right_click_at(x + w / 2, y + h / 2)
        return {"right_clicked": selector, "rect": target["rect"]}

    def op_right_click_at(self, value) -> dict:
        """A second-button press at a point of the window, for the menu empty
        space offers and nothing names: `[x, y]`, as the step file writes it."""
        x, y = value
        self.session.right_click_at(float(x), float(y))
        return {"right_clicked_at": [x, y]}

    def op_context_menu(self, value) -> dict:
        """Open the menu the control `selector` names shows, without a point:
        the same menu a right press opens, addressed by what the control is."""
        selector = pick(value, self.variant)
        press(self.session, selector, verb="show-context-menu")
        return {"context_menu": selector}

    def op_type(self, value) -> dict:
        text = pick(value, self.variant)
        self.session.type(text)
        return {"typed": text}

    def op_key(self, value) -> dict:
        self.session.key(pick(value, self.variant) if isinstance(value, dict) else value)
        return {"key": value}

    def op_chord(self, value) -> dict:
        """Keys held while another is typed: `["Control", "n"]`, or
        `["Control", "Shift", "n"]` for a chord of more than one modifier."""
        *modifiers, key = value
        for modifier in modifiers:
            self.session.control({"op": "key_down", "key": modifier})
        try:
            self.session.key(key)
        finally:
            for modifier in reversed(modifiers):
                self.session.control({"op": "key_up", "key": modifier})
        return {"chord": "+".join(value)}

    def op_invoke(self, value) -> dict:
        """Call an action by its address: the app's own verb, for a step the app
        offers no control for. The answer is recorded, a refusal does not end
        the walk — a person who cannot do it from the app cannot do it here
        either, and that is the finding."""
        provider, ident = value[0], value[1]
        args = value[2] if len(value) > 2 else None
        try:
            answer = self.session.invoke(provider, ident, args)
        except Failed as error:
            return {"invoked": f"{provider}{ID_SEP}{ident}", "refused": str(error)}
        try:
            return {"invoked": f"{provider}{ID_SEP}{ident}", "value": value_of(answer)}
        except Failed:
            return {"invoked": f"{provider}{ID_SEP}{ident}"}

    def op_notes(self, _value) -> dict:
        """How many notes the app holds now, read back through its own read;
        what the read answered is kept where it does not count as notes, so the
        reading can be seen rather than guessed."""
        try:
            read = read_back(self.session, self.v["id"])
        except Failed as error:
            try:
                listed = self.actions()
            except Failed:
                listed = None
            return {"notes": None, "read_failed": str(error), "actions": listed}
        count = notes_held(read)
        found = {"notes": count}
        if count is None:
            found["read"] = {key: (list(value) if isinstance(value, dict) else type(value).__name__)
                             for key, value in (read or {}).items()} \
                if isinstance(read, dict) else str(read)[:1000]
        return found


    def op_wait_for(self, value) -> dict:
        """Wait for what a person waits for before going on: a control of the
        app's, its window, the actions it publishes, a note it holds, or the
        title its window wears (the note it shows)."""
        if value == "window":
            until_value(f"the window of {self.v['id']}", self.window_of, 120)
            return {"window": True}
        if value == "actions":
            # The app is ready when it publishes what it can do: a window that
            # is drawn for a client still connecting publishes nothing.
            def listed():
                return [a for a in self.session.actions("")
                        if a.get("application") == self.v["id"]]

            return {"actions": len(until_value(
                f"{self.v['id']} to publish its actions", listed, 120))}
        if value == "note":
            # Waited for is the app's own read of its notes answering at all: an
            # empty notebook answers zero, which is an answer.
            def held_now():
                try:
                    count = notes_held(read_back(self.session, self.v["id"]))
                except Failed:
                    return None
                return None if count is None else {"held": count}

            return {"notes": until_value("the app to read its own notes", held_now, 30)["held"]}
        if isinstance(value, dict) and "title" in value:
            # The window names what it shows: the note open reads in the bar of
            # the desktop, where a person sees which note their window is on.
            wanted = value["title"]
            until_value(f"the window to name {wanted!r}",
                        lambda: (self.window_of() or {}).get("title") == wanted, 30)
            return {"title": wanted}
        if isinstance(value, dict) and "shown" in value:
            # A control the window draws: a menu's item stands nowhere until its
            # menu opens, and a press aimed at it before then lands on nothing.
            wanted = value["shown"]
            until_value(
                f"the control {wanted!r} to be drawn",
                lambda: (lambda target: target if target is not None and drawn(target) else None)(
                    control(self.session, wanted)),
                30)
            return {"shown": wanted}
        selector = pick(value, self.variant)
        until_value(f"the control {selector!r}", lambda: control(self.session, selector), 30)
        return {"control": selector}

    def op_tree(self, value) -> dict:
        """What a person sees of the notes' tree: `{"rows": [...]}` that each of
        them is a row the window draws, all at one leading edge (rows at the top
        of the tree), and `{"under": [child, parent]}` that a row of the child's
        words stands indented under the parent's — the note an action made under
        the note it was made in. A row that is not drawn is the finding: the app
        may hold what its window does not show."""
        def row(name: str) -> dict:
            return until_value(
                f"the row {name!r} in the window",
                lambda: row_of(self.session, self.window_of(), name), 30)

        if "under" in value:
            child_name, parent_name = value["under"]
            parent = row(parent_name)
            px, py, _pw, _ph = parent["rect"]

            def under() -> dict | None:
                # The child's words may name other notes too: the row sought is
                # the one drawn under the parent — deeper in the tree's own
                # levels, where the window publishes them, and the row the
                # parent's own leading edge does not reach where it does not.
                parent_level = parent.get("level")

                def below(target: dict) -> bool:
                    if target["rect"][1] <= py or target["rect"][0] >= px + 320.0:
                        return False
                    level = target.get("level")
                    if parent_level is not None and level is not None:
                        return level > parent_level
                    return target["rect"][0] > px

                drawn_under = [
                    target
                    for target in self.session.targets()
                    if target.get("name") == child_name
                    and drawn(target)
                    and below(target)
                ]
                return min(drawn_under, key=lambda target: target["rect"][1]) if drawn_under else None

            child = until_value(
                f"the row {child_name!r} under {parent_name!r}", under, 30)
            return {"under": value["under"], "child": child["rect"], "parent": parent["rect"]}
        rows = [row(name) for name in value["rows"]]
        lefts = sorted({round(found["rect"][0]) for found in rows})
        if len(lefts) != 1:
            raise Failed(
                f"the rows {value['rows']!r} are not drawn at one leading edge: "
                f"{[(found.get('name'), found['rect']) for found in rows]}")
        return {"rows": value["rows"], "left": lefts[0], "rects": [found["rect"] for found in rows]}

    def op_form(self, value) -> dict:
        """Answer the form the window shows, as a person does: `{"field": "Tag", "text":
        "travel"}` clicks the field the form asks for and types into it, and `{"submit":
        "Tag"}` presses the button that takes the answer, at the foot of the sheet, under the
        field the same words name."""
        answered = {}
        if "field" in value:
            name = value["field"]
            target = until_value(
                f"the form's field {name!r}", lambda: control(self.session, name), 30)
            x, y, w, h = target["rect"]
            self.session.click_at(x + w / 2, y + h / 2)
            self.session.type(pick(value["text"], self.variant))
            answered["field"] = {"name": name, "typed": value["text"]}
        if "submit" in value:
            name = value["submit"]
            def button():
                found = [t for t in self.session.targets() if t.get("name") == name]
                return max(found, key=lambda t: t["rect"][1]) if found else None

            target = until_value(f"the form's submit {name!r}", button, 30)
            x, y, w, h = target["rect"]
            self.session.click_at(x + w / 2, y + h / 2)
            answered["submit"] = {"name": name, "rect": target["rect"]}
        return answered

    def op_search(self, value) -> dict:
        """Find a note the way a person does: the system's search, opened on the
        words of the note's title. Its rows are the desktop's own answer, from
        every app that publishes a search for what it holds — the shell's own
        offers ("Search the web for …") name the words without finding them,
        so they do not count as a found note."""
        query = pick(value, self.variant)
        self.session.invoke(SHELL, f"{SHELL_RESOURCE}{ID_SEP}search", {"query": query})

        def found():
            answer = self.session.search_rows()
            if answer.get("query") != query or answer.get("searching"):
                return None
            hits = [row.get("title") or "" for row in answer.get("rows") or []
                    if query in (row.get("title") or "")
                    and not (row.get("title") or "").startswith(
                        ("Search the web for", "Send to Assistant"))]
            return hits or None

        hits = until_value(f"the search for {query!r} to find it", found, 45)
        return {"query": query, "rows": hits}

    def op_actions(self, value) -> dict:
        """The actions the app publishes for `value`, as the desktop lists them:
        what a person can reach from the palette, whatever the app's window
        draws. A verb with no control is still a verb here."""
        query = pick(value, self.variant)
        return {"actions": [a for a in self.actions(only_app=True)
                            if query.lower() in str(a.get("title", "")).lower()
                            or query.lower() in str(a.get("operation", "")).lower()]}

    def op_targets(self, _value) -> dict:
        """What the desktop offers now, kept as evidence: the controls this
        version draws, by name and by what each does."""
        listed = self.targets()
        return {"targets": len(listed),
                "app_controls": [t for t in listed
                                 if (t.get("identity") and ID_SEP in t["identity"])]}

    def op_questions(self, _value) -> dict:
        """What is asking the person now: a file chooser, a sheet."""
        return {"questions": self.session.control({"op": "questions"})["answer"]["questions"]}

    def op_theme(self, value) -> dict:
        self.set_dark(bool(value))
        return {"theme": "dark" if value else "light"}

    def op_close(self, _value) -> dict:
        self.close_window()
        return {"closed": True}

    def op_file(self, value) -> dict:
        """Write the step's fixture file where a person's file chooser reaches
        it, and record its path: a Markdown file the app imports."""
        name, text = value["name"], value["text"]
        folder = self.state / "home"
        folder.mkdir(parents=True, exist_ok=True)
        path = folder / name
        path.write_text(text)
        return {"file": str(path)}

    def chooser_question(self) -> dict | None:
        """The file chooser asking the person now, if one is."""
        questions = self.session.control({"op": "questions"})["answer"]["questions"]
        return next((q for q in questions if q.get("kind") == FILE_CHOOSER_QUESTION), None)

    def chooser_step(self, ident: str) -> dict | None:
        """The chooser's own step `ident` ("select", "confirm"), as the sheet
        publishes it: the person's way through the chooser, not a shortcut past
        it. The session lists the sheet's steps on its request resource
        (`file-chooser.request`), the step's name its operation; a session that
        lists them another way still names the sheet in the operation or its
        provider."""
        def of(action: dict) -> bool:
            operation = str(action.get("operation") or "")
            if action.get("resource") == FILE_CHOOSER_RESOURCE:
                return ident in (action.get("id"), operation)
            return (action.get("id") == ident
                    and (operation.startswith(FILE_CHOOSER_STEP)
                         or FILE_CHOOSER in operation
                         or FILE_CHOOSER in str(action.get("provider") or "")))

        return next((a for a in self.session.actions("") if of(a)), None)

    def op_choose(self, value) -> dict:
        """Choose the fixture file a person's chooser offers: the sheet the app's
        request opens is answered by its own steps. The file is selected through
        the chooser's published step, which waits for it to have listed the file;
        then its confirm button is pressed as a person's pointer does. Giving an
        app a file is the person's alone: the chooser refuses its confirm step to
        any caller that is not at the computer, so the walk never invokes it."""
        name = value["name"]
        path = (self.state / "home" / name).resolve()
        question = until_value("the file chooser to ask for a file",
                               self.chooser_question, 60)
        handle = question["handle"]
        # What each step last said when it did not take: the walk's finding when
        # the chooser never does.
        said: dict[str, str] = {}

        def act(ident: str) -> bool:
            action = self.chooser_step(ident)
            if action is None:
                said[ident] = "the chooser lists no such step"
                return False
            try:
                self.session.invoke(action["provider"], action["id"],
                                    {"handle": handle, "path": str(path)}
                                    if ident == "select" else {"handle": handle})
                return True
            except Failed as error:
                # The chooser has not listed the file yet, or the answer is
                # already given: what it says is the finding, not a failure.
                said[ident] = str(error)
                return False

        def confirm() -> bool:
            """Press the chooser's confirm button: its own word for what it does,
            or the one the asking app gave it."""
            names = [question.get("accept_label"), "Save" if question.get("save") else "Open",
                     "Choose folder"]
            for button in filter(None, names):
                if control(self.session, button) is not None:
                    press(self.session, button)
                    return True
            said["confirm"] = f"no confirm button among {names}"
            return False

        deadline = time.monotonic() + 60
        selected = used = False
        while time.monotonic() < deadline:
            if not selected:
                selected = act("select")
            if selected and not used:
                used = confirm()
            if used or self.chooser_question() is None:
                break
            time.sleep(0.1)
        answered = self.chooser_question() is None
        if not (selected and (used or answered)):
            raise Failed(f"the chooser did not take {name!r} "
                         f"(selected: {selected}, used: {used}, answered: {answered}; "
                         f"it said: {said})")
        return {"chose": str(path), "question": handle}

    # -- a step ---------------------------------------------------------------------------

    def step(self, index: int, step: dict) -> dict:
        began = time.monotonic()
        record = {"id": step["id"], "said": step["said"], "ops": [], "found": []}
        dark = step.get("dark", False)
        ops = step.get("ops") or []
        if isinstance(ops, dict):
            # The same errand, reached differently: a menu here, a context menu
            # there. The step says what the person is doing; the version says how.
            ops = ops[self.variant]
        try:
            for op in ops:
                record["ops"].append({**op, "found": self.op(op)})
            if step.get("shot", True):
                name = f"{index:02d}-{step['id']}"
                record["shot"] = self.shot(name)
                if dark:
                    record["shot_dark"] = self.theme_shot(name, True)
                    self.set_dark(False)
            if self.discover:
                record["targets"] = [
                    {k: t[k] for k in ("name", "identity", "rect") if k in t}
                    for t in self.session.targets()]
            record["took_ms"] = round((time.monotonic() - began) * 1000)
        except Failed as error:
            record["failed"] = str(error)
            record["took_ms"] = round((time.monotonic() - began) * 1000)
            # What a failed step looks like is the finding: shoot it, and go on.
            # What the app offered instead is the finding too, so a step that
            # cannot be walked says what it was met with.
            try:
                record["shot"] = self.shot(f"{index:02d}-{step['id']}-as-it-was")
            except Failed:
                pass
            try:
                record["offered"] = self.targets()
                record["actions"] = self.actions()
            except Failed:
                pass
        self.steps.append(record)
        print(f"  {record['id']}: {'failed — ' + record['failed'] if 'failed' in record else 'walked'}"
              f" ({record['took_ms']} ms)", file=sys.stderr)
        return record

    def walk(self) -> dict:
        spec = self.spec
        self.shots.mkdir(parents=True, exist_ok=True)
        native = spec.get("native", "")
        tapps = self.tapps()
        began = time.monotonic()
        self.session.start(apps=native if self.variant == "rust" else "",
                           fresh=True, out=self.out / "logs", tag=self.variant,
                           tapps=tapps)
        started = round((time.monotonic() - began) * 1000)
        print(f"==> {self.variant}: session up in {started} ms", file=sys.stderr)
        try:
            for index, step in enumerate(spec["steps"], start=1):
                self.step(index, step)
        finally:
            found = {"variant": self.variant, "app": self.v["id"], "session_start_ms": started,
                     "steps": self.steps}
            try:
                if self.discover:
                    found["actions"] = [a for a in self.session.actions("")
                                        if a.get("application") == self.v["id"]]
                    found["windows"] = self.session.windows()
            except Failed:
                pass
            self.session.stop()
        return found

    def tapps(self) -> list[Path]:
        """The tilolang app's signed package, built against this state's runtime."""
        if self.variant != "tl":
            return []
        return [build_tapp(self.spec["tl"]["package"], self.out / "build", self.state)]


# --- building ---------------------------------------------------------------------------------

def tilolang_tool() -> Path:
    """The `tilolang` CLI, which builds an app's package into a signed Tilo app:
    the one `TILO_TILOLANG` names (a check's own), the session build's, or the
    tree's debug build (dist/dev/nested-session-build.sh makes one)."""
    for tool in (os.environ.get("TILO_TILOLANG"), BUILD / "bin/tilolang",
                 real_apps.TARGET / "debug/tilolang"):
        if tool and Path(tool).is_file():
            return Path(tool)
    raise Failed("no tilolang tool: dist/dev/nested-session-build.sh builds one, "
                 "or name it with TILO_TILOLANG")


def build_tapp(name: str, out: Path, state: Path) -> Path:
    """`tilolang build` the package `apps/tl/<name>` against the session's runtime:
    its signed package, which the session installs as an app."""
    tool = tilolang_tool()
    runtime = (state / "runtimes/.version").read_text().strip()
    print(f"==> tilolang build {name}", file=sys.stderr)
    run([str(tool), "build", str(REPO / "apps/tl" / name), "--search", str(REPO / "apps/tl"),
         "--out", str(out), "--tilo-runtime", f"tilo-runtime/{runtime}"])
    tapp = out / f"{name}.tapp"
    if not tapp.is_file():
        raise Failed(f"tilolang build made no {tapp}")
    return tapp


def prepare_state(state: Path, apps: str, build: bool) -> None:
    real_apps.prepare(state, apps=apps, built=build,
                      remote=os.environ.get("TILO_NESTED_SESSION_BUILD_EXEC"))


# --- the walk file ----------------------------------------------------------------------------

def step_found(record: dict) -> str:
    """What one step found, in a sentence: what it pressed, how many notes the
    app held, or that the version offers nothing for it."""
    said = []
    for op in record.get("ops") or []:
        found = op.get("found") or {}
        if "pressed" in found:
            said.append(f"pressed {found['pressed']}" if found["pressed"]
                        else "no control for it")
        elif "offered" in found:
            said.append(f"offers {found['offered']}" if found["offered"] else "no control for it")
        elif "count" in found and found["count"] is not None:
            said.append(f"{found['count']} held")
        elif "rows" in found:
            said.append(f"search rows {found['rows']}")
        elif "questions" in found:
            said.append(f"asked {len(found['questions'] or [])} question(s)")
        elif "actions" in found:
            named = found["actions"]
            if isinstance(named, int):
                said.append(f"{named} actions listed")
            else:
                said.append("actions: " + (", ".join(
                    str(a.get("title")) for a in named) or "none"))
        elif "clicked" in found:
            said.append(f"clicked {found['clicked']}")
        elif "rested_on" in found:
            said.append(f"rested on {found['rested_on']}")
        elif "pressed_ms_after_launch" in found:
            said.append(f"pressed {found['pressed_ms_after_launch']} ms after launch")
    if "failed" in record:
        said.append(f"stopped: {record['failed']}")
    return ", ".join(said)


def write_report(report: dict, out: Path) -> Path:
    """The walk as a page: each step of both versions side by side, its
    snapshots beside each other, and what each one found."""
    variants = [v for v in VARIANTS if v in report["variants"]]
    lines = [f"# {report['walk'].capitalize()}: the same first hour, "
             f"{' and '.join(variants)}", ""]
    for variant in variants:
        walked = report["variants"][variant]
        lines.append(f"- **{variant}**: `{walked['app']}`, its session up in "
                     f"{walked['session_start_ms']} ms")
    lines += ["", "| Step | " + " | ".join(variants) + " | What each found |",
              "|---|" + "---|" * len(variants) + "---|"]
    steps = {v: {step["id"]: step for step in report["variants"][v]["steps"]}
             for v in variants}
    for step in report["steps"]:
        cells = []
        for v in variants:
            record = steps[v].get(step["id"], {})
            shot = record.get("shot")
            dark = record.get("shot_dark")
            cell = f"![]({Path(shot).relative_to(out)})" if shot else "—"
            if dark:
                cell += f" dark: ![]({Path(dark).relative_to(out)})"
            cells.append(cell)
        found = " / ".join(f"{v}: {step_found(steps[v].get(step['id'], {}))}" for v in variants)
        lines.append(f"| **{step['id']}** — {step['said']} | " + " | ".join(cells)
                     + f" | {found} |")
    lines.append("")
    (out / "walk.md").write_text("\n".join(lines))
    return out / "walk.md"


def load_walk(name: str | None, path: str | None = None) -> tuple[Path, dict]:
    """The walk `name` names under app-walks/, or the file `path` names: a walk
    being written for an app whose steps are not settled yet."""
    if path:
        at = Path(path)
        if not at.is_file():
            raise Failed(f"no walk file at {at}")
        return at, json.loads(at.read_text())
    at = WALKS / f"{name}.json"
    if not at.is_file():
        raise Failed(f"no walk named {name}: {at} is not there")
    return at, json.loads(at.read_text())


def walks() -> list[str]:
    return sorted(path.stem for path in WALKS.glob("*.json"))


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Walk an app's first hour in each version its step file names.")
    parser.add_argument("app", nargs="?", help="the walk's name under app-walks/ (notes)")
    parser.add_argument("--list", action="store_true", help="print the walks there are")
    parser.add_argument("--only", choices=VARIANTS, help="walk one version alone")
    parser.add_argument("--out", type=Path, help="where the shots, logs and walk.json go")
    parser.add_argument("--file", help="a walk file to read instead of app-walks/APP.json")
    parser.add_argument("--state", type=Path,
                        help="where each version's session state goes (default: under --out)")
    parser.add_argument("--build", action="store_true",
                        help="build the session and its apps first")
    parser.add_argument("--keep", action="store_true", help="leave the session running")
    parser.add_argument("--discover", action="store_true",
                        help="keep every step's controls in walk.json, while writing a walk")
    options = parser.parse_args()
    if options.list:
        print("\n".join(walks()))
        return 0
    if not options.app and not options.file:
        parser.error("name a walk (--list shows them)")

    try:
        path, spec = load_walk(options.app, options.file)
        app = options.app or path.stem
        out = (options.out or HERE.parents[1] / "target" / "app-walk" / app).resolve()
        out.mkdir(parents=True, exist_ok=True)
        if options.build and spec.get("native"):
            real_apps.build(apps=spec["native"])
        # The versions this walk's step file names: a native app that was deleted in the change
        # its tilolang version took over leaves only its `tl` entry, and is walked alone.
        named = [variant for variant in VARIANTS if variant in spec]
        if not named:
            raise Failed(f"the walk names no version: {path}")
        if options.only is not None and options.only not in named:
            raise Failed(f"the walk names no {options.only} version: {path}")
        variants = [options.only] if options.only else named
        report = {"walk": app, "file": str(path), "variants": {},
                  "steps": [{"id": s["id"], "said": s["said"]} for s in spec["steps"]]}
        for variant in variants:
            print(f"==> {app}: walking the {variant} version", file=sys.stderr)
            # Each version walks on a session of its own: one desktop's notes are
            # its own, and the two versions must not meet in a search or a window.
            state = ((options.state or out) / f"state-{variant}").resolve()
            # The session's apps, and the runtime a tilolang package is built against.
            prepare_state(state, spec.get("native", ""), build=False)
            walk = Walk(spec, variant, out, state, discover=options.discover)
            report["variants"][variant] = walk.walk()
        (out / "walk.json").write_text(json.dumps(report, indent=1))
        report_path = write_report(report, out) if len(report["variants"]) > 1 else None
        print(json.dumps({"walk": str(out / "walk.json"),
                          "report": str(report_path) if report_path else None,
                          "shots": str(out / "shots")}))
        return 0
    except Failed as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
