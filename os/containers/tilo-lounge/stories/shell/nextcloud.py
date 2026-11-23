#!/usr/bin/env python3
"""The Nextcloud a story meets, and the account Ada adds from the shell's Settings.

`connect_account` is what a person does in the shell: Settings' Accounts page,
the Nextcloud provider, the server's address and Continue. Settings opens the
server's sign-in page in Tilo's Browser, where Ada signs in and grants this
device access on Nextcloud's own pages, and Settings then keeps the account.
The page is used through the Browser's own page actions (it finds a control by
what it is named, fills it, presses it), on the page the Browser shows.

The server's own state is read the way another client would: a calendar's
events over CalDAV, a file written over WebDAV. Every wait is decided by what
the shell draws (its controls' accessible names), by what the Browser's page
says, or by the server's state, never by what an action answered.
"""

from __future__ import annotations

import html
import json
import re
import time
import urllib.error
import urllib.parse
import urllib.request

from story import Failed, until

# How long a page of the server may take to answer, on a server on the check's own loopback.
PAGE_SECONDS = 30.0


class SignInFailed(Exception):
    """The server did not take what a client asked of it."""


def _basic(user: str, password: str) -> str:
    import base64
    return "Basic " + base64.b64encode(f"{user}:{password}".encode()).decode()


def _http(method: str, url: str, auth: str, *, body: str | None = None,
          headers: dict | None = None) -> tuple[int, str]:
    request = urllib.request.Request(url, data=body.encode() if body is not None else None,
                                     method=method, headers={"Authorization": auth,
                                                             **(headers or {})})
    try:
        with urllib.request.urlopen(request, timeout=PAGE_SECONDS) as answer:
            return answer.status, answer.read().decode(errors="replace")
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode(errors="replace")


def calendar_entries(base: str, who: str, password: str) -> list[str]:
    """Everything a calendar of `who` holds on the server, as its raw iCalendar
    texts: what an invitation delivered to that person would be among. `base`
    is the server's CalDAV root (`…/remote.php/dav`)."""
    auth = _basic(who, password)
    home = f"{base}/calendars/{who}/"
    status, xml = _http(
        "PROPFIND", home, auth, headers={"Depth": "1", "Content-Type": "application/xml"},
        body='<?xml version="1.0" encoding="utf-8"?>'
             '<d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/></d:prop></d:propfind>')
    if status // 100 != 2:
        return []
    parts = urllib.parse.urlsplit(home).path.rstrip("/").split("/")
    hrefs = [html.unescape(href) for href in re.findall(r"<d:href>([^<]+)</d:href>", xml)]
    entries: list[str] = []
    for href in hrefs:
        # The calendars of the home, never the home itself: a depth-1 listing
        # has both, and only a calendar (or an inbox) answers a calendar-query.
        if not href.endswith("/") or len(href.rstrip("/").split("/")) <= len(parts):
            continue
        status, body = _http(
            "REPORT", urllib.parse.urljoin(base, href), auth,
            headers={"Depth": "1", "Content-Type": "application/xml"},
            body='<?xml version="1.0" encoding="utf-8"?>'
                 '<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">'
                 "<d:prop><d:getetag/><c:calendar-data/></d:prop>"
                 '<c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT"/>'
                 "</c:comp-filter></c:filter></c:calendar-query>")
        if status // 100 != 2:
            continue
        # The server names the calendar-data in whatever prefix it answers with
        # (`cal:` here), never the one the query wrote.
        for data in re.findall(r"<(?:\w+:)?calendar-data[^>]*>([\s\S]*?)</(?:\w+:)?calendar-data>",
                               body):
            entries.append(html.unescape(data))
    return entries


def change_file(files: str, path: str, text: str, user: str, password: str) -> None:
    """Write `path` in Ada's Nextcloud, as another Nextcloud client would."""
    status, body = _http("PUT", f"{files}/{path}", _basic(user, password),
                         body=text, headers={"Content-Type": "text/plain"})
    if status // 100 != 2:
        raise SignInFailed(f"changing {path} in Nextcloud answered {status}: {body[:300]}")


# -- what a person does in the shell's Settings --

# The Accounts page in Settings' sidebar, by the name it draws.
ACCOUNTS = "Accounts"
# The provider row Settings' Add account section offers.
ADD_NEXTCLOUD = "Add Nextcloud"
# The sign-in form's server field and its finish.
SERVER_FIELD = "Server URL"
CONTINUE = "Continue"
# The account's calendars are chosen from its row's More menu, named after the
# user the account signs in as, and saved from the form. The row's one button
# browses files; the menu holds the rest. The conformance suite's seed gives Ada
# a calendar named this (servers.sh).
ACCOUNT_MORE = "More for {user}"
CONNECT_CALENDARS = "Connect calendars"
SAVE = "Save account"
SELECT_CALENDAR = "Select"
DESELECT_CALENDAR = "Deselect"
SEEDED_CALENDAR = "Personal"


