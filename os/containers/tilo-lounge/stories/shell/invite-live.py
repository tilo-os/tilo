#!/usr/bin/env python3
"""Make a workspace, invite someone, and work together live, in two Tilo shells.

Ada's shell makes a new workspace, "Garden club", and an invitation into it, the way a person
does: the shell's own actions and controls. Grace's Tilo opens the invitation on a computer that
has never seen Tilo, and she joins. Each sees the other in their people list. Grace follows Ada
from Ada's card, and Ada's shell says Grace follows her. Ada opens Notes and starts a note, and
Grace's followed view shows it; what Ada types reaches Grace.

Every step's pass or fail is read off the shells' own state: the state dump (workspaces, who is
here and whom they follow, windows) and the controls each shell shows, by their accessible names.

    python3 invite-live.py OUT_DIR
"""

from __future__ import annotations

import json
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from story import Failed, Person, Story, until, walk  # noqa: E402

WORKSPACE = "Garden club"
SHELL = "so.tilo.session"
CHROME = "desktop:chrome"
INVITATIONS = "session:invite"
JOIN_INVITATION = "TILO_SHELL_JOIN_INVITATION_FILE"
NOTES = "so.tilo.notes"
# How the top bar's workspace control is named, before the workspace in front.
SWITCHER = "Switch workspace, now in "
# How a person's own face in their top bar is named, after their name.
PROFILE = " (you): your profile"
# What both computers have: Notes, where Ada writes, and what a person's desktop has besides.
APPS = "notes file-explorer chat settings"


def tree(one: Person) -> dict:
    return (one.session.state_dump() or {}).get("workspace_tree") or {}


def workspaces(one: Person) -> list[dict]:
    return tree(one).get("workspaces") or []


def front(one: Person) -> dict | None:
    """The workspace in front on this person's shell."""
    held = workspaces(one)
    active = tree(one).get("active")
    return next((w for w in held if w.get("id") == active), held[0] if held else None)


def in_front_named(one: Person, name: str) -> bool:
    """Whether this person's shell says the workspace in front is `name`: its top bar's workspace
    control is named after it (the state dump's tree carries no names)."""
    return one.shows(f"{SWITCHER}{name}")


def peers(one: Person, workspace: str) -> tuple[dict, list[dict]]:
    """This person's own row, and everyone else here in `workspace`, as their shell's people
    list draws them."""
    seen = (one.session.state_dump() or {}).get("peers") or {}
    own = seen.get("own") or {}
    for live in seen.get("workspaces") or []:
        if live.get("workspace") == workspace:
            members = [m for m in live.get("members") or []
                       if m.get("publisher") != own.get("publisher")]
            return own, members
    return own, []


def actions_matching(one: Person, query: str) -> list[dict]:
    """The actions this shell lists for `query`, each by provider, address and name."""
    return [{k: row.get(k) for k in ("provider", "id", "name", "title", "args", "contract")
             if row.get(k) is not None} for row in one.session.actions(query)]


def stand_in(one: Person, workspace: str) -> None:
    current = front(one)
    if current is None or current.get("id") != workspace:
        one.session.invoke(SHELL, f"{CHROME}\u001fswitch-workspace", {"workspace": workspace})
    until(f"{one.name}'s shell to stand in the workspace",
          lambda: (front(one) or {}).get("id") == workspace, 60, 0.25)


def holds(one: Person, text: str) -> bool:
    """Whether `one`'s computer keeps `text` in a Notes record: the bytes on its disk."""
    return any(text.encode() in path.read_bytes()
               for path in one.state.rglob("*.automerge") if NOTES in str(path))


def notes_listed(one: Person) -> int:
    """How many notes this person's Notes window lists: rows inside the window named as notes
    are, besides its New note buttons (each a fresh note is named "New note" until it has a
    title, so the rows are counted by where they stand: the window's list column)."""
    window = next((w for w in one.session.windows() if w.get("app_id") == NOTES), None)
    if window is None:
        return 0
    left, top, _, _ = window["rect"]
    # The list column holds the rows; the sidebar's own New note is the square button at its top.
    return sum(1 for t in one.targets()
               if t.get("name") == "New note" and t.get("rect")
               and t["rect"][2] > 120 and t["rect"][0] < left + 300)


def name_self(one: Person, name: str) -> None:
    """Give `one` the name `name` as a person does: their own face in the top bar opens their
    profile card, whose field asks for the name. Read back off the top bar and the identity record
    the card keeps it in."""
    one.press(PROFILE, f"{one.name}'s own face in their top bar", exact=False)
    field = until(f"the name field on {one.name}'s profile card", lambda: next(
        (label for label in ("Add your name", "Your name") if one.target(label)), None), 15, 0.2)
    # A person types once the caret is in the field: a shell just started may still be showing
    # its first hint, which takes the first press.
    def caret_in_field():
        one.press(field)
        return until(f"the caret in {one.name}'s name field", lambda: (
            ((one.session.state_dump() or {}).get("chrome") or {}).get("caret")), 3, 0.1)

    for attempt in range(3):
        try:
            caret_in_field()
            break
        except Failed:
            if attempt == 2:
                raise
    one.session.chord("Control", "a")
    one.session.type(name)
    one.session.key("Enter")
    until(f"{one.name}'s top bar naming them", lambda: one.shows(f"{name}{PROFILE}"), 15, 0.2)
    record = one.state / "config/tilo/identity/primary.json"
    kept = json.loads(record.read_text()).get("display_name") if record.is_file() else None
    if kept != name:
        raise Failed(f"{one.name}'s identity keeps the name {kept!r}, not {name!r}")
    one.session.key("Escape")


