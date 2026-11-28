#!/usr/bin/env python3
"""Add a second computer with a ticket from the first, and read the workspace from it.

Ada signs in on her computer — a computer where the `tilo` command is the
whole of Tilo — and the act that makes her person issues a ticket for another
computer. On the second computer she runs `tilo login` with the ticket:
the first certifies the second's key with Ada's root, the second enters Ada's
workspace, and it is one of her member devices from then on.

Every claim is read off the two computers with the same commands a person
runs: each computer's own device key (`tilo device public-key`), the person's
devices as a device of hers lists them (`tilo device ls`), and the workspace's
own files — Ada puts one from her computer with `tilo files put`, and the
second reads it back with `tilo read`.

    python3 cli-login-ticket.py OUT_DIR
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from cli_host import Computer, workspace_of  # noqa: E402
from story import Failed, Story, walk  # noqa: E402

# What Ada writes on her computer, and puts in the workspace: words a reader
# cannot mistake for another file's.
FILE = "spring-plan.md"
WORDS = "The avocet lands at nine on the twenty-first."


def steps(story: Story) -> None:
    ada = Computer(story, "Ada’s computer", "Ada’s computer")
    second = Computer(story, "Ada’s second computer", "Ada’s desk")
    shared: dict = {}
    try:
        def signs_in():
            # The person's identity on this computer, which holds its root. Her
            # computer's daemon makes the person, so it runs first, and the act
            # that makes her issues the ticket the other computer joins with: a
            # program of hers issues no ticket on its own.
            ada.start_daemon()
            said = ada.json("login", "--new-identity", "--name", "Ada", "--json",
                            "--ticket", "device")
            if not said.get("user_public_key"):
                raise Failed(f"the sign-in made no identity: {said}")
            shared["person"] = said["user_public_key"]
            tickets = [ticket for ticket in said.get("tickets") or []
                       if ticket.get("kind") == "device"]
            ticket = tickets[0].get("ticket") if tickets else None
            if not ticket or not ticket.startswith("tilo1"):
                raise Failed(f"the sign-in issued no ticket for a computer: {said}")
            shared["ticket"] = ticket
            return {}

        story.step("Ada signs in on her computer, and its founding issues a ticket for "
                   "another", signs_in)

        def logs_in():
            # The second computer spends the ticket: the first certifies its
            # key, and it enters Ada's workspace as one of her devices. Its
            # daemon is started here as its service would run it: this machine
            # has no service manager for `tilo login` to start it through.
            second.start_daemon()
            said = second.ok("login", shared["ticket"], "--label", second.label)
            if "Logged in" not in said:
                raise Failed(f"the second computer did not say it logged in: {said[-1200:]}")
            if "Entered" not in said:
                raise Failed(f"the second computer logged in but entered no workspace: "
                             f"{said[-1200:]}")
            # Its own login lists Ada's workspace: the one the first serves, and
            # the one Ada writes in next. What `tilo device ls` says there is
            # kept beside the story: a computer joined this way lists no device
            # of the person's today, and the story's claim is the workspace.
            listed = workspace_of(second)
            shared["workspace"] = listed["id"]
            try:
                story.note("devices-listed", second.json("device", "ls", "--json"))
            except Failed as refusal:
                story.note("devices-listed", str(refusal))
            if second.device_key() == ada.device_key():
                raise Failed("both computers answer one device key: the second reused the first's")
            story.note("workspace", listed)
            return {}

        story.step("The second computer logs in with the ticket", logs_in)

        def puts():
            (ada.root / "home" / FILE).write_text(f"# Spring\n\n{WORDS}\n")
            rows = ada.json("files", "put", str(ada.root / "home" / FILE),
                            "-w", shared["workspace"], "--json")
            row = rows[0] if isinstance(rows, list) and rows else {}
            if not row.get("path") or not row.get("content"):
                raise Failed(f"the file did not reach the workspace's files: {rows}")
            shared["path"], shared["content"] = row["path"], row["content"]
            return {}

        story.step("Ada puts a file in the workspace from her computer", puts)

        def reads():
            # The bytes the workspace keeps for the file, fetched by the second
            # computer over its own login: the file Ada put is readable there.
            said = second.ok("files", "get", shared["content"], "-w", shared["workspace"])
            if WORDS not in said:
                raise Failed(f"the second computer fetched no {WORDS!r} from "
                             f"{shared['content']}: {said.strip()[:1200]}")
            return {}

        story.step("The second computer reads the file from the workspace", reads)
    finally:
        story.note("commands", {one.name: one.said for one in (ada, second)})
        second.stop()
        ada.stop()


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: cli-login-ticket.py OUT_DIR", file=sys.stderr)
        return 2
    return walk("cli-login-ticket", Path(sys.argv[1]), steps)


if __name__ == "__main__":
    sys.exit(main())
