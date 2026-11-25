#!/usr/bin/env python3
"""Two people of one workspace read one mail account, through the workspace's one vault.

Ada makes a workspace, "The lab", and invites Grace, who joins on a computer of her own (kept in
the light look, so the story is seen in both). Ada adds the lab's mail account in Settings'
Accounts page, standing in the lab: her name, the address and its password. The password is kept
in the workspace's vault, Passwords' vault document, and the account names that entry as where its
sign-in is. Ada's Mail lists the account's inbox and she opens the newest message. Then Grace
opens Mail on her own computer and, typing no password, reads the same inbox: her Mail asks her
session for the password by the account's vault entry, and her session releases it from her own
copy of the vault document, as her grants on Passwords let it.

The account's provider is the tree's stand-in mail server (mail_provider.py), which says each IMAP
sign-in on its log with the process that made the connection: the story reads off the server that
a process of Ada's session signed in, and then one of Grace's, as the same login, with the one
password the stand-in takes. Last, each person's computer is searched for the password: no file of
either holds it in the clear, and neither keeps the per-device stores a password used to be copied
into (its accounts store, its device secret vault, and the account-credentials hand-over).

    python3 shared-mailbox.py OUT_DIR
"""

from __future__ import annotations

import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from mail_provider import (  # noqa: E402
    ADDRESS, PASSWORD_FIELD, SEARCH_CHORD, TILOLANG, StandIn, account_kept, adds_account,
    opens_accounts, launches_mail, opens_newest, press_in, pressed, shows_inbox, window_named,
)
from story import (  # noqa: E402
    CHROME, SHELL, Failed, Person, Story, front, in_front_named, invite_link, until, walk,
    workspaces,
)
from story import stand_in as stands_in  # noqa: E402
from real_apps import record as read_record  # noqa: E402

WORKSPACE = "The lab"
JOIN_INVITATION = "TILO_SHELL_JOIN_INVITATION_FILE"
# What both computers have: Settings, where the workspace's accounts are kept, and Mail, both
# tilolang apps (TILOLANG), and no native app.
APPS = ""
# Settings' page with the look, as the shell's search names it, and the light look on it.
APPEARANCE_PAGE = "Appearance"
LIGHT = "Light"
MODE = "Mode"
# The workspace's accounts record, which Settings keeps and Mail reads: by its document, as the
# shell reads a record.
ACCOUNTS_OWNER = "so.tilo.settings"
ACCOUNTS_RECORD = "derived:tilo.accounts@tab:0000000000000000"
# Where a device kept an account's password before the workspace's vault held it, under the
# device's config root (`$XDG_CONFIG_HOME/tilo`): the accounts store, the device's own secret
# vault, and the account-credentials hand-over to the storage daemon (crates/integrations/accounts).
ACCOUNTS_STORE = "accounts/store.json"
DEVICE_VAULT = "accounts/vault"
HANDOFF = "accounts/handoff"
# A file larger than this is an app's or runtime's installed bundle, never what a device kept.
LARGEST_KEPT = 64 * 1024 * 1024


def without_installing(took: dict) -> float:
    """What the development launcher spent installing before a session ran, which no person's
    computer does: taken out of what a step says a person waited."""
    return max(0.0, took["launcherMs"] - took["tookMs"])


def keeps_light(one: Person) -> None:
    """Pick the light look in Settings' Appearance page, reached through the shell's search as a
    person reaches any page."""
    one.session.chord(*SEARCH_CHORD)
    one.session.type("appearance")
    one.session.wait_search_rows("appearance", [APPEARANCE_PAGE], 30)
    pressed(one, APPEARANCE_PAGE, f"the {APPEARANCE_PAGE} page in the shell's search")
    settings = until("Settings, where the look is picked",
                     lambda: window_named(one, "so.tilo.settings"), 60, 0.25)
    # The modes stand side by side in the Mode setting: the person presses Light among them.
    until(f"the {LIGHT} look in the {MODE} setting", lambda: one.target(LIGHT), 30, 0.2)
    press_in(one, LIGHT, settings, f"the {LIGHT} look")


def whose(sign_in: dict, people: list[Person]) -> str | None:
    """The person whose session made the connection a sign-in came on: one of the processes the
    stand-in names is that person's session."""
    for one in people:
        proc = one.session.proc
        if proc is not None and proc.pid in sign_in["lineage"]:
            return one.name
    return None


