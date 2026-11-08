"""Several coding agents working independently on one machine, each in its own workspace.

The person makes two workspaces, opens Terminal in each, makes an environment in each
Terminal and starts an MCP client in each environment's shell, configured as a person
configures Claude Code: `{"command": "tilo-runtime", "args": ["--connect"]}`. The client
(`guest/mcp_agent.sh`, bash: the environment's runtime has no python) names itself
`claude-code`; what it sends comes from the harness, one request at a time, standing in for
the model. The person answers each admission on the sheet.

Everything goes through the production paths: `workspace.create` and `workspace.select.*`,
Terminal opened from the app catalogue, its own actions (`create-environment`, `open-in`,
`run`, `read`), the sheet's own answers (`so.tilo.portal-sheet`) and the desk's Stop
(`session:desk` `stop-agent`). The harness reads the agents' transcripts and the workspace
trees through the slot's own account (`tilo-vm slot exec`), never through ush or the door.

The checks share one scenario and run in order (E01..E07, `run.py` E2E_ORDER); each needs
the ones before it. `t test e2e-agents` runs this area alone (tools/ci/vm-acceptance.sh
e2e-agents); the vm-acceptance shards never run it.
"""

from __future__ import annotations

import base64
import json
import re
import shlex
import time
import uuid
from pathlib import Path

from ..action_jobs import invoke as invoke_job
from ..core import CheckFailed, Unobservable, check, expect
from ..vm import VmError, wait_until

AREA = "agents-e2e"
TERMINAL = "so.tilo.terminal"
NOTES = "so.tilo.notes"
SHEET = "so.tilo.portal-sheet"
DESK = "session:desk"
AGENT = Path(__file__).resolve().parents[2] / "guest" / "mcp_agent.sh"
# Where in each workspace's tree (the environments' `~`) the agents keep their transcripts.
AGENTS_DIR = ".e2e-agents"
# The client names itself as Claude Code does; the door proposes it as this agent.
CLIENT = "claude-code"
PROTOCOL = "2025-06-18"
# The two workspaces, and the environment made in each.
WORKSPACES = (("Studio", "Studio dev"), ("Garden", "Garden site"))
# The sheet's choices for an agent's admission (session questions.rs `admitted`).
ALLOW_ONCE, ALLOW_SESSION, ALLOW_ALWAYS = 0, 1, 2
# How long a step may take: an environment's first shell (the runtime's guest set up), the
# sheet appearing after an initialize, a reply after a request.
SHELL_S = 120
SHEET_S = 60
REPLY_S = 45
# The guest's files root and the trees in it (tilo_paths::workspace_tree).
GUEST_TREES = "/var/workspace/.workspaces"


# -- the desk ---------------------------------------------------------------------------


def workspaces(ctx) -> list[dict]:
    return ctx.state()["workspace_tree"]["workspaces"]


def held_here(state: dict, workspace_id) -> bool:
    return workspace_id in {row["workspace"] for row in state.get("held_workspaces") or []}


def create_workspace(ctx, name: str) -> dict:
    before = {w["id"] for w in workspaces(ctx)}
    ctx.vm.invoke("actions", "workspace.create", {"name": name})
    st = ctx.wait_state(lambda s: [w for w in s["workspace_tree"]["workspaces"] if w["id"] not in before],
                        15, f"the workspace {name}")
    made = next(w for w in st["workspace_tree"]["workspaces"] if w["id"] not in before)
    select_workspace(ctx, made)
    ctx.wait_state(lambda s: held_here(s, made["id"]), 45, f"{name} held on this device")
    return made


def select_workspace(ctx, workspace: dict) -> None:
    if ctx.state()["workspace_tree"]["active"] == workspace["id"]:
        return
    ctx.vm.invoke("actions", f"workspace.select.{workspace['room']}")
    ctx.wait_state(lambda s: s["workspace_tree"]["active"] == workspace["id"], 10,
                   f"workspace {workspace.get('name', workspace['id'])} in front")


# -- Terminal, through its own actions --------------------------------------------------


def identity(row: dict) -> tuple:
    return tuple(row.get(k) for k in ("application", "room", "record", "resource"))


def terminal_rows(ctx) -> list[dict]:
    return [r for r in ctx.vm.actions(TERMINAL, programmatic=True)
            if r.get("application") == TERMINAL
            and (r.get("semantic_identity") or {}).get("kind") == "terminal.session"]


