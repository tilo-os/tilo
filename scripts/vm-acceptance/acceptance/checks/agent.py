"""Essential 7: the in-session MCP server, driven the way Claude Code drives it.

Inside the booted session, `tilo-runtime --connect` (no profile) must relay to the
desktop's own agent socket: the person is asked in trusted chrome, an admitted client is an
agent member of the workspace, and `tilo_ls` lists the desktop's running apps, not the
empty catalogue of a second, standalone runtime. The MCP server serves the path-addressed agent
tools (`tilo_ls`, `tilo_read`, `tilo_do`, `tilo_find`, `tilo_status`), which answer in text. The headless proof
(`scripts/headless-examples/agent-sessions-proof.py`) covers the protocol and its refusals
against a compositor with no session around it; only a session has the MCP server.

The client is `guest/mcp_client.py`, run in the guest through `tilo-vm account exec` (the
session's view, as the account's own process). The MCP server asks the person on its own sheet
(`so.tilo.portal-sheet`, `session.mcp-server`), and the harness answers as the person does:
it presses the sheet's "Allow once" and then Allow, or its Deny (`acceptance/sheets.py`).

Until the MCP server is in the image (`$XDG_RUNTIME_DIR/tilo-session/agent.sock`), these are
expected failures that say so (core.ExpectedFailure); once it is, they are ordinary checks.
"""

from __future__ import annotations

import json
import shlex
import threading
import time
from pathlib import Path

from .. import core
from ..core import CheckFailed, ExpectedFailure, Unobservable, check, expect
from ..sheets import ALLOW_ONCE, answer_sheet, deny_sheet
from ..vm import VmError, address, wait_until

AREA = "agent"

CLIENT = Path(__file__).resolve().parents[2] / "guest" / "mcp_client.py"
GUEST_CLIENT = ".cache/acceptance/mcp_client.py"
MCP_SERVER = '"${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/tilo-session/agent.sock"'
MARK = "ACC-MCP "
# The tools the MCP server serves.
MCP_TOOLS = ["tilo_do", "tilo_find", "tilo_ls", "tilo_read", "tilo_status"]
# The client names itself so (clientInfo), and the MCP server admits it as this agent.
AGENT = "acceptance-agent"

NO_MCP_SERVER = ("the session serves no MCP server ($XDG_RUNTIME_DIR/tilo-session/agent.sock): this image "
           "predates the in-session MCP server (fix/mcp-server, 2d43ddb51e), so `tilo-runtime "
           "--connect` in a Tilo login starts a second, empty standalone runtime instead of "
           "reaching the desktop")


def parse_transcript(output: str) -> dict:
    """The client's one `ACC-MCP <json>` line out of `tilo-vm account exec` output."""
    for line in reversed(output.splitlines()):
        if line.startswith(MARK):
            return json.loads(line[len(MARK):])
    raise CheckFailed(f"the guest client printed no transcript: {output[-400:]!r}")


def tool_names(transcript: dict) -> list[str]:
    try:
        return sorted(t["name"] for t in transcript["tools_list"]["result"]["tools"])
    except (KeyError, TypeError):
        return []


def text_of(reply) -> str:
    try:
        return "\n".join(b.get("text", "") for b in reply["result"]["content"])
    except (KeyError, TypeError, AttributeError):
        return ""


def created_resource(text: str) -> str | None:
    """The resource a `tilo_do` reply says it made: its JSON line's `resource`."""
    for line in text.splitlines():
        if line.startswith("{"):
            try:
                resource = json.loads(line).get("resource")
            except (ValueError, AttributeError):
                continue
            if isinstance(resource, str) and resource:
                return resource
    return None


def is_error(reply) -> bool | None:
    try:
        value = reply["result"].get("isError", False)
        return value if isinstance(value, bool) else None
    except (KeyError, TypeError, AttributeError):
        return None


