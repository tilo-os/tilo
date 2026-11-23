#!/usr/bin/env python3
"""Connect a calendar, create an event, and invite someone who receives it.

Ada adds her Nextcloud account in the shell's Settings, approves the app password
on the server's own sign-in page (her browser), and Calendar picks up the calendar
she keeps there. She makes an event, invites Grace by her address, and Grace's
calendar on the server receives the invitation.

The calendar is a real Nextcloud's CalDAV, the one the check's server starts
(crates/integrations/cloud-source-dav/tests/servers.sh), and Grace is a second person on
it, with a calendar of her own. Grace's receipt is looked for where an invitation
could arrive: her calendar on that server. Every step is decided off the shell
itself (the controls it draws, the event it shows) or off the server.

    python3 calendar-invite.py OUT_DIR
"""

from __future__ import annotations

import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import nextcloud  # noqa: E402
from story import Failed, Story, until, walk  # noqa: E402

# What Ada's computer has: Settings to connect the account with, Calendar, and
# the browser the sign-in opens the server's page in.
APPS = "settings calendar browser"
# The event Ada makes, and the person she invites to it.
EVENT = f"Planning {int(time.time()) % 100000:05d}"


def steps(story: Story) -> None:
    server = (os.environ.get("TILO_TEST_NEXTCLOUD_URL") or "").rstrip("/")
    user = os.environ.get("TILO_TEST_NEXTCLOUD_USER") or ""
    password = os.environ.get("TILO_TEST_NEXTCLOUD_PASSWORD") or ""
    caldav = (os.environ.get("TILO_TEST_NEXTCLOUD_CALDAV") or "").rstrip("/")
    grace = os.environ.get("TILO_TEST_NEXTCLOUD_GRACE_EMAIL") or "grace@example.test"
    grace_user = os.environ.get("TILO_TEST_NEXTCLOUD_GRACE_USER") or "grace"
    if not (server and caldav and user and password):
        raise Failed("this story meets a real Nextcloud, and no seeded one is named: "
                     "TILO_TEST_NEXTCLOUD_URL, _USER, _PASSWORD and _CALDAV "
                     "(servers.sh start and seed); it runs as the check's user-stories-shell "
                     "story, whose servers come from stories.json's `servers`")

    ada = story.person("Ada")
    ada.prepare(APPS)

    def connects():
        took = ada.start(apps=APPS)
        story.timing("Ada's session start, to its first frame", took["tookMs"])
        story.timing("Ada's session start, to its first frame, "
                     "with the development launcher's installing", took["launcherMs"])
        nextcloud.connect_account(story, ada, server, user, password, calendar=True)

    story.step("Ada connects their calendar", connects, people=[ada])

    def creates():
        ada.session.launch("calendar")
        ada.session.answering("calendar", deadline=180)
        # The calendar the server seeded is hers to write in; Calendar lists it
        # among its calendars, and New event goes to the one in front.
        until("Calendar's own controls", lambda: ada.target("New event"), 30, 0.25)
        ada.press("New event", "Calendar's New event")
        # A new event opens on its title, which the person fills in.
        ada.press("Edit event title", "the event's title", deadline=20)
        ada.session.type(EVENT)
        until("the event named in Calendar", lambda: ada.shows(EVENT), 20, 0.25)

    story.step("Ada creates an event", creates, people=[ada])

    def invites():
        # The guest field is on the event's sheet, which the new event opened.
        ada.press("Invite attendee by email", "the event's guest field", deadline=20)
        ada.session.type(grace)
        ada.session.key("Enter")
        until("Grace among the event's guests", lambda: ada.shows(grace), 20, 0.25)

    story.step("Ada invites Grace to it", invites, people=[ada])

    def receives():
        # Where an invitation could reach Grace: her own calendar on the server,
        # put there by the server's scheduling. Look for it the way a person's
        # own client would see it arrive.
        deadline = time.monotonic() + 30

        def delivered():
            held = nextcloud.calendar_entries(caldav, grace_user, password)
            return next((text for text in held if EVENT in text), None)

        while time.monotonic() < deadline:
            received = delivered()
            if received:
                break
            time.sleep(2)
        else:
            held = nextcloud.calendar_entries(caldav, grace_user, password)
            story.note("grace-calendar", held)
            raise Failed(
                f"Grace received nothing: her calendar on the server holds no event named "
                f"“{EVENT}” 30 s after Ada added her as a guest, and nothing was sent to "
                f"{grace}. It holds {len(held)} event(s)")
        if grace not in received:
            raise Failed(f"Grace's calendar holds the event, but not as an invitation to "
                         f"{grace}: {received[:600]}")

    story.step("Grace receives the invitation", receives, people=[ada])


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: calendar-invite.py OUT_DIR", file=sys.stderr)
        return 2
    return walk("calendar-invite", Path(sys.argv[1]), steps)


if __name__ == "__main__":
    sys.exit(main())