class Terminal:
    """One Terminal window's session, by the identity its actions are bound to."""

    def __init__(self, ctx, bound: tuple, name: str, workspace: dict):
        self.ctx, self.bound, self.name, self.workspace = ctx, bound, name, workspace
        self.calls = 0

    def row(self, *operations: str) -> dict:
        found = [r for r in terminal_rows(self.ctx)
                 if identity(r) == self.bound and r.get("operation") in operations]
        if len(found) != 1:
            raise CheckFailed(f"{self.name}: expected one Terminal {'/'.join(operations)} action, "
                              f"got {len(found)}")
        return found[0]

    def call(self, operations, args: dict | None = None, timeout: float = 60):
        operations = (operations,) if isinstance(operations, str) else operations
        # The person works a Terminal in its own workspace, which they bring to the front.
        select_workspace(self.ctx, self.workspace)
        self.calls += 1
        name = f"{self.name}-{self.calls:02d}-{operations[0]}"
        return invoke_job(self.ctx.vm, self.row(*operations), args, self.ctx.out / "data", name,
                          timeout=timeout)

    def inspect(self) -> dict:
        return self.call("inspect-session") or {}

    def screen(self) -> str:
        return (self.call(("read", "read-screen")) or {}).get("output", "")

    def run(self, command: str) -> None:
        self.call("run", {"command": command, "confirm": True})

    def ask(self, command: str, timeout: float = 30) -> list[str]:
        """Run `command` and return the lines it printed starting with `{M}` (without it),
        once the prompt is back after them. `{M}` is written in the command as a shell word
        the shell joins (`E2E''<nonce>`), so the command's own echo never matches."""
        nonce = uuid.uuid4().hex[:8]
        marker = f"E2E{nonce}"
        self.run(command.replace("{M}", f"E2E''{nonce}"))
        deadline = time.monotonic() + timeout
        while True:
            lines = self.screen().splitlines()
            hits = [i for i, line in enumerate(lines) if line.startswith(marker)]
            if hits and any(re.search(r"\]\$\s*$", line) for line in lines[hits[-1] + 1:]):
                return [lines[i][len(marker):] for i in hits]
            if time.monotonic() > deadline:
                self.ctx.write(f"{self.name}-screen.txt", "\n".join(lines))
                raise CheckFailed(f"{self.name}: `{command[:80]}` printed nothing and returned no prompt "
                                  f"within {timeout:.0f} s")
            time.sleep(0.5)


def open_terminal(ctx, name: str, workspace: dict) -> Terminal:
    before = {identity(r) for r in terminal_rows(ctx)}
    ctx.open_app(TERMINAL)

    def fresh():
        new = {identity(r) for r in terminal_rows(ctx)} - before
        return new if len(new) == 1 else None

    new = wait_until(fresh, 60, interval=0.5)
    if new is None:
        raise CheckFailed(f"{name}: the Terminal opened published no session of its own")
    terminal = Terminal(ctx, next(iter(new)), name, workspace)
    info = wait_until(lambda: (i if (i := terminal.inspect()).get("lifecycle") == "open" else None),
                      SHELL_S, interval=1.0)
    if info is None:
        raise CheckFailed(f"{name}: Terminal's first session never opened")
    return terminal


def enter_environment(terminal: Terminal, name: str) -> dict:
    """Make the environment `name` from Terminal and move its session there, as the person
    does from Terminal's environment picker. Its record, from Terminal's own inspection."""
    terminal.call("create-environment", {"name": name, "confirm": True})
    info = terminal.inspect()
    made = [e for e in info.get("known_environments") or [] if e.get("name") == name]
    if len(made) != 1:
        raise CheckFailed(f"{terminal.name}: Terminal does not know the environment {name!r} it made: "
                          f"{info.get('known_environments')}")
    record = made[0]["record"]
    terminal.call("open-in", {"target": f"environment:{record}", "confirm": True})

    def there():
        now = terminal.inspect()
        target = now.get("target") or {}
        return now if now.get("lifecycle") == "open" and target.get("record") == record else None

    info = wait_until(there, SHELL_S, interval=1.0)
    if info is None:
        raise CheckFailed(f"{terminal.name}: the session did not open in {name} ({record}): "
                          f"{terminal.inspect()}")
    return {"record": record, "inspect": info}


# -- the slot's own view of the trees -----------------------------------------------------


