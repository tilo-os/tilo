#!/usr/bin/env python3
"""What people who were invited get up to, in Tilo shells.

Ada makes a workspace, "Garden club", and makes an invitation into it for each of Grace and Lin.
Each of them starts their own Tilo on a computer that has never seen the workspace, joins with
their own link, gives their name, and opens an app to work in: Grace writes a note, Lin opens
Tasks. Ada sees who is here and what each is working on, follows Grace and has the window Grace
works in brought in front with her, mentions Lin in Chat, and Lin is told of the mention and opens
it.

Every step's pass or fail is read off the shells' own state: the state dump (workspaces, who is
here and what each has in front, whom they follow, what was told), the controls each shell shows
by their accessible names, and the windows it holds, never off what an action answered.

    python3 invited-activity.py OUT_DIR
"""

from __future__ import annotations

import json
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from story import (  # noqa: E402
    CHROME, SHELL, Failed, Person, Story, front, in_front_named, invite_link, peers, stand_in,
    until, walk, workspaces,
)

WORKSPACE = "Garden club"
JOIN_INVITATION = "TILO_SHELL_JOIN_INVITATION_FILE"
NOTES = "so.tilo.notes"
TASKS = "so.tilo.tasks"
CHAT = "so.tilo.chat"
# The control that opens who is here.
EVERYONE = "Everyone here"
# What every computer has: each app anyone in this story works in, and the shell's own.
APPS = "notes tasks chat file-explorer settings"
# The chat's first part of messages, as its app names it (its `messages` chain of the workspace's
# default document): where a fresh workspace's posts land.
CHAT_MESSAGES = "derived:part.0@derived:messages@derived:default@tab:0000000000000000"


def named(rows: list[dict], label: str) -> dict:
    return next((row for row in rows if row.get("label") == label), {})


def active_window(one: Person) -> dict | None:
    """The window in front on this person's shell."""
    return next((w for w in one.session.windows() if w.get("active")), None)


def open_here(one: Person, face: str) -> None:
    """Open who is here as a person does: a face in the bar opens that person's card, whose
    Everyone here opens the roster. Where the bar has room for no face at all it offers the
    control itself, and that opens the roster."""
    if one.target(EVERYONE):
        one.press(EVERYONE, "Everyone here in the bar")
        return
    one.press(face, f"the face of {face} in the bar")
    one.press(EVERYONE, "Everyone here on the card")


def person_root(row: dict, one: Person | None = None) -> str:
    """The person `row` is, by their root key: what a mention of them names them by. Their
    person's other devices carry the same root, so any of them says it."""
    if row.get("user"):
        return row["user"]
    member = row.get("member")
    if isinstance(member, dict) and member.get("Person"):
        return member["Person"]
    if one is not None:
        own = ((one.session.state_dump() or {}).get("peers") or {}).get("own") or {}
        return own.get("user") or ""
    return ""


def reads_as_json(value):
    """`value` as the JSON it holds, where the control socket writes it as JSON text."""
    while isinstance(value, str):
        try:
            value = json.loads(value)
        except json.JSONDecodeError:
            return value
    return value


def message_naming(records, words: str, person: str) -> bool:
    """Whether the records hold one message with `words` in it that names `person`: a mention, not
    the letters typed into a line."""
    def walk(value) -> bool:
        if isinstance(value, dict):
            text = value.get("text")
            mentions = value.get("mentions")
            names = isinstance(mentions, list) and any(
                isinstance(mention, dict) and person in json.dumps(mention.get("user"))
                for mention in mentions)
            if isinstance(text, str) and words in text and names:
                return True
            return any(walk(child) for child in value.values())
        if isinstance(value, list):
            return any(walk(child) for child in value)
        return False

    return walk(reads_as_json(records))


