#!/usr/bin/env python3
"""Connect a mail account with only an address and a password, in a Tilo shell.

Ada opens Mail in her shell. Mail holds no account, so she opens Settings' Accounts page and adds
an email account there, typing her name, her address and her password and naming no server: the
account's mail is the address's own domain's servers (`imap.<domain>` and `smtp.<domain>` on the
ports mail programs use), which Mail works out for her. The account signs in, and Mail lists its
inbox; she opens the newest message, searches for an older one and answers it.

The account is the tree's stand-in mail server (`crates/platform/account-probe/examples/
mail_stand_in.rs`), the same program a tilo.sh stack runs for a stack's mail account
(`stack.sh mail-account`): an IMAP and an SMTP server over real sockets that take one made-up
login, hold a few made-up messages and keep the mail they are handed. Here it is started by this
story alone, with no stack around it, answering the address's own server names on this machine
(the names are given in `/etc/hosts`) and with a certificate this machine trusts (it is added to
the system's CA bundle for the story and taken out again after).

What the shell itself makes of the exchange is the only thing a step passes on: the controls each
window draws and their accessible names, the windows' own records, and, for the reply, the
stand-in's own inbox, read back over IMAP. Nothing is taken from what an action answered.

The account's sign-in is checked before it is kept. Settings' sandbox has no network of its own
(`apps/settings/tilo-app.toml` declares `[sandbox.net] mode = "off"`), so the session makes that
connection for it, on the `accounts` service the launcher hands Settings alone: the story's
provider is reached by the session, at the names the account gives.

    python3 mail-password.py OUT_DIR
"""

from __future__ import annotations

import contextlib
import imaplib
import json
import os
import signal
import ssl
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from story import (  # noqa: E402
    Failed, Person, Story, can_act_as_machine, machine, until, walk,
)

# What both windows need: Mail, and Settings, where the person's accounts are kept.
APPS = "settings mail"
# The provider this story rehearses: the address's own domain and the server names Mail works out
# from it, answered on this machine's loopback for the length of the story.
DOMAIN = "story.tilo.test"
ADDRESS = f"lab@{DOMAIN}"
DISPLAY_NAME = "Ada"
# The ports a mail program asks a domain's own IMAP and SMTP servers on, with TLS from the first
# byte; the account Mail prepares keeps both.
IMAP_PORT = 993
SMTP_PORT = 465
PROVIDER_NAMES = [DOMAIN, f"imap.{DOMAIN}", f"smtp.{DOMAIN}"]
# What the stand-in's inbox holds, oldest first: the newest message Ada opens (what its words
# are) and the older one she searches for.
NEWEST_SUBJECT = "Welcome to the lab"
OLDER_SUBJECT = "Quarterly plan"
OLDER_WORDS = "The draft is in the shared folder."
# The chord whose palette is the shell's own search (Settings' shortcuts name it: Super+space).
SEARCH_CHORD = ("Super", "Space")
# Settings' page holding the person's accounts, as the shell's search names it, and the provider
# row that adds a mail account by its address.
ACCOUNTS_PAGE = "Accounts"
ADD_EMAIL = "Add Email"
# Mail's record of what its mailbox keeps of each message: the read flags opening one changes.
MESSAGES_RECORD = "mail.messages"


def describe(one: Person) -> str:
    """What a shell shows now, by the names of its controls: what a failure says happened."""
    try:
        names = [name for name in one.names() if name]
    except Exception as error:  # noqa: BLE001 - a lost shell says why in its log
        return f"(its controls are unreadable: {error})"
    return json.dumps(names[:100], ensure_ascii=False)


def pressed(one: Person, name: str, what: str | None = None, *, exact: bool = True,
            deadline: float = 30.0) -> dict:
    """Press the control `name` where the shell draws it."""
    return one.press(name, what or f"the control {name!r}", deadline=deadline, exact=exact)


def window_named(one: Person, app_id: str) -> dict | None:
    """The placed window of `app_id` on this shell, as the shell draws it, or None."""
    return next((window for window in one.session.windows()
                 if window.get("app_id") == app_id and window.get("rect")), None)


def inside(rect: list, window: dict, margin: float = 24.0) -> bool:
    """Whether the control at `rect` stands where a person can reach it inside `window`."""
    if not rect or not window.get("rect"):
        return False
    x, y, width, height = rect
    left, top, wide, high = window["rect"]
    cx, cy = x + width / 2, y + height / 2
    return (left <= cx <= left + wide
            and top + margin <= cy <= top + high - margin)


