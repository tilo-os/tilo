"""Essential 7: the in-session agent door, driven the way Claude Code drives it.

Inside the booted session, `tilo-runtime-mcp --connect` (no profile) must relay to the
desktop's own agent socket: the person is asked in trusted chrome, an admitted client is an
agent member of the workspace, and `tilo_ls` lists the desktop's running apps, not the
empty catalogue of a second, standalone runtime. The door serves the path-addressed agent
tools (`tilo_ls`, `tilo_read`, `tilo_do`, `tilo_find`, `tilo_status`), which answer in text. The headless proofs
(`scripts/headless-examples/mcp-live-proof.py`, `agent-seats-proof.py`) cover the protocol
against a compositor with no session around it; only a session has the door.

The client is `guest/mcp_client.py`, run in the guest through `tilo-vm slot exec` (the
session's view, as the account's own process). The person's answer goes through the same
decision the owner has remotely: the `permission-prompts` provider's `allow-once@<prompt>` and
`deny@<prompt>` actions on the control socket.

Until the door is in the image (`$XDG_RUNTIME_DIR/tilo-session/agent.sock`), these are
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
from ..vm import VmError, wait_until

AREA = "agent"

CLIENT = Path(__file__).resolve().parents[2] / "guest" / "mcp_client.py"
GUEST_CLIENT = ".cache/acceptance/mcp_client.py"
PROVIDER = "permission-prompts"
DOOR = '"${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/tilo-session/agent.sock"'
MARK = "ACC-MCP "
# How long the person's prompt may take to appear after the client's initialize.
PROMPT_S = 60
# The tools the door serves.
DOOR_TOOLS = ["tilo_do", "tilo_find", "tilo_ls", "tilo_read", "tilo_status"]
# The client names itself so (clientInfo), and the door admits it as this agent.
AGENT = "acceptance-agent"
# Admission makes the agent an Observer of the room: it may read. Writing to Notes is
# the person's grant, as the Agents tab gives it, taken back when the session is over.
GRANT = {"op": "allow", "subject": f"agent:{AGENT}", "node": "app:so.tilo.notes",
         "kinds": ["read", "undoable"], "answer": "allow", "id": "acceptance-agent-notes"}

NO_DOOR = ("the session serves no agent door ($XDG_RUNTIME_DIR/tilo-session/agent.sock): this image "
           "predates the in-session agent door (fix/agent-door, 2d43ddb51e), so `tilo-runtime-mcp "
           "--connect` in a Tilo login starts a second, empty standalone runtime instead of "
           "reaching the desktop")


def parse_transcript(output: str) -> dict:
    """The client's one `ACC-MCP <json>` line out of `tilo-vm slot exec` output."""
    for line in reversed(output.splitlines()):
        if line.startswith(MARK):
            return json.loads(line[len(MARK):])
    raise CheckFailed(f"the guest client printed no transcript: {output[-400:]!r}")


def decision_rows(actions: list[dict], decision: str) -> list[str]:
    """Ids of the waiting prompt's `<decision>@...` rows in an actions listing."""
    return [a["id"] for a in actions if a.get("provider") == PROVIDER and a["id"].startswith(decision + "@")]


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


def is_error(reply) -> bool | None:
    try:
        value = reply["result"].get("isError", False)
        return value if isinstance(value, bool) else None
    except (KeyError, TypeError, AttributeError):
        return None


def _door_or_xfail(ctx) -> None:
    if "agent_door" not in ctx.facts:
        out = ctx.vm.exec(f"test -S {DOOR} && echo door || echo no-door; "
                          "command -v tilo-runtime-mcp >/dev/null && echo mcp || echo no-mcp; "
                          "command -v python3 >/dev/null && echo python || echo no-python")
        words = set(out.split())
        for need, what in (("mcp", "tilo-runtime-mcp"), ("python", "python3")):
            if need not in words:
                raise Unobservable(f"the guest has no {what} on PATH")
        ctx.facts["agent_door"] = "door" in words
    if not ctx.facts["agent_door"]:
        raise ExpectedFailure(NO_DOOR)


