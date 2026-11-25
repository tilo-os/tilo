#!/usr/bin/env python3
"""A newcomer finds their way around the desktop, in a Tilo shell.

Someone new to Tilo opens their workspace, adds a strip, switches to another workspace and back,
finds their settings and picks a light and a dark look there, all through what the bar and the
workspace show: the controls the shell publishes, pressed where they stand. Then they come back to
their desk the next day: what they arranged is where they left it. No keyboard chord a newcomer would not know is used to find
anything, so what is recorded is that each thing was findable.

Every step's pass or fail is read off the shell's own state: the state dump (the workspaces it
holds and which is in front, the strip it is on) and the controls it shows by their accessible
names and whether each shows itself chosen, never off what a press answered. What the look and the
accent are set to is read from the appearance settings the person changed.

    python3 desktop-easy.py OUT_DIR
"""

from __future__ import annotations

import json
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from mail_provider import press_in, window_named  # noqa: E402
from story import PROFILE, SWITCHER, Failed, Person, Story, front, until, walk, workspaces  # noqa: E402

SETTINGS = "so.tilo.settings"
STRIPS = "Show strips, now on "
APPS = "files settings"
# The tilolang apps beside them, by their directory under apps/tl.
TILOLANG = ("tasks",)
# Settings' page with the look, as Settings names it, and its light-or-dark choice.
APPEARANCE_PAGE = "Appearance"
MODE = "Light or dark"


def bar_workspace(one: Person) -> str | None:
    """The workspace the bar says the person is in, as its own control names it."""
    seen = next((name for name in one.names() if name.startswith(SWITCHER)), None)
    return seen[len(SWITCHER):] if seen else None


def bar_strip(one: Person) -> str | None:
    """The strip the bar says the person is on, as its own control names it."""
    seen = next((name for name in one.names() if name.startswith(STRIPS)), None)
    return seen[len(STRIPS):] if seen else None


def press_bar(one: Person, prefix: str, what: str) -> str:
    """Press the bar's control named `prefix` and after it (`Switch workspace, now in Garden
    club`: the workspace it names), and answer the name it carried."""
    found = until(f"{what} in {one.name}'s bar", lambda: next(
        (label for label in one.names() if label.startswith(prefix)), None), 30, 0.2)
    one.press(found, what)
    return found[len(prefix):]


def drawn(one: Person, app_id: str) -> dict | None:
    """The window of `app_id` the shell places and has drawn: a tab with no window in it, or one
    that never drew, shows the person nothing, whatever its accessible names say."""
    window = window_named(one, app_id)
    return window if window and window.get("frames") else None


def picks_mode(one: Person, mode: str) -> None:
    """Pick the `mode` look on the Appearance page of the Settings window that is open, as a person
    goes on from the settings they just found, and read back that the mode shows itself chosen and
    the appearance setting holds it."""
    settings = until("Settings, where the look is picked, drawn",
                     lambda: drawn(one, SETTINGS), 30, 0.25)
    if not one.target(MODE, exact=False):
        press_in(one, APPEARANCE_PAGE, settings, f"the {APPEARANCE_PAGE} page in Settings")
    until(f"the {MODE} setting", lambda: one.target(MODE, exact=False), 30, 0.2)
    if not one.target(mode):
        # The modes are a choice that opens to show them.
        press_in(one, MODE, settings, f"the {MODE} setting", exact=False)
        until(f"the {mode} look among the modes", lambda: one.target(mode), 10, 0.2)
    press_in(one, mode, settings, f"the {mode} look")
    chosen(one, mode, f"the {mode} look")
    held = appearance(one).get("mode")
    if held != mode.lower():
        raise Failed(f"the {mode} look was pressed but the appearance holds the mode {held!r}")
    # The look changes through the shell's motion: the shot is of where it came to rest.
    time.sleep(1.0)


def picks_accent(one: Person, accent: str) -> None:
    """Pick the `accent` colour from the swatch row, and read back that the swatch shows itself
    chosen and the appearance setting holds it."""
    settings = until("Settings, where the accent is picked, drawn",
                     lambda: drawn(one, SETTINGS), 30, 0.25)
    try:
        until(f"the {accent} swatch", lambda: one.target(accent), 15, 0.2)
    except Failed:
        names = [target.get("name") for target in one.targets() if target.get("name")]
        raise Failed(f"no {accent} swatch; the shell shows {json.dumps(names)[:800]}") from None
    press_in(one, accent, settings, f"the {accent} accent swatch")
    chosen(one, accent, f"the {accent} accent")
    held = appearance(one).get("accent")
    if held != accent.lower():
        raise Failed(f"the {accent} accent was pressed but the appearance holds {held!r}")
    time.sleep(1.0)


def appearance(one: Person) -> dict:
    """The appearance setting as Settings reads it now: the look's mode and the accent it holds,
    read from the resource itself rather than from what a press answered."""
    for row in one.session.actions("inspect-appearance"):
        address = (row.get("action") or {}).get("address") or {}
        if address.get("action") != "inspect-appearance":
            continue
        answer = one.session.invoke(row["provider"], address["resource"],
                                    "inspect-appearance", {})
        value = (answer.get("Completed") or {}).get("output", {}).get("value")
        if isinstance(value, dict):
            return value
        raise Failed(f"Settings' appearance read answered nothing: {json.dumps(answer)[:600]}")
    raise Failed("Settings offers no reading of the appearance")


def chosen(one: Person, name: str, what: str) -> dict:
    """The control `name` once it shows itself the chosen one: a segment that holds the look, a
    swatch that holds the accent. A press that changed nothing leaves it unchosen, and fails
    here."""
    def held():
        target = one.target(name)
        return target if target and target.get("selected") else None
    return until(f"{what} to show itself chosen", held, 15, 0.2)