def slot_write(ctx, path: str, data: bytes) -> None:
    blob = base64.b64encode(data).decode()
    q = shlex.quote(path)
    ctx.vm.exec(f"mkdir -p \"$(dirname {q})\" && echo {blob} | base64 -d > {q}.e2e-tmp && mv -f {q}.e2e-tmp {q}")


def slot_append(ctx, path: str, line: str) -> None:
    ctx.vm.exec(f"printf '%s\\n' {shlex.quote(line)} >> {shlex.quote(path)}")


def slot_read(ctx, path: str) -> str | None:
    out = ctx.vm.exec(f"if [ -f {shlex.quote(path)} ]; then echo E2E-FILE; cat {shlex.quote(path)}; fi",
                      check=False)
    head, sep, rest = out.partition("E2E-FILE\n")
    return rest if sep else None


def find_tree(ctx, probe: str) -> str:
    """Where, on the device, the tree is whose `~/<probe>` a guest just wrote: the slot's
    account searches for it as any program of the login would."""
    out = ctx.vm.exec(
        "find / -maxdepth 8 \\( -path /proc -o -path /sys -o -path /usr -o -path /sysroot "
        f"-o -path /ostree -o -path /dev \\) -prune -o -name {shlex.quote(probe)} -print 2>/dev/null "
        "| head -5", timeout=180, check=False)
    hits = [line.strip() for line in out.splitlines() if line.strip().endswith(probe)]
    if len(hits) != 1:
        raise Unobservable(f"the slot's account finds the guest's ~/{probe} at {hits or 'no path'}")
    return hits[0].rsplit("/", 1)[0]


# -- the agents ----------------------------------------------------------------------------


class Agent:
    """One MCP client in an environment's shell: its transcript in its workspace's tree."""

    def __init__(self, ctx, env: dict, name: str, server: str = ""):
        self.ctx, self.env, self.name = ctx, env, name
        self.host_dir = f"{env['tree']}/{AGENTS_DIR}/{name}"
        self.guest_dir = f"~/{AGENTS_DIR}/{name}"
        self.server = server
        self.next_id = 0

    def start(self) -> None:
        self.env["terminal"].run(f"bash ~/{AGENTS_DIR}/mcp_agent.sh {self.guest_dir} {self.server}"
                                 " >/dev/null 2>&1 &")
        hello = wait_until(lambda: slot_read(self.ctx, f"{self.host_dir}/hello"), 30, interval=0.5)
        if hello is None:
            raise CheckFailed(f"{self.name}: the client did not start in {self.env['name']}")
        self.hello = json.loads(hello)

    def lines(self) -> list[dict]:
        text = slot_read(self.ctx, f"{self.host_dir}/out") or ""
        out = []
        for line in text.splitlines():
            try:
                out.append(json.loads(line))
            except json.JSONDecodeError:
                out.append({"unparsed": line[:300]})
        return out

    def send(self, method: str, params: dict | None = None, notify: bool = False) -> int | None:
        message = {"jsonrpc": "2.0", "method": method, **({"params": params} if params else {})}
        want = None
        if not notify:
            self.next_id += 1
            want = message["id"] = self.next_id
        slot_append(self.ctx, f"{self.host_dir}/in", json.dumps(message))
        return want

    def reply(self, want: int, timeout: float = REPLY_S) -> dict | None:
        """The reply to request `want`, `{"e2e": "eof"}` once the connection closed first, or
        None when neither came in time."""
        def got():
            for line in self.lines():
                if line.get("id") == want or line.get("e2e") == "eof":
                    return line
            return None

        return wait_until(got, timeout, interval=0.5)

    def call(self, method: str, params: dict | None = None, timeout: float = REPLY_S) -> dict:
        reply = self.reply(self.send(method, params), timeout)
        if reply is None:
            raise CheckFailed(f"{self.name}: no reply to {method} within {timeout:.0f} s")
        return reply

    def tool(self, name: str, arguments: dict) -> dict:
        return self.call("tools/call", {"name": name, "arguments": arguments})

    def initialize(self) -> int:
        return self.send("initialize", {"protocolVersion": PROTOCOL, "capabilities": {},
                                        "clientInfo": {"name": CLIENT, "version": "1"}})

    def quit(self) -> None:
        slot_append(self.ctx, f"{self.host_dir}/in", "quit")

    def keep(self) -> None:
        self.ctx.write(f"{self.name}-transcript.json", json.dumps(
            {"hello": getattr(self, "hello", None), "out": self.lines(),
             "err": slot_read(self.ctx, f"{self.host_dir}/err")}, indent=1))


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


