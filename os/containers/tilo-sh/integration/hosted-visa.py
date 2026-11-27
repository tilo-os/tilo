"""A visa application filled in a hosted workspace's Browser, end to end.

The stack's local tilo.sh runs the Browser of a hosted workspace in that
workspace's VM (docs/hosted-browser.md). This is the flow a person and their
agent take there, with the person answering on their own page's question
sheet (integration/person.mjs --answer, integration/page_person.py):

  a. a key opens Browser (`browser#open`); the window it answers with is the
     workspace's, and the time from the open to a live window is printed (the
     VM's cold start), then a second open attaches to the same window;
  b. the agent navigates the window to the visa test page and fills it with
     page actions (`find_page_element`, `fill_page`, `select_page`,
     `click_page`), then reads it back — the key operates web pages, so the
     person is asked nothing and Browser runs every one of them; a person
     invited into the workspace, in their own browser, sees that page on their
     shell and sees it change as the agent fills one more field: what the
     person approving a submit is looking at;
  c. `capture_page` answers a picture of the filled form, saved as a PNG;
  d. the person refuses one submission: the window keeps it, the call ends
     refused, and the form stays filled and unsent — and the sheet they were
     asked on names the page it leaves from, and the form being sent, and
     where it goes;
  e. a second form instance is submitted: the person is asked exactly once,
     the sheet names the submission, and they allow it; the same call returns,
     and the page shows its receipt and reference (`read_page`); then a third
     is allowed on a browser of theirs signed in through a ticket from their
     page, which holds none of their identity (`person.mjs --pair`): the
     person is asked on whichever of their devices they are at;
  e3. the page the person's root is in shows another workspace, and a second
     browser of theirs, signed in by that page and with none of the root,
     is open in the applicant's: the question reaches that browser, and
     allowing there releases the submission. A submission they deny there is
     refused to the agent with the refusal's own code (`not_permitted`), and
     the filled page stays unsent;
  f. a click on the submit button waits and is asked the same way; the sheet
     names what leaves, just as the submission's did;
  g. a reviewer who joined by invitation reads the agent's run in History;
  h. a key minted with the scope unticked is asked about the page actions
     that act on the page, which is what the scope answers for;
  i. past the plans file's free page actions the next one is refused with
     `no_payment_method` and a step to add a card. The card is the account's,
     so the step after this one adds it (integration/plans.py) and holds what
     it releases: this window's next page action, and a Linux command in the
     same workspace's VM.

usage: hosted-visa.py API_URL SERVER_KEY OUT_DIR FORM_URL [--open|--page|--fill]

`--page` opens the visa test page in the window `--open` made and stops
there, so what renders in the VM is what its proofs of the sandbox and the
memory read.

SERVER_KEY is the startup's server key (`tilo do account/keys#create`). OUT_DIR keeps
what it found: the capture, the page as the agent read it back, the reviewer's
History, the questions the person was asked, and its report. TILO_INVITEE
names the stack's reviewer helper (integration/reviewer.py);
HOSTED_VISA_UNSCOPED_KEY is a key of the same person that may not operate web
pages in Browser. HOSTED_VISA_PAGE_ASKS and HOSTED_VISA_PAGE_ANSWERS name the
page's asks FIFO and the file it answers in, and HOSTED_VISA_SITE and
HOSTED_VISA_SITE_SERVER the site and the server a person enters through, for
the person who answers on their page and for the one who watches the page on
their shell (integration/person.mjs).

Prints one JSON line with the timings and what it asserted.
"""

from __future__ import annotations

import asyncio
import base64
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

from pathlib import Path

from page_person import PagePerson
from tilo import Refused, Tilo, run

API_URL, SERVER_KEY, OUT, FORM_URL = sys.argv[1:5]