def kept_calendar_names(one) -> list[str]:
    """The calendars kept for a Nextcloud account on `one`'s computer, as its
    account store holds them: what the Calendar app will show."""
    store = one.state / "config/tilo/accounts/store.json"
    try:
        kept = json.loads(store.read_text())
    except (OSError, json.JSONDecodeError):
        return []
    names: list[str] = []
    for account in kept.get("accounts") or []:
        if account.get("type_id") != "nextcloud":
            continue
        config = account.get("config") or {}
        for calendars in (config.get("calendars"),
                          (config.get("calendar") or {}).get("calendars")):
            for calendar in calendars or []:
                if isinstance(calendar, dict) and calendar.get("name"):
                    names.append(calendar["name"])
    return names


def open_accounts(one) -> None:
    """Open Settings' Accounts page in `one`'s shell, as a person finds it.

    The page sits below the part of the sidebar a window this size shows, so
    the press is delivered to the row itself, by the name it is drawn under,
    rather than to a point past the window's bottom edge."""
    one.session.launch("settings")
    one.session.answering("settings", deadline=180)
    until("Settings' sidebar", lambda: one.target("Appearance") or one.target(ACCOUNTS),
          60, 0.25)
    one.session.click_named(ACCOUNTS)
    until("the Accounts page", lambda: one.shows("Add account") or one.target(ADD_NEXTCLOUD),
          30, 0.25)


def press_settled(one, name: str, what: str, deadline: float = 30.0) -> dict:
    """Press the control `name` once it has stopped moving.

    A surface that is arriving lays its controls out over a few frames: a press
    at the place a field stood a frame ago lands on the field above it, which is
    how an address typed into the server field once went to the display name."""
    def still():
        first = one.target(name)
        if first is None:
            return None
        time.sleep(0.2)
        second = one.target(name)
        return second if second and second.get("rect") == first.get("rect") else None

    target = until(f"{what} to settle", still, deadline, 0.05)
    x, y, width, height = target["rect"]
    one.session.click_at(x + width / 2, y + height / 2)
    return target


def type_into(one, field: str, text: str, what: str) -> None:
    """Type `text` into the field `field` in `one`'s shell, and read it back off
    the session's own clipboard: a field's value is not drawn as a control's
    name, so the person's copy of it is the shell's word that the keys landed."""
    for _ in range(3):
        press_settled(one, field, f"the {field} field")
        one.session.chord("Control", "a")
        one.session.type(text)
        one.session.chord("Control", "a")
        one.session.chord("Control", "c")
        try:
            until("the field's text on the shell's clipboard",
                  lambda: one.session.state_dump().get("clipboard") == text, 5, 0.1)
            return
        except Failed:
            continue
    raise Failed(f"{what} did not reach the {field} field; the shell holds "
                 f"{one.session.state_dump().get('clipboard')!r} on its clipboard, and offers "
                 f"{json.dumps(one.names()[:80])}")


# Tilo's Browser, and the row its page's actions are offered on.
BROWSER = "so.tilo.browser"
PAGE = "browser.page"
# What Nextcloud's pages say, by the page: the one a sign-in opens on, its login form, the page
# that grants this device access, and the one that says it is done.
CONNECT_PAGE = ("Connect to your account",)
LOGIN_FORM = ("Account name or email", "Login or email", "Username or email")
GRANT_PAGE = ("Grant access",)
CONNECTED_PAGE = ("Account connected", "now be connected")


def page_action(one, operation: str) -> dict | None:
    """The page action `operation` of the Browser window in `one`'s shell, as the desktop lists
    it: the one offered on the window's page."""
    for action in one.session.actions(""):
        if (action.get("application") == BROWSER and action.get("operation") == operation
                and names_page(str(action.get("resource", "")))):
            return action
    return None


def names_page(resource: str) -> bool:
    """Whether the desktop's name `resource` is the Browser window's page: a window's own rows
    are named after the window's ids, the row's id hex-encoded."""
    return resource == PAGE or resource.endswith(f":{PAGE.encode().hex()}")