def tool_until(agent: Agent, name: str, arguments: dict, ready, timeout: float = 30) -> dict:
    deadline = time.monotonic() + timeout
    while True:
        reply = agent.tool(name, arguments)
        if is_error(reply) is False and ready(text_of(reply)):
            return reply
        if time.monotonic() >= deadline:
            return reply
        time.sleep(1.0)


def tool_answering(ctx, agent: Agent, name: str, arguments: dict, what: str) -> tuple[dict, list]:
    """Call a tool whose action the person may be asked about on the sheet while the call
    waits: they allow it once, as they would for what they asked the agent to do. The reply,
    and what the sheet showed each time it asked."""
    want = agent.send("tools/call", {"name": name, "arguments": arguments})
    asked = []
    deadline = time.monotonic() + REPLY_S + SHEET_S
    while time.monotonic() < deadline:
        reply = agent.reply(want, 2)
        if reply is not None:
            return reply, asked
        if portal_dialog(ctx) and len(asked) < 3:
            asked.append(answer_sheet(ctx, ALLOW_ONCE, f"{what}-{len(asked) + 1}"))
    raise CheckFailed(f"{agent.name}: no reply to {name} within {REPLY_S + SHEET_S:.0f} s "
                      f"(the sheet asked {len(asked)} times)")


# -- the sheet -------------------------------------------------------------------------------


# The sheet's choices for an admission, as the person reads them (sheets en-US.ftl).
CHOICE_LABELS = {ALLOW_ONCE: "Allow once", ALLOW_SESSION: "Allow for this session", ALLOW_ALWAYS: "Always allow"}
# What the sheet's controls are called, in any question it asks.
SHEET_WORDS = ("Allow", "Deny", "Always allow")


def sheet_targets(ctx) -> list[dict]:
    """The controls the person can press on the sheet up now (`list-targets`, the desk's
    named controls, as an assistive technology reads them), or [] when no sheet is up."""
    try:
        targets = ctx.vm.targets()
    except VmError:
        return []
    names = [t for t in targets if any(w.lower() in str(t.get("name", "")).lower() for w in SHEET_WORDS)]
    return targets if names else []


def portal_dialog(ctx) -> dict | None:
    """The question on the sheet, as far as the desk shows it: its controls."""
    targets = sheet_targets(ctx)
    return {"targets": [t.get("name") for t in targets]} if targets else None


def sheet_rows(ctx) -> list[dict]:
    try:
        return [r for r in ctx.vm.actions(programmatic=True) if r.get("application") == SHEET]
    except VmError:
        return []


def press(ctx, name: str) -> None:
    reply = ctx.vm.raw({"op": "click_named", "name": name})
    if isinstance(reply, dict) and reply.get("err"):
        raise CheckFailed(f"pressing {name!r} on the sheet was refused: {reply}")


def answer_sheet(ctx, choice: int, what: str) -> dict:
    """Answer the question up on the sheet with `choice`, as the person does: press the
    choice, then the sheet's Allow if it asks for one. What the sheet showed."""
    dialog = wait_until(lambda: portal_dialog(ctx), SHEET_S, interval=0.5)
    if dialog is None:
        raise CheckFailed(f"{what}: the person was never asked (no question on the sheet within {SHEET_S} s)")
    targets = sheet_targets(ctx)
    ctx.write(f"sheet-{what}.json", json.dumps({"targets": targets, "rows": sheet_rows(ctx)}, indent=1))
    ctx.shot(f"sheet-{what}")
    label = CHOICE_LABELS[choice]
    chosen = [t["name"] for t in targets if label.lower() in str(t.get("name", "")).lower()]
    if not chosen:
        raise Unobservable(f"{what}: the sheet shows no control {label!r}: {[t.get('name') for t in targets]}")
    press(ctx, chosen[0])
    # A sheet whose choices are highlighted first answers on its Allow.
    gone = wait_until(lambda: portal_dialog(ctx) is None or None, 3, interval=0.3)
    if gone is None:
        confirm = [t["name"] for t in sheet_targets(ctx)
                   if str(t.get("name", "")).strip().lower() in ("allow", "confirm", "continue")]
        if confirm:
            press(ctx, confirm[0])
    if wait_until(lambda: portal_dialog(ctx) is None or None, 15, interval=0.5) is None:
        raise CheckFailed(f"{what}: the question stayed up after the person answered it "
                          f"({[t.get('name') for t in sheet_targets(ctx)]})")
    dialog["chosen"] = chosen[0]
    return dialog


