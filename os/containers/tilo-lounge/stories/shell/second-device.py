#!/usr/bin/env python3
"""One person's laptop and phone, in two Tilo shells: how far a shell lets Ada get.

Ada works in Tilo on her laptop's shell and asks it to add her phone as another of her devices,
the way a person does: the shell's own Add a device sheet, which is what the search's "Pair a new
device" and the card's "Pair another device" open. The steps read back what that sheet offers —
a link a second device opens, or the way to add one nearby — off the shell's own controls, and
say plainly where a shell stops.

    python3 second-device.py OUT_DIR
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from story import Person, Story, until, walk, Failed  # noqa: E402

SHELL = "so.tilo.session"
INVITATIONS = "session:invite"
SHOW_PAIR_DEVICE = f"{INVITATIONS}\u001fshow-pair-device"
# What the sheet's body says where the workspace in front has no host on the web: there is no
# link to show, and the device is added nearby instead.
NEARBY = "Add it nearby"
# The sheet calls the link it would show "The link", and the field a machine's own line is pasted
# into "The machine's link".
LINK = "The link"
MACHINE_LINK = "The machine's link"
APPS = "chat tasks notes file-explorer settings activity"


def note_actions(story: Story, one: Person, query: str) -> None:
    """What the shell lists for `query`, beside the story: the action Ada's press takes."""
    story.note(f"actions-{query}", [
        {k: row.get(k) for k in ("provider", "id", "title") if row.get(k) is not None}
        for row in one.session.actions(query)])


def steps(story: Story) -> None:
    laptop = story.person("Ada’s laptop")

    def laptop_opens():
        took = laptop.start(apps=APPS)
        story.timing("Ada's session start, to its first frame", took["tookMs"])
        story.timing("Ada's session start, to its first frame, with the development launcher's installing", took["launcherMs"])
        return {"tookMs": took["tookMs"]}

    story.step("Ada opens Tilo on her laptop", laptop_opens, people=[laptop])

    offered: dict = {}

    def asks():
        note_actions(story, laptop, "pair")
        laptop.session.invoke(SHELL, SHOW_PAIR_DEVICE, {})
        # The sheet is up once it draws what it offers: a link, or the words for adding a device
        # nearby, or the field a machine's own line is pasted into.
        def drawing() -> dict | None:
            names = laptop.names()
            found = {"link": any(name.startswith(LINK) for name in names),
                     "nearby": any(NEARBY in name for name in names),
                     "machine_link": any(MACHINE_LINK in name for name in names)}
            return found if found["link"] or found["nearby"] or found["machine_link"] else None

        offered.update(until("the Add a device sheet on Ada's laptop", drawing, 30, 0.2))
        story.note("pair-sheet-controls", laptop.names())
        story.note("pair-sheet-windows", laptop.session.windows())

    story.step("Ada asks her laptop for a way to add a new device", asks, people=[laptop])

    def adds_phone():
        if offered.get("link"):
            raise Failed(
                "the laptop shows a pairing link, and the shell has no way to open one on a "
                "second device: Join a workspace redeems invitations, and a link that carries "
                "a pairing secret is read as a workspace link and its pairing ignored.")
        raise Failed(
            "the laptop offers no link for the phone. The workspace in front is kept by no host "
            "on the web, so the sheet says to add the device nearby. That way runs the image's "
            "tilo-enroll and the polkit helper that admits what it finds, and a Tilo session on "
            "this machine has neither. The sheet's paste field wants a tilo://enroll? line, "
            "which a nearby pairing does not print. So Ada's phone cannot become one of her "
            "devices from a shell.")

    story.step("Ada adds her phone as one of her devices", adds_phone, people=[laptop])


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print("usage: second-device.py OUT_DIR", file=sys.stderr)
        sys.exit(2)
    sys.exit(walk("second-device", Path(sys.argv[1]), steps))
