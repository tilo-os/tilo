#!/usr/bin/env python3
"""The Nextcloud a story meets, and the account Ada adds from the shell's Settings.

Two sides of one server. A person's own browser is asked to approve the app
Nextcloud gives this device: `sign_in` walks the server's pages as a browser
does (its connect page, its login form, its grant), over HTTP with the server's
cookies, because the shell's browser is another program the person's page is
not a part of. And the server's own state is read the way another client would:
a calendar's events over CalDAV, a file written over WebDAV.

`connect_account` is the part a person does in the shell: Settings' External
accounts page, the Nextcloud provider, the server's address, and the finish.
Every wait is decided by what the shell draws (its controls' accessible names)
or by the server's state, never by what an action answered.
"""

from __future__ import annotations

import html
import http.cookiejar
import json
import re
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request

from story import Failed, until

# How long the person's browser's side of a sign-in may take, on a server on the check's own
# loopback.
PAGE_SECONDS = 30.0


class SignInFailed(Exception):
    """The server's pages did not give this device its app password."""


class _StopRedirect(Exception):
    """A redirect the browser cannot follow, such as Nextcloud's `nc://login`
    that hands the app password over: the end of the sign-in."""

    def __init__(self, location: str):
        super().__init__(location)
        self.location = location


class _Redirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if not newurl.startswith(("http://", "https://")):
            raise _StopRedirect(newurl)
        return super().redirect_request(req, fp, code, msg, headers, newurl)


class Answer:
    def __init__(self, status: int, url: str, text: str):
        self.status = status
        self.url = url
        self.text = text


class Browser:
    """A browser's session with one server: its cookies, its redirects, its
    forms. Enough of a browser to log in and approve an app on Nextcloud's own
    pages, which is what the person does on a page the product opens for them."""

    def __init__(self):
        self.jar = http.cookiejar.CookieJar()
        self.opener = urllib.request.build_opener(
            urllib.request.HTTPCookieProcessor(self.jar), _Redirect())

    def _open(self, request: urllib.request.Request) -> Answer:
        try:
            with self.opener.open(request, timeout=PAGE_SECONDS) as answer:
                return Answer(answer.status, answer.geturl(),
                              answer.read().decode(errors="replace"))
        except _StopRedirect as stop:
            return Answer(302, stop.location, "")
        except urllib.error.HTTPError as error:
            return Answer(error.code, error.geturl(),
                          error.read().decode(errors="replace"))

    def get(self, url: str, headers: dict | None = None) -> Answer:
        request = urllib.request.Request(url, headers=headers or {})
        return self._open(request)

    def post(self, url: str, fields: dict, headers: dict | None = None) -> Answer:
        body = urllib.parse.urlencode(fields).encode()
        request = urllib.request.Request(
            url, data=body, headers={"Content-Type": "application/x-www-form-urlencoded",
                                     **(headers or {})})
        return self._open(request)


def _plain(text: str) -> str:
    """The words a page shows: its script and style left out, and a button
    written as an input folded in, which the page's text leaves out."""
    buttons = re.findall(r'<input[^>]*type="submit"[^>]*value="([^"]*)"', text)
    text = re.sub(r"<script[\s\S]*?</script>", " ", text)
    text = re.sub(r"<style[\s\S]*?</style>", " ", text)
    text = re.sub(r"<[^>]+>", " ", text)
    return " ".join(html.unescape(" ".join([text, *buttons])).split())


def _hidden(page: str) -> dict:
    return {name: html.unescape(value) for name, value in
            re.findall(r'<input[^>]*type="hidden"[^>]*name="([^"]+)"[^>]*value="([^"]*)"', page)}


def _request_token(page: str) -> str | None:
    found = re.search(r'data-requesttoken="([^"]+)"', page)
    return html.unescape(found.group(1)) if found else None


def _same_origin(url: str) -> str:
    parts = urllib.parse.urlsplit(url)
    return f"{parts.scheme}://{parts.netloc}"