def no_sheet(ctx, seconds: float) -> dict | None:
    """The question that came up within `seconds`, or None."""
    return wait_until(lambda: portal_dialog(ctx), seconds, interval=0.5)


def seated(ctx) -> list[dict]:
    """The agents at the person's desk, as the desk lists them (the state dump's `agents`)."""
    return [a for a in ctx.state().get("agents") or [] if isinstance(a, dict)]


def seated_keys(ctx) -> list[str]:
    return sorted(a.get("key", "") for a in seated(ctx) if a.get("seat", True))


# -- the scenario ------------------------------------------------------------------------------


def _scene(ctx) -> dict:
    scene = ctx.facts.get("e2e")
    if scene is None:
        raise Unobservable(f"precondition not met: {ctx.facts.get('e2e:why', 'E01 did not set the desk up')}")
    return scene


def _need(scene: dict, key: str, by: str) -> object:
    if key not in scene:
        raise Unobservable(f"precondition not met: {by} did not finish")
    return scene[key]


@check("E01", AREA, "Two workspaces, Terminal in each, an environment of each workspace: each "
                    "environment's `~` is its workspace's own tree and the other's is out of reach")
def environments(ctx, res):
    ctx.facts["e2e:why"] = "E01 failed"
    ctx.reset()
    run = uuid.uuid4().hex[:8]
    envs = []
    for name, env_name in WORKSPACES:
        workspace = create_workspace(ctx, f"{name} {run}")
        # Notes opened there, as the person would before asking an agent for a note.
        ctx.open_app(NOTES)
        if ctx.wait_presented(NOTES, 45) is None:
            raise CheckFailed(f"Notes showed no frame in {name}")
        terminal = open_terminal(ctx, f"terminal-{name.lower()}", workspace)
        entered = enter_environment(terminal, env_name)
        line = terminal.ask(
            "printf '%s %s %s %s %s\\n' {M}-ENV \"$TILO_WORKSPACE\" \"$TILO_ENVIRONMENT\" "
            "\"${TILO_AGENT_SOCK:-none}\" \"$HOME\"", SHELL_S)[-1].split()
        env = {"name": name, "env_name": env_name, "workspace": workspace, "terminal": terminal,
               "record": entered["record"], "tilo_workspace": line[1], "environment": line[2],
               "agent_sock": line[3], "home": line[4], "probe": f".e2e-probe-{run}-{name.lower()}"}
        envs.append(env)
    res.metrics["environments"] = [{k: e[k] for k in ("name", "record", "tilo_workspace", "environment",
                                                     "agent_sock", "home")} for e in envs]
    problems = []

    def want(ok: bool, message: str) -> None:
        if not ok:
            problems.append(message)

    for e in envs:
        want(e["environment"] == f"{e['tilo_workspace']}/{e['record']}",
             f"{e['name']}: TILO_ENVIRONMENT is {e['environment']!r}, not "
             f"<workspace>/{e['record']} (TILO_WORKSPACE {e['tilo_workspace']!r})")
        want(e["home"] == f"{GUEST_TREES}/{e['tilo_workspace']}",
             f"{e['name']}: ~ is {e['home']}, not its workspace's tree {GUEST_TREES}/{e['tilo_workspace']}")
        want(e["agent_sock"] == "/run/tilo/agent.sock",
             f"{e['name']}: TILO_AGENT_SOCK is {e['agent_sock']!r}, not the environment's door")
    one, two = envs
    want(one["tilo_workspace"] != two["tilo_workspace"], "both environments name one workspace")
    # Each shell writes in its `~`; each sees its own file alone, and not the other's tree.
    for e in envs:
        e["terminal"].run(f"echo {e['name']} > ~/{e['probe']}")
    for e, other in ((one, two), (two, one)):
        lines = e["terminal"].ask(
            "for f in $(ls -a ~); do printf '%s %s\\n' {M}-LS \"$f\"; done; "
            f"if cat {GUEST_TREES}/{other['tilo_workspace']}/{other['probe']} >/dev/null 2>&1; "
            "then echo {M}-OTHER readable; else echo {M}-OTHER refused; fi; "
            f"for f in $(ls {GUEST_TREES} 2>/dev/null); do printf '%s %s\\n' {{M}}-TREE \"$f\"; done",
            30)
        listed = {l.split(" ", 1)[1] for l in lines if l.startswith("-LS ")}
        e["ls"] = sorted(listed)
        want(e["probe"] in listed, f"{e['name']}: `ls ~` does not show the file its shell wrote: {sorted(listed)}")
        want(other["probe"] not in listed, f"{e['name']}: `ls ~` shows {other['name']}'s file")
        verdict = [l for l in lines if l.startswith("-OTHER")]
        want(verdict == ["-OTHER refused"], f"{e['name']}: reading {other['name']}'s tree from its shell: {verdict}")
        trees = [l.split(" ", 1)[1] for l in lines if l.startswith("-TREE ")]
        want(other["tilo_workspace"] not in trees, f"{e['name']}: its shell lists {other['name']}'s tree among {trees}")
    for e in envs:
        e["tree"] = find_tree(ctx, e["probe"])
        slot_write(ctx, f"{e['tree']}/{AGENTS_DIR}/mcp_agent.sh", AGENT.read_bytes())
    want(one["tree"] != two["tree"], f"both workspaces' files are one tree: {one['tree']}")
    res.metrics["trees"] = {e["name"]: e["tree"] for e in envs}
    # The agents' checks run on the desk set up here, whatever of it was wrong.
    ctx.facts["e2e"] = {"envs": envs, "run": run}
    if problems:
        raise CheckFailed("; ".join(problems))