def steps(story: Story) -> None:
    ada, grace = story.person("Ada"), story.person("Grace")
    # Both computers have their apps before anyone opens Tilo.
    ada.prepare(APPS)
    grace.prepare(APPS)

    def ada_opens():
        took = ada.start(apps=APPS)
        story.timing("Ada's session start, to its first frame", took["tookMs"])
        story.timing("Ada's session start, to its first frame, with the development launcher's installing", took["launcherMs"])
        after = time.monotonic()
        until("Ada's shell to hold a workspace", lambda: front(ada), 60)
        return {"tookMs": took["tookMs"] + (time.monotonic() - after) * 1000}

    story.step("Ada opens Tilo", ada_opens, people=[ada])

    def ada_again():
        ada.stop()
        took = ada.start(apps=APPS, fresh=False)
        story.timing("Ada's session start again, to its first frame", took["tookMs"])
        story.timing("Ada's session start again, to its first frame, with the development launcher's installing", took["launcherMs"])
        after = time.monotonic()
        until("Ada's shell to hold a workspace again", lambda: front(ada), 60)
        return {"tookMs": took["tookMs"] + (time.monotonic() - after) * 1000}

    story.step("Ada opens Tilo again", ada_again, people=[ada])

    story.step("Ada gives their name", lambda: name_self(ada, "Ada"), people=[ada])

    shared = {}

    def new_workspace():
        story.note("actions-workspace", actions_matching(ada, "workspace"))
        before = {w.get("id") for w in workspaces(ada)}
        ada.session.invoke(SHELL, f"{CHROME}\u001fnew-workspace", {"name": WORKSPACE})
        made = until("a new workspace on Ada's shell", lambda: next(
            (w for w in workspaces(ada) if w.get("id") not in before), None), 30, 0.2)
        shared["id"] = made["id"]
        stand_in(ada, made["id"])
        until(f"Ada's top bar saying she is in {WORKSPACE}",
              lambda: in_front_named(ada, WORKSPACE), 15, 0.2)

    story.step(f"Ada makes a new workspace, “{WORKSPACE}”", new_workspace, people=[ada])

    link = {}

    def invitation():
        story.note("actions-invite", actions_matching(ada, "invit"))
        answer = ada.session.invoke(SHELL, f"{INVITATIONS}\u001fissue-invitation",
                                    {"role": "editor", "expires_in_seconds": 3600})
        issued = (answer.get("receipt") or {}).get("update", {}).get("output", {}).get("value") or {}
        if not issued.get("link"):
            raise Failed(f"the shell made no invitation: {json.dumps(answer)[:1500]}")
        link["text"] = issued["link"]

    story.step("Ada makes an invitation link", invitation, people=[ada])

    def grace_opens():
        path = story.out / "invitation.txt"
        path.write_text(link["text"])
        took = grace.start(apps=APPS, env={JOIN_INVITATION: str(path)})
        story.timing("Grace's session start with the invitation, to its first frame", took["tookMs"])
        story.timing("Grace's session start with the invitation, to its first frame, with the development launcher's installing", took["launcherMs"])
        return {"tookMs": took["tookMs"]}

    story.step("Grace opens the invitation on her computer", grace_opens)

    def grace_joins():
        began = time.monotonic()
        wanted = shared["id"]
        until("Grace's shell to hold Garden club",
              lambda: any(w.get("id") == wanted for w in workspaces(grace)), 240, 0.5)
        stand_in(grace, wanted)
        until(f"Grace's top bar saying she is in {WORKSPACE}",
              lambda: in_front_named(grace, WORKSPACE), 30, 0.2)
        story.timing("Grace's join, to standing in Garden club", (time.monotonic() - began) * 1000)
        # A computer new to Tilo knows no name for its person yet: Grace gives hers, once her
        # shell has settled in the workspace (switching closes what the top bar had open).
        name_self(grace, "Grace")

    story.step(f"Grace joins {WORKSPACE}", grace_joins)

    def see_each_other():
        workspace = shared["id"]

        def both():
            _, ada_sees = peers(ada, workspace)
            _, grace_sees = peers(grace, workspace)
            return (ada_sees and grace_sees) and (ada_sees, grace_sees)

        try:
            ada_sees, grace_sees = until("Ada and Grace in each other's people lists", both, 60, 0.1)
        finally:
            story.note("peers-ada", (ada.session.state_dump() or {}).get("peers"))
            story.note("peers-grace", (grace.session.state_dump() or {}).get("peers"))
            story.note("controls-ada-people", ada.names())
            story.note("controls-grace-people", grace.names())
        if not any("Ada" in (row.get("label") or "") for row in grace_sees):
            raise Failed(f"Grace's people list does not name Ada: {[r.get('label') for r in grace_sees]}")
        if not any("Grace" in (row.get("label") or "") for row in ada_sees):
            raise Failed(f"Ada's people list does not name Grace: {[r.get('label') for r in ada_sees]}")

    story.step("Ada and Grace see each other in their people lists", see_each_other)

    def grace_follows():
        workspace = shared["id"]
        before = grace.names()
        story.note("controls-grace-before-follow", grace.targets())
        # Ada's face in Grace's top bar opens Ada's card, where Follow is; where the bar has no
        # face of her own, Everyone here opens the people panel, whose row says Follow Ada.
        face = next((t for t in grace.targets() if (t.get("name") or "").startswith("Ada")
                     and PROFILE not in t["name"]), None)
        grace.press(face["name"] if face else "Everyone here", "Ada's face in Grace's top bar")
        follow = until("Follow on Grace's shell", lambda: next(
            (name for name in ("Follow Ada", "Follow") if grace.target(name)), None), 15, 0.2)
        story.note("controls-grace-card", grace.targets())
        grace.press(follow)
        own, _ = peers(ada, workspace)
        # Grace's shell follows Ada's device: its own row says so, which is what draws the
        # "Following Ada" banner over her desktop.
        until("Grace's shell following Ada", lambda: (peers(grace, workspace)[0].get("following")
                                                      == own.get("publisher")), 10, 0.2)

        def ada_told():
            _, rows = peers(ada, workspace)
            return any(row.get("following") == own.get("publisher") for row in rows)

        # Ada is told without looking: her people list says Grace follows her, which is what puts
        # the eye on Grace's face in her top bar.
        until("Ada's people list saying Grace follows her", ada_told, 10, 0.2)
        story.note("controls-ada-followed", ada.targets())
        story.note("before", before)

    story.step("Grace follows Ada", grace_follows)

    words = f"Seeds for spring {int(time.time()) % 100000:05d}"
    typed = {}

    def ada_starts_note():
        ada.session.launch("notes")
        ada.session.answering("notes", deadline=120)
        ada.press("New note", "Notes' New note on Ada's shell", deadline=30)
        # The press makes a note, which Notes opens: its title and its first block.
        try:
            until("the note Ada's press of New note makes, open in her Notes",
                  lambda: ada.target("Block text"), 10, 0.2)
        except Failed:
            raise Failed("Ada pressed New note and Notes made no note: it still shows "
                         f"{[n for n in ada.names() if n in ('New note', 'Start your first note')]}")
        typed["notes"] = notes_listed(ada)
        # Following Ada, Grace is shown what Ada works in: Notes, as her people list says Ada is.
        workspace = shared["id"]
        own, _ = peers(ada, workspace)

        def grace_shown():
            _, rows = peers(grace, workspace)
            ada_row = next((r for r in rows if r.get("publisher") == own.get("publisher")), {})
            focus = (ada_row.get("focus") or {}).get("app")
            on_screen = any(w.get("app_id") == NOTES for w in grace.session.windows())
            return focus == NOTES and on_screen

        try:
            until("Ada's Notes on Grace's screen", grace_shown, 15, 0.2)
        finally:
            story.note("grace-windows-at-note", grace.session.windows())
            story.note("grace-peers-at-note", (grace.session.state_dump() or {}).get("peers"))
            story.note("grace-controls-at-note", grace.names())

    story.step("Ada opens Notes and starts a note", ada_starts_note)


    def ada_types():
        # She clicks into the note's body, where it asks her to write, and types.
        ada.press("Block text", "the body of Ada's new note")
        for ch in words:
            ada.session.type(ch)
            time.sleep(0.08)
        typed["last"] = time.monotonic()
        try:
            until("Ada's words in Ada's note", lambda: holds(ada, words), 15, 0.1)
        except Failed:
            made = notes_listed(ada) - typed["notes"]
            story.note("ada-controls-after-typing", ada.targets())
            if made > 0:
                raise Failed(f"Ada's keys never reached her note, and her typing made {made} more "
                             "notes instead") from None
            raise

    story.step("Ada types a line in the note", ada_types, people=[ada])

    def grace_sees():
        until("Ada's words on Grace's computer", lambda: holds(grace, words), 30, 0.05)
        took = (time.monotonic() - typed["last"]) * 1000
        story.timing("Ada's words on Grace's computer, after Ada's last key", took)
        try:
            until("Ada's words on Grace's screen", lambda: grace.shows(words), 10, 0.1)
        finally:
            story.note("grace-controls-at-words", grace.names())
        return {"tookMs": took}

    story.step("Grace sees Ada’s words", grace_sees)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print("usage: invite-live.py OUT_DIR", file=sys.stderr)
        sys.exit(2)
    sys.exit(walk("invite-live", Path(sys.argv[1]), steps))
