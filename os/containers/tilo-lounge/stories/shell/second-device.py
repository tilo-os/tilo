#!/usr/bin/env python3
"""Add Ada's second computer from a local workspace with no web host.

Ada's laptop runs Tilo, and its workspace has no host on the web, so Add a
device shows a ticket from the laptop itself. She copies it, and her second
computer logs in with it (`tilo login`): the laptop certifies the second
computer's key with her root, and the second computer is one of her devices.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from cli_host import Computer, TICKET  # noqa: E402
from story import Failed, Story, until, walk  # noqa: E402

SHELL = "so.tilo.session"
SHOW_PAIR_DEVICE = ("session:invite", "show-pair-device")
APPS = "chat tasks file-explorer settings activity"


def steps(story: Story) -> None:
    laptop = story.person("Ada’s laptop")
    second = Computer(story, "Ada’s second computer", "Ada’s desk")
    shared: dict = {}

    def laptop_opens():
        took = laptop.start(apps=APPS)
        laptop.name_self("Ada")
        story.timing("Ada's session start, to its first frame", took["tookMs"])
        story.timing("Ada's session start, to its first frame, with the development launcher's installing", took["launcherMs"])
        return {"tookMs": took["tookMs"]}

    story.step("Ada opens Tilo on her laptop", laptop_opens, people=[laptop])

    def asks():
        laptop.session.invoke(SHELL, *SHOW_PAIR_DEVICE, {})
        laptop.press("Copy ticket", "Copy ticket in Add a device", deadline=12)
        ticket = until("the Add a device ticket on the clipboard",
                       lambda: (laptop.session.state_dump() or {}).get("clipboard"), 3, 0.1)
        if not TICKET.fullmatch(ticket or ""):
            raise Failed("Add a device did not put a ticket on the clipboard")
        shared["ticket"] = ticket
        story.note("pair-sheet-controls", laptop.names())
        laptop.snapshot(story.out / "shots/02-add-a-device-ticket-ada-s-laptop.png")

    story.step("Ada asks her laptop for a way to add a new device", asks, people=[laptop])

    def adds_second():
        said = second.ok("login", shared["ticket"], "--label", second.label)
        if "Logged in" not in said:
            raise Failed(f"the second computer did not say it logged in: {said[-1200:]}")
        listed = second.json("device", "ls", "--json")
        if not isinstance(listed, list):
            raise Failed(f"`tilo device ls` listed no devices: {listed}")
        mine = second.device_key()
        keys = {row.get("device") for row in listed}
        if mine not in keys:
            raise Failed(f"the second computer's own key {mine[:12]}… is not among its devices")
        if len(keys - {mine}) < 1:
            raise Failed(f"the laptop that issued the ticket is not among the devices: {listed}")
        story.note("devices", listed)
        return {}

    try:
        story.step("Ada adds her second computer as one of her devices", adds_second,
                   people=[laptop])
    finally:
        second.stop()


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print("usage: second-device.py OUT_DIR", file=sys.stderr)
        sys.exit(2)
    sys.exit(walk("second-device", Path(sys.argv[1]), steps))