@check("E02", AREA, "An agent in each environment is asked about on the sheet, naming its workspace and "
                    "environment, and both are admitted at once with distinct ids naming their environments")
def admitted(ctx, res):
    scene = _scene(ctx)
    one, two = scene["envs"]
    agents = {}
    for e, choice in ((one, ALLOW_ALWAYS), (two, ALLOW_ONCE)):
        agent = Agent(ctx, e, f"{e['name'].lower()}-1")
        agent.start()
        want = agent.initialize()
        dialog = answer_sheet(ctx, choice, f"{e['name']}-admission")
        reply = agent.reply(want, REPLY_S)
        agent.keep()
        expect(isinstance(reply, dict) and "result" in reply,
               f"{e['name']}: initialize was not answered after the person allowed it: {str(reply)[:300]}")
        e["dialog"] = dialog
        agent.send("notifications/initialized", notify=True)
        agents[e["name"]] = agent
    res.metrics["sheets"] = {e["name"]: e["dialog"] for e in (one, two)}
    keys = wait_until(lambda: (k if len(k := seated_keys(ctx)) >= 2 else None), 20, interval=0.5) or seated_keys(ctx)
    res.metrics["seated"] = seated(ctx)
    for e in (one, two):
        want = f"{CLIENT}@{e['record'].lower()}#"
        mine = [k for k in keys if k.startswith(want)]
        expect(len(mine) == 1, f"{e['name']}: the desk seats no single agent {want}<n>: {keys}")
        e["agent_id"] = mine[0]
    expect(one["agent_id"] != two["agent_id"], f"both agents are {one['agent_id']}")
    res.metrics["agents"] = {e["name"]: e["agent_id"] for e in (one, two)}
    scene["agents"] = agents


@check("E03", AREA, "Each agent's tool calls act in its own workspace: its note lands there and not in the "
                    "other, and the change is its own")