RUN_TITLE = "Fill Jane Doe's visa application"
REVIEWER_EMAIL = "rosa@example.com"
# The reviewer's helper gives a reviewer 48 steps of the page drivers'
# `TILO_STEP_SECONDS` and stops one who outlasts them (reviewer.py); three
# steps more hear its answer, the person it stopped included, instead of
# timing out ahead of its page.
STEP_SECONDS = float(os.environ.get("TILO_STEP_SECONDS") or 5)
REVIEWER_SECONDS = 51 * STEP_SECONDS
UNSCOPED_KEY = os.environ.get("HOSTED_VISA_UNSCOPED_KEY", "")
# The person in their own browser, watching the hosted Browser's page on their
# shell: the site they arrive at, the server it enters through, and the window
# they watch, by the name the shell gives the app that hosts it.
SITE = os.environ.get("HOSTED_VISA_SITE", "")
SITE_SERVER = os.environ.get("HOSTED_VISA_SITE_SERVER", "")
# The `tilo` command the check built, which the browser it signs in runs
# `tilo device add` with, on a computer that holds none of the person's root.
TILO = os.environ.get("HOSTED_VISA_TILO", "")
SHELL_EMAIL = "shell-person@example.invalid"
# The tile on their shell, by the name the shell gives it: the hosted window is
# the workspace's Browser, and a shell that names it by the page it shows names
# it by the page's title.
SHELL_WINDOW = "browser|visa"
PERSON_PAGE = Path(__file__).with_name("person.mjs")
# The person's own page, where it stays open on the site (person.mjs
# --answer): the questions of the submissions are answered on its sheet, and
# what each sheet drew is kept beside its answers.
PAGE_ASKS = os.environ.get("HOSTED_VISA_PAGE_ASKS", "")
PAGE_ANSWERS = os.environ.get("HOSTED_VISA_PAGE_ANSWERS", "")
# The workspace the person's page is open on, which is not the one their
# agent works in: what the waiting page is put to them on a device of theirs,
# wherever their page is showing.
PAGE_WORKSPACE = os.environ.get("HOSTED_VISA_PAGE_WORKSPACE", "")
# What the agent puts in the page's fields, by the name the page gives each
# of them: the words of its label, which is what `find_page_element` matches.
FILLED = {
    "Given names": "Jane",
    "Family name": "Doe",
    "Date of birth": "1990-04-17",
    "Email": "jane@example.com",
    "Passport number": "X1234567",
    "Expires": "2031-04-16",
    "Arrival": "2026-12-01",
    "Departure": "2026-12-14",
    "Company or address you will visit": "Tilo GmbH, Berlin",
}
# The choice fields, by the name the page gives them and the label of what
# the agent picks: the page reads a select back by the option's label.
CHOSEN = {"Nationality": "Germany", "Purpose": "Business"}
DECLARATION = "I declare that the information above is true"
SUBMIT = "Submit application"
RECEIPT = "Application received"

# The shells this run opened, stopped whichever way the run ends.
SHELLS: list[Shell] = []
# The browsers the person paired this run, stopped whichever way it ends.
BROWSERS: list[PairedBrowser] = []


def say(step: str, detail: str = "") -> None:
    print(f"[{time.strftime('%H:%M:%S')}] {step}" + (f": {detail}" if detail else ""), flush=True)


def check(condition: object, what: str) -> None:
    if not condition:
        raise AssertionError(what)
    print(f"    ok  {what}", flush=True)


def write(name: str, data: bytes) -> str:
    path = os.path.join(OUT, name)
    with open(path, "wb") as out:
        out.write(data)
    return path


def write_json(name: str, value: object) -> str:
    return write(name, json.dumps(value, indent=2, sort_keys=True, default=str).encode())


def content_ref(value: object) -> str | None:
    """The first `tilo.content:` reference anywhere in an answer."""
    if isinstance(value, str):
        return value if value.startswith("tilo.content:") else None
    if isinstance(value, dict):
        value = list(value.values())
    if isinstance(value, list):
        for item in value:
            found = content_ref(item)
            if found:
                return found
    return None


async def outcome(call: object) -> object:
    """What a page action answered: its result, or the run it waits in,
    followed to its end (a submission that waits is answered when the person
    does)."""
    answer = await call
    if not isinstance(answer, dict):
        return answer
    result = answer.get("result")
    if result is not None:
        return result
    made = [
        reference
        for reference in answer.get("resources") or []
        if isinstance(reference, str) and reference.startswith("tilo.content:")
    ]
    return made[0] if len(made) == 1 else made or None


async def refusal_of(call: object) -> str:
    """How the API refused `call`: its code and message and the steps it
    named, or `allowed`."""
    try:
        await outcome(call)
    except Refused as error:
        steps = ", ".join(error.next_steps or [])
        return f"{error.code}: {error.message}" + (f" (next: {steps})" if steps else "")
    return "allowed"


def code_of(text: str) -> str:
    return text.split(":", 1)[0].strip()


def origin_of(url: str) -> str:
    """The origin of `url`: the page a departure leaves from, as a window
    reports it."""
    parts = urllib.parse.urlsplit(url)
    return f"{parts.scheme}://{parts.netloc}"


def shown_by(question: dict) -> str:
    """What the sheet drew for `question`, as the person reads it."""
    return "\n".join(question.get("shown") or [])