def _mcp_or_xfail(ctx) -> None:
    if "mcp_server" not in ctx.facts:
        out = ctx.vm.exec(f"test -S {MCP_SERVER} && echo mcp || echo no-mcp; "
                          "command -v tilo-runtime >/dev/null && echo mcp || echo no-mcp; "
                          "command -v python3 >/dev/null && echo python || echo no-python")
        words = set(out.split())
        for need, what in (("mcp", "tilo-runtime"), ("python", "python3")):
            if need not in words:
                raise Unobservable(f"the guest has no {what} on PATH")
        ctx.facts["mcp_server"] = "mcp" in words
    if not ctx.facts["mcp_server"]:
        raise ExpectedFailure(NO_MCP_SERVER)


def _answer_prompt(ctx, decision: str) -> str:
    """Answer the MCP server's question as the person: admit the client once, or refuse it."""
    what = f"mcp-server-{decision}"
    if decision == "deny":
        return deny_sheet(ctx, what)["chosen"]
    return answer_sheet(ctx, ALLOW_ONCE, what)["chosen"]


def _run_client(ctx, scenario: str, decision: str, title: str = "") -> dict:
    ctx.vm.write_home_file(GUEST_CLIENT, CLIENT.read_bytes())
    box: dict = {}
    env = f"ACC_MCP_TITLE={shlex.quote(title)} " if title else ""

    def run():
        try:
            box["out"] = ctx.vm.exec(f'{env}python3 "$HOME/{GUEST_CLIENT}" {scenario}', timeout=240)
        except VmError as exc:
            box["error"] = str(exc)

    thread = threading.Thread(target=run, daemon=True)
    thread.start()
    answered = None
    try:
        answered = _answer_prompt(ctx, decision)
    finally:
        thread.join(timeout=240)
    if "out" not in box:
        raise CheckFailed(f"the guest client did not finish: {box.get('error', 'still running')[:400]}")
    transcript = parse_transcript(box["out"])
    transcript["answered"] = answered
    ctx.write(f"agent-{scenario}-transcript.json", json.dumps(transcript, indent=1))
    if transcript.get("client_error"):
        raise CheckFailed(f"the guest client failed: {transcript['client_error']}")
    return transcript


def _session(ctx) -> dict:
    """The admitted agent's whole conversation, run once for M01..M03."""
    _mcp_or_xfail(ctx)
    if "agent_session" not in ctx.facts:
        ctx.reset()
        ctx.open_app("so.tilo.notes")
        if ctx.wait_presented("so.tilo.notes", 30) is None:
            raise CheckFailed("Notes showed no frame within 30 s")
        title = f"Acceptance agent note {int(time.time())}"
        # What they admit works as themselves once: the agent's changes are the person's own,
        # in their history and undo, so Notes' new note needs no grant beside the admission.
        transcript = _run_client(ctx, "session", "allow-once", title)
        transcript["title"] = title
        ctx.facts["agent_session"] = transcript
    return ctx.facts["agent_session"]


@check("M01", AREA, "tilo-runtime --connect in a session: the person is asked, then initialize and "
                    "tools/list answer, with no pixel tools on the MCP server")
def handshake(ctx, res):
    t = _session(ctx)
    init = t.get("initialize")
    expect(isinstance(init, dict) and "result" in init,
           f"initialize was not answered after the person allowed it: {str(init)[:300]}")
    expect(init["result"].get("serverInfo"), f"initialize names no server: {init['result']}")
    names = tool_names(t)
    res.metrics["tools"] = names
    expect(not [n for n in names if n.startswith("computer")],
           f"the MCP server offers pixel tools, which it must not: {names}")
    missing = [n for n in MCP_TOOLS if n not in names]
    expect(not missing, f"tools/list offers no {missing}: {names}")


