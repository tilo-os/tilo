"""Essential 7: the in-session agent door, driven the way Claude Code drives it.

Inside the booted session, `tilo-runtime-mcp --connect` (no profile) must relay to the
desktop's own agent socket: the person is asked in trusted chrome, an admitted client is an
agent member of the workspace, and `tilo_list_apps` lists the desktop's live apps, not the
empty catalogue of a second, standalone runtime. The headless proofs
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


def is_error(reply) -> bool | None:
    try:
        return bool(reply["result"].get("isError"))
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


def _run_client(ctx, scenario: str, decision: str) -> dict:
    ctx.vm.write_home_file(GUEST_CLIENT, CLIENT.read_bytes())
    box: dict = {}

    def run():
        try:
            box["out"] = ctx.vm.exec(f'python3 "$HOME/{GUEST_CLIENT}" {scenario}', timeout=240)
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
        ctx.facts["agent_session"] = _run_client(ctx, "session", "allow-once")
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
    expect("tilo_list_apps" in names, f"tools/list offers no tilo_list_apps: {names}")
    expect(not [n for n in names if n.startswith("computer")],
           f"the door offers pixel tools, which an agent door must not: {names}")


@check("M02", AREA, "The door lists the desktop's live apps (Notes is open), not an empty standalone runtime")
def live_apps(ctx, res):
    t = _session(ctx)
    reply = t.get("list_apps")
    expect(is_error(reply) is False, f"tilo_list_apps was refused or unanswered: {str(reply)[:300]}")
    apps = t.get("app_ids") or []
    res.metrics["app_ids"] = apps
    expect("so.tilo.notes" in apps, f"Notes is open on the desktop, but tilo_list_apps lists {apps}: "
                                    "this is a second runtime, not the desktop")
    started = sorted(set(t.get("daemons_after", [])) - set(t.get("daemons_before", [])))
    expect(not started, f"--connect started a standalone runtime daemon (pids {started}) beside the desktop")


@check("M03", AREA, "An agent creates a Notes page through new-page and the page is there when read back "
                    "independently")
def new_page(ctx, res):
    t = _session(ctx)
    expect(t.get("new_page_target"),
           f"the directory offers no Notes new-page action: {str(t.get('query_new_page'))[:300]}")
    reply = t.get("new_page")
    expect(is_error(reply) is False, f"new-page was refused: {str(reply)[:300]}")
    pages = t.get("created_pages") or []
    res.metrics["created_pages"] = pages
    expect(pages, f"new-page answered without the page it created: {str(reply)[:300]}")
    # Read it back through the desktop's own control socket, not through the agent's reply.
    seen = ""
    try:
        seen = ctx.vm.run("control", "read", "so.tilo.notes", "--json")
    except VmError as exc:
        res.metrics["read_unavailable"] = exc.reason()
        seen = json.dumps(ctx.vm.actions("notes"))
    expect(any(p in seen for p in pages),
           f"the created page {pages} is not in what Notes holds (read back: {seen[:300]!r})")
    ctx.shot("M03-notes-new-page")


@check("M04", AREA, "Refusals reach the agent as errors: an unknown action is isError, a denied connection "
                    "gets a JSON-RPC error and is closed")
def refusals(ctx, res):
    t = _session(ctx)
    refusal = t.get("refusal")
    expect(is_error(refusal) is True,
           f"an invocation of an action that does not exist was not reported as isError: {str(refusal)[:300]}")
    from_tool = (refusal["result"]["content"][0].get("text") or "") if is_error(refusal) else ""
    expect(from_tool.strip(), "the refusal carries no reason")
    res.metrics["refusal"] = from_tool[:200]

    denied = _run_client(ctx, "deny", "deny")
    init = denied.get("initialize")
    res.metrics["denied_initialize"] = init
    expect(isinstance(init, dict) and "error" in init and "result" not in init,
           f"a denied client did not get a JSON-RPC error on initialize: {str(init)[:300]}")
    expect(init["error"].get("message"), f"the denial names no reason: {init}")
    expect(denied.get("after_refusal") is None,
           f"the door stayed open after refusing: {str(denied.get('after_refusal'))[:200]}")
