#!/usr/bin/env python3
"""Make a workspace, invite someone, and work together live.

Ada opens Tilo, gives her name, and makes a workspace, "Garden club". The top
bar's Invite makes a native invitation link, which she hands to Grace, who
opens it on a computer of her own and joins. Each sees the other in the people
list within about two seconds. Grace opens Chat and the conversation with Ada
its sidebar lists her under, and a line Ada writes in the same conversation
shows on Grace's screen within about a second.

Every claim is read off the two shells: the workspaces each holds, the people
its own presence lists, the controls each draws, and the words Chat shows.

    python3 invite-live.py OUT_DIR
"""

from __future__ import annotations

import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from story import (  # noqa: E402
    CHROME, SHELL, Failed, Person, Story, front, in_front_named, invite_link, peers,
    stand_in as stands_in, until, walk, workspaces,
)

WORKSPACE = "Garden club"
JOIN_INVITATION = "TILO_SHELL_JOIN_INVITATION_FILE"
# What both computers have beside the shell: the tilolang Chat (its directory
# under apps/tl), where the two write to each other.
TL_APPS = ("chat",)
CHAT = "so.tilo.chat"
# The line Ada writes.
LINE = f"Grace, the tomatoes are planted ({int(time.time()) % 10000:04d})."
# What Chat says under a line no other device has taken in yet (std.reach_note).
NOT_REACHED = "Not reached anyone yet"
# How soon after the join the people list must show the other person.
PEOPLE_SECONDS = 2.0


def people_list(one: Person, workspace: str) -> list[str]:
    """The names this person's shell lists everyone else here under."""
    _, members = peers(one, workspace)
    return [member.get("label") or "" for member in members]


def composer(other: str) -> str:
    """The composer of the direct conversation with `other`, named by what it asks for."""
    return f"Message {other}"


def opens_conversation(one: Person, other: str) -> None:
    """Open Chat and, in its sidebar's direct messages, the conversation with `other`: the one
    conversation the two of them have, whichever of them opened it first."""
    one.session.launch("chat")
    one.session.answering("chat", deadline=120)
    try:
        one.press(other, f"{other} in Chat's sidebar on {one.name}'s shell", exact=False,
                  inside=CHAT)
        until(f"Chat's composer to {other} on {one.name}'s shell",
              lambda: one.target(composer(other)), 30, 0.25)
    finally:
        one.story.note(f"chat-controls-{one.key}", one.names())


def steps(story: Story) -> None:
    ada, grace = story.person("Ada"), story.person("Grace")
    shared: dict = {}

    def ada_opens():
        ada.prepare()
        took = ada.start(tilolang=TL_APPS)
        story.timing("Ada's session start, to its first frame", took["tookMs"])
        story.timing("Ada's session start, to its first frame, with the development launcher's "
                     "installing", took["launcherMs"])
        until("Ada's shell to hold a workspace", lambda: front(ada), 60)
        return {"tookMs": took["tookMs"]}

    story.step("Ada opens Tilo", ada_opens, people=[ada])

    story.step("Ada gives their name", lambda: ada.name_self("Ada"), people=[ada])

    def makes():
        before = {w.get("id") for w in workspaces(ada)}
        ada.session.invoke(SHELL, CHROME, "new-workspace", {"name": WORKSPACE})
        made = until("a new workspace on Ada's shell", lambda: next(
            (w for w in workspaces(ada) if w.get("id") not in before), None), 30, 0.2)
        shared["id"] = made["id"]
        stands_in(ada, shared["id"])
        until(f"Ada's top bar saying she is in {WORKSPACE}",
              lambda: in_front_named(ada, WORKSPACE), 15, 0.2)

    story.step("Ada makes a new workspace, “Garden club”", makes, people=[ada])

    def invites():
        link = invite_link(ada)
        if not link.startswith("tilo://invite#1."):
            raise Failed(f"Ada copied no native invitation: {link[:80]!r}")
        shared["link"] = link
        # Ada sends Grace the link (how is no part of Tilo), and closes her sheet.
        ada.session.key("Escape")
        until("Ada's invite sheet to close", lambda: not ada.target("Copy link"), 10, 0.2)

    story.step("Ada makes an invitation link", invites, people=[ada])

    def grace_opens():
        grace.prepare()
        invitation = story.out / "invitation.txt"
        invitation.write_text(shared["link"])
        took = grace.start(tilolang=TL_APPS, env={JOIN_INVITATION: str(invitation)})
        story.timing("Grace's session start with the invitation, to its first frame",
                     took["tookMs"])
        return {"tookMs": took["tookMs"]}

    story.step("Grace opens the invitation on her computer", grace_opens, people=[grace])

    def joins():
        joined = time.monotonic()
        until(f"Grace's shell to hold {WORKSPACE}",
              lambda: any(w.get("id") == shared["id"] for w in workspaces(grace)), 240, 0.5)
        stands_in(grace, shared["id"])
        until(f"Grace's top bar saying she is in {WORKSPACE}",
              lambda: in_front_named(grace, WORKSPACE), 30, 0.2)
        story.timing(f"Grace's join, to {WORKSPACE} in front", (time.monotonic() - joined) * 1000)
        grace.name_self("Grace")
        return {"tookMs": (time.monotonic() - joined) * 1000}

    story.step("Grace joins Garden club", joins, people=[ada, grace])

    def see_each_other():
        began = time.monotonic()

        def both():
            here = people_list(ada, shared["id"])
            there = people_list(grace, shared["id"])
            return (here, there) if "Grace" in here and "Ada" in there else None

        try:
            here, there = until("each shell's people list to show the other", both,
                                PEOPLE_SECONDS + 15, 0.1)
        except Failed:
            raise Failed(f"Ada's people list shows {people_list(ada, shared['id'])} and Grace's "
                         f"{people_list(grace, shared['id'])} {PEOPLE_SECONDS + 15:.0f} s after "
                         f"Grace joined") from None
        story.note("people-lists", {"Ada": here, "Grace": there})
        return {"tookMs": (time.monotonic() - began) * 1000}

    story.step("Ada and Grace see each other in their people lists", see_each_other,
               people=[ada, grace])

    story.step("Grace opens Chat", lambda: opens_conversation(grace, "Ada"), people=[grace])

    def ada_writes():
        opens_conversation(ada, "Grace")
        ada.press(composer("Grace"), "Chat's composer")
        ada.session.type(LINE)
        # Enter sends what the composer holds, as Chat's own field and button both do.
        ada.session.key("Enter")
        # The line stands in Ada's conversation at once, saying it has reached nobody until a
        # device that keeps the workspace takes it in: the note going is the workspace taking it.
        try:
            until("Ada's line to reach the workspace",
                  lambda: ada.shows(LINE) and not ada.shows(NOT_REACHED), 60, 0.1)
        finally:
            story.note("chat-controls-after-send", ada.names())

    story.step("Ada writes a line in Chat", ada_writes, people=[ada])

    def grace_sees():
        began = time.monotonic()
        until(f"Ada's words {LINE!r} on Grace's screen",
              lambda: grace.shows(LINE), 20, 0.05)
        return {"tookMs": (time.monotonic() - began) * 1000}

    story.step("Grace sees Ada’s words", grace_sees, people=[grace])


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: invite-live.py OUT_DIR", file=sys.stderr)
        return 2
    return walk("invite-live", Path(sys.argv[1]), steps)


if __name__ == "__main__":
    sys.exit(main())
