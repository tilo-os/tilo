#!/usr/bin/env python3
"""A newcomer finds their way around the desktop, in a Tilo shell.

Someone new to Tilo opens their workspace, adds a strip, switches to another workspace and back,
and finds their settings, all through what the bar and the workspace show: the controls the shell
publishes, pressed where they stand. No keyboard chord a newcomer would not know is used to find
anything, so what is recorded is that each thing was findable.

Every step's pass or fail is read off the shell's own state: the state dump (the workspaces it
holds and which is in front, the strip it is on) and the controls it shows by their accessible
names, never off what an action answered.

    python3 desktop-easy.py OUT_DIR
"""

from __future__ import annotations

import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from story import PROFILE, SWITCHER, Failed, Person, Story, front, until, walk, workspaces  # noqa: E402

SETTINGS = "so.tilo.settings"
STRIPS = "Show strips, now on "
APPS = "notes chat file-explorer settings"


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


def steps(story: Story) -> None:
    robin = story.person("Robin")
    robin.prepare(APPS)
    first: dict = {}

    def opens():
        took = robin.start(apps=APPS)
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
        until("Settings' window", lambda: next(
            (w for w in robin.session.windows() if w.get("app_id") == SETTINGS), None), 60, 0.25)
        # Its own pages, so what opened is Settings and not a window named after it.
        until("Settings' own pages", lambda: robin.shows("Settings categories"), 30, 0.25)
        story.timing("Settings, from the press to its window", (time.monotonic() - took) * 1000)

    story.step("They find their settings", finds_settings, people=[robin])


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print("usage: desktop-easy.py OUT_DIR", file=sys.stderr)
        sys.exit(2)
    sys.exit(walk("desktop-easy", Path(sys.argv[1]), steps))