def check_leaves(question: dict, what: str) -> None:
    """Say the sheet the person was shown names what leaves the page: the
    page it leaves from, and where what it sends goes. The sheet says it in
    the person's words — "send the form to <site>" — and keeps the method out
    of them (`PageDeparture::doing`: a person thinks in sending and opening,
    not in verbs), so a submission reads as a form sent, never an open. The
    question the sheet was drawn from still holds what leaves exactly: the
    method it goes with and the picture of the filled page the person is
    shown, as the page records it beside what the sheet draws (`page`)."""
    shown = shown_by(question)
    origin = origin_of(FORM_URL)
    check(
        origin in shown,
        f"the sheet about {what} names the page it leaves from ({shown})",
    )
    check(
        f"send the form to {origin}" in shown,
        f"the sheet about {what} names the submission and where it goes ({shown})",
    )
    page = question.get("page") or {}
    check(page.get("method") == "POST", f"the question about {what} names the method ({page})")
    check(page.get("destination"), f"the question about {what} names where it goes ({page})")
    check(page.get("picture"), f"the question about {what} shows the filled page ({page})")


class PairedBrowser(PagePerson):
    """A second browser of the person's, signed in to them through a ticket by
    their root's page and holding none of it, open in the workspace their agent
    works in (integration/person.mjs --pair --answer). The page their root is
    in shows another workspace, so what the agent holds is put to this one."""

    def __init__(self, workspace: str, directory: str) -> None:
        check(bool(SITE and SITE_SERVER), "the check names the site and the server the person's browsers enter through")
        check(bool(TILO), "the check names the tilo command the browser signs in with (HOSTED_VISA_TILO)")
        Path(directory).mkdir(parents=True, exist_ok=True)
        self.log = Path(directory) / "browser.log"
        self.out = Path(directory) / "browser.out"
        asks = str(Path(directory) / "asks")
        answers = str(Path(directory) / "answers.jsonl")
        for stale in (asks, answers):
            try:
                os.unlink(stale)
            except FileNotFoundError:
                pass
        os.mkfifo(asks)
        # Opened read-write, as check.sh opens the person's own FIFO: a
        # write never waits for the browser to reach a line, and one that
        # comes as it stops is kept rather than refused.
        self.fifo = os.open(asks, os.O_RDWR)
        self.stdout = open(self.out, "w")
        self.stderr = open(self.log, "w")
        self.workspace = workspace
        self.ran = subprocess.Popen(
            ["node", str(PERSON_PAGE), SITE, SITE_SERVER, directory, "--pair", workspace,
             "--tilo", TILO, "--answer", asks],
            stdout=self.stdout, stderr=self.stderr,
        )
        super().__init__(asks, answers)
        self.stopped = False
        # Listed before it is waited for: a browser that never signs in is
        # still this run's to stop.
        BROWSERS.append(self)
        self.report: dict = {}

    def paired(self) -> dict:
        """The report the browser printed once it was signed in and in."""
        deadline = time.monotonic() + 420
        while time.monotonic() < deadline:
            try:
                text = self.out.read_text()
            except OSError:
                text = ""
            for line in text.splitlines():
                try:
                    said = json.loads(line)
                except ValueError:
                    continue
                if isinstance(said, dict) and said.get("paired"):
                    return said
            if self.ran.poll() not in (None, 0):
                raise AssertionError(
                    f"the signed-in browser ended ({self.ran.returncode}): "
                    f"{self.log.read_text()[-2000:]}"
                )
            time.sleep(0.2)
        raise AssertionError(f"the browser was not signed in in 420 s (see {self.log})")

    def stop(self) -> None:
        """Its browser goes, as one a person closes does."""
        if self.stopped:
            return
        self.stopped = True
        try:
            with open(self.asks, "w") as asks:
                asks.write("done\n")
        except OSError:
            pass
        try:
            self.ran.wait(timeout=45)
        except subprocess.TimeoutExpired:
            self.ran.terminate()
            try:
                self.ran.wait(timeout=30)
            except subprocess.TimeoutExpired:
                self.ran.kill()
            self.ran.wait()
        self.stdout.close()
        self.stderr.close()
        os.close(self.fifo)
        self.fifo = -1


class Page:
    """The workspace's Browser window, as an agent addresses it: the app's
    name finds its window, the one the workspace runs. `record` is the record
    the open made, which a second open answers again."""

    def __init__(self, workspace: object, record: str) -> None:
        self.workspace = workspace
        self.record = record

    def path(self, action: str) -> str:
        return f"browser/page#{action}"

    async def do(self, action: str, **args: object) -> object:
        return await run(self.workspace, {"path": "browser/page", "action": action}, args)

    async def find(self, role: str, name: str, nth: int = 1) -> str:
        """The ref of the `nth` `role` on the page answering to `name`."""
        answer = await outcome(self.do("find_page_element", role=role, name=name, nth=nth))
        element = answer.get("element") if isinstance(answer, dict) else None
        check(isinstance(element, str) and element, f"the page names {role} {name!r} as {element}")
        return str(element)

    async def read(self) -> dict:
        answer = await outcome(self.do("read_page"))
        return answer if isinstance(answer, dict) else {}

    async def title(self) -> str:
        return str((await self.read()).get("title") or "")