@check("M02", AREA, "The MCP server lists the desktop's running apps (Notes is open), not an empty standalone runtime")
def live_apps(ctx, res):
    t = _session(ctx)
    reply = t.get("list_apps")
    expect(is_error(reply) is False, f"tilo_ls was refused or unanswered: {str(reply)[:300]}")
    lines = (t.get("apps_text") or "").splitlines()
    res.metrics["apps"] = lines[:12]
    expect(any(line.startswith("notes ") and line.endswith("running") for line in lines),
           f"Notes is open on the desktop, but tilo_ls does not list it running: {lines[:12]}: "
           "this is a second runtime, not the desktop")
    started = sorted(set(t.get("daemons_after", [])) - set(t.get("daemons_before", [])))
    expect(not started, f"--connect started a standalone runtime daemon (pids {started}) beside the desktop")


@check("M03", AREA, "An agent creates a Notes note through new_note and the note is there when read back "
                    "independently")
def new_note(ctx, res):
    t = _session(ctx)
    title = t["title"]
    # The action a tilolang app publishes is named as its declaration is (`new_note`), where the
    # native app it replaced published `new-note`.
    expect("#new_note" in text_of(t.get("notes_listing")),
           f"Notes never listed its new_note action on the MCP server: {str(t.get('notes_listing'))[:300]}")
    params = text_of(t.get("new_note_params"))
    expect(is_error(t.get("new_note_params")) is False and "title" in params,
           f"Notes' new_note action names no title parameter: {str(t.get('new_note_params'))[:300]}")
    reply = t.get("new_note")
    expect(is_error(reply) is False and text_of(reply).startswith("ok"),
           f"new_note was refused: {str(reply)[:300]}")
    res.metrics["new_note"] = text_of(reply)[:200]
    expect(title in text_of(t.get("found_note")),
           f"tilo_find does not find the note {title!r}: {str(t.get('found_note'))[:300]}")
    # Read it back through the desktop's own control socket, not through the agent's reply:
    # the desktop's listing for the note's title, which carries Notes' published pages. The
    # control socket has no generic read, and a listing for "notes" answers with every
    # action that mentions the word, not with Notes' records. The listing answers a title
    # with the page's own actions (Copy note link, Delete note), which name the page they
    # act on (semantic_identity) rather than repeat its title: a row of Notes for the page
    # new-note made, found by its title, is the note read back.
    page = created_resource(text_of(reply))

    def listed():
        rows = [row for row in ctx.vm.actions(title) if row["app_id"] == "so.tilo.notes"
                or row["provider"].isdigit()]
        return rows if any(title in json.dumps(row, ensure_ascii=False)
                           or (page and (row["action"].get("semantic_identity") or {}).get("key")
                               == page)
                           for row in rows) else None

    rows = wait_until(listed, 15, 1.0)
    if rows is None:
        seen = json.dumps(ctx.vm.actions(title), ensure_ascii=False)
        raise CheckFailed(f"the created note {title!r} is not among what the desktop lists for it "
                          f"(listing: {seen[:300]!r})")
    res.metrics["listed_as"] = ["#".join(address(row))[:120] for row in rows[:3]]
    ctx.shot("M03-notes-new-note")


@check("M04", AREA, "Refusals reach the agent as errors: an unknown action is isError, a denied connection "
                    "gets a JSON-RPC error and is closed")
def refusals(ctx, res):
    t = _session(ctx)
    refusal = t.get("refusal")
    expect(is_error(refusal) is True,
           f"an invocation of an action that does not exist was not reported as isError: {str(refusal)[:300]}")
    from_tool = text_of(refusal) if is_error(refusal) else ""
    expect(from_tool.strip(), "the refusal carries no reason")
    res.metrics["refusal"] = from_tool[:200]

    denied = _run_client(ctx, "deny", "deny")
    init = denied.get("initialize")
    res.metrics["denied_initialize"] = init
    expect(isinstance(init, dict) and "error" in init and "result" not in init,
           f"a denied client did not get a JSON-RPC error on initialize: {str(init)[:300]}")
    expect(init["error"].get("message"), f"the denial names no reason: {init}")
    expect("after_refusal" in denied and denied["after_refusal"] is None,
           f"the MCP server stayed open after refusing: {str(denied.get('after_refusal'))[:200]}")
