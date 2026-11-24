#!/usr/bin/env python3
"""A home server joins the person's workspace with a ticket, and holds what is written there.

Ada signs in on her computer, and the act that makes her person issues a
ticket for a home server — a
computer with its own key, where the `tilo` command and the daemon it starts
are the whole of Tilo, as a server in the corner of a home is. The server
spends the ticket with `tilo login`, is certified as one of Ada's devices, and
enters her workspace.

Then it holds the workspace's resources. Ada puts a file in the workspace from
her computer, and the server reads it back with `tilo read` — and its own
state, on its own disk, holds the file's words: not a query it answered for
someone else, but what the server keeps.

    python3 home-server-joins.py OUT_DIR
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from cli_host import Computer, holds, workspace_of  # noqa: E402
from story import Failed, Story, until, walk  # noqa: E402

FILE = "house-inventory.md"
WORDS = "Two ladders and a spare key for the shed."


def steps(story: Story) -> None:
    ada = Computer(story, "Ada’s computer", "Ada’s computer")
    server = Computer(story, "Ada’s home server", "Ada’s home server")
    shared: dict = {}
    try:
        def asks():
            # Her computer's daemon makes the person, so it runs first, and the
            # act that makes her issues the ticket the home server joins with:
            # `server` is what makes the certified computer one that keeps a
            # person's workspaces, not an ordinary member device. A program of
            # hers issues no ticket on its own, so the ticket comes with her
            # founding rather than from `tilo device add`.
            ada.start_daemon()
            said = ada.json("login", "--new-identity", "--name", "Ada", "--json",
                            "--ticket", "server")
            if not said.get("user_public_key"):
                raise Failed(f"the sign-in made no identity: {said}")
            tickets = [ticket for ticket in said.get("tickets") or []
                       if ticket.get("kind") == "server"]
            ticket = tickets[0].get("ticket") if tickets else None
            if not ticket or not ticket.startswith("tilo1"):
                raise Failed(f"the sign-in issued no ticket for a server: {said}")
            shared["ticket"] = ticket
            return {}

        story.step("Ada signs in on her computer, and its founding issues a ticket for a "
                   "home server", asks)

        def joins():
            said = server.ok("login", shared["ticket"], "--label", server.label)
            if "Logged in" not in said:
                raise Failed(f"the home server did not say it logged in: {said[-1200:]}")
            if "Entered" not in said:
                raise Failed(f"the home server logged in but entered no workspace: "
                             f"{said[-1200:]}")
            # The workspace the server's own login lists: the one Ada writes in
            # next. What `tilo device ls` says there is kept beside the story:
            # a computer joined this way lists no device of the person's today.
            workspace = workspace_of(server)
            shared["workspace"] = workspace["id"]
            try:
                story.note("devices-listed", server.json("device", "ls", "--json"))
            except Failed as refusal:
                story.note("devices-listed", str(refusal))
            if server.device_key() == ada.device_key():
                raise Failed("the server and the computer answer one device key")
            story.note("workspace", workspace)
            return {}

        story.step("The home server joins with the ticket", joins)

        def puts():
            (ada.root / "home" / FILE).write_text(f"# Shed\n\n{WORDS}\n")
            rows = ada.json("files", "put", str(ada.root / "home" / FILE),
                            "-w", shared["workspace"], "--json")
            row = rows[0] if isinstance(rows, list) and rows else {}
            if not row.get("path") or not row.get("content"):
                raise Failed(f"the file did not reach the workspace's files: {rows}")
            shared["path"], shared["content"] = row["path"], row["content"]
            return {}

        story.step("Ada puts a file in the workspace from her computer", puts)

        def reads():
            said = server.ok("files", "get", shared["content"], "-w", shared["workspace"])
            if WORDS not in said:
                raise Failed(f"the server fetched no {WORDS!r} from {shared['content']}: "
                             f"{said.strip()[:1200]}")
            # And it keeps them: the words are in the server's own state on its
            # own disk, before anything asks it again. What the daemon said as
            # it worked is no store of the workspace's, so its log is left out.
            try:
                kept = until(f"the server's own state to hold {WORDS!r}",
                             lambda: holds(server.root, WORDS, skip=(".log",)), 30, 0.5)
            except Failed:
                raise Failed(f"the server fetched the file but keeps no {WORDS!r} of its own "
                             f"under {server.root}") from None
            story.note("held-by-the-server", kept)
            return {}

        story.step("The home server reads the file from its own state", reads)
    finally:
        story.note("commands", {one.name: one.said for one in (ada, server)})
        server.stop()
        ada.stop()


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: home-server-joins.py OUT_DIR", file=sys.stderr)
        return 2
    return walk("home-server-joins", Path(sys.argv[1]), steps)


if __name__ == "__main__":
    sys.exit(main())