class Shell:
    """A person in their own browser watching the hosted Browser's page on
    their shell (integration/person.mjs --page).

    The window is another device's, so what the person sees of it is the page
    the session in the VM publishes: the check asks this person to keep a
    picture of it, and every picture that differs from the one before is a
    change they saw. It says those as lines while the check goes on working
    the page, which is how the person's sight of the agent's work is checked,
    and not only the fact that they could have looked."""

    def __init__(self, link: str) -> None:
        check(bool(SITE and SITE_SERVER), "the check names the site and the server a person enters through")
        self.lines = Path(OUT) / "shell.jsonl"
        self.log = Path(OUT) / "shell.log"
        self.out = open(self.lines, "w")
        self.err = open(self.log, "w")
        self.seen = 0
        self.ran = subprocess.Popen(
            ["node", str(PERSON_PAGE), SITE, SITE_SERVER, str(Path(OUT) / "shell"),
             "--follow", link, "--page", SHELL_WINDOW, "--changes", "8"],
            stdout=self.out, stderr=self.err,
        )
        SHELLS.append(self)

    def said(self) -> list[dict]:
        try:
            text = self.lines.read_text()
        except OSError:
            return []
        events = []
        for line in text.splitlines():
            try:
                event = json.loads(line)
            except ValueError:
                continue
            if isinstance(event, dict):
                events.append(event)
        return events

    def wait(self, kind: str, within: float = 180) -> dict:
        """The next `kind` the person's shell says (`shell`, or each `change` in
        turn), waited for `within` seconds."""
        deadline = time.monotonic() + within
        while time.monotonic() < deadline:
            events = self.said()
            if kind == "shell":
                for event in events:
                    if "shell" in event:
                        return event["shell"]
            else:
                changes = [event["change"] for event in events if "change" in event]
                if len(changes) > self.seen:
                    self.seen += 1
                    return changes[self.seen - 1]
            if self.ran.poll() not in (None, 0):
                raise AssertionError(f"the person's browser ended ({self.ran.returncode}): {self.log.read_text()[-2000:]}")
            time.sleep(0.2)
        raise AssertionError(f"the person's shell said no {kind} in {within:.0f} s (see {self.log})")

    def stop(self) -> None:
        """Their browser goes, as one a person closes does: the watcher is
        told to finish, and is stopped only if it does not."""
        try:
            (Path(OUT) / "shell" / "stop").write_text("")
        except OSError:
            pass
        try:
            self.ran.wait(timeout=45)
        except subprocess.TimeoutExpired:
            if self.ran.poll() is None:
                self.ran.terminate()
                try:
                    self.ran.wait(timeout=30)
                except subprocess.TimeoutExpired:
                    self.ran.kill()
        self.out.close()
        self.err.close()


async def open_browser(workspace: object) -> tuple[Page, float]:
    """Open the workspace's Browser and answer its window once it is live.

    The open runs until the window is live (the server places the session and
    waits for it), so following its run to its end is the time from the open
    to a live window. The window's actions reach the workspace a moment after,
    so the listing is asked again until they do, and the last refusal is what
    a window that never published says."""
    began = time.perf_counter()
    opened = await outcome(run(workspace, {"path": "browser", "action": "open"}))
    live = time.perf_counter() - began
    say("   opened", str(opened)[:200])
    rows: list[str] = []
    refused = ""
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        try:
            listing = await workspace.list_resources(path="browser")
        except Refused as error:
            refused = str(error)
            await asyncio.sleep(1)
            continue
        # The listing an agent reads: what the running window offers.
        rows = [str(row["path"]) for row in listing["rows"]]
        if rows:
            break
        await asyncio.sleep(0.2)
    if not rows:
        try:
            apps = await workspace.list_resources()
            say(
                "   the workspace's apps",
                "; ".join(f"{row['path']} {row.get('state')}" for row in apps["rows"]),
            )
        except Refused as error:
            say("   the workspace's apps were not listed", str(error))
    check(rows, f"the workspace's Browser window is live ({rows}; last refusal: {refused})")
    say("   listed", ", ".join(rows)[:200])
    record = str(opened.get("record") or "") if isinstance(opened, dict) else ""
    check(record, f"the open names the record its window is on ({opened})")
    return Page(workspace, record), live