def own_workspace(ctx, res):
    scene = _scene(ctx)
    agents = _need(scene, "agents", "E02")
    one, two = scene["envs"]
    titles = {e["name"]: f"E2E {e['name']} note {scene['run']}" for e in (one, two)}
    for e in (one, two):
        agent = agents[e["name"]]
        tools = agent.call("tools/list")
        names = sorted(t.get("name") for t in (tools.get("result") or {}).get("tools") or [])
        expect("tilo_do" in names and "tilo_find" in names, f"{e['name']}: tools/list offers {names}")
        listing = tool_until(agent, "tilo_ls", {"path": "notes"}, lambda t: "#new-note" in t, 45)
        expect("#new-note" in text_of(listing),
               f"{e['name']}: Notes, open in {e['name']}, lists no new-note on the door: {str(listing)[:300]}")
        made, e["asked_for_note"] = tool_answering(
            ctx, agent, "tilo_do", {"path": "notes#new-note", "args": {"title": titles[e["name"]]}},
            f"{e['name']}-new-note")
        e["new_note"] = made
        agent.keep()
        expect(is_error(made) is False, f"{e['name']}: new-note was refused: {str(made)[:400]}")
    res.metrics["new_note"] = {e["name"]: text_of(e["new_note"])[:200] for e in (one, two)}
    res.metrics["asked_for_note"] = {e["name"]: e["asked_for_note"] for e in (one, two)}
    for e, other in ((one, two), (two, one)):
        agent = agents[e["name"]]
        found = tool_until(agent, "tilo_find", {"query": titles[e["name"]]}, lambda t: titles[e["name"]] in t)
        expect(titles[e["name"]] in text_of(found),
               f"{e['name']}: the agent does not find its own note: {str(found)[:300]}")
        theirs = agent.tool("tilo_find", {"query": titles[other["name"]]})
        expect(titles[other["name"]] not in text_of(theirs),
               f"{e['name']}: the agent finds {other['name']}'s note: {text_of(theirs)[:300]}")
        e["read_back"] = text_of(found)[:400]
    # The desk itself, not the agents' replies: what each workspace holds, read by the person.
    for e, other in ((one, two), (two, one)):
        select_workspace(ctx, e["workspace"])
        rows = ctx.vm.actions(titles[e["name"]])
        mine = [r for r in rows if r.get("application") == NOTES]
        ctx.write(f"E03-{e['name']}-listing.json", json.dumps(rows, indent=1))
        expect(mine, f"{e['name']}: the desk does not list the note {titles[e['name']]!r} in {e['name']}")
        rooms = {r.get("room") for r in mine}
        e["note_rooms"] = sorted(str(r) for r in rooms)
    expect(not set(one["note_rooms"]) & set(two["note_rooms"]),
           f"the two notes are in one room: {one['note_rooms']} / {two['note_rooms']}")
    res.metrics["note_rooms"] = {e["name"]: e["note_rooms"] for e in (one, two)}
    attribution = {}
    for e in (one, two):
        agent = agents[e["name"]]
        status = agent.tool("tilo_status", {})
        attribution[e["name"]] = text_of(status)[:600]
    res.metrics["status"] = attribution
    for e in (one, two):
        expect(e["agent_id"] in attribution[e["name"]] or e["agent_id"] in e["read_back"]
               or e["agent_id"] in text_of(e["new_note"]),
               f"{e['name']}: nothing the agent's change shows names {e['agent_id']}: "
               f"{attribution[e['name']][:200]!r}")


@check("E04", AREA, "An agent in an environment asking for the other workspace is refused, before anyone "
                    "is asked")
def other_workspace(ctx, res):
    scene = _scene(ctx)
    _need(scene, "agents", "E02")
    one, two = scene["envs"]
    agent = Agent(ctx, one, "studio-asks-garden",
                  server=f"tilo-runtime --connect --workspace {two['workspace']['id']}")
    agent.start()
    reply = agent.reply(agent.initialize(), REPLY_S)
    asked = no_sheet(ctx, 3)
    agent.keep()
    res.metrics["reply"] = reply
    if asked:
        answer_sheet(ctx, ALLOW_ONCE, "studio-asks-garden-unexpected")
    expect(asked is None, f"the person was asked about it: {asked}")
    expect(isinstance(reply, dict) and ("error" in reply or reply.get("e2e") == "eof") and "result" not in reply,
           f"asking for {two['name']} from {one['name']}'s environment was not refused: {str(reply)[:300]}")
    agent.quit()


@check("E05", AREA, "\"Always allow\" admits a second session in the same environment without the sheet, "
                    "as an agent of its own with the next instance number")
def always_allow(ctx, res):
    scene = _scene(ctx)
    _need(scene, "agents", "E02")
    one, _ = scene["envs"]
    agent = Agent(ctx, one, f"{one['name'].lower()}-2")
    agent.start()
    want = agent.initialize()
    asked = no_sheet(ctx, 5)
    if asked:
        answer_sheet(ctx, ALLOW_ONCE, "second-session-unexpected")
    reply = agent.reply(want, REPLY_S)
    agent.keep()
    expect(asked is None, f"the person was asked again after Always allow: {asked}")
    expect(isinstance(reply, dict) and "result" in reply, f"the second session was not admitted: {str(reply)[:300]}")
    agent.send("notifications/initialized", notify=True)
    first = one["agent_id"]
    base, _, n = first.rpartition("#")
    second = f"{base}#{int(n) + 1}" if n.isdigit() else None
    keys = wait_until(lambda: (k if second in (k := seated_keys(ctx)) else None), 20, interval=0.5) or seated_keys(ctx)
    res.metrics["seated"] = keys
    expect(second in keys and first in keys, f"the desk does not seat {first} and {second}: {keys}")
    scene["second"] = agent


