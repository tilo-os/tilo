"""The mail a shell story reads: the stand-in mail server it runs as the account's provider, and
what a person does in Settings and Mail to use it.

The account is the tree's stand-in mail server (`crates/integrations/account-probe/examples/
mail_stand_in.rs`), the same program a tilo.sh stack runs for a stack's mail account
(`stack.sh mail-account`): an IMAP and an SMTP server over real sockets that take one made-up
login, hold a few made-up messages and keep the mail they are handed. Here it is started by the
story alone, with no stack around it, answering the address's own server names on this machine
(the names are given in `/etc/hosts`) and with a certificate this machine trusts (it is added to
the system's CA bundle for the story and taken out again after). It says every IMAP sign-in on its
log, with the process that made the connection, which is how a story tells whose computer signed
in.

The account's sign-in is checked before it is kept. Settings' sandbox has no network of its own
(`apps/settings/tilo-app.toml` declares `[sandbox.net] mode = "off"`), so the session makes that
connection for it, on the `accounts` service the launcher hands Settings alone: the story's
provider is reached by the session, at the names the account gives.
"""

from __future__ import annotations

import contextlib
import imaplib
import json
import os
import re
import signal
import ssl
import subprocess
import time
from pathlib import Path

from story import Failed, Person, actions_matching, can_act_as_machine, machine, until

# The provider a mail story rehearses: the address's own domain and the server names Mail works
# out from it, answered on this machine's loopback for the length of the story.
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
# Mail, the tilolang app (apps/tl/mail), installed as its signed package.
MAIL = "so.tilo.mail"
TILOLANG = ("mail",)
# What a conversation's read action is called once it is read (apps/tl/mail `mark_read`).
READ = "Mark as unread"
# How the stand-in says an IMAP sign-in on its log (`stand_in::said_sign_in`).
SIGN_IN = re.compile(r"mail stand-in: IMAP sign-in as (?P<login>.*?) from (?P<peer>\S+) "
                     r"by (?P<by>.+): (?P<verdict>taken|refused)$")


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


def mail_actions(one: Person) -> list[dict]:
    """What Mail offers on its resources now, as the shell lists them to agents and people alike:
    each conversation's actions name it and say whether it is read."""
    return [row for row in actions_matching(one, "") if row.get("provider") == MAIL]


def launches_mail(one: Person) -> bool:
    """Open Mail from the launcher, and wait for its window. Mail reads mail from the account's
    server, so the first time it opens in a workspace the shell asks whether it may use the
    internet; the person allows it, as anyone who wants their mail does. Whether it asked."""
    one.session.open(MAIL)
    asked = False

    def up() -> dict | None:
        nonlocal asked
        if one.target("Allow") and one.target("Deny"):
            asked = True
            pressed(one, "Allow", "the press that lets Mail use the internet")
        return window_named(one, MAIL)

    until("Mail's window", up, 120, 0.25)
    one.session.answering(MAIL, deadline=180)
    return asked


def has_read(one: Person, subject: str) -> bool:
    """Whether Mail holds the conversation about `subject` as read: its read action, which only a
    read conversation offers as Mark as unread, names it."""
    return any(READ in (row.get("title") or row.get("name") or "")
               and subject in json.dumps(row) for row in mail_actions(one))


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
        block = ["", "# a mail story's provider, at this machine's own loopback"]
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

    def sign_ins(self) -> list[dict]:
        """Every IMAP sign-in the stand-in said on its log, in order: the login it named, whether
        it was taken (only the stand-in's own password is), where it came from, and the process
        that made the connection with each process that started it, by their ids."""
        text = self.log.read_text(errors="replace") if self.log.is_file() else ""
        found = []
        for line in text.splitlines():
            said = SIGN_IN.match(line)
            if not said:
                continue
            by = said["by"]
            lineage = ([int(part.split()[0]) for part in by.removeprefix("process ").split(" < ")]
                       if by.startswith("process ") else [])
            found.append({"login": said["login"], "taken": said["verdict"] == "taken",
                          "from": said["peer"], "by": by, "lineage": lineage})
        return found

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



def opens_accounts(one: Person) -> dict:
    """Open Settings' Accounts page the way a person reaches any page: the shell's own search.
    (Mail's empty window links there with a `settings://` URL the desktop portal opens, and a
    headless session runs no portal backend to open it, so that link is not the way a person gets
    there here.) Answers Settings' window."""
    one.session.chord(*SEARCH_CHORD)
    one.session.type("accounts")
    one.session.wait_search_rows("accounts", [ACCOUNTS_PAGE], 30)
    pressed(one, ACCOUNTS_PAGE, f"the {ACCOUNTS_PAGE} page in the shell's search")
    return until("Settings, where the workspace's accounts are kept",
                 lambda: window_named(one, "so.tilo.settings"), 60, 0.25)


def adds_account(one: Person, settings: dict, password: str) -> None:
    """Add the mail account on Settings' Accounts page: what the form asks a person for is their
    name, their address and their password. The two server fields are left as the form shows
    them, which is how a person names no server."""
    press_in(one, ADD_EMAIL, settings, "the offer to add a mail account")
    until("the form a mail account is added with", lambda: one.target("Save account"), 30, 0.2)
    type_into(one, "Display name", DISPLAY_NAME, "the display name field", settings)
    type_into(one, "Email address", ADDRESS, "the address field", settings)
    type_into(one, "Password or app password", password, "the password field", settings)
    press_in(one, "Save account", settings, "the press that connects the account")


def account_kept(one: Person) -> None:
    """Wait for the account to be kept. It is kept only after its sign-in was checked against
    the servers the person never named: Mail works them out from the address (the domain's own
    imap. and smtp. servers on the ports mail programs use). A check that could not reach them
    leaves the form up with what failed."""
    def kept() -> str | None:
        if one.target("Save account"):
            return None
        return "kept" if one.shows(ADDRESS) or one.shows(DISPLAY_NAME) else None

    try:
        until("the account to be kept", kept, 20, 0.25)
    except Failed as error:
        raise Failed(
            "the account was not kept: Settings kept its Add Email form up after Save, so the "
            "sign-in check did not pass. Settings connects through the session's accounts "
            f"service to imap.{DOMAIN}:{IMAP_PORT}; the form's own message is in this step's "
            "shot, and the session's log says what the service did") from error


def shows_inbox(one: Person) -> None:
    """Wait for Mail's window to list the account's inbox and name the account by its address.
    Beside Settings, Mail's window is too narrow to keep its sidebar open, so the person opens
    it, as a person does to see which account a mailbox is, and folds it away again."""
    until("the account's inbox in Mail", lambda: one.target(NEWEST_SUBJECT, exact=False), 60, 0.2)
    if not one.shows(ADDRESS):
        pressed(one, "Show sidebar", "Mail's sidebar")
        until(f"{ADDRESS} in Mail's sidebar", lambda: one.shows(ADDRESS), 10, 0.2)
        pressed(one, "Hide sidebar", "the press that folds Mail's sidebar away")


def opens_newest(one: Person) -> None:
    """Open the newest message from Mail's list. Opening is the read boundary: the mailbox's own
    record of the message then says it was read, which is what the person's press did."""
    row = until("the newest message in Mail's list",
                lambda: one.target(NEWEST_SUBJECT, exact=False), 60, 0.2)
    pressed(one, row["name"], "the newest message's row")
    try:
        until("the newest message to be marked read", lambda: has_read(one, NEWEST_SUBJECT),
              30, 0.2)
    except Failed:
        one.story.note(f"mail-actions-{one.key}", mail_actions(one))
        raise