def brought_into_view(one: Person, name: str, window: dict, what: str, *,
                      exact: bool = True) -> dict:
    """The control `name`, once a person can reach it in `window`: the page is scrolled there
    when it stands below or above what the window shows, as a person turns the wheel. The place
    it stands is read twice the same before it is answered, so a control the page moved is
    pressed where it now is."""
    last = None
    for _ in range(40):
        target = until(f"{what} on {one.name}'s shell", lambda: one.target(name, exact=exact),
                       30, 0.2)
        rect = target["rect"]
        if rect == last and inside(rect, window):
            return target
        if not inside(rect, window):
            left, top, wide, high = window["rect"]
            centre = top + high / 2
            below = rect[1] + rect[3] / 2 > centre
            # A wheel's dy is how far it turns up: a negative one scrolls the page down.
            one.session.control({"op": "scroll", "x": left + wide / 2, "y": centre,
                                 "dy": -180 if below else 180})
        time.sleep(0.25)
        last = rect
    raise Failed(f"{what} never came where a person can reach it in the window")


def press_in(one: Person, name: str, window: dict, what: str, *, exact: bool = True) -> None:
    """Press the control `name` where this shell draws it inside `window`."""
    target = brought_into_view(one, name, window, what, exact=exact)
    x, y, width, height = target["rect"]
    one.session.click_at(x + width / 2, y + height / 2)
    time.sleep(0.2)


def type_into(one: Person, label: str, text: str, what: str, window: dict) -> None:
    """Press the field a person types into and type `text` over what it held, a key at a time,
    as fingers do."""
    press_in(one, label, window, what)
    one.session.chord("Control", "a")
    for letter in text:
        one.session.type(letter)
        time.sleep(0.03)


def message_states(one: Person) -> dict:
    """What Mail's mailbox keeps of each message, read from the workspace's own record: its
    subject, sender and read flag, which opening a message changes."""
    try:
        answer = one.session.control({"op": "resource_read", "app": "so.tilo.mail",
                                      "records": [MESSAGES_RECORD], "wait_ms": 5000})
    except Exception as error:  # noqa: BLE001 - the shell's refusal is what says why
        raise Failed(
            f"the shell would not read Mail's record {MESSAGES_RECORD!r}: {error}") from error
    records = (answer.get("answer") or {}).get("records") or []
    for record in records:
        messages = ((record.get("value") or {}).get("messages")) or {}
        if messages:
            return messages
    return {}


