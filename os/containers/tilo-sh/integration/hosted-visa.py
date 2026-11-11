"""A visa application filled in a hosted workspace's Browser, end to end.

The stack's local tilo.sh runs the Browser of a hosted workspace in that
workspace's VM (docs/hosted-browser.md). This is the flow a person and their
agent take there:

  a. a key opens Browser (`browser#open`); the window it answers with is the
     workspace's, and the time from the open to a live window is printed (the
     VM's cold start), then a second open attaches to the same window;
  b. the agent navigates the window to the visa test page and fills it with
     page actions (`find-page-element`, `fill-page`, `select-page`,
     `click-page`), then reads it back;
  c. `capture-page` answers a picture of the filled form, saved as a PNG;
  d. the agent stops before submitting: `submit-page` is refused for want of
     the person's answer, and the form is not submitted;
  e. a reviewer who joined by invitation reads the agent's run in History;
  g. past the plans file's free page actions the next one is refused with
     `no_payment_method` and a step to add a card; adding the account to the
     plans file lets the next one through.

usage: hosted-visa.py DOOR SERVER_KEY OUT_DIR FORM_URL [--open|--fill]

SERVER_KEY is the startup's server key (`tilo keys create`). OUT_DIR keeps
what it found: the capture, the page as the agent read it back, the reviewer's
History and its report. TILO_INVITEE names the stack's reviewer helper
(integration/reviewer.py), HOSTED_VISA_PLANS the plans file the relay counts
page actions by, and HOSTED_VISA_ACCOUNT the account it counts them for.

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

from tilo import AsyncTilo, ProtocolError, Refused

DOOR, SERVER_KEY, OUT, FORM_URL = sys.argv[1:5]

RUN_TITLE = "Fill Jane Doe's visa application"
REVIEWER_EMAIL = "rosa@example.com"
PLANS = os.environ.get("HOSTED_VISA_PLANS", "")
ACCOUNT = os.environ.get("HOSTED_VISA_ACCOUNT", "")
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


async def refusal_of(call: object) -> str:
    """How the door refused `call`: its code and message and the steps it
    named, or `allowed`."""
    try:
        await call
    except ProtocolError as error:
        return f"{error.code or ''}: {error.message or error.status or error}".strip(": ")
    except Refused as error:
        steps = ", ".join(error.next_steps or [])
        return f"{error.code}: {error.message}" + (f" (next: {steps})" if steps else "")
    return "allowed"


def code_of(text: str) -> str:
    return text.split(":", 1)[0].strip()


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
        answer = await self.do("find-page-element", role=role, name=name, nth=nth)
        element = answer.get("element") if isinstance(answer, dict) else None
        check(isinstance(element, str) and element, f"the page names {role} {name!r} as {element}")
        return str(element)

    async def read(self) -> dict:
        answer = await self.do("read-page")
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
    try:
        await page.do("navigate", url=FORM_URL)
    except (Refused, ProtocolError) as error:
        code = getattr(error, "code", "") or ""
        if "confirmation_required" in str(code) or "confirmation_required" in str(error):
            raise SystemExit(
                "FAIL: the hosted Browser's window asks the person about every page action "
                "(a window holding a person's page is Outside, and the session in the VM opens "
                "the workspace's window as the person's, not an agent's), and nothing asks them: "
                "the relay records that the window asks and refuses the same invocation again "
                "(tilo-relay's hosted_browser::HostedBrowsers::relayed), so an agent cannot fill "
                "the page it opened. The window's page actions and the person's answer both have "
                f"to work before an agent can drive a hosted Browser: {error}"
            )
        raise
    await page.do("wait-page-settled", quiet_ms=300, timeout_ms=60000)
    title = await page.title()
    check("Visa application" in title, f"the form is the page the agent opened ({title!r})")
    for name, value in FILLED.items():
        element = await page.find("textbox", name)
        await page.do("fill-page", element=element, text=value)
    element = await page.find("combobox", "Nationality")
    await page.do("select-page", element=element, values=[CHOSEN["Nationality"]])
    element = await page.find("radio", CHOSEN["Purpose"])
    await page.do("click-page", element=element)
    element = await page.find("checkbox", DECLARATION)
    await page.do("click-page", element=element)
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
        json.dump({"workspace": workspace.id, "key": minted.token, "window": page.provider}, opened)
    await agent.aclose()
    await backend.aclose()
    return 0


async def fill_phase(report: dict[str, object]) -> int:
    """The form filled through page actions, captured, and stopped before
    submitting; the reviewer's History; and the page actions' metering."""
    with open(os.path.join(OUT, "open.json")) as opened:
        state = json.load(opened)
    workspace = state["workspace"]
    agent = AsyncTilo(key=state["key"], door=DOOR)
    aws = agent.workspace(workspace)
    page = Page(aws, str(state["window"]))
    reviewer = None

    say("b. the agent fills the visa form through page actions")
    async with aws.run(RUN_TITLE):
        submit = await fill(page)
        read_back = await page.read()
        body = json.dumps(read_back)
        missing = [value for value in FILLED.values() if value not in body]
        check(not missing, f"the page holds every value the agent filled (missing: {missing})")
        check(CHOSEN["Nationality"] in body, "the page holds the nationality the agent chose")
        check(CHOSEN["Purpose"].lower() in body.lower(), "the page holds the purpose the agent chose")
        write_json("filled-page.json", read_back)

        say("c. the agent captures the filled form")
        captured = await page.do("capture-page")
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

        say("d. the agent stops before submitting")
        before = await page.title()
        refused = await refusal_of(page.do("submit-page", element=submit))
        check(
            code_of(refused) == "action_confirmation_required",
            f"submit-page without the person's answer is refused ({refused})",
        )
        after = await page.title()
        check("Application received" not in after and after == before, "the form was not submitted")
        report["refused"] = refused

        say("e. a reviewer who joined by invitation reads the agent's run in History")
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
        check(RUN_TITLE in json.dumps(history), f"History names the agent's run {RUN_TITLE!r}")
        report["history_by_reviewer"] = True

    say("g. metering: the plans file's free page actions, then a card")
    refused = "allowed"
    for _ in range(20):
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