def _relative(url: str) -> str:
    parts = urllib.parse.urlsplit(url)
    return f"{parts.path}?{parts.query}" if parts.query else parts.path


def sign_in(login_url: str, user: str, password: str) -> dict:
    """Approve the app password Nextcloud minted for `login_url`, as the
    person does on the sign-in page their browser opened.

    Nextcloud's login flow: the connect page names the grant that finishes it;
    a person who is not logged in is taken to the login form first, and comes
    back to the grant. Answers `{"server", "login"}`: where the approval was
    given. Raises [`SignInFailed`] with what the page said when it could not."""
    browser = Browser()
    origin = _same_origin(login_url)
    page = browser.get(login_url)
    flow = _plain(page.text)

    # The connect page's own form posts to the grant, with the flow's token.
    found = re.search(r'<form id="login-form"[^>]*action="([^"]+)"', page.text)
    if found:
        grant = html.unescape(found.group(1))
        # Logging in first: the login form is where the person types, and it
        # sends them back to the grant through the session's redirect.
        login = browser.get(f"{origin}/login?redirect_url="
                            + urllib.parse.quote(_relative(grant), safe=""))
        token = _request_token(login.text)
        if token is None:
            raise SignInFailed(
                f"the server's login page offered no sign-in form: {_plain(login.text)[:400]}")
        posted = browser.post(
            f"{origin}/login",
            {"user": user, "password": password, "requesttoken": token,
             "timezone": "UTC", "timezone_offset": "0"},
            {"Origin": origin, "Referer": login.url})
        if "invalidOrigin" in posted.text or "Wrong password" in _plain(posted.text):
            raise SignInFailed(
                f"the server refused the password for {user}: {_plain(posted.text)[:400]}")
        approval = browser.get(grant, {"Referer": login.url})
    else:
        approval = page
    words = _plain(approval.text)
    if "Grant access" not in words:
        raise SignInFailed(
            f"the server's page offered no way to grant access: {words[:400]}")

    fields = _hidden(approval.text)
    if "stateToken" not in fields:
        raise SignInFailed(f"the server's grant page named no flow: {words[:400]}")
    granted = browser.post(f"{origin}/login/v2/grant", fields,
                           {"Origin": origin, "Referer": approval.url})
    # A granted flow either redirects to `nc://login/…` (which the browser
    # cannot follow and the product reads as its app password) or shows the
    # page that says the client is connected.
    landed = granted.url if granted.status == 302 else ""
    said = _plain(granted.text).lower()
    if not (landed.startswith("nc://login/") or "account connected" in said
            or "now be connected" in said):
        raise SignInFailed(
            f"the server did not grant access: {(_plain(granted.text) or landed)[:400]}")
    return {"server": origin, "login": user}


def _log_since(container: str, since: int) -> str:
    """What `container` logged since the unix second `since` (a server this
    check did not start answers nothing)."""
    try:
        read = subprocess.run(["podman", "logs", "--since", str(since), container],
                              text=True, capture_output=True, timeout=30)
    except (OSError, subprocess.TimeoutExpired):
        return ""
    return read.stdout + read.stderr


def flow_urls_in_log(container: str, since: int) -> list[str]:
    """Every login-flow page `container` was asked for since the unix second
    `since`, as the paths its access log names: the page a person's browser
    opens when a sign-in starts. A server not run by this check answers none."""
    paths = re.findall(r'"GET ([^" ]*login/v2/flow[^" ]*) HTTP/[\d.]+"',
                       _log_since(container, since))
    return list(dict.fromkeys(paths))


def asked_for_a_flow(container: str, since: int) -> bool:
    """Whether `container` was asked to begin a login flow since the unix
    second `since`: the request a sign-in makes before any page opens."""
    return bool(re.search(r'"POST [^" ]*login/v2[^" ]* HTTP/[\d.]+"',
                          _log_since(container, since)))


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
# The account's calendars are chosen on its row, and saved from the form. The
# conformance suite's seed gives Ada a calendar named this (servers.sh).
CONNECT_CALENDARS = "Connect calendars"
SAVE = "Save account"
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