def steps(story: Story) -> None:
    ada, grace, lin = story.person("Ada"), story.person("Grace"), story.person("Lin")
    # Both computers have their apps before anyone opens Tilo.
    for one in (ada, grace, lin):
        one.prepare(APPS)

    shared: dict = {}

    def ada_invites():
        began = time.monotonic()
        took = ada.start(apps=APPS)
        story.timing("Ada's session start, to its first frame", took["tookMs"])
        story.timing("Ada's session start, to its first frame, with the development launcher's installing", took["launcherMs"])
        until("Ada's shell to hold a workspace", lambda: front(ada), 60)
        ada.name_self("Ada")
        before = {w.get("id") for w in workspaces(ada)}
        ada.session.invoke(SHELL, f"{CHROME}\u001fnew-workspace", {"name": WORKSPACE})
        made = until("a new workspace on Ada's shell", lambda: next(
            (w for w in workspaces(ada) if w.get("id") not in before), None), 30, 0.2)
        shared["id"] = made["id"]
        stand_in(ada, made["id"])
        until(f"Ada's top bar saying she is in {WORKSPACE}",
              lambda: in_front_named(ada, WORKSPACE), 15, 0.2)
        # A link each, as an invitation admits one person: the Invite sheet the bar opens makes
        # one, then another, and lists both of them as links that still work.
        shared["links"] = [invite_link(ada), invite_link(ada)]
        if shared["links"][0] == shared["links"][1]:
            raise Failed("the two invitations are the same link")

        def still_work():
            """The sheet lists the links that still work, each with its Take back. A control is
            published once by its name, so the sheet is read for the section rather than by
            counting its rows."""
            return ada.target("Take back")

        until("the Invite sheet listing the links that still work", still_work, 20, 0.25)
        story.note("invite-sheet", ada.names())
        # The sheet stays up to be read: it is done with at the person's next step.
        # What a person waited for: the session's own start and what they did, without what the
        # development launcher installs before the session runs.
        return {"tookMs": (time.monotonic() - began) * 1000
                - (took["launcherMs"] - took["tookMs"])}

    story.step("Ada invites Grace and Lin into a workspace", ada_invites, people=[ada])

    def grace_and_lin_join():
        began = time.monotonic()
        installing = 0.0
        for one, invitation in ((grace, shared["links"][0]), (lin, shared["links"][1])):
            path = story.out / f"invitation-{one.key}.txt"
            path.write_text(invitation)
            took = one.start(apps=APPS, env={JOIN_INVITATION: str(path)})
            installing += took["launcherMs"] - took["tookMs"]
            story.timing(f"{one.name}'s session start with the invitation, to its first frame",
                         took["tookMs"])
            story.timing(f"{one.name}'s session start with the invitation, to its first frame, "
                         f"with the development launcher's installing", took["launcherMs"])
        # From here on, what each person waits for is theirs: both shells have started.
        joining = time.monotonic()
        wanted = shared["id"]
        for one in (grace, lin):
            until(f"{one.name}'s shell to hold {WORKSPACE}",
                  lambda one=one: any(w.get("id") == wanted for w in workspaces(one)), 240, 0.5)
            stand_in(one, wanted)
            # A computer new to Tilo knows no name for its person yet: each gives theirs.
            one.name_self(one.name)
            until(f"{one.name}'s top bar saying they are in {WORKSPACE}",
                  lambda one=one: in_front_named(one, WORKSPACE), 30, 0.2)
        together = time.monotonic()
        story.timing("Grace's and Lin's join, until both are in Garden club",
                     (together - joining) * 1000)
        # Each opens an app to work in: the workspace's desktop is the one every member sees, so
        # what each opens is what Ada finds them in.
        grace.session.launch("notes")
        grace.session.answering("notes", deadline=120)
        grace.press("New note", "Notes' New note on Grace's shell", deadline=30)
        lin.session.launch("tasks")
        lin.session.answering("tasks", deadline=120)
        story.note("grace-controls", grace.names())
        story.note("lin-controls", lin.names())
        # What a person waited for: each session's own start, the join, and the app each opened,
        # without what the development launcher installs before a session runs.
        return {"tookMs": (time.monotonic() - began) * 1000 - installing}

    story.step("Grace and Lin join the workspace", grace_and_lin_join)

    def sees_here():
        # The invitation sheet is done with: Escape puts it away, as a person does.
        if ada.target("Take back"):
            ada.session.key("Escape")
            until("the Invite sheet closing", lambda: not ada.target("Take back"), 10, 0.2)
        open_here(ada, "Grace")
        until("Grace and Lin in Ada's people list",
              lambda: ada.target("Follow Grace") and ada.target("Follow Lin"), 20, 0.25)
        workspace = shared["id"]

        def working():
            """What Ada's shell says each of them is working in: the window each has in front, as
            their own device publishes it."""
            _, rows = peers(ada, workspace)
            g = (named(rows, "Grace").get("focus") or {}).get("app")
            l = (named(rows, "Lin").get("focus") or {}).get("app")
            return (g == NOTES and l == TASKS) or None

        try:
            until("Grace in Notes and Lin in Tasks, as Ada's shell says", working, 30, 0.25)
        finally:
            story.note("peers-ada", (ada.session.state_dump() or {}).get("peers"))
            story.note("controls-ada-people", ada.targets())

    story.step("Ada sees who is here and what each is working on", sees_here, people=[ada])

    def follows_grace():
        workspace = shared["id"]
        # The window Grace works in, as her own shell publishes it.
        grace_window = active_window(grace)
        if not grace_window or grace_window.get("app_id") != NOTES:
            raise Failed(f"Grace works in no Notes window: {grace.session.windows()}")
        if not ada.target("Follow Grace") and not ada.target("Stop following Grace"):
            open_here(ada, "Grace")
        ada.press("Follow Grace", "Follow Grace in Ada's people list")
        own_grace, _ = peers(grace, workspace)
        # Ada's shell follows Grace's device: its own row says so, which is what draws the
        # "Following Grace" banner over her desktop.
        until("Ada's shell following Grace", lambda: (
            peers(ada, workspace)[0].get("following") == own_grace.get("publisher")), 15, 0.2)
        until("Ada's bar saying she follows Grace", lambda: ada.shows("Following Grace"), 10, 0.2)

        def brought_forward():
            """The window Grace works in, in front on Ada's screen: the same window of the shared
            workspace, the same record, brought forward."""
            shown = active_window(ada)
            return (shown
                    and shown.get("tab") == grace_window.get("tab")
                    and shown.get("app_id") == NOTES
                    and shown.get("title") == grace_window.get("title"))

        try:
            until("Grace's window brought forward on Ada's screen", brought_forward, 20, 0.25)
        finally:
            story.note("grace-window", grace_window)
            story.note("ada-windows-after-follow", ada.session.windows())

    story.step("Ada follows Grace and sees Grace’s screen live", follows_grace, people=[ada])

    words = f"the seed packets are in the shed {int(time.time()) % 100000:05d}"

    def mentions_lin():
        # The roster is done with: Escape puts it away, as a person does.
        if ada.target("Follow Grace") or ada.target("Stop following Grace"):
            ada.session.key("Escape")
        opened = time.monotonic()
        ada.session.launch("chat")
        ada.session.answering("chat", deadline=120)
        ada.press("Message composer", "Chat's message composer on Ada's shell")
        story.timing("Chat, from the launch to its composer", (time.monotonic() - opened) * 1000)
        # A mention is typed and picked from the list Chat offers under the `@`, as a person
        # writes one: the name, the pick, then the words.
        ada.session.type("@Li")
        ada.session.key("Enter")
        ada.session.type(f" {words}")
        ada.press("Send message", "Chat's Send message on Ada's shell")
        sent = time.monotonic()
        until("Ada's words in the conversation", lambda: ada.shows(words), 15, 0.2)
        until("the mention of Lin in the conversation", lambda: ada.shows("@Lin"), 15, 0.2)
        story.timing("The words, from Send to the conversation showing them",
                     (time.monotonic() - sent) * 1000)
        # The workspace's own chat record keeps the message, naming Lin: a mention, not the
        # letters typed before it.
        _, rows = peers(ada, shared["id"])
        lin_person = person_root(named(rows, "Lin"), lin)
        if not lin_person:
            raise Failed("Ada's people list does not say which person Lin is")

        def naming():
            answer = ada.session.control({
                "op": "resource_read", "app": CHAT, "workspace": shared["id"],
                "records": ["", CHAT_MESSAGES], "wait_ms": 5000})
            return message_naming((answer.get("answer") or {}).get("records"), words, lin_person)

        try:
            until("the workspace's chat holding the message that names Lin", naming, 20, 0.5)
        finally:
            story.note("chat-controls", ada.names())
        story.timing("The message, from Send to the workspace's chat holding it",
                     (time.monotonic() - sent) * 1000)

    story.step("Ada mentions Lin in Chat", mentions_lin, people=[ada])

    def lin_told():
        def notice():
            told = (lin.session.state_dump() or {}).get("notifications") or []
            return next((row for row in told
                         if "mentioned you" in (row.get("title") or "")), None)

        def unread_notice():
            row = notice()
            return row if row and row.get("unread") else None

        unread = until("the mention in Lin's notifications", unread_notice, 60, 0.25)
        story.note("lin-notice", unread)
        # The bell counts what Lin has not seen: pressing it opens the centre, where the notice is.
        bell = until("Lin's bell counting the mention", lambda: next(
            (t for t in lin.targets()
             if (t.get("name") or "").startswith("Notifications")), None), 15, 0.2)
        lin.press(bell["name"], "Lin's bell")
        until("the mention in Lin's notification centre",
              lambda: lin.shows(unread.get("title") or ""), 15, 0.2)
        # Opening the centre is seeing it: nothing of it stays unread.
        until("the notice read", lambda: not (notice() or {}).get("unread"), 10, 0.2)
        lin.press("Open", "the mention's own Open")
        # It opens what it told: the conversation, in front on Lin's screen.
        until("the conversation in front on Lin's screen", lambda: (
            (shown := active_window(lin)) and shown.get("app_id") == CHAT), 30, 0.25)
        story.note("lin-windows-at-open", lin.session.windows())

    story.step("Lin is told of the mention and opens it", lin_told, people=[lin])


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print("usage: invited-activity.py OUT_DIR", file=sys.stderr)
        sys.exit(2)
    sys.exit(walk("invited-activity", Path(sys.argv[1]), steps))