def holding(root: Path, secret: bytes) -> list[str]:
    """Every file under `root` holding `secret` as it stands, by its path below `root`."""
    found = []
    for path in sorted(root.rglob("*")):
        try:
            if path.is_symlink() or not path.is_file() or path.stat().st_size > LARGEST_KEPT:
                continue
            if secret in path.read_bytes():
                found.append(str(path.relative_to(root)))
        except OSError:
            continue
    return found


def found_under(root: Path, pattern: str) -> list[str]:
    """Every file under `root` at a path ending in `pattern`, by its path below `root`."""
    return sorted(str(path.relative_to(root)) for path in root.rglob(pattern)
                  if path.is_file() and not path.is_symlink())


def steps(story: Story) -> None:
    ada, grace = story.person("Ada"), story.person("Grace")
    people = [ada, grace]
    provider = StandIn(story.out)
    shared: dict = {}

    def sign_ins_by() -> list[dict]:
        return [{**said, "person": whose(said, people)} for said in provider.sign_ins()]

    def signed_in_as(name: str, after: int = 0) -> list[dict]:
        return [said for said in sign_ins_by()[after:]
                if said["taken"] and said["person"] == name]

    def makes_and_invites():
        # The provider is the world around them, there before either opens Tilo.
        provider.prepare()
        ada.prepare(APPS, TILOLANG)
        grace.prepare(APPS, TILOLANG)
        began = time.monotonic()
        took = ada.start(apps=APPS, tilolang=TILOLANG)
        installing = without_installing(took)
        story.timing("Ada's session start, to its first frame", took["tookMs"])
        until("Ada's shell to hold a workspace", lambda: front(ada), 60)
        ada.name_self("Ada")
        before = {w.get("id") for w in workspaces(ada)}
        ada.session.invoke(SHELL, CHROME, "new-workspace", {"name": WORKSPACE})
        made = until("a new workspace on Ada's shell", lambda: next(
            (w for w in workspaces(ada) if w.get("id") not in before), None), 30, 0.2)
        shared["id"] = made["id"]
        stands_in(ada, made["id"])
        until(f"Ada's top bar saying she is in {WORKSPACE}",
              lambda: in_front_named(ada, WORKSPACE), 15, 0.2)
        link = invite_link(ada)
        if not link.startswith("tilo://invite#1."):
            raise Failed(f"Ada copied no native invitation: {link[:80]!r}")
        # Ada sends Grace the link (how is no part of Tilo), and closes her sheet.
        ada.session.key("Escape")
        until("Ada's invite sheet to close", lambda: not ada.target("Copy link"), 10, 0.2)
        invitation = story.out / "invitation.txt"
        invitation.write_text(link)
        took = grace.start(apps=APPS, tilolang=TILOLANG, env={JOIN_INVITATION: str(invitation)})
        installing += without_installing(took)
        story.timing("Grace's session start with the invitation, to its first frame",
                     took["tookMs"])
        joined = time.monotonic()
        until(f"Grace's shell to hold {WORKSPACE}",
              lambda: any(w.get("id") == shared["id"] for w in workspaces(grace)), 240, 0.5)
        stands_in(grace, shared["id"])
        until(f"Grace's top bar saying she is in {WORKSPACE}",
              lambda: in_front_named(grace, WORKSPACE), 30, 0.2)
        story.timing(f"Grace's join, to {WORKSPACE} in front", (time.monotonic() - joined) * 1000)
        grace.name_self("Grace")
        keeps_light(grace)
        story.shot(grace, "in the light look")
        # What the people waited for is the step without the development launcher's installing,
        # which happens before a person's computer starts Tilo at all.
        return {"tookMs": (time.monotonic() - began) * 1000 - installing}

    def adds_shared_account():
        before = len(provider.sign_ins())
        settings = opens_accounts(ada)
        adds_account(ada, settings, provider.password)
        account_kept(ada)
        # The account was kept only after its sign-in was checked: the stand-in says so, and
        # that it was Ada's session that signed in.
        try:
            until("the stand-in to say Ada's session signed in",
                  lambda: signed_in_as("Ada", before), 30, 0.25)
        finally:
            story.note("sign-ins-after-ada-adds", sign_ins_by())

    def accounts_seen(one: Person) -> None:
        """What a shell reads of the workspace's accounts, and what its Mail shows of them, kept
        beside the story when Mail shows none."""
        try:
            shown = front(one) or {}
            record = read_record(one.session.control, shown.get("id", ""), ACCOUNTS_OWNER,
                                 ACCOUNTS_RECORD, wait_ms=2000)
        except Exception as error:  # noqa: BLE001 - the refusal is the evidence
            record = {"refused": str(error)}
        story.note(f"{one.key}-accounts-record", record)
        story.note(f"{one.key}-mail-controls", [n for n in one.names() if n])

    def ada_reads():
        story.note("ada-mail-asked-for-the-internet", launches_mail(ada))
        try:
            shows_inbox(ada)
        except Failed:
            accounts_seen(ada)
            raise
        if not ada.shows(ADDRESS):
            raise Failed(f"Ada's Mail does not name the account {ADDRESS}")
        opens_newest(ada)
        shared["ada-sign-ins"] = len(provider.sign_ins())

    def grace_reads():
        before = shared["ada-sign-ins"]
        story.note("grace-mail-asked-for-the-internet", launches_mail(grace))
        try:
            try:
                shows_inbox(grace)
            except Failed:
                accounts_seen(grace)
                raise
            if not grace.shows(ADDRESS):
                raise Failed(f"Grace's Mail does not name the account {ADDRESS}")
            # Grace typed nothing, and nothing asked her to: no password field is on her shell.
            if grace.target(PASSWORD_FIELD):
                raise Failed("Grace's shell asks her for the account's password")
            graces = until("the stand-in to say Grace's session signed in",
                           lambda: signed_in_as("Grace", before), 30, 0.25)
        finally:
            story.note("sign-ins-after-grace-reads", sign_ins_by())
        adas = signed_in_as("Ada")
        logins = {said["login"] for said in adas + graces}
        if not adas or logins != {ADDRESS}:
            raise Failed(f"the stand-in's sign-ins are not one login from both sessions: Ada's "
                         f"{[s['login'] for s in adas]}, Grace's {[s['login'] for s in graces]}")
        story.note("sessions-signed-in", {
            "Ada": len(adas), "Grace": len(graces), "login": ADDRESS,
            "taken": "every one: the stand-in takes only the password the vault entry holds"})

    def nothing_copied():
        secret = provider.password.encode()
        seen = {}
        for one in people:
            # Every config root on the computer: the session's own and each app sandbox's.
            # An accounts store names the account and its vault entry; it fails the step only by
            # holding the password, which the search for it in the clear covers.
            seen[one.name] = {
                "holding the password": holding(one.state, secret),
                "accounts stores": found_under(one.state, ACCOUNTS_STORE),
                "device secret vaults": found_under(one.state, f"{DEVICE_VAULT}/*"),
                "account hand-overs": found_under(one.state, f"{HANDOFF}/*"),
            }
        story.note("per-device-copies", seen)
        for name, kept in seen.items():
            if kept["holding the password"]:
                raise Failed(f"{name}'s computer holds the password in the clear in "
                             f"{kept['holding the password']}")
            if kept["device secret vaults"]:
                raise Failed(f"{name}'s computer keeps a secret vault of its own: "
                             f"{kept['device secret vaults']}")
            if kept["account hand-overs"]:
                raise Failed(f"{name}'s computer keeps an account's credential handed over: "
                             f"{kept['account hand-overs']}")

    try:
        story.step("Ada makes a workspace and invites Grace, and Grace joins", makes_and_invites)
        story.step("Ada adds the lab's mail account in Settings", adds_shared_account,
                   people=[ada])
        story.step("Ada's Mail shows the lab's inbox, and she opens the newest message",
                   ada_reads, people=[ada])
        story.step("Grace opens Mail and reads the same inbox, typing no password", grace_reads)
        story.step("Neither computer keeps a copy of the password", nothing_copied)
    finally:
        provider.stop()


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: shared-mailbox.py OUT_DIR", file=sys.stderr)
        return 2
    return walk("shared-mailbox", Path(sys.argv[1]), steps)


if __name__ == "__main__":
    sys.exit(main())