async def fill(page: Page) -> str:
    """Fill the form the agent has open, through the page's own actions, and
    answer the ref of the button that submits it."""
    await outcome(page.do("navigate", url=FORM_URL))
    await outcome(page.do("wait_page_settled", quiet_ms=300, timeout_ms=60000))
    title = await page.title()
    check("Visa application" in title, f"the form is the page the agent opened ({title!r})")
    for name, value in FILLED.items():
        element = await page.find("textbox", name)
        await outcome(page.do("fill_page", element=element, text=value))
    element = await page.find("combobox", "Nationality")
    await outcome(page.do("select_page", element=element, values=[CHOSEN["Nationality"]]))
    element = await page.find("radio", CHOSEN["Purpose"])
    await outcome(page.do("click_page", element=element))
    element = await page.find("checkbox", DECLARATION)
    await outcome(page.do("click_page", element=element))
    return await page.find("button", SUBMIT)


def post_invitee(link: str) -> dict:
    """The stack's reviewer helper: the invitation is posted to it and it
    answers the reviewer's own key (integration/reviewer.py)."""
    endpoint = os.environ.get("TILO_INVITEE")
    if not endpoint:
        raise SystemExit("TILO_INVITEE names the stack's reviewer helper")
    request = urllib.request.Request(
        endpoint,
        data=json.dumps({"link": link}).encode(),
        headers={"content-type": "application/json"},
    )
    try:
        with urllib.request.urlopen(request, timeout=REVIEWER_SECONDS) as answer:
            return json.load(answer)
    except urllib.error.HTTPError as error:
        said = json.load(error) if error.headers.get_content_type() == "application/json" else {}
        raise SystemExit(said.get("error") or f"the reviewer did not get in ({error.code}): {error.reason}") from error
    except TimeoutError as error:
        raise SystemExit(
            f"the reviewer did not get in: its helper did not answer in {REVIEWER_SECONDS:.0f} s"
        ) from error


async def open_phase(report: dict[str, object]) -> int:
    """The workspace, its agent's key, and Browser opened in its VM."""
    backend = Tilo(key=SERVER_KEY, url=API_URL)
    say("a. the startup makes an applicant's workspace and mints their agent's key")
    workspace = (
        await run(
            backend,
            {"path": "account/workspaces", "action": "create"},
            {"name": "Visa applicant: Jane Doe"},
        )
    )["result"]["workspace"]
    report["workspace"] = workspace["id"]
    minted = (
        await run(
            backend,
            {"path": "account/keys", "action": "mint"},
            {"workspace": workspace["id"], "name": "Jane Doe's agent"},
        )
    )["result"]
    check(
        minted["mcp_url"].endswith(f"/w/{workspace['id']}/mcp"),
        "the key reaches that workspace alone",
    )
    agent = Tilo(key=minted["token"], url=API_URL)
    aws = agent.workspace(workspace["id"])

    say("a. the key opens Browser in the workspace's VM")
    page, live = await open_browser(aws)
    report["open_seconds"] = round(live, 2)
    say("   first open", f"{live:.1f} s from the open to a live window (the VM's cold start)")
    attached_began = time.perf_counter()
    again, _ = await open_browser(aws)
    attached = time.perf_counter() - attached_began
    report["attach_seconds"] = round(attached, 2)
    check(again.record == page.record, f"a second open attaches to the same window ({attached:.2f} s)")
    report["window"] = page.record
    with open(os.path.join(OUT, "open.json"), "w") as opened:
        json.dump(
            {
                "workspace": workspace["id"],
                "key": minted["token"],
                "window": page.record,
            },
            opened,
        )
    await agent.aclose()
    await backend.aclose()
    return 0


async def page_phase(report: dict[str, object]) -> int:
    """The visa page open in the window the open made: what renders in the
    VM, which is what the proofs of its sandbox and memory read."""
    with open(os.path.join(OUT, "open.json")) as opened:
        state = json.load(opened)
    agent = Tilo(key=state["key"], url=API_URL)
    aws = agent.workspace(state["workspace"])
    page = Page(aws, str(state["window"]))
    say("b. the agent opens the visa form in the window")
    began = time.perf_counter()
    await outcome(page.do("navigate", url=FORM_URL))
    await outcome(page.do("wait_page_settled", quiet_ms=300, timeout_ms=60000))
    title = await page.title()
    check("Visa application" in title, f"the form is the page the agent opened ({title!r})")
    report["page_seconds"] = round(time.perf_counter() - began, 2)
    say("   open", f"the form was open in the window in {report['page_seconds']} s")
    await agent.aclose()
    return 0