def _answer_prompt(ctx, decision: str) -> str:
    """Answer the waiting prompt as the person: `decision` is allow-once or deny."""
    def waiting():
        try:
            rows = decision_rows(ctx.vm.actions(PROVIDER), decision)
        except VmError:
            return None
        return rows[0] if rows else None

    row = wait_until(waiting, PROMPT_S, interval=0.5)
    if row is None:
        raise Unobservable(f"no `{decision}@...` row of the {PROVIDER} provider within {PROMPT_S} s of the "
                           "client's initialize: the person is never asked, or cannot be answered over "
                           "the control socket")
    ctx.vm.invoke(PROVIDER, row, {"confirm": True})
    return row


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
    _door_or_xfail(ctx)
    if "agent_session" not in ctx.facts:
        ctx.reset()
        ctx.open_app("so.tilo.notes")
        ctx.settle(2.0)
        title = f"Acceptance agent note {int(time.time())}"
        try:
            granted = ctx.vm.raw(GRANT)
        except VmError as exc:
            granted = {"err": exc.reason()}
        if isinstance(granted, dict) and granted.get("err"):
            raise Unobservable(f"the person's grant of Notes to the agent was refused: {granted}")
        try:
            transcript = _run_client(ctx, "session", "allow-once", title)
        finally:
            try:
                ctx.vm.raw({"op": "disallow", "id": GRANT["id"]})
            except VmError:
                pass  # a grant left behind names only this test's agent
        transcript["title"] = title
        ctx.facts["agent_session"] = transcript
    return ctx.facts["agent_session"]


@check("M01", AREA, "tilo-runtime-mcp --connect in a session: the person is asked, then initialize and "
                    "tools/list answer, with no pixel tools on the door")
def handshake(ctx, res):
    t = _session(ctx)
    init = t.get("initialize")
    expect(isinstance(init, dict) and "result" in init,
           f"initialize was not answered after the person allowed it: {str(init)[:300]}")
    expect(init["result"].get("serverInfo"), f"initialize names no server: {init['result']}")
    names = tool_names(t)
    res.metrics["tools"] = names
    expect(not [n for n in names if n.startswith("computer")],
           f"the door offers pixel tools, which an agent door must not: {names}")
    missing = [n for n in DOOR_TOOLS if n not in names]
    expect(not missing, f"tools/list offers no {missing}: {names}")


@check("M02", AREA, "The door lists the desktop's running apps (Notes is open), not an empty standalone runtime")
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


@check("M03", AREA, "An agent creates a Notes note through new-note and the note is there when read back "
                    "independently")
def new_note(ctx, res):
    t = _session(ctx)
    title = t["title"]
    expect("#new-note" in text_of(t.get("notes_listing")),
           f"Notes never listed its new-note action on the door: {str(t.get('notes_listing'))[:300]}")
    params = text_of(t.get("new_note_params"))
    expect(is_error(t.get("new_note_params")) is False and "title" in params,
           f"Notes' new-note action names no title parameter: {str(t.get('new_note_params'))[:300]}")
    reply = t.get("new_note")
    expect(is_error(reply) is False and text_of(reply).startswith("ok"),
           f"new-note was refused: {str(reply)[:300]}")
    res.metrics["new_note"] = text_of(reply)[:200]
    expect(title in text_of(t.get("found_note")),
           f"tilo_find does not find the note {title!r}: {str(t.get('found_note'))[:300]}")
    # Read it back through the desktop's own control socket, not through the agent's reply.
    seen = ""
    try:
        seen = ctx.vm.run("control", "read", "so.tilo.notes", "--json")
    except VmError as exc:
        res.metrics["read_unavailable"] = exc.reason()
        seen = json.dumps(ctx.vm.actions("notes"))
    expect(title in seen, f"the created note {title!r} is not in what Notes holds (read back: {seen[:300]!r})")
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
           f"the door stayed open after refusing: {str(denied.get('after_refusal'))[:200]}")
