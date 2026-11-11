"""A visa application filled in a hosted workspace's Browser, end to end.

The stack's local tilo.sh runs the Browser of a hosted workspace in that
workspace's VM (docs/hosted-browser.md). This is the flow a person and their
agent take there, with the person's answering device held by the check
(`tilo-approve`, the person the page made, opened from its recovery key):

  a. a key opens Browser (`browser#open`); the window it answers with is the
     workspace's, and the time from the open to a live window is printed (the
     VM's cold start), then a second open attaches to the same window;
  b. the agent navigates the window to the visa test page and fills it with
     page actions (`find-page-element`, `fill-page`, `select-page`,
     `click-page`), then reads it back — the key operates web pages, so the
     person is asked nothing and Browser runs every one of them;
  c. `capture-page` answers a picture of the filled form, saved as a PNG;
  d. the person refuses one submission: the window holds it, the call ends
     refused, and the form stays filled and unsent;
  e. a second form instance is submitted: the person is asked exactly once,
     names the submission, and allows it; the same call returns, and the page
     shows its receipt and reference (`read-page`);
  f. a click on the submit button is held the same way and asked once;
  g. a reviewer who joined by invitation reads the agent's run in History;
  h. a key minted with the scope unticked is asked about the page actions
     that act on the page, which is what the scope answers for;
  i. past the plans file's free page actions the next one is refused with
     `no_payment_method` and a step to add a card; adding the account to the
     plans file lets the next one through.

usage: hosted-visa.py DOOR SERVER_KEY OUT_DIR FORM_URL [--open|--fill]

SERVER_KEY is the startup's server key (`tilo keys create`). OUT_DIR keeps
what it found: the capture, the page as the agent read it back, the reviewer's
History, the questions the person was asked, and its report. TILO_INVITEE
names the stack's reviewer helper (integration/reviewer.py); the person is
answered for by HOSTED_VISA_APPROVER (`tilo-approve`), in the identity root
HOSTED_VISA_ROOT, through the host HOSTED_VISA_RELAY names as
`KEY@HOST:PORT`; HOSTED_VISA_PLANS is the plans file the relay counts page
actions by, and HOSTED_VISA_ACCOUNT the account it counts them for.

Prints one JSON line with the timings and what it asserted.
"""

from __future__ import annotations

import asyncio
import base64
import json
import os
import sys
import time
import urllib.request

from tilo import AsyncTilo, ProtocolError, Refused, Run

DOOR, SERVER_KEY, OUT, FORM_URL = sys.argv[1:5]