def connect_account(story, one, server: str, user: str, password: str,
                    container: str, calendar: bool = False) -> None:
    """Ada adds her Nextcloud account in the shell's Settings, and approves the
    app Nextcloud gives this device in her browser. `calendar` also connects the
    calendars she keeps there, choosing them from what the server lists. The
    step is decided by what Settings shows when it settles: the account listed,
    or the product's own words for why it is not."""
    open_accounts(one)
    press_settled(one, ADD_NEXTCLOUD, "Nextcloud among the providers Settings adds")
    until("the Nextcloud sign-in form", lambda: one.target(SERVER_FIELD), 30, 0.2)
    type_into(one, SERVER_FIELD, server, "the server's address")
    story.note("settings-controls", one.targets())
    began_at = int(time.time()) - 5
    press_settled(one, CONTINUE, "the sign-in form's Continue")

    # A sign-in that begins asks the server for a login flow, which the product
    # then opens in the person's browser. Neither is drawn in the pane, so the
    # step reads them where they happen: the server's access log names the
    # flow, and a browser window proves the page opened.
    def began():
        pages = flow_urls_in_log(container, began_at)
        if pages:
            return "page", pages[0]
        if asked_for_a_flow(container, began_at):
            return "asked", ""
        return None

    try:
        kind, path = until("Settings to begin a Nextcloud sign-in", began, 30, 0.5)
    except Failed as error:
        raise Failed(
            f"Settings never reached {server} to begin a Nextcloud sign-in: no request for a "
            f"login flow arrived at the server, and no sign-in page opened. The step's shot "
            f"shows the message the Accounts page drew for the attempt.") from error
    if kind == "asked":
        # The flow exists; the page the person approves it on has to open.
        try:
            path = until("the server's sign-in page to be opened",
                         lambda: next(iter(flow_urls_in_log(container, began_at)), None), 30, 0.5)
        except Failed as error:
            raise Failed(f"Nextcloud gave Settings a login flow, but the page to approve it on "
                         f"never opened: {error}") from error
    story.note("nextcloud-login-page", path)
    try:
        approved = sign_in(urllib.parse.urljoin(server.rstrip("/") + "/", path.lstrip("/")),
                           user, password)
    except SignInFailed as error:
        raise Failed(f"Nextcloud's own sign-in page did not approve this device: {error}") from error
    story.note("nextcloud-approved", approved)

    # Nextcloud's flow answers the app's poll with the app password; Settings
    # keeps the account and lists it. A person sees the account on the page.
    until("the Nextcloud account listed in Settings",
          lambda: one.target("Nextcloud") or next(
              (t for t in one.targets()
               if (t.get("identity") or "").startswith("settings.account:")), None),
          60, 0.25)
    if not calendar:
        return
    # The calendars she keeps on the server are read from it and chosen here:
    # a new account has none kept until the person confirms the choice.
    press_settled(one, CONNECT_CALENDARS, "the account's Connect calendars")

    def choosing():
        if one.target(SEEDED_CALENDAR) and one.target(SAVE):
            return "form", []
        # The form did not come: say what the pane did instead.
        said = [name for name in one.names()
                if any(part in name for part in ("Could not", "check the server",
                                                 "Select at least", "unavailable"))]
        return ("said", said) if said else None

    try:
        kind, said = until("the server's calendars to choose from", choosing, 60, 0.25)
    except Failed as error:
        raise Failed(f"Settings did not list the calendars kept on {server}: {error}. "
                     f"It offers {json.dumps(one.names()[:100])}") from error
    if kind != "form":
        raise Failed(f"Settings could not read the calendars kept on {server}: {said}")
    press_settled(one, SAVE, "the calendar choice's Save")
    until(f"the chosen calendars kept on Ada's computer",
          lambda: SEEDED_CALENDAR in kept_calendar_names(one), 30, 0.25)
    story.note("calendars-kept", kept_calendar_names(one))
