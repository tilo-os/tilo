#!/usr/bin/env python3
"""Connect a mail account with only an address and a password, in a Tilo shell.

Ada opens Mail in her shell. Mail holds no account, so she opens Settings' Accounts page and adds
an email account there, typing her name, her address and her password and naming no server: the
account's mail is the address's own domain's servers (`imap.<domain>` and `smtp.<domain>` on the
ports mail programs use), which Mail works out for her. The account signs in, and Mail lists its
inbox; she opens the newest message, searches for an older one and answers it.

The account is the tree's stand-in mail server, run by this story alone as the address's
provider (mail_provider.py says how).

What the shell itself makes of the exchange is the only thing a step passes on: the controls each
window draws and their accessible names, the windows' own records, and, for the reply, the
stand-in's own inbox, read back over IMAP. Nothing is taken from what an action answered.

    python3 mail-password.py OUT_DIR
"""

from __future__ import annotations

import json
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from mail_provider import (  # noqa: E402
    OLDER_SUBJECT, OLDER_WORDS, SEARCH_CHORD, TILOLANG, StandIn, account_kept,
    adds_account, describe, has_read, launches_mail, mail_actions, opens_accounts, opens_newest,
    pressed, shows_inbox,
)
from story import Failed, Story, until, walk  # noqa: E402

# What both windows need: Settings, where the person's accounts are kept, and Mail, both
# tilolang apps (TILOLANG), and no native app.
APPS = ""


def steps(story: Story) -> None:
    ada = story.person("Ada")
    stand_in = StandIn(story.out)

    def opens_mail() -> None:
        stand_in.prepare()
        took = ada.start(apps=APPS, tilolang=TILOLANG)
        answered = time.monotonic()
        story.timing("The session, to its first frame", took["tookMs"])
        story.timing("The session, to its first frame, with the development launcher's installing",
                     took["launcherMs"])
        # Mail's window is up, saying to add an account in Settings: a page with no control.
        story.note("mail-asked-for-the-internet", launches_mail(ada))
        settings = opens_accounts(ada)
        story.timing("The shell, to Settings' Accounts page", (time.monotonic() - answered) * 1000)
        adds_account(ada, settings, stand_in.password)
        # What a person waited for is the desktop's own start, and then their own acts: the
        # launcher's installing happens before a person's computer starts Tilo at all.
        return {"tookMs": took["tookMs"] + (time.monotonic() - answered) * 1000}

    try:
        story.step("Ada opens Mail and enters their address and password", opens_mail,
                   people=[ada])

        found: dict = {}

        def finds_servers() -> None:
            account_kept(ada)
            found["servers-named-by-the-person"] = False
            # Mail's own window takes the account it was given, with nothing reloaded.
            shows_inbox(ada)
            story.note("settings-after-account", describe(ada))
            story.note("mail-actions", mail_actions(ada))

        story.step("Mail finds the server by itself and connects", finds_servers, people=[ada])
        story.note("servers-found-by-mail", found)

        def reads_newest() -> None:
            began = time.monotonic()
            opens_newest(ada)
            story.timing("The newest message, from the press to its reader",
                         (time.monotonic() - began) * 1000)

        story.step("Ada reads the newest mail", reads_newest, people=[ada])

        def searches_older() -> None:
            ada.session.chord(*SEARCH_CHORD)
            ada.session.type("quarterly")
            answer = ada.session.wait_search_rows("quarterly", [OLDER_SUBJECT], deadline=30)
            rows = answer.get("rows") or []
            hit = next((row for row in rows if OLDER_SUBJECT in (row.get("title") or "")), None)
            if hit is None:
                raise Failed(f"the shell's search for 'quarterly' found no {OLDER_SUBJECT}: "
                             f"{json.dumps(rows)[:1200]}")
            if OLDER_WORDS not in ((hit.get("subtitle") or "") + " " + " ".join(
                    map(str, hit.get("facts") or []))):
                raise Failed(f"the search's row for {OLDER_SUBJECT} carries not its words: "
                             f"{json.dumps(hit)[:800]}")
            pressed(ada, OLDER_SUBJECT, "the older message's row in the search")
            until("the older message to open", lambda: has_read(ada, OLDER_SUBJECT), 30, 0.2)

        story.step("Ada searches for an older one", searches_older, people=[ada])

        def sends_reply() -> None:
            # The open message is what a reply answers: its own key, as the window offers it.
            ada.session.key("r")
            until("the composer's Send", lambda: ada.shows("Send"), 30, 0.2)
            token = f"k{os.urandom(4).hex()}"
            words = f"Thanks, see you Monday {token}"
            # A reply opens with the caret in its body, where a person writing one finds it.
            ada.session.type(words)
            pressed(ada, "Send", "the composer's Send")
            # It arrives: the account's server holds it, and Mail shows it once it asks the
            # server again. The server's own inbox says, never what Mail said.
            until("the reply in the account's server", lambda: stand_in.received(words, token)
                  or None, 60, 0.5)
            ada.session.key("F5")
            row = until("the reply in Mail's list", lambda: ada.target(f"Re: {OLDER_SUBJECT}",
                                                                        exact=False), 30, 0.3)
            pressed(ada, row["name"], "the reply's row")
            until("the reply to open", lambda: has_read(ada, f"Re: {OLDER_SUBJECT}"), 30, 0.2)

        story.step("Ada sends a reply, and it arrives", sends_reply, people=[ada])
    finally:
        stand_in.stop()


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: mail-password.py OUT_DIR", file=sys.stderr)
        return 2
    return walk("mail-password", Path(sys.argv[1]), steps)


if __name__ == "__main__":
    sys.exit(main())