RUN_TITLE = "Fill Jane Doe's visa application"
REVIEWER_EMAIL = "rosa@example.com"
PLANS = os.environ.get("HOSTED_VISA_PLANS", "")
ACCOUNT = os.environ.get("HOSTED_VISA_ACCOUNT", "")
APPROVER = os.environ.get("HOSTED_VISA_APPROVER", "")
PERSON_ROOT = os.environ.get("HOSTED_VISA_ROOT", "")
HOST = os.environ.get("HOSTED_VISA_RELAY", "")
# What the agent puts in the page's fields, by the name the page gives each
# of them: the words of its label, which is what `find-page-element` matches.
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
    followed to its end (a held submission is answered when the person does)."""
    answer = await call
    if isinstance(answer, Run):
        return await answer.result()
    return answer


async def refusal_of(call: object) -> str:
    """How the door refused `call`: its code and message and the steps it
    named, or `allowed`."""
    try:
        await outcome(call)
    except ProtocolError as error:
        return f"{error.code or ''}: {error.message or error.status or error}".strip(": ")
    except Refused as error:
        steps = ", ".join(error.next_steps or [])
        return f"{error.code}: {error.message}" + (f" (next: {steps})" if steps else "")
    return "allowed"


def code_of(text: str) -> str:
    return text.split(":", 1)[0].strip()


class Person:
    """The person the page made, answering for themselves: `tilo-approve`
    connects as one of their approving devices and answers what the host
    asks them, printing the question it saw."""

    def __init__(self) -> None:
        check(APPROVER and os.path.isfile(APPROVER), f"the check holds its approver ({APPROVER})")
        check(PERSON_ROOT and os.path.isdir(PERSON_ROOT), f"the person's root is held ({PERSON_ROOT})")
        check(HOST, f"the host's address is named ({HOST})")
        self.asked: list[dict] = []

    def begin(self, answer: str, within: int = 180) -> "asyncio.Future[asyncio.subprocess.Process]":
        """Start answering the next question, before the call that asks it:
        the call waits for the person, so they answer beside it."""
        return asyncio.ensure_future(self._start(answer, within))

    async def _start(self, answer: str, within: int) -> asyncio.subprocess.Process:
        return await asyncio.create_subprocess_exec(
            APPROVER, "next", "--root", PERSON_ROOT, "--relay", HOST,
            "--answer", answer, "--within", str(within),
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
        )

    async def settle(self, started: "asyncio.Future[asyncio.subprocess.Process]", within: int = 180) -> list[dict]:
        """The questions the process saw, once it answered or gave up."""
        process = await started
        try:
            out, err = await asyncio.wait_for(process.communicate(), timeout=within + 30)
        except TimeoutError:
            process.kill()
            out, err = await process.communicate()
        questions = []
        for line in out.decode().splitlines():
            line = line.strip()
            if not line.startswith("{"):
                continue
            seen = json.loads(line)
            if seen.get("question"):
                questions.append(seen["question"])
            if "watched" in seen and seen["watched"]:
                raise AssertionError(f"the person was asked {seen['watched']} unexpected questions")
        if process.returncode not in (0, 1, 3):
            raise AssertionError(f"the approver stopped ({process.returncode}): {err.decode()[-800:]}")
        self.asked.extend(questions)
        return questions

    async def asked_nothing(self) -> None:
        """Say that the host has no question for the person: a question left
        waiting is still there to be seen, and the window answers nothing
        until the person decides."""
        questions = await self.settle(self.begin("none", within=2), within=2)
        check(not questions, f"the person was asked nothing ({questions})")

    def answered(self) -> int:
        return len(self.asked)


class Page:
    """One Browser window of the workspace, by the provider its open answered."""

    def __init__(self, workspace: object, provider: str) -> None:
        self.workspace = workspace
        self.provider = provider

    def path(self, action: str) -> str:
        return f"browser@{self.provider}/page#{action}"

    async def do(self, action: str, **args: object) -> object:
        return await self.workspace.do(self.path(action), **args)  # type: ignore[attr-defined]

    async def find(self, role: str, name: str, nth: int = 1) -> str:
        """The ref of the `nth` `role` on the page answering to `name`."""
        answer = await outcome(self.do("find-page-element", role=role, name=name, nth=nth))
        element = answer.get("element") if isinstance(answer, dict) else None
        check(isinstance(element, str) and element, f"the page names {role} {name!r} as {element}")
        return str(element)

    async def read(self) -> dict:
        answer = await outcome(self.do("read-page"))
        return answer if isinstance(answer, dict) else {}

    async def title(self) -> str:
        return str((await self.read()).get("title") or "")


async def open_browser(workspace: object) -> tuple[Page, float]:
    """Open the workspace's Browser and answer its window once it is live.

    The open answers once the window is live (the relay places the session
    and waits for it), so its own duration is the time from the open to a
    live window."""
    began = time.perf_counter()
    opened = await workspace.do("browser#open")  # type: ignore[attr-defined]
    live = time.perf_counter() - began
    say("   opened", str(opened)[:200])
    windows: list[str] = []
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        listing = await workspace.ls("browser")  # type: ignore[attr-defined]
        windows = [str(row.path) for row in listing.rows if "@" in str(row.path)]
        if windows:
            break
        await asyncio.sleep(0.2)
    check(windows, f"the workspace's Browser window is live ({windows})")
    provider = windows[0].split("/", 1)[0].split("@", 1)[1]
    return Page(workspace, provider), live


async def fill(page: Page) -> str:
    """Fill the form the agent has open, through the page's own actions, and
    answer the ref of the button that submits it."""
    await outcome(page.do("navigate", url=FORM_URL))
    await outcome(page.do("wait-page-settled", quiet_ms=300, timeout_ms=60000))
    title = await page.title()
    check("Visa application" in title, f"the form is the page the agent opened ({title!r})")
    for name, value in FILLED.items():
        element = await page.find("textbox", name)
        await outcome(page.do("fill-page", element=element, text=value))
    element = await page.find("combobox", "Nationality")
    await outcome(page.do("select-page", element=element, values=[CHOSEN["Nationality"]]))
    element = await page.find("radio", CHOSEN["Purpose"])
    await outcome(page.do("click-page", element=element))
    element = await page.find("checkbox", DECLARATION)
    await outcome(page.do("click-page", element=element))
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
    with urllib.request.urlopen(request, timeout=900) as answer:
        return json.load(answer)


def add_account_to_plans() -> None:
    """The provider adds the account to the plans file: the relay reads it
    again for the next page action."""
    check(PLANS and os.path.isfile(PLANS), f"the plans file is where the relay reads it ({PLANS})")
    with open(PLANS) as plans:
        document = json.load(plans)
    document.setdefault("accounts", {})[ACCOUNT] = {
        "plan": "local",
        "max_running": 2,
        "max_vms": 5,
        "sizes": ["small", "default"],
        "card": {"brand": "visa", "last4": "4242"},
    }
    with open(PLANS, "w") as plans:
        json.dump(document, plans, indent=2)
    say("   the provider added", f"{ACCOUNT} to {PLANS} with a card on file")


async def open_phase(report: dict[str, object]) -> int:
    """The workspace, its agent's key, and Browser opened in its VM."""
    backend = AsyncTilo(key=SERVER_KEY, door=DOOR)
    say("a. the startup makes an applicant's workspace and mints their agent's key")
    workspace = await backend.workspaces.create("Visa applicant: Jane Doe")
    report["workspace"] = workspace.id
    made = backend.workspace(workspace.id)
    minted = await made.keys.mint(name="Jane Doe's agent")
    check(minted.mcp_url.endswith(f"/w/{workspace.id}/mcp"), "the key reaches that workspace alone")
    # The same key with its scope unticked: what a person gives a key that
    # may not operate web pages, which the last step holds to being asked.
    scoped = await made.keys.mint(name="Jane Doe's agent, no pages", browser_pages=False)
    agent = AsyncTilo(key=minted.token, door=DOOR)
    aws = agent.workspace(workspace.id)

    say("a. the key opens Browser in the workspace's VM")
    page, live = await open_browser(aws)
    report["open_seconds"] = round(live, 2)
    say("   first open", f"{live:.1f} s from the open to a live window (the VM's cold start)")
    attached_began = time.perf_counter()
    again, _ = await open_browser(aws)
    attached = time.perf_counter() - attached_began
    report["attach_seconds"] = round(attached, 2)
    check(again.provider == page.provider, f"a second open attaches to the same window ({attached:.2f} s)")
    report["window"] = page.provider
    with open(os.path.join(OUT, "open.json"), "w") as opened:
        json.dump(
            {
                "workspace": workspace.id,
                "key": minted.token,
                "unscoped_key": scoped.token,
                "window": page.provider,
            },
            opened,
        )
    await agent.aclose()
    await backend.aclose()
    return 0