@check("E06", AREA, "The person's Stop on one agent ends that one alone; the other keeps working")
def stop_one(ctx, res):
    scene = _scene(ctx)
    agents = _need(scene, "agents", "E02")
    one, two = scene["envs"]
    ctx.vm.invoke(*_desk_stop(ctx, two["agent_id"]))
    stopped = agents[two["name"]]
    ended = wait_until(lambda: any(l.get("e2e") == "eof" for l in stopped.lines()), 20, interval=0.5)
    keys = seated_keys(ctx)
    res.metrics["seated_after"] = keys
    stopped.keep()
    expect(ended, f"{two['name']}'s connection did not close on the person's Stop")
    expect(two["agent_id"] not in keys, f"{two['agent_id']} is still at the desk: {keys}")
    expect(one["agent_id"] in keys, f"{one['agent_id']} left the desk with it: {keys}")
    still = agents[one["name"]]
    reply = still.tool("tilo_ls", {})
    still.keep()
    expect(is_error(reply) is False, f"{one['name']}'s agent stopped working too: {str(reply)[:300]}")


def _desk_stop(ctx, agent_id: str) -> tuple[str, str, dict]:
    rows = [r for r in ctx.vm.actions(programmatic=True) if r.get("operation") == "stop-agent"]
    ctx.write("E06-stop-rows.json", json.dumps(rows, indent=1))
    if not rows:
        raise Unobservable("the desk offers no Stop (stop-agent) action")
    row = rows[0]
    return row["provider"], row["id"], {"agent": agent_id, "confirm": True}


# The host probe: a program of the login, outside every environment, connecting to an
# environment's door directly, its path given as the client finds a door (TILO_AGENT_SOCK).
HOST_PROBE = r'''
import json, os, socket, sys
path = sys.argv[1]
out = {"path": path}
try:
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(20)
    s.connect(path)
    s.sendall((json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "claude-code", "version": "1"}}}) + "\n").encode())
    data = b""
    while b"\n" not in data:
        chunk = s.recv(65536)
        if not chunk:
            break
        data += chunk
    out["reply"] = data.decode(errors="replace")[:600]
except Exception as exc:
    out["error"] = f"{type(exc).__name__}: {exc}"
print("E2E-HOST " + json.dumps(out))
'''


@check("E07", AREA, "A program of the login outside the environments cannot use an environment's door")
def host_refused(ctx, res):
    scene = _scene(ctx)
    _need(scene, "agents", "E02")
    listed = ctx.vm.exec('ls -1 "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/tilo-session/environments/" 2>&1; true')
    socks = [l.strip() for l in listed.splitlines() if l.strip() and not l.startswith("receipt")]
    res.metrics["doors"] = socks
    if not socks:
        raise Unobservable(f"no environment door under tilo-session/environments: {listed[:300]}")
    results = []
    for sock in socks:
        path = f'"${{XDG_RUNTIME_DIR:-/run/user/$(id -u)}}"/tilo-session/environments/{shlex.quote(sock)}'
        out = ctx.vm.exec(f"python3 -c {shlex.quote(HOST_PROBE)} {path}", timeout=60, check=False)
        line = next((l for l in out.splitlines() if l.startswith("E2E-HOST ")), None)
        results.append(json.loads(line[len("E2E-HOST "):]) if line else {"raw": out[-300:]})
    asked = no_sheet(ctx, 2)
    res.metrics["host"] = results
    if asked:
        answer_sheet(ctx, ALLOW_ONCE, "host-unexpected")
    expect(asked is None, f"the person was asked about a host program on an environment's door: {asked}")
    for r in results:
        reply = r.get("reply", "")
        expect('"result"' not in reply, f"a host program was served on {r.get('path')}: {reply[:300]}")
        expect("error" in r or '"error"' in reply or reply == "",
               f"no refusal from {r.get('path')}: {r}")
    # The end: every client still connected ends, as Claude Code quitting does.
    for agent in list(scene.get("agents", {}).values()) + [scene.get("second")]:
        if agent is not None:
            agent.quit()