class StandIn:
    """The tree's stand-in mail server, run for this story alone, and what it takes to be reached
    as the address's provider is."""

    def __init__(self, out: Path):
        self.dir = out / "stand-in"
        self.dir.mkdir(parents=True, exist_ok=True)
        self.login = ADDRESS
        self.password = os.urandom(9).hex()
        self.password_file = self.dir / "password"
        self.password_file.write_text(self.password + "\n")
        self.password_file.chmod(0o600)
        self.certificate = self.dir / "certificate.pem"
        self.log = self.dir / "stand-in.log"
        self.proc: subprocess.Popen | None = None
        self.hosts = Path("/etc/hosts")
        self.hosts_before: bytes | None = None
        # Each CA bundle the certificate joined, and what it held before (None for one made).
        self.trusted: list[tuple[Path, bytes | None]] = []

    def binary(self) -> Path:
        named = os.environ.get("TILO_MAIL_STAND_IN")
        build = os.environ.get("TILO_NESTED_SESSION_BUILD")
        for candidate in ([Path(named)] if named else []) + (
                [Path(build) / "bin/mail-stand-in"] if build else []):
            if candidate.is_file() and os.access(candidate, os.X_OK):
                return candidate
        raise Failed("no stand-in mail server to run: set TILO_MAIL_STAND_IN to the tree's "
                     "mail_stand_in program (the nested-session build stages it as "
                     "bin/mail-stand-in)")

    def prepare(self) -> None:
        """Name the provider's servers here, start it, and trust it."""
        if not can_act_as_machine():
            raise Failed("the story rehearses a mail provider on this machine, which takes root "
                         "(/etc/hosts, the CA bundle and the mail ports)")
        # The provider is the world around Ada, so the machine stages it; Ada uses it as herself.
        with machine():
            self.name_itself()
            self.start()
            self.trust()

    def name_itself(self) -> None:
        """Name the provider and its servers in `/etc/hosts`, so the names Mail works out from
        the address reach the stand-in on this machine's loopback."""
        self.hosts_before = self.hosts.read_bytes()
        block = ["", "# the mail-password story's provider, at this machine's own loopback"]
        block += [f"127.0.0.1 {name}" for name in PROVIDER_NAMES]
        try:
            self.hosts.write_bytes(self.hosts_before + ("\n".join(block) + "\n").encode())
        except OSError as error:
            self.hosts_before = None
            raise Failed(f"the story cannot name its provider in /etc/hosts: {error}") from error

    def start(self) -> None:
        argv = [
            str(self.binary()),
            "--name", DOMAIN, "--name", f"imap.{DOMAIN}", "--name", f"smtp.{DOMAIN}",
            "--login", self.login,
            "--password-file", str(self.password_file),
            "--write-certificate", str(self.certificate),
            # The ports a domain's own servers are asked on, both TLS from the first byte.
            "--imap", f"127.0.0.1:{IMAP_PORT}",
            "--smtp", f"127.0.0.1:{SMTP_PORT}",
        ]
        with self.log.open("w") as said:
            self.proc = subprocess.Popen(argv, stdout=said, stderr=subprocess.STDOUT,
                                         start_new_session=True)

        def serving() -> str | None:
            text = self.log.read_text(errors="replace") if self.log.is_file() else ""
            if "mail stand-in: IMAP at" in text:
                return text
            if self.proc is not None and self.proc.poll() is not None:
                raise Failed(f"the stand-in mail server exited: {text[-1500:]}")
            return None

        until("the stand-in mail server to answer", serving, 60, 0.1)

    def trust(self) -> None:
        """Trust the stand-in's certificate: a mail program checks the provider's name against
        the machine's CA bundle, so the certificate joins that bundle for this story, and every
        program the session starts reads it there (an app's sandbox forwards no `SSL_CERT_FILE`)."""
        certificate = self.certificate.read_bytes()
        candidates = [Path(path) for path in (
            "/etc/pki/tls/certs/ca-bundle.crt",
            "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
            "/etc/ssl/certs/ca-certificates.crt",
            "/etc/ssl/cert.pem",
        )]
        present = [path for path in candidates if path.exists()]
        if present:
            # Every bundle a reader might pick up, so which one it reads changes nothing.
            for path in present:
                bundle = path.resolve()
                before = bundle.read_bytes()
                self.trusted.append((bundle, before))
                bundle.write_bytes(before + b"\n" + certificate)
        else:
            # A guest with no CA bundle at all: the one path a reader looks at, made with the
            # stand-in's certificate, which is all this story's mail needs.
            bundle = Path("/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem")
            try:
                bundle.parent.mkdir(parents=True, exist_ok=True)
                bundle.write_bytes(certificate)
            except OSError as error:
                raise Failed(f"this machine has no CA bundle and none can be made at "
                             f"{bundle}: {error}") from error
            self.trusted.append((bundle, None))
        # What this script starts outside an app's sandbox reads the same roots by name.
        roots = self.dir / "roots.pem"
        roots.write_bytes(certificate)
        os.environ["SSL_CERT_FILE"] = str(roots)

    def received(self, words: str, token: str) -> str | None:
        """The message the stand-in's inbox holds that carries `token` and `words`, as the
        server answers it, or None: the server's own state, not what Mail said."""
        context = ssl.create_default_context(cafile=str(self.certificate))
        box = imaplib.IMAP4_SSL(host=f"imap.{DOMAIN}", port=IMAP_PORT, ssl_context=context,
                               timeout=10)
        try:
            box.login(self.login, self.password)
            box.select("INBOX")
            found, data = box.search(None, "TEXT", f'"{token}"')
            if found != "OK" or not (data and data[0]):
                return None
            for number in data[0].split():
                fetched, raw = box.fetch(number, "(BODY[])")
                if fetched != "OK":
                    continue
                text = b"".join(part[1] for part in raw if isinstance(part, tuple))
                if words.encode() in text:
                    return text.decode(errors="replace")
            return None
        finally:
            try:
                box.logout()
            except Exception:  # noqa: BLE001 - the connection ends either way
                pass

    def stop(self) -> None:
        # Only the machine could have staged it; a walk that could not stages nothing to undo.
        with machine() if can_act_as_machine() else contextlib.nullcontext():
            self.unstage()

    def unstage(self) -> None:
        if self.proc is not None and self.proc.poll() is None:
            try:
                os.killpg(self.proc.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                self.proc.wait(15)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(self.proc.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                self.proc.wait(5)
        self.proc = None
        if self.hosts_before is not None:
            try:
                self.hosts.write_bytes(self.hosts_before)
            except OSError:
                pass
            self.hosts_before = None
        for bundle, before in self.trusted:
            try:
                if before is None:
                    bundle.unlink(missing_ok=True)
                else:
                    bundle.write_bytes(before)
            except OSError:
                pass
        self.trusted.clear()
        os.environ.pop("SSL_CERT_FILE", None)
        for leftover in (self.dir / "roots.pem",):
            leftover.unlink(missing_ok=True)


def steps(story: Story) -> None:
    ada = story.person("Ada")
    stand_in = StandIn(story.out)

    def opens_mail() -> None:
        stand_in.prepare()
        took = ada.start(apps=APPS)
        answered = time.monotonic()
        story.timing("The session, to its first frame", took["tookMs"])
        story.timing("The session, to its first frame, with the development launcher's installing",
                     took["launcherMs"])
        ada.session.launch("mail")
        ada.session.answering("mail", deadline=180)
        until("Mail's first window", lambda: ada.shows("No email account yet")
              or ada.shows("Add an email account"), 30, 0.25)
        # Where an account is added is the person's accounts in Settings. A person reaches it the
        # way they reach any page: the shell's own search. (Mail's empty window links there with
        # a `settings://` URL the desktop portal opens, and a headless session runs no portal
        # backend to open it, so that link is not the way a person gets there here.)
        ada.session.chord(*SEARCH_CHORD)
        ada.session.type("accounts")
        ada.session.wait_search_rows("accounts", [ACCOUNTS_PAGE], 30)
        pressed(ada, ACCOUNTS_PAGE, f"the {ACCOUNTS_PAGE} page in the shell's search")
        settings = until("Settings, where the person's accounts are kept",
                         lambda: window_named(ada, "so.tilo.settings"), 60, 0.25)
        story.timing("The shell, to Settings' Accounts page", (time.monotonic() - answered) * 1000)
        press_in(ada, ADD_EMAIL, settings, "the offer to add a mail account")
        until("the form a mail account is added with", lambda: ada.target("Save account"), 30, 0.2)
        # What the form asks a person for: their name, their address and their password. The two
        # server fields are left as the form shows them, which is how a person names no server.
        type_into(ada, "Display name", DISPLAY_NAME, "the display name field", settings)
        type_into(ada, "Email address", ADDRESS, "the address field", settings)
        type_into(ada, "Password or app password", stand_in.password, "the password field",
                  settings)
        press_in(ada, "Save account", settings, "the press that connects the account")
        # What a person waited for is the desktop's own start, and then their own acts: the
        # launcher's installing happens before a person's computer starts Tilo at all.
        return {"tookMs": took["tookMs"] + (time.monotonic() - answered) * 1000}

    try:
        story.step("Ada opens Mail and enters their address and password", opens_mail,
                   people=[ada])

        found: dict = {}

        def finds_servers() -> None:
            # The account is kept only after its sign-in was checked against the servers the
            # person never named: Mail works them out from the address (the domain's own imap.
            # and smtp. servers on the ports mail programs use). A check that could not reach
            # them leaves the form up with what failed.
            def kept() -> str | None:
                if ada.target("Save account"):
                    return None
                return "kept" if ada.shows(ADDRESS) or ada.shows(DISPLAY_NAME) else None

            try:
                until("the account to be kept", kept, 20, 0.25)
            except Failed as error:
                raise Failed(
                    "the account was not kept: Settings kept its Add Email form up after Save, "
                    "so the sign-in check did not pass. Settings connects through the session's "
                    f"accounts service to imap.{DOMAIN}:{IMAP_PORT}; the form's own message is "
                    "in this step's shot, and the session's log says what the service did"
                ) from error
            found["servers-named-by-the-person"] = False
            # Mail's own window takes the account it was given, with nothing reloaded.
            until(f"{ADDRESS} in Mail, with no reload", lambda: ada.shows(ADDRESS), 60, 0.2)
            story.note("settings-after-account", describe(ada))
            story.note("messages-record", message_states(ada))

        story.step("Mail finds the server by itself and connects", finds_servers, people=[ada])
        story.note("servers-found-by-mail", found)

        def reads_newest() -> None:
            began = time.monotonic()
            row = until("the newest message in Mail's list",
                        lambda: ada.target(NEWEST_SUBJECT, exact=False), 60, 0.2)
            pressed(ada, row["name"], "the newest message's row")
            # Opening is the read boundary: the mailbox's own record of the message now says it
            # was read, which is what the person's press did.
            until("the newest message to be marked read", lambda: any(
                state.get("read") and NEWEST_SUBJECT in (state.get("label") or "")
                for state in message_states(ada).values()), 30, 0.2)
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
            until("the older message to open", lambda: any(
                state.get("read") and OLDER_SUBJECT in (state.get("label") or "")
                for state in message_states(ada).values()), 30, 0.2)

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
            until("the reply to open", lambda: any(
                state.get("read") and f"Re: {OLDER_SUBJECT}" in (state.get("label") or "")
                for state in message_states(ada).values()), 30, 0.2)

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