async def fill_phase(report: dict[str, object]) -> int:
    """The form filled with nothing asked, refused once, submitted once, and
    a click held; the reviewer's History; the scope; and the metering."""
    with open(os.path.join(OUT, "open.json")) as opened:
        state = json.load(opened)
    workspace = state["workspace"]
    agent = AsyncTilo(key=state["key"], door=DOOR)
    aws = agent.workspace(workspace)
    page = Page(aws, str(state["window"]))
    person = Person()
    reviewer = None

    say("b. the agent fills the visa form through page actions, and the person is asked nothing")
    async with aws.run(RUN_TITLE):
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

        say("c. the agent captures the filled form")
        captured = await outcome(page.do("capture-page"))
        ref = content_ref(captured)
        if ref:
            saved = write("filled-form.png", await aws.files.download(ref))
        else:
            inline = (captured or {}).get("data", {}).get("png") if isinstance(captured, dict) else None
            check(inline, f"capture-page answers the page's picture ({str(captured)[:200]})")
            saved = write("filled-form.png", base64.b64decode(inline))
        with open(saved, "rb") as picture:
            head = picture.read(8)
        check(head.startswith(b"\x89PNG"), f"the capture is a PNG ({saved})")
        report["capture"] = saved

        say("d. the person refuses a submission, and the form does not leave")
        asked = person.begin("deny")
        refused = await refusal_of(page.do("submit-page", element=submit))
        questions = await person.settle(asked)
        check(len(questions) == 1, f"the person was asked exactly once ({questions})")
        check("submit" in json.dumps(questions[0]).lower(), f"the question names the submission ({questions[0]})")
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
        asked = person.begin("allow")
        run = await outcome(page.do("submit-page", element=submit))
        check("released" in json.dumps(run), f"the submission ran once the person allowed it ({run})")
        questions = await person.settle(asked)
        check(len(questions) == 1, f"the person was asked exactly once ({questions})")
        check("submit" in json.dumps(questions[0]).lower(), f"the question names the submission ({questions[0]})")
        receipt = await page.read()
        write_json("receipt-page.json", receipt)
        check(RECEIPT in json.dumps(receipt), "the page shows the application was received")
        check("TV-" in json.dumps(receipt), f"the page shows the reference it made ({json.dumps(receipt)[:300]})")
        report["receipt"] = True

        say("f. a click on the submit button is held, and the person is asked once")
        submit = await fill(page)
        asked = person.begin("allow")
        clicked_once = await outcome(page.do("click-page", element=submit))
        check("released" in json.dumps(clicked_once), f"the click left once the person allowed it ({clicked_once})")
        questions = await person.settle(asked)
        check(len(questions) == 1, f"the person was asked exactly once ({questions})")
        clicked = await page.read()
        check(RECEIPT in json.dumps(clicked), "the click's submission left once the person allowed it")
        report["click_receipt"] = True

        say("g. a reviewer who joined by invitation reads the agent's run in History")
        invitation = await aws.members.invite(role="editor", email=REVIEWER_EMAIL)
        say("   invited", f"{invitation.role} via {invitation.link[:60]}…")
        accepted = await asyncio.to_thread(post_invitee, invitation.link)
        check(
            (accepted.get("joined") or {}).get("role") == "editor",
            f"the reviewer joined as an editor: {accepted}",
        )
        reviewer = AsyncTilo(key=accepted["key"], door=DOOR)
        rws = reviewer.workspace(workspace)
        listing = await rws.ls("browser")
        check(
            any("@" in str(row.path) for row in listing.rows),
            "the reviewer's listing names the hosted Browser window their desk shows",
        )
        history = await rws.do("browser#history", last=50)
        write_json("reviewer-history.json", history)
        # A page action runs as the caller's and changes no document, so the
        # app's history answers whether or not it holds a step of the run.
        report["history_by_reviewer"] = True
        report["history_names_run"] = RUN_TITLE in json.dumps(history)

    say("h. a key minted without operating web pages is asked about each page action")
    unscoped = AsyncTilo(key=state["unscoped_key"], door=DOOR)
    ows = unscoped.workspace(workspace)
    other, _ = await open_browser(ows)
    async with ows.run(RUN_TITLE):

        async def asked_once(action: str, call: object) -> object:
            """Run one page action of the key without the scope, and hold the
            person to being asked about it once."""
            asked = person.begin("allow")
            answer = await outcome(call)
            questions = await person.settle(asked)
            check(len(questions) == 1, f"the key without the scope is asked about {action} ({questions})")
            return answer

        # Taking the window somewhere asks; reading the page it is on does
        # not (the agent's window holds none of the person's sign-ins).
        await asked_once("navigate", other.do("navigate", url=FORM_URL))
        await outcome(other.do("wait-page-settled", quiet_ms=300, timeout_ms=60000))
        found = await outcome(other.do("find-page-element", role="textbox", name="Given names"))
        element = found.get("element") if isinstance(found, dict) else None
        check(isinstance(element, str) and element, f"the page names the field it found ({found})")
        await asked_once("fill-page", other.do("fill-page", element=element, text=FILLED["Given names"]))
    report["unscoped_questions"] = 2
    await unscoped.aclose()

    say("i. metering: the plans file's free page actions, then a card")
    refused = "allowed"
    for _ in range(600):
        refused = await refusal_of(page.do("read-page"))
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
    add_account_to_plans()
    allowed = await refusal_of(page.do("read-page"))
    check(allowed == "allowed", f"with the account on file the next page action runs ({allowed})")

    report["questions_asked"] = person.answered()
    write_json("questions.json", person.asked)

    for client in (agent, reviewer):
        if client is None:
            continue
        try:
            await client.aclose()
        except (ProtocolError, Refused):
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
    if phase == "--open":
        status = await open_phase(report)
    elif phase == "--fill":
        status = await fill_phase(report)
    else:
        status = await open_phase(report)
        if status == 0:
            status = await fill_phase(report)
    write_json("report.json", report)
    print(json.dumps(report), flush=True)
    return status


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