def on_page(one, operation: str, args: dict | None = None):
    """Do `operation` on the page Tilo's Browser shows, and answer what the Browser says of it."""
    try:
        action = until(f"the Browser's {operation}", lambda: page_action(one, operation), 60, 0.5)
    except Failed as error:
        offered = sorted({a.get("operation") for a in one.session.actions("")
                          if a.get("application") == BROWSER
                          and names_page(str(a.get("resource", "")))})
        raise Failed(f"Tilo's Browser offers no {operation} on its page; it offers "
                     f"{offered[:60]}") from error
    answer = one.session.invoke_row(action, args or {})
    try:
        return answer["Completed"]["output"]["value"]
    except (KeyError, TypeError):
        raise Failed(f"the Browser's {operation} on the page answered "
                     f"{json.dumps(answer)[:1500]}") from None


def page_says(one) -> str:
    """What the page in Tilo's Browser says now, as the Browser reads it, or nothing while it
    has none."""
    try:
        return json.dumps(on_page(one, "read-page"), ensure_ascii=False)
    except Failed:
        return ""


def page_shown(one) -> str:
    """What the page in Tilo's Browser says, or why the Browser could not read it: for a failure
    to name."""
    try:
        return json.dumps(on_page(one, "read-page"), ensure_ascii=False)[:600]
    except Failed as error:
        return f"nothing the Browser could read ({error})"


def says_any(said: str, words: tuple[str, ...]) -> bool:
    return any(word.lower() in said.lower() for word in words)


def element(one, role: str | None, name: str) -> str:
    """The page's control named `name` (of `role`), found by what it says: the element the
    Browser's other page actions take."""
    found = on_page(one, "find-page-element",
                    {**({"role": role} if role else {}), "name": name})
    ref = isinstance(found, dict) and found.get("element")
    if not ref:
        raise Failed(f"the page in Tilo's Browser has no {role or 'control'} named {name!r}: "
                     f"{json.dumps(found)[:800]}")
    return ref


def signs_in_on_the_page(story, one, server: str, user: str, password: str) -> None:
    """Ada signs in on Nextcloud's page in Tilo's Browser, and grants this device access.

    The page is read after each press, and what it says decides what she does next: the page a
    sign-in opens on asks her to log in, the login form takes her account name and password, and
    the grant page asks whether this device may have access."""
    # The address names the server before its page has drawn anything: what the page says decides.
    known = CONNECT_PAGE + LOGIN_FORM + GRANT_PAGE + CONNECTED_PAGE

    def nextclouds_page():
        said = page_says(one)
        return said if says_any(said, known) else None

    try:
        said = until("Nextcloud's sign-in page in Tilo's Browser", nextclouds_page, 60, 1.0)
    except Failed as error:
        raise Failed(f"Settings began a sign-in at {server}, but Tilo's Browser never showed "
                     f"the server's sign-in page; the page it shows says "
                     f"{page_shown(one)}") from error
    story.note("nextcloud-login-page", said[:2000])
    pressed: list[str] = []
    for _ in range(6):
        if says_any(said, CONNECTED_PAGE):
            story.note("nextcloud-approved", {"server": server, "pressed": pressed})
            return
        if says_any(said, GRANT_PAGE):
            on_page(one, "click-page", {"element": element(one, "button", GRANT_PAGE[0])})
            pressed.append("Grant access")
        elif says_any(said, LOGIN_FORM):
            name = next(word for word in LOGIN_FORM if word.lower() in said.lower())
            on_page(one, "fill-page", {"element": element(one, "textbox", name), "text": user})
            secret = element(one, None, "Password")
            on_page(one, "fill-page-secret", {"element": secret, "text": password})
            on_page(one, "press-page", {"key": "Enter", "element": secret})
            pressed.append("Log in")
        elif says_any(said, CONNECT_PAGE):
            on_page(one, "click-page", {"element": element(one, "button", "Log in")})
            pressed.append("Connect")
        else:
            raise Failed(f"Nextcloud's page in Tilo's Browser asked nothing Ada knows how to "
                         f"answer: {said[:800]!r}")
        before = said

        def moved_on():
            # The sign-in's page closes itself once Nextcloud gave this device its password: the
            # page saying so may be gone before it is read.
            if not one.window_rect(BROWSER):
                return CONNECTED_PAGE[0]
            now = page_says(one)
            return now if now != before and says_any(now, known) else None

        said = until("Nextcloud's next page in Tilo's Browser", moved_on, PAGE_SECONDS, 0.5)
    raise Failed(f"Nextcloud's pages never said the account is connected, after {pressed}: "
                 f"{said[:800]!r}")


def kept_accounts(one) -> list[dict]:
    """The Nextcloud accounts `one`'s computer keeps, as its account store holds them."""
    store = one.state / "config/tilo/accounts/store.json"
    try:
        kept = json.loads(store.read_text())
    except (OSError, json.JSONDecodeError):
        return []
    return [account for account in kept.get("accounts") or []
            if account.get("type_id") == "nextcloud"]