async def fill_phase(report: dict[str, object]) -> int:
    """The form filled with nothing asked, refused once, submitted once, and
    a click that waits; the reviewer's History; the scope; and the metering."""
    with open(os.path.join(OUT, "open.json")) as opened:
        state = json.load(opened)
    workspace = state["workspace"]
    agent = Tilo(key=state["key"], url=API_URL)
    aws = agent.workspace(workspace)
    page = Page(aws, str(state["window"]))
    person = PagePerson(PAGE_ASKS, PAGE_ANSWERS, check=check)
    reviewer = None

    say("b. the agent fills the visa form through page actions, and the person is asked nothing")
    async with aws.invocation(RUN_TITLE):
        submit = await fill(page)
        read_back = await page.read()
        body = json.dumps(read_back)
        missing = [value for value in FILLED.values() if value not in body]
        check(not missing, f"the page holds every value the agent filled (missing: {missing})")
        check(CHOSEN["Nationality"] in body, "the page holds the nationality the agent chose")
        check(CHOSEN["Purpose"].lower() in body.lower(), "the page holds the purpose the agent chose")
        write_json("filled-page.json", read_back)
        await person.asked_nothing()
        report["questions_while_filling"] = person.answered()

        say("b2. a person invited into the workspace sees the agent's page on their shell")
        shell = Shell(
            (
                await run(
                    aws,
                    {"path": "workspace/invitations", "action": "invite"},
                    {"role": "editor", "invitee_name": SHELL_EMAIL},
                )
            )["result"]["link"]
        )
        on_shell = await asyncio.to_thread(shell.wait, "shell", 240)
        check(
            on_shell.get("content", 0) >= 0.02,
            f"the hosted Browser's page is drawn on the person's shell, not a card waiting for one ({on_shell})",
        )
        report["shell"] = on_shell
        say("   drawn", f"the page was on their shell in {on_shell.get('drawnMs')} ms, {on_shell.get('content')} of it its content")
        # The agent fills a field while they watch: what the person approving
        # the submit sees happen on the page before they are asked about it.
        element = await page.find("textbox", "Given names")
        await outcome(page.do("fill_page", element=element, text=f"{FILLED['Given names']} Wilhelmine"))
        seen = await asyncio.to_thread(shell.wait, "change", 60)
        check(int(seen.get("moved", 0)) > 150, f"the agent's fill reached the person's shell ({seen})")
        say("   seen", f"the fill changed {seen['moved']} pixels of the page on their shell, {seen['ms']} ms after the change before")
        report["shell_fill"] = seen
        # What the rest of the flow asserts, back in its field.
        element = await page.find("textbox", "Given names")
        await outcome(page.do("fill_page", element=element, text=FILLED["Given names"]))
        await asyncio.to_thread(shell.wait, "change", 60)

        say("c. the agent captures the filled form")
        captured = await outcome(page.do("capture_page"))
        ref = content_ref(captured)
        if ref:
            saved = write("filled-form.png", await aws.download(ref))
        else:
            inline = (captured or {}).get("data", {}).get("png") if isinstance(captured, dict) else None
            check(inline, f"capture_page answers the page's picture ({str(captured)[:200]})")
            saved = write("filled-form.png", base64.b64decode(inline))
        with open(saved, "rb") as picture:
            head = picture.read(8)
        check(head.startswith(b"\x89PNG"), f"the capture is a PNG ({saved})")
        report["capture"] = saved

        say("d. the person refuses a submission, and the form does not leave")
        asked = person.begin("deny", "submission-denied")
        began = time.perf_counter()
        refused = await refusal_of(page.do("submit_page", element=submit))
        report["deny_seconds"] = round(time.perf_counter() - began, 2)
        questions = await person.settle(asked)
        check(len(questions) == 1, f"the person was asked exactly once ({questions})")
        check("send the form" in shown_by(questions[0]).lower(), f"the sheet names the submission ({shown_by(questions[0])})")
        check_leaves(questions[0], "a submission")
        check(refused != "allowed", f"the submission the person refused did not run ({refused})")
        after = await page.read()
        check(RECEIPT not in await page.title(), "the page shows no receipt after a refusal")
        check(
            all(value in json.dumps(after) for value in FILLED.values()),
            "the form is still filled, and unsent",
        )
        report["denied"] = refused

        say("e. a second form instance is submitted, and the person allows it")
        submit = await fill(page)
        asked = person.begin("allow", "submission-allowed")
        began = time.perf_counter()
        run = await outcome(page.do("submit_page", element=submit))
        report["allow_seconds"] = round(time.perf_counter() - began, 2)
        check("released" in json.dumps(run), f"the submission ran once the person allowed it ({run})")
        questions = await person.settle(asked)
        check(len(questions) == 1, f"the person was asked exactly once ({questions})")
        check("send the form" in shown_by(questions[0]).lower(), f"the sheet names the submission ({shown_by(questions[0])})")
        receipt = await page.read()
        write_json("receipt-page.json", receipt)
        check(RECEIPT in json.dumps(receipt), "the page shows the application was received")
        check("TV-" in json.dumps(receipt), f"the page shows the reference it made ({json.dumps(receipt)[:300]})")
        report["receipt"] = True

        say("e2. a browser the person signs in through a ticket, with none of their identity, allows a submission there")
        phone = PairedBrowser(workspace, os.path.join(OUT, "phone"))
        person.sign_in(workspace)
        phone.report = phone.paired()
        check(
            phone.report.get("workspace") == workspace,
            f"the signed-in browser is in the agent's workspace ({phone.report.get('workspace')})",
        )
        submit = await fill(page)
        asked = phone.begin("allow", "phone-allowed")
        began = time.perf_counter()
        run = await outcome(page.do("submit_page", element=submit))
        report["phone_allow_seconds"] = round(time.perf_counter() - began, 2)
        check("released" in json.dumps(run), f"the submission ran once the person allowed it on their paired browser ({run})")
        questions = await phone.settle(asked)
        check(len(questions) == 1, f"the browser the page let in was asked exactly once ({questions})")
        check_leaves(questions[0], "a submission answered on a browser the page let in")
        phone.stop()
        person.asked.extend(questions)
        # The page the person allowed the submission of shows its receipt,
        # and so does the page on their shell.
        seen = await asyncio.to_thread(shell.wait, "change", 60)
        check(int(seen.get("moved", 0)) > 500, f"the receipt reached the person's shell ({seen})")
        say("   seen", f"the receipt changed {seen['moved']} pixels of the page on their shell")
        report["shell_receipt"] = seen
        shell.stop()

        say("e3. the root's page shows another workspace, and the browser the person signed in is asked there")
        check(
            PAGE_WORKSPACE and PAGE_WORKSPACE != workspace,
            f"the page holding the person's root shows another workspace ({PAGE_WORKSPACE}), not the agent's ({workspace})",
        )
        paired = PairedBrowser(workspace, os.path.join(OUT, "paired-browser"))
        person.sign_in(workspace)
        paired.report = paired.paired()
        check(
            paired.report.get("workspace") == workspace,
            f"the signed-in browser is in the agent's workspace ({paired.report.get('workspace')})",
        )
        report["paired_browser"] = {
            key: paired.report.get(key) for key in ("workspace", "pairedMs", "paired")
        }
        submit = await fill(page)
        asked = paired.begin("allow", "browser-allowed")
        began = time.perf_counter()
        started = await aws.run_action(
            "browser/page", "submit_page", {"element": submit}
        )
        if (
            isinstance(started, dict)
            and started.get("status") == "pending"
            and isinstance(started.get("invocation"), str)
        ):
            # The agent following it reads where it stands while it waits,
            # not only that nothing answered yet.
            report["while_waiting"] = await aws.get_invocation(started["invocation"])
            released = started
            while isinstance(released, dict) and released.get("status") == "pending":
                released = await aws.get_invocation(released["invocation"], wait="60")
        released = released.get("result") if isinstance(released, dict) else released
        report["browser_allow_seconds"] = round(time.perf_counter() - began, 2)
        check(
            "released" in json.dumps(released),
            f"the submission ran once the person allowed it on their paired browser ({released})",
        )
        questions = await paired.settle(asked)
        check(len(questions) == 1, f"the paired browser was asked exactly once ({questions})")
        check_leaves(questions[0], "a submission answered on a browser paired to the person")
        check(
            not [seen for seen in person.seen if seen.get("name") == "browser-allowed"],
            "the answer came from the paired browser, not from the page holding the root",
        )
        allowed_page = await page.read()
        check(RECEIPT in json.dumps(allowed_page), "the page shows the application was received")
        say(
            "   allowed",
            f"the person allowed it on their paired browser in {report['browser_allow_seconds']} s",
        )

        say("e4. a submission the person denies on their paired browser is refused to the agent by name")
        submit = await fill(page)
        asked = paired.begin("deny", "browser-denied")
        refused = await refusal_of(page.do("submit_page", element=submit))
        questions = await paired.settle(asked)
        check(len(questions) == 1, f"the paired browser was asked exactly once ({questions})")
        check_leaves(questions[0], "a submission denied on a browser paired to the person")
        check(
            code_of(refused) == "not_permitted",
            f"the agent's call is refused with the refusal's own code when the person denies it ({refused})",
        )
        denied_page = await page.read()
        check(RECEIPT not in json.dumps(denied_page), "the page shows no receipt after the denial")
        check(
            all(value in json.dumps(denied_page) for value in FILLED.values()),
            "the form is still filled, and unsent",
        )
        report["browser_denied"] = refused
        paired.stop()

        say("f. a click on the submit button waits, and the person is asked once")
        submit = await fill(page)
        asked = person.begin("allow", "click-allowed")
        clicked_once = await outcome(page.do("click_page", element=submit))
        check("released" in json.dumps(clicked_once), f"the click left once the person allowed it ({clicked_once})")
        questions = await person.settle(asked)
        check(len(questions) == 1, f"the person was asked exactly once ({questions})")
        check_leaves(questions[0], "a click on the submit button")
        clicked = await page.read()
        check(RECEIPT in json.dumps(clicked), "the click's submission left once the person allowed it")
        report["click_receipt"] = True

        say("g. a reviewer who joined by invitation reads the agent's run in History")
        invitation = (
            await run(
                aws,
                {"path": "workspace/invitations", "action": "invite"},
                {"role": "editor", "invitee_name": REVIEWER_EMAIL},
            )
        )["result"]
        say("   invited", f"{invitation['role']} via {invitation['link'][:60]}…")
        accepted = await asyncio.to_thread(post_invitee, invitation["link"])
        check(
            (accepted.get("joined") or {}).get("role") == "editor",
            f"the reviewer joined as an editor: {accepted}",
        )
        reviewer = Tilo(key=accepted["key"], url=API_URL)
        rws = reviewer.workspace(workspace)
        listing = await rws.list_resources(path="browser")
        check(
            bool(listing["rows"]),
            "the reviewer's listing names the hosted Browser window their shell shows",
        )
        history = (
            await run(rws, {"path": "browser", "action": "history"}, {"last": 50})
        ).get("result")
        write_json("reviewer-history.json", history)
        # A page action runs as the caller's and changes no document, so the
        # app's history answers whether or not it holds a step of the run.
        report["history_by_reviewer"] = True
        report["history_names_run"] = RUN_TITLE in json.dumps(history)

    say("h. a key that may not operate web pages is asked about each page action")
    check(UNSCOPED_KEY, "the startup's key without the Browser scope is named")
    unscoped = Tilo(key=UNSCOPED_KEY, url=API_URL)
    ows = unscoped.workspace(workspace)
    other, _ = await open_browser(ows)
    async with ows.invocation(RUN_TITLE):

        async def asked_once(action: str, call: object) -> object:
            """Run one page action of the key without the scope, and check the
            person to being asked about it once."""
            asked = person.begin("allow", f"page-{action}")
            answer = await outcome(call)
            questions = await person.settle(asked)
            check(len(questions) == 1, f"the key without the scope is asked about {action} ({questions})")
            return answer

        # Taking the window somewhere asks; reading the page it is on does
        # not (the agent's window holds none of the person's sign-ins).
        await asked_once("navigate", other.do("navigate", url=FORM_URL))
        await outcome(other.do("wait_page_settled", quiet_ms=300, timeout_ms=60000))
        found = await outcome(other.do("find_page_element", role="textbox", name="Given names"))
        element = found.get("element") if isinstance(found, dict) else None
        check(isinstance(element, str) and element, f"the page names the field it found ({found})")
        await asked_once("fill_page", other.do("fill_page", element=element, text=FILLED["Given names"]))
    report["unscoped_questions"] = 2
    await unscoped.aclose()

    say("i. metering: the plans file's free page actions are used up")
    refused = "allowed"
    for _ in range(600):
        refused = await refusal_of(page.do("read_page"))
        if refused != "allowed":
            break
    check(
        "no_payment_method" in refused or "free Browser actions are used up" in refused,
        f"past the free allowance a page action is refused with no_payment_method ({refused})",
    )
    check(
        "add_payment_method" in refused or "card" in refused.lower(),
        f"the refusal says a card continues, and how ({refused})",
    )
    report["metered_refusal"] = refused
    # The provider's card is added next, and the step that adds it holds what
    # it releases: this window's next page action and a Linux command in the
    # workspace's VM (integration/hosted-terminal.py).

    report["questions_asked"] = person.answered()
    write_json("questions.json", person.asked)
    report["answered_on_page"] = person.seen

    for client in (agent, reviewer):
        if client is None:
            continue
        try:
            await client.aclose()
        except Refused:
            pass
    return 0


async def main() -> int:
    os.makedirs(OUT, exist_ok=True)
    report: dict[str, object] = {"form": FORM_URL}
    # The open's own facts stay in the report the fill phase writes: one
    # report of the whole flow, whichever phase ran.
    previous = os.path.join(OUT, "report.json")
    if os.path.isfile(previous):
        with open(previous) as saved:
            report.update(json.load(saved))
    phase = sys.argv[5] if len(sys.argv) > 5 else "--all"
    try:
        if phase == "--open":
            status = await open_phase(report)
        elif phase == "--page":
            status = await page_phase(report)
        elif phase == "--fill":
            status = await fill_phase(report)
        else:
            status = await open_phase(report)
            if status == 0:
                status = await fill_phase(report)
    finally:
        # The person's browser goes with the run, whichever way it ends: a
        # shell left open would sit in the stack for the next step.
        for shell in SHELLS:
            shell.stop()
        for browser in BROWSERS:
            browser.stop()
    write_json("report.json", report)
    print(json.dumps(report), flush=True)
    return status


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