def no_sheets(one: Person) -> None:
    """No sheet stands over Settings: opening it must ask nothing before the person touches a
    control, and a question about a pane they never opened would take their clicks instead."""
    answer = one.session.control({"op": "questions"}).get("answer") or {}
    pending = answer.get("questions") or []
    if pending:
        raise Failed(f"a sheet stands over Settings before a look is picked: "
                     f"{json.dumps(pending)[:800]}")


def steps(story: Story) -> None:
    robin = story.person("Robin")
    robin.prepare(APPS)
    first: dict = {}

    def opens():
        took = robin.start(apps=APPS, tilolang=TILOLANG)
        story.timing("Robin's session start, to its first frame", took["tookMs"])
        story.timing("Robin's session start, to its first frame, with the development launcher's installing", took["launcherMs"])
        first["workspace"] = until("their workspace, named in the bar",
                                   lambda: bar_workspace(robin), 60)
        until("their shell holding a workspace", lambda: front(robin), 30)
        return {"tookMs": took["tookMs"]}

    story.step("A newcomer opens their workspace", opens, people=[robin])

    def adds_strip():
        # The New strip button stands at the strip column's foot, at rest: pressing it makes the
        # strip and asks for its name in place.
        robin.press("New strip", "the New strip button by the strips")
        field = until("the new strip's name, asked in place", lambda: robin.target("Strip name"), 20, 0.2)
        robin.press("Strip name", "the new strip's name field")
        robin.session.type("Writing")
        robin.session.key("Enter")
        try:
            until("the bar naming the strip Writing",
                  lambda: bar_strip(robin) == "Writing", 15, 0.2)
        finally:
            story.note("strip-controls", robin.targets())
            story.note("strip-field", field)

    story.step("They add a strip", adds_strip, people=[robin])

    def switches_and_back():
        # The bar's workspace name opens the workspaces: the one they are in, a new one, and a
        # way to make one.
        made = press_bar(robin, SWITCHER, "the workspace in the bar")
        if made != first["workspace"]:
            raise Failed(f"the bar names {made!r}, not the workspace they arrived in "
                         f"({first['workspace']!r})")
        robin.press("New workspace", "the switcher's New workspace")

        def another():
            """The bar naming a workspace that is not the one they arrived in."""
            name = bar_workspace(robin)
            return name if name and name != first["workspace"] else None

        second = until("another workspace in the bar", another, 30, 0.25)
        until("the new workspace holding", lambda: len(workspaces(robin)) == 2, 20, 0.25)
        # And back: the bar's name opens the switcher, where the workspace they arrived in is
        # listed by its name.
        press_bar(robin, SWITCHER, f"the workspace in the bar ({second})")
        robin.press(first["workspace"], f"{first['workspace']} in the switcher")
        until("the workspace they arrived in, in front again",
              lambda: bar_workspace(robin) == first["workspace"], 15, 0.25)
        story.note("workspaces", [w.get("id") for w in workspaces(robin)])

    story.step("They switch to another workspace and back", switches_and_back, people=[robin])

    def finds_settings():
        # Their own face in the bar opens their card, where Settings leads.
        robin.press(PROFILE, "their own face in the bar", exact=False)
        took = time.monotonic()
        robin.press("Settings", "Settings on their own card")
        until("Settings' window, placed and drawn", lambda: drawn(robin, SETTINGS), 60, 0.25)
        # Its own pages, so what opened is Settings and not a window named after it.
        until("Settings' own pages", lambda: robin.shows("Settings categories"), 30, 0.25)
        story.timing("Settings, from the press to its window", (time.monotonic() - took) * 1000)

    story.step("They find their settings", finds_settings, people=[robin])

    def picks_looks():
        # Opening Settings asks nothing: a sheet about a pane they never opened would take the
        # first click instead of the control they meant.
        no_sheets(robin)
        # The light palette paints two hues as one accent and offers them once, so the Pink
        # swatch exists while the look is dark.
        picks_mode(robin, "Dark")
        picks_accent(robin, "Pink")
        story.shot(robin, "the dark look with the pink accent")
        picks_mode(robin, "Light")
        held = appearance(robin).get("accent")
        if held != "pink":
            raise Failed(f"the accent did not follow the look into light: {held!r}")
        story.shot(robin, "the light look with the pink accent")

    story.step("They pick a light and a dark look", picks_looks, people=[robin])

    def comes_back():
        # Their computer starts again with what it kept: the workspaces, the strip they made, and
        # the one they were in, as they left them.
        made = [w.get("id") for w in workspaces(robin)]
        robin.stop()
        took = robin.start(apps=APPS, tilolang=TILOLANG, fresh=False)
        until("the workspace they were in, named in the bar",
              lambda: bar_workspace(robin) == first["workspace"], 60, 0.25)
        until("both their workspaces",
              lambda: sorted(w.get("id") for w in workspaces(robin)) == sorted(made), 30, 0.25)
        # The bar names the strip they were on, without anything being opened over the desk, and
        # the Settings window they left there opens again and draws.
        until("the Writing strip they made, named in the bar",
              lambda: bar_strip(robin) == "Writing", 15, 0.2)
        until("Settings' window again, placed and drawn", lambda: drawn(robin, SETTINGS), 60, 0.25)
        return {"tookMs": took["tookMs"]}

    story.step("They come back to their desk", comes_back, people=[robin])


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print("usage: desktop-easy.py OUT_DIR", file=sys.stderr)
        sys.exit(2)
    sys.exit(walk("desktop-easy", Path(sys.argv[1]), steps))