def sign_in_page_closes(one) -> None:
    """The sign-in's page in Tilo's Browser closes by itself once the sign-in is over: Ada never
    closes a page that did its job, and nothing offers to undo a close she did not make."""
    try:
        until("the sign-in's page in Tilo's Browser to close by itself",
              lambda: not one.window_rect(BROWSER), 30, 0.25)
    except Failed as error:
        raise Failed(f"Settings keeps the account, but the sign-in's page in Tilo's Browser "
                     f"stayed open; it says {page_shown(one)}") from error
    undo = [name for name in one.names() if name.endswith(" closed")]
    if undo:
        raise Failed(f"the sign-in's page closed, and the shell says {undo}, as if Ada had "
                     f"closed it")


def connect_account(story, one, server: str, user: str, password: str,
                    calendar: bool = False) -> None:
    """Ada adds her Nextcloud account in the shell's Settings, and signs in and grants this
    device access on the server's page in Tilo's Browser. `calendar` also connects the
    calendars she keeps there, choosing them from what the server lists. The step is decided by
    what Settings shows when it settles and what her computer keeps: the account listed and
    kept, or the product's own words for why it is not."""
    open_accounts(one)
    press_settled(one, ADD_NEXTCLOUD, "Nextcloud among the providers Settings adds")
    until("the Nextcloud sign-in form", lambda: one.target(SERVER_FIELD), 30, 0.2)
    type_into(one, SERVER_FIELD, server, "the server's address")
    story.note("settings-controls", one.targets())
    press_settled(one, CONTINUE, "the sign-in form's Continue")
    signs_in_on_the_page(story, one, server, user, password)

    # Nextcloud's flow answers the app's poll with the app password; Settings
    # keeps the account and lists it. A person sees the account on the page.
    until("the Nextcloud account listed in Settings, by the user it signs in as",
          lambda: one.target(user) or next(
              (t for t in one.targets()
               if (t.get("identity") or "").startswith("settings.account:")), None),
          60, 0.25)
    try:
        kept = until("the Nextcloud account kept on Ada's computer", lambda: kept_accounts(one),
                     30, 0.25)
    except Failed as error:
        raise Failed(f"Settings lists a Nextcloud account, but Ada's computer keeps none: "
                     f"{error}") from error
    story.note("nextcloud-account-kept",
               [{key: account.get(key) for key in ("id", "type_id", "label")} for account in kept])
    sign_in_page_closes(one)
    if not calendar:
        return
    # The calendars she keeps on the server are read from it and chosen here:
    # a new account has none kept until the person confirms the choice.
    press_settled(one, ACCOUNT_MORE.format(user=user), "the account row's More menu")
    until("the account's menu open", lambda: one.target(CONNECT_CALENDARS), 15, 0.2)
    story.note("account-menu", one.names()[:60])
    press_settled(one, CONNECT_CALENDARS, "the account menu's Connect calendars")

    def offered(calendar: str) -> bool:
        # A calendar the form offers is its name on the row of its Select (or Deselect) button;
        # the same words elsewhere, such as the sidebar's Personal heading, are not it.
        buttons = [t.get("rect") for t in one.targets()
                   if t.get("name") in (SELECT_CALENDAR, DESELECT_CALENDAR) and t.get("rect")]
        return any(abs((t["rect"][1] + t["rect"][3] / 2) - (b[1] + b[3] / 2)) < 24
                   for t in one.targets() if t.get("name") == calendar and t.get("rect")
                   for b in buttons)

    def choosing():
        if offered(SEEDED_CALENDAR) and one.target(SAVE):
            return "form", []
        # The form did not come: say what the pane did instead.
        said = [name for name in one.names()
                if any(part in name for part in ("Could not", "check the server",
                                                 "Select at least", "unavailable",
                                                 "discovery", "No calendars"))]
        return ("said", said) if said else None

    try:
        kind, said = until("the server's calendars to choose from", choosing, 60, 0.25)
    except Failed as error:
        raise Failed(f"Settings did not list the calendars kept on {server}: {error}. "
                     f"It offers {json.dumps(one.names()[:100])}") from error
    if kind != "form":
        raise Failed(f"Settings could not read the calendars kept on {server}: {said}")
    press_settled(one, SAVE, "the calendar choice's Save")
    try:
        until("the chosen calendars kept on Ada's computer",
              lambda: SEEDED_CALENDAR in kept_calendar_names(one), 30, 0.25)
    except Failed as error:
        raise Failed(f"{error}; after Save, Settings shows "
                     f"{json.dumps([n for n in one.names() if '/' not in n][:80])}") from error
    story.note("calendars-kept", kept_calendar_names(one))
