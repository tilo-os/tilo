"""A workspace set up once and branched, end to end, on one booted tilo box.

A person sets a workspace up the way they work in it: a note, files (a question, a tool in
`~/.local/bin`, a git checkout where the environment has git, and a few hundred megabytes of
data), a Terminal environment, windows on its shell. They branch it per research question or per
customer: through the shell's `workspace.branch` (twice) and `tilo workspace branch` (once). A
branch starts as a copy of everything but the people: its owner is its only member, and the
source's chat stays behind. From then on the branches and their source change apart: an edit in
one (a note, a file, an environment) shows in no other, also after the session restarts. An
agent started in a branch's environment works in that branch alone.

Everything goes through the production paths, as `agents_e2e` does: the shell's own actions
(`new-workspace`, `switch-workspace`, `workspace.branch`), the apps' own actions (Notes'
`new_note` and `read`, Terminal's `create-environment`, `open-in`, `run`, `read`), the person's
`tilo` command, the sheet's own answers, and a scripted MCP client configured as Claude Code
(`guest/mcp_agent.sh`). The harness reads the workspaces' trees through the account's own account
(`tilo-vm account exec`).

The checks share one scenario and run in order (WB01..WB07, `run.py` WSB_ORDER); each needs the
ones before it. `t test e2e-ws-branch` runs this area alone (tools/ci/vm-acceptance.sh
e2e-ws-branch); the vm-acceptance shards never run it.
"""

from __future__ import annotations

import json
import re
import shlex
import statistics
import threading
import time
import uuid

from .. import core, session_unit
from ..core import CheckFailed, Unobservable, check, expect
from ..vm import VmError, address, label as action_label, wait_until
from . import agents_e2e as ae

AREA = "ws-branch-e2e"
# The person's workspace, and the branches they make of it: two through the shell's action, one
# through `tilo workspace branch`.
SOURCE = "Research"
BRANCHES = ("Question A", "Question B")
CLI_BRANCH = "Question C"
ENVIRONMENT = "Lab"
# What the person keeps in the workspace's files, under its tree (the environments' `~`).
QUESTION = "research/question.md"
TOOL = ".local/bin/e2e-tool"
CHECKOUT = "src/notes-tools"
DATA_DIR = "research/data"
# A few hundred megabytes of data: one large file and many small ones, as a research corpus is.
BULK_MB = 256
SMALL_FILES = 400
SMALL_KB = 64
# How long a branch of that may take before the check gives up on it.
BRANCH_S = 600
# How long one answer of the shell may take while a branch runs before the branch blocks the shell:
# this many seconds, or four times the slowest answer just before it. A shell the copy held would
# not answer for as long as the copy runs, a minute and more; the busy VM, drawing in software
# beside the copy, answers a state dump in 0.3-1 s and now and then in 3-4 s.
SHELL_STALL_S = 5.0
# How long an app may take to open a window while the box is busy copying and drawing in software.
TERMINAL_S = 120
# The shell's own Branch workspace entry, as the person reads it, and the action's names.
BRANCH_ENTRY = "Branch workspace"
WORKSPACE_BRANCH = "workspace.branch"
BRANCH_IDS = (WORKSPACE_BRANCH, "branch-workspace", "workspace-branch")
CHAT = "so.tilo.chat"
# Where Chat keeps its documents: the id its package was first published under (apps/tl/chat
# app.tl `previously:`).
CHAT_DOCUMENTS = "so.tilo.lang.chat"
# The channel the person says something in (apps/tl/chat app.tl `new_channel`).
CHAT_CHANNEL = "e2e"
# An app's own data in a workspace, on this device (launcher appdir.rs).
WORKSPACE_DATA = ".local/share/tilo-workspaces"


# -- the shell ------------------------------------------------------------------------------


def workspace_rows(state: dict) -> list[dict]:
    return (state.get("workspace_tree") or {}).get("workspaces") or []


def find_ws(state: dict, ws: dict) -> dict | None:
    return next((w for w in workspace_rows(state) if w["id"] == ws["id"]), None)


def shell_apps(state: dict, ws: dict) -> list[str]:
    """The apps of the windows on `ws`'s shell, sorted, a tab each."""
    row = find_ws(state, ws) or {}
    return sorted(str(core.tab_app(t)) for s in row.get("strips") or [] for g in s.get("window_groups") or []
                  for t in g.get("tabs") or [])


def close_app_windows(ctx, app: str) -> bool:
    """Close every window of `app` through its group's own close action, as the person does;
    whether they all went (a window whose app ended may offer no close of its own)."""
    gids = {str(g["id"]) for g in core.all_groups(ctx.state())
            if any(app in str(core.tab_app(t)) for t in g.get("tabs") or [])}
    for gid in gids:
        ctx.vm.invoke(core.SESSION, *core.CHROME_CLOSE_WINDOW, {"group": gid})
    return bool(wait_until(lambda: not {str(g["id"]) for g in core.all_groups(ctx.state())} & gids or None, 20,
                           interval=0.5))


def awake(ctx) -> bool:
    """Keep the session from its idle lock through a long wait, as a person at it would (run.py
    does so between checks only); always true, to sit in a wait's predicate."""
    core.keep_awake(ctx)
    return True


def ws_key(ws: dict, field: str) -> str:
    return str(ws.get(field))


def branch_action(ctx) -> dict:
    """The shell's `workspace.branch`, where `workspace.create` is published: its row."""
    def found():
        rows = [r for r in ctx.vm.actions("branch") if address(r)[1] in BRANCH_IDS]
        return rows or None

    rows = wait_until(found, 20, interval=1.0)
    if not rows:
        ctx.write("WB02-branch-rows.json", json.dumps(ctx.vm.actions("branch"), indent=1))
        raise CheckFailed(f"the shell publishes no {WORKSPACE_BRANCH}")
    ctx.write("WB02-branch-action.json", json.dumps(rows, indent=1))
    return rows[0]


def branch_args(row: dict, source: str, name: str) -> dict:
    """{source, name} under the names the action's own parameters give them."""
    keys = {p.get("name") for p in row["action"].get("params") or []}
    which = next((k for k in ("source", "workspace", "from") if k in keys), "source")
    called = next((k for k in ("name", "new_name", "title") if k in keys), "name")
    return {which: source, called: name}


def answered_id(value) -> str | None:
    """The workspace id an action or the command answered: a string, or a map naming it."""
    if isinstance(value, (str, int)) and not isinstance(value, bool):
        return str(value)
    if isinstance(value, dict):
        for key in ("workspace", "id", "workspace", "workspace_id"):
            if isinstance(value.get(key), (str, int)) and not isinstance(value.get(key), bool):
                return str(value[key])
    return None


def by_answer(state: dict, answer: str, before: set) -> dict | None:
    """The new workspace the answer names, by any of its ids."""
    for w in workspace_rows(state):
        if w["id"] not in before and answer == str(w.get("id")):
            return w
    return None


class ShellProbe:
    """How long the shell takes to answer, sampled while something else runs: a thread asks for
    the state dump back to back and keeps each answer's time."""

    def __init__(self, ctx):
        self.ctx, self.times, self.errors = ctx, [], []
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self.loop, daemon=True)

    def loop(self) -> None:
        while not self.stop.is_set():
            started = time.monotonic()
            try:
                self.ctx.vm.state()
                self.times.append(round(time.monotonic() - started, 3))
            except VmError as exc:
                self.errors.append(exc.reason()[:200])
                self.times.append(round(time.monotonic() - started, 3))
            self.stop.wait(0.2)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *exc):
        self.stop.set()
        self.thread.join(timeout=120)


def shell_baseline(ctx, n: int = 10) -> list[float]:
    times = []
    for _ in range(n):
        started = time.monotonic()
        ctx.vm.state()
        times.append(round(time.monotonic() - started, 3))
        time.sleep(0.2)
    return times


# -- Notes, through its own actions ------------------------------------------------------------


def app_rows(ctx, app: str) -> list[dict]:
    """What `app` offers, from the whole listing: a listing that changes while it is paged (apps
    opening again after a restart) is read again a little later."""
    for attempt in range(6):
        try:
            return [r for r in ctx.vm.actions() if r.get("app_id") == app]
        except VmError as exc:
            if "changed on each" not in str(exc) or attempt == 5:
                raise
            time.sleep(5)
    return []


def offered(ctx, app: str) -> str:
    """What `app` offers in front, by action and provider, for a failure to say."""
    return json.dumps(sorted({(address(r)[1], str(r.get("provider")), r["resource"]["id"])
                              for r in app_rows(ctx, app)}))


def note_rows(ctx, operation: str) -> list[dict]:
    return [r for r in app_rows(ctx, ae.NOTES) if address(r)[1] == operation]


def ensure_notes(ctx, ws: dict) -> None:
    """Notes open in `ws`, in front: the one the branch brought on its shell, else a new one."""
    ae.select_workspace(ctx, ws)
    if wait_until(lambda: len(note_rows(ctx, "new_note")) == 1 or None, 10, interval=1.0):
        return
    ctx.open_app(ae.NOTES, timeout=TERMINAL_S)
    # A Notes started on the busy, software-drawn box publishes its actions once it has drawn.
    if not wait_until(lambda: len(note_rows(ctx, "new_note")) == 1 or None, TERMINAL_S, interval=1.0):
        everything = ctx.vm.root_exec(
            "journalctl --no-pager -o cat | sed 's/\\x1b\\[[0-9;]*m//g' | grep -a 'notes' "
            "| grep -avE 'FRAME-LIFECYCLE|paint-stats|andlock|DBUS_SESSION|optional service' "
            "| tail -n 80 | cut -c1-500", timeout=180, check=False)
        names = sorted({str(t.get("name")) for t in ctx.vm.targets() if "/" not in str(t.get("name"))})
        raise Unobservable(f"{ws.get('name')}: Notes in front offers no single new_note; it offers "
                           f"{offered(ctx, ae.NOTES)}; the shell names {names[:60]}; what was said of Notes: "
                           f"{everything[-9000:]}")


def note_titles(ctx, ws: dict) -> list[str]:
    """The titles of the notes in `ws`'s notebook, as Notes' own `read` answers them."""
    ensure_notes(ctx, ws)
    rows = wait_until(lambda: note_rows(ctx, "read"), 30, interval=1.0)
    if not rows:
        raise Unobservable(f"{ws.get('name')}: Notes in front offers no read; it offers {offered(ctx, ae.NOTES)}")
    answer = ae.invoke(ctx, rows[0]["provider"], *address(rows[0]), {}, name=f"notes-read-{ws['id']}")
    if isinstance(answer, str):
        try:
            answer = json.loads(answer)
        except json.JSONDecodeError:
            return [answer]
    pages = (answer or {}).get("pages") if isinstance(answer, dict) else None
    if pages is None:
        raise Unobservable(f"{ws.get('name')}: Notes' read answered no pages: {str(answer)[:300]}")
    return sorted(str(p.get("title")) for p in pages)


def new_note(ctx, ws: dict, title: str) -> None:
    ensure_notes(ctx, ws)
    try:
        ae.person_note(ctx, {"name": ws.get("name"), "workspace": ws}, title)
    except VmError as exc:
        # What the job ended with (action_jobs.PendingJob keeps it), and what Notes said.
        status = ctx.out / "data" / f"{ws.get('name')}-person-note-status.json"
        ended = status.read_text()[-1500:] if status.exists() else "no status kept"
        everything = ctx.vm.root_exec(
            "journalctl --no-pager -o cat | sed 's/\\x1b\\[[0-9;]*m//g' | grep -a 'so.tilo.notes' "
            "| grep -avE 'FRAME-LIFECYCLE|paint-stats|andlock|DBUS_SESSION|optional service' "
            "| tail -n 120 | cut -c1-600", timeout=180, check=False)
        kept = ctx.vm.exec(f'cd "$HOME/{WORKSPACE_DATA}/{ws.get("id")}/{ae.NOTES}" 2>/dev/null && '
                           "find . -type f -printf '%s %p\\n' | sort -k2 | head -n 80", check=False)
        ctx.write(f"{ws.get('name')}-notes-said.txt", everything + "\n---\n" + kept)
        raise CheckFailed(f"{ws.get('name')}: Notes' new_note {title!r} did not succeed: {exc.reason()}; "
                          f"it ended {ended}; Notes keeps here {kept.split(chr(10))[:80]}; "
                          f"Notes said: {everything[-9000:]}") from exc


# -- the trees ---------------------------------------------------------------------------------


def long_exec(ctx, script: str, seconds: int) -> str:
    """`account exec` of a script that may take longer than its default bound: writing or reading
    the hundreds of megabytes of a workspace's data."""
    return ctx.vm.run("account", "exec", "--timeout", str(seconds), "--", "sh", "-c", script,
                      timeout=seconds + 60, check=False)


def tree_read(ctx, tree: str, rel: str) -> str | None:
    return ae.account_read(ctx, f"{tree}/{rel}")


def tree_digest(ctx, tree: str, rel: str) -> str | None:
    out = long_exec(ctx, f"f={shlex.quote(tree + '/' + rel)}; [ -f \"$f\" ] && sha256sum < \"$f\" | cut -c1-64",
                    600).strip()
    return out or None


def tree_count(ctx, tree: str, rel: str) -> int:
    out = long_exec(ctx, f"find {shlex.quote(tree + '/' + rel)} -type f 2>/dev/null | wc -l", 300).strip()
    return int(out) if out.isdigit() else 0


def tree_bytes(ctx, tree: str) -> int:
    out = long_exec(ctx, f"du -sb {shlex.quote(tree)} 2>/dev/null | cut -f1", 600).strip()
    return int(out) if out.isdigit() else 0


# -- Terminal in a workspace -------------------------------------------------------------------


def shell_in(ctx, ws: dict, label: str, environment: str, record: str | None = None) -> tuple[ae.Terminal, dict]:
    """A Terminal in `ws` moved into its environment named `environment`, as the person picks it
    from Terminal's environment picker, which lists what the runtime that keeps the environments
    lists for the workspace (`runtime_environments`). `record`, where given, is the key it must
    have: a branch keeps its source's environments under their keys. The Terminal, and the record
    with the environments listed."""
    known = runtime_environments(ctx, ws)
    named = [e for e in known if e.get("name") == environment]
    if len(named) != 1 or (record is not None and named[0]["record"] != record):
        raise CheckFailed(f"{label}: the environments of {ws.get('name')} are {known}, not one {environment!r}"
                          + (f" kept as {record}" if record else ""))
    record = named[0]["record"]
    terminal = terminal_in(ctx, ws, label)
    terminal.call("open-in", {"target": f"environment:{record}", "confirm": True})

    def there():
        now = terminal.inspect()
        target = now.get("target") or {}
        return now if now.get("lifecycle") == "open" and target.get("record") == record else None

    if wait_until(there, ae.SHELL_S, interval=1.0) is None:
        raise CheckFailed(f"{label}: the session did not open in {environment} ({record}) in "
                          f"{ws.get('name')}: {terminal.inspect()}")
    return terminal, {"record": record, "known": [e.get("name") for e in known]}


RUNTIME_LIST = r'''
import json, os, socket, sys
path = os.path.join(os.environ.get("XDG_RUNTIME_DIR") or f"/run/user/{os.getuid()}", "tilo/services/ush.sock")
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.settimeout(30)
s.connect(path)
s.sendall((json.dumps({"op": "list-environments", "workspace": sys.argv[1]}) + "\n").encode())
data = b""
while b"\n" not in data:
    chunk = s.recv(65536)
    if not chunk:
        break
    data += chunk
print("E2E-RUNTIME " + data.decode(errors="replace").strip())
'''


def runtime_environments(ctx, ws: dict) -> list[dict]:
    """The environments of `ws` as the runtime that keeps them lists them (`list-environments`,
    which Terminal's picker asks when it opens): `[{record, name}]`."""
    out = ctx.vm.exec(f"python3 -c {shlex.quote(RUNTIME_LIST)} {shlex.quote(str(ws.get('id')))}", check=False)
    line = next((l for l in out.splitlines() if l.startswith("E2E-RUNTIME ")), None)
    try:
        reply = json.loads(line[len("E2E-RUNTIME "):]) if line else {}
    except json.JSONDecodeError:
        reply = {}
    if reply.get("result") != "environments":
        raise CheckFailed(f"the runtime did not list {ws.get('name')}'s environments: {(line or out)[-400:]}")
    return [{"record": e.get("key"), "name": e.get("name")} for e in reply.get("environments") or []]


def terminal_in(ctx, ws: dict, label: str) -> ae.Terminal:
    """A Terminal session in `ws`: one on its shell already (a branch's shell opens the source's
    windows again), else a Terminal the person opens there."""
    ae.select_workspace(ctx, ws)

    def mine():
        return sorted({ae.identity(r) for r in ae.terminal_rows(ctx)}, key=str) or None

    there = wait_until(mine, 5, interval=1.0)
    if there:
        terminal = ae.Terminal(ctx, there[0], label, ws)
    else:
        before = {ae.identity(r) for r in ae.terminal_rows(ctx)}
        try:
            ctx.open_app(ae.TERMINAL, timeout=TERMINAL_S)
        except CheckFailed as exc:
            raise CheckFailed(f"Terminal did not open in {ws.get('name')}: {exc}; "
                              f"{app_evidence(ctx, ae.TERMINAL)}") from exc
        new = wait_until(lambda: (n if len(n := {ae.identity(r) for r in ae.terminal_rows(ctx)} - before) == 1
                                  else None), TERMINAL_S, interval=1.0)
        if new is None:
            raise CheckFailed(f"{label}: the Terminal opened in {ws.get('name')} published no session of its own")
        terminal = ae.Terminal(ctx, next(iter(new)), label, ws)
    info = wait_until(lambda: (i if (i := terminal.inspect()).get("lifecycle") == "open" else None),
                      ae.SHELL_S, interval=1.0)
    if info is None:
        raise CheckFailed(f"{label}: Terminal's session in {ws.get('name')} never opened")
    return terminal


SCRIPTS = ".e2e-scripts"


def in_shell(ctx, terminal: ae.Terminal, tree: str, name: str, script: str) -> None:
    """Run `script` in `terminal`'s shell, as a script the person keeps in the workspace's files
    and runs there: the shell types one short line, never the script, so nothing of it is lost to
    the typing on a busy box. Returns once the script has run."""
    done = f"{SCRIPTS}/{name}.done"
    # Every command's complaint goes to the script's log, and the end writes its own exit status:
    # a person's commands that succeed say nothing.
    body = "\n".join(f"{line} || echo \"failed ($?): {line.strip()}\" >&2" if line.strip() and not
                      line.lstrip().startswith(("if ", "fi", "then", "else")) and not line.rstrip().endswith(
                          ("then", "do", "\\")) else line for line in script.strip().splitlines())
    ae.account_write(ctx, f"{tree}/{SCRIPTS}/{name}.sh",
                  (f"exec 2>/tmp/e2e-{name}.log\n{body}\ncat /tmp/e2e-{name}.log > ~/{done}\n"
                   f"echo ok >> ~/{done}\n").encode())
    terminal.bash(f"bash ~/{SCRIPTS}/{name}.sh")
    said = wait_until(lambda: tree_read(ctx, tree, done), ae.SHELL_S, interval=1.0)
    if said is None:
        ctx.write(f"{name}-screen.txt", terminal.screen())
        raise CheckFailed(f"{terminal.name}: ~/{SCRIPTS}/{name}.sh did not finish in its shell: "
                          f"{terminal.screen()[-1500:]}")
    if said.strip() != "ok":
        raise CheckFailed(f"{terminal.name}: ~/{SCRIPTS}/{name}.sh failed in its shell: {said[-1500:]}")


# What a shell in an environment says about itself and its tree, a field a line.
SAY = ("for v in TILO_WORKSPACE TILO_ENVIRONMENT HOME; do printf '%s %s %s\\n' {M}-ENV \"$v\" \"${!v:-none}\"; "
       "done; printf '%s %s\\n' {M}-TOOL \"$(~/" + TOOL + " 2>&1 | head -1 | tr ' ' _)\"; "
       "printf '%s %s\\n' {M}-QUESTION \"$(head -1 ~/" + QUESTION + " 2>/dev/null | tr ' ' _)\"; "
       "if command -v git >/dev/null; then printf '%s %s\\n' {M}-GIT "
       "\"$(git -C ~/" + CHECKOUT + " log -1 --format=%s 2>&1 | tr ' ' _)\"; else echo {M}-GIT absent; fi")


def said(lines: list[str]) -> dict:
    out = {}
    for line in lines:
        parts = line.split()
        if len(parts) == 3 and parts[0] == "-ENV":
            out[parts[1]] = parts[2]
        elif len(parts) == 2 and parts[0] in ("-TOOL", "-QUESTION", "-GIT"):
            out[parts[0][1:].lower()] = parts[1].replace("_", " ")
        elif len(parts) == 1 and parts[0] in ("-TOOL", "-QUESTION", "-GIT"):
            out[parts[0][1:].lower()] = ""
    return out


def reach(terminal: ae.Terminal, others: list[tuple[str, str]]) -> dict:
    """From `terminal`'s shell: whether each other tree's question is readable, and whether
    the trees folder lists it."""
    script = ""
    for name, tid in others:
        path = f"{ae.GUEST_TREES}/{tid}/{QUESTION}"
        script += (f"if cat {shlex.quote(path)} >/dev/null 2>&1; then echo {{M}}-READ {name} readable; "
                   f"else echo {{M}}-READ {name} refused; fi; ")
    script += f"for f in $(ls {ae.GUEST_TREES} 2>/dev/null); do printf '%s %s\\n' {{M}}-TREE \"$f\"; done"
    lines = terminal.ask(script, 30)
    read = {l.split()[1]: l.split()[2] for l in lines if l.startswith("-READ ") and len(l.split()) == 3}
    trees = [l.split(" ", 1)[1] for l in lines if l.startswith("-TREE ")]
    return {"read": read, "trees": trees}


# -- the scenario ------------------------------------------------------------------------------


def _scene(ctx) -> dict:
    scene = ctx.facts.get("wsb")
    if scene is None:
        raise Unobservable(f"precondition not met: {ctx.facts.get('wsb:why', 'WB01 did not set the workspace up')}")
    return scene


def _need(scene: dict, key: str, by: str):
    if key not in scene:
        raise Unobservable(f"precondition not met: {by} did not finish")
    return scene[key]


class Problems(list):
    def want(self, ok: bool, message: str) -> None:
        if not ok:
            self.append(message)

    def judge(self) -> None:
        if self:
            raise CheckFailed("; ".join(self))


@check("WB01", AREA, "The person sets a workspace up: a note, files, a Terminal environment with a tool "
                     "and a checkout in it, windows on its shell, a few hundred MB of data")
def set_up(ctx, res):
    ctx.facts["wsb:why"] = "WB01 failed"
    ctx.facts["wsb:debug-switch"] = ensure_debug_switch(ctx)
    ctx.reset()
    # The device's files are served, or nothing after this can be: a files service that does not
    # come names itself here (fs.service, /var/workspace), not as an environment's lost probe.
    ae.files_served(ctx)
    # A restart here left the environment's files unfound (rdb278da293): the session keeps
    # running, and what it said is read since the machine booted.
    watch_drawing_threads(ctx)
    since = 0
    ctx.facts["wsb:since"] = since
    run = uuid.uuid4().hex[:8]
    ws = ae.create_workspace(ctx, f"{SOURCE} {run}")
    ws["name"] = f"{SOURCE} {run}"
    note = f"Sources {run}"
    new_note(ctx, ws, note)
    # The workspace's chat says something, which its branches leave behind; the person closes
    # Chat again, so the shell keeps Notes and Terminal.
    chat = f"Chat line {run}"
    problems = Problems()
    # No drawing thread crashed since the machine started (Mesa's device-select layer, at the
    # session's start on a machine without 3D): told first, with the processes they were.
    crashed = ctx.vm.root_exec("journalctl -k -b --no-pager 2>/dev/null "
                               "| grep -a 'native-render-p.*segfault' | sort -u | tail -n 6", timeout=60,
                               check=False).strip()
    problems.want(not crashed, f"drawing threads crashed since the machine started: {crashed[-1200:]}; "
                               f"their processes: {crashed_processes(ctx)}")
    watch_chat(ctx)
    try:
        post_chat(ctx, ws, chat)
        heard = wait_until(lambda: (h if (h := chat_says(ctx, ws, chat, "source"))["read"] or h["listed"]
                                    or h["shown"] else None), 30, interval=2.0) or chat_says(ctx, ws, chat, "source")
        problems.want(heard["read"] or heard["listed"] or heard["shown"], f"the chat of {ws['name']} does not hold what the "
                                                        f"person said there: {heard}")
    except CheckFailed as exc:
        # The rest of the workspace is set up all the same; the branches' chat is then judged by
        # what this device keeps of it alone.
        problems.append(str(exc))
        chat = None
    res.metrics["chat_closed"] = close_app_windows(ctx, CHAT)
    terminal = ae.open_terminal(ctx, "terminal-source", ws)
    entered = ae.enter_environment(terminal, ENVIRONMENT)
    probe = f".e2e-probe-{run}"
    terminal.bash(f"echo source > ~/{probe}")
    tree = ae.find_tree(ctx, probe)
    # The person's files and their environment's tool, made from the environment's shell as they
    # would: a question, a tool on PATH, a git checkout where the environment has git.
    in_shell(ctx, terminal, tree, "set-up", f"""
mkdir -p ~/{DATA_DIR} ~/.local/bin
printf '%s\\n' 'Question {run}' 'first line of thought' > ~/{QUESTION}
printf '%s\\n' '#!/bin/bash' 'echo e2e-tool {run} source' > ~/{TOOL}
chmod +x ~/{TOOL}
if command -v git >/dev/null; then
  mkdir -p ~/{CHECKOUT} && cd ~/{CHECKOUT} && git init -q && git config user.email p@e2e.example
  git config user.name Person && echo tools > README && git add README && git commit -qm 'checkout {run}'
fi
""")
    lines = terminal.ask(SAY, ae.SHELL_S)
    facts = said(lines)
    res.metrics["source_shell"] = facts
    problems.want(facts.get("tool") == f"e2e-tool {run} source",
                  f"the tool the person wrote does not run in {ENVIRONMENT}: {facts.get('tool')!r}")
    written = tree_read(ctx, tree, QUESTION)
    problems.want(written == f"Question {run}\nfirst line of thought\n",
                  f"the question the person wrote reads {written!r} in the workspace's files")
    # The data, written into the workspace's tree by a program of the login as Files does.
    started = time.monotonic()
    wrote = long_exec(
        ctx, f"cd {shlex.quote(tree)} && head -c {BULK_MB}M /dev/urandom > {DATA_DIR}/corpus.bin && "
        f"i=0; while [ $i -lt {SMALL_FILES} ]; do head -c {SMALL_KB}K /dev/urandom > {DATA_DIR}/part-$i.bin; "
        "i=$((i + 1)); done; sync; echo E2E-WROTE", 1800)
    res.metrics["data_write_s"] = round(time.monotonic() - started, 1)
    if "E2E-WROTE" not in wrote:
        raise CheckFailed(f"the data did not go into {tree} within 1800 s: {wrote[-400:]}")
    ws.update(tree=tree, tilo_workspace=facts.get("TILO_WORKSPACE", ""), record=entered["record"])
    field = "id" if ws_key(ws, "id") == ws["tilo_workspace"] else None
    problems.want(field is not None, f"TILO_WORKSPACE {ws['tilo_workspace']!r} is not the workspace's id "
                                     f"{ws.get('id')!r}")
    problems.want(tree.endswith("/" + ws["tilo_workspace"]),
                  f"the workspace's tree {tree} is not named for {ws['tilo_workspace']}")
    state = ctx.state()
    apps = shell_apps(state, ws)
    problems.want(ae.NOTES in apps and ae.TERMINAL in apps, f"the shell of {ws['name']} shows {apps}")
    size = tree_bytes(ctx, tree)
    ctx.record("ws_branch.source_mib", "MiB", [round(size / 2**20, 1)])
    # The branches are judged against this, whatever of the reading below goes wrong.
    ctx.facts["wsb"] = {
        "run": run, "source": ws, "note": note, "chat": chat, "field": field or "id", "apps": apps, "terminal": terminal,
        "facts": facts, "question": tree_read(ctx, tree, QUESTION),
        "corpus": tree_digest(ctx, tree, f"{DATA_DIR}/corpus.bin"),
        "files": tree_count(ctx, tree, DATA_DIR), "bytes": size,
        "git": facts.get("git", "absent"),
    }
    res.metrics.update(workspace={k: ws.get(k) for k in ("id", "name", "tilo_workspace", "tree", "record")},
                       shell=apps, bytes=size, git=facts.get("git"))
    try:
        titles = note_titles(ctx, ws)
        res.metrics["notes"] = titles
        problems.want(note in titles, f"Notes in {ws['name']} lists {titles}, not {note!r}")
    except CheckFailed as exc:
        problems.append(str(exc))
    if chat:
        problems.want(chat_dir(ctx, ws), f"this device keeps no chat of {ws['name']} after the person used it")
    try:
        members = members_of(ctx, ws)
        res.metrics["members"] = members
        problems.want(bool(members["owner"]), f"{ws['name']} has no owner: {members}")
    except CheckFailed as exc:
        problems.append(str(exc))
    problems.judge()


def branch_by_action(ctx, source: dict, name: str, field: str) -> tuple[dict, dict]:
    """Branch `source` into `name` through the shell's `workspace.branch`, as the person's shell
    asks for it, timing it to the branch's data in its tree and sampling the shell meanwhile.
    The new workspace, and what was measured."""
    before = {w["id"] for w in workspace_rows(ctx.state())}
    row = branch_action(ctx)
    args = branch_args(row, str(source["id"]), name)
    baseline = shell_baseline(ctx)
    started = time.monotonic()
    with ShellProbe(ctx) as probe:
        answer = ae.invoke(ctx, row["provider"], *address(row), args, confirmed=True,
                           name=f"branch-{name.split()[-1].lower()}", timeout=BRANCH_S)
        answered = time.monotonic() - started
        new_id = answered_id(answer)
        if new_id is None:
            raise CheckFailed(f"{WORKSPACE_BRANCH} answered no workspace id: {str(answer)[:300]}")
        # The branch goes on the shell once what it holds is copied.
        made = wait_until(lambda: awake(ctx) and by_answer(ctx.state(), new_id, before), BRANCH_S, interval=0.5)
        if made is None:
            raise CheckFailed(f"the shell shows no workspace {new_id} {WORKSPACE_BRANCH} answered")
        ctx.wait_state(lambda s: ae.held_here(s, made["id"]), 120, f"{name} held on this device")
        held = time.monotonic() - started
    return made, {"answered_s": round(answered, 2), "held_s": round(held, 2), "answer": answer,
                  "shell_baseline_s": baseline, "shell_during_s": probe.times, "shell_errors": probe.errors}


def branch_said(ctx, name: str, workspace: str) -> dict:
    """What the shell told the person when the branch `name` was made ("<name> is ready", or "<name>
    is not complete" with what did not come along), as its notifications name it, and what the
    runtime that keeps the environments recorded of the branch into `workspace`."""
    def told():
        names = [str(t.get("name")) for t in ctx.vm.targets() if name in str(t.get("name", ""))]
        return [n for n in names if "is ready" in n or "is not complete" in n] or None

    notices = wait_until(told, 15, interval=0.5) or []
    # What did not come along, as the session logged it (the notice's body is not on screen).
    why = session_said(ctx, "not complete|did not reach|did not come|keys did not|branch",
                       30, ctx.facts.get("wsb:since", 0)) if any(
        "is not complete" in n for n in notices) else ""
    audit = ctx.vm.exec("cat \"$HOME\"/.local/share/ush/audit/* \"$HOME\"/.local/share/ush/*/audit/* 2>/dev/null "
                        f"| grep -a branch-workspace | grep -a {shlex.quote(workspace)} | tail -n 2 | cut -c1-600",
                        check=False)
    shown = []
    if any("is not complete" in n for n in notices):
        # Anywhere in the shell's state: the notice's body, one missing part a line.
        try:
            state = ctx.state()
        except VmError:
            state = {}
        said = re.compile(r"did not reach|did not come|keys did not| of so\.tilo\.|the Terminal environments|"
                          r"the workspace's files|what the apps keep|could not found")
        found: list[str] = []

        def walk(node):
            if isinstance(node, dict):
                for value in node.values():
                    walk(value)
            elif isinstance(node, list):
                for value in node:
                    walk(value)
            elif isinstance(node, str) and said.search(node) and node not in found:
                found.append(node[:600])
        walk(state)
        shown += found[:10]
        # The notice's body lists what did not come along, one part a line ("<record> of <app>:
        # <why>", "… did not reach this device"), where the shell shows it.
        shown += sorted({str(t.get("name")) for t in ctx.vm.targets()
                        if re.search(r" of [a-z0-9-]+\.[a-z0-9.-]+|did not reach|keys|could not|this device",
                                     str(t.get("name", "")))})[:20]
        why += "\n" + ctx.vm.exec("journalctl --user --no-pager 2>/dev/null | grep -ac 'Suppressed' ; "
                                   "journalctl --no-pager 2>/dev/null | grep -a 'Suppressed' | tail -n 3",
                                   timeout=60, check=False)
    return {"ready": [n for n in notices if "is ready" in n], "missing": [n for n in notices if "is not complete" in n],
            "why": [l for l in why.splitlines() if l.strip()], "shown": shown,
            "runtime": [l for l in audit.splitlines() if l.strip()]}


def branch_by_cli(ctx, source: dict, name: str, field: str) -> tuple[dict, dict]:
    """Branch `source` into `name` with `tilo workspace branch`, as the person types it."""
    before = {w["id"] for w in workspace_rows(ctx.state())}
    started = time.monotonic()
    out = ctx.vm.exec(f"tilo workspace branch {shlex.quote(ws_key(source, field))} {shlex.quote(name)} 2>&1; "
                      "echo E2E-STATUS $?", timeout=BRANCH_S, check=False)
    seconds = round(time.monotonic() - started, 2)
    status = out.rsplit("E2E-STATUS", 1)[-1].strip()
    if status != "0":
        raise CheckFailed(f"`tilo workspace branch` exited {status}: {out[-600:]}")
    text = out.rsplit("E2E-STATUS", 1)[0].strip()
    # The branch goes on the shell once what it holds is copied; the command says its id at once.
    made = wait_until(lambda: awake(ctx) and next((w for w in workspace_rows(ctx.state())
                                    if w["id"] not in before and (w.get("name") == name or str(w.get("id")) in text)),
                                   None), BRANCH_S, interval=1.0)
    if made is None:
        raise CheckFailed(f"`tilo workspace branch` said {text[-300:]!r}; the shell shows no workspace {name!r}")
    ctx.wait_state(lambda s: ae.held_here(s, made["id"]), 120, f"{name} held on this device")
    named = [w for w in (str(made.get("id")),) if w in text]
    return made, {"seconds": seconds, "said": text[-300:], "names_it": bool(named)}


@check("WB02", AREA, "The person branches the workspace twice through the shell's workspace.branch and once "
                     "with `tilo workspace branch`; each branch is timed, and none blocks the shell")
def branch(ctx, res):
    scene = _scene(ctx)
    source, run, field = scene["source"], scene["run"], scene["field"]
    problems = Problems()
    # The shell offers the person its Branch workspace entry.
    entries = ctx.vm.actions("Branch")
    ctx.write("WB02-entries.json", json.dumps(entries, indent=1))
    # By its action, whatever words the catalogue gives its title ("Branch the workspace").
    offered = [r for r in entries if address(r)[1] in BRANCH_IDS
               or action_label(r).lower() in (BRANCH_ENTRY.lower(), "branch a workspace", "branch the workspace")]
    res.metrics["entry"] = [ae.listing_row(r) for r in offered][:5]
    problems.want(bool(offered), f"the shell offers no {BRANCH_ENTRY!r} entry")
    branches, timings = [], {}
    for label in BRANCHES:
        name = f"{label} {run}"
        made, timing = branch_by_action(ctx, source, name, field)
        made["name"] = made.get("name") or name
        branches.append(made)
        # The branch's data, in its own tree, whole: the large file's digest and the count of files.
        tid = ws_key(made, field)
        tree = scene["source"]["tree"].rsplit("/", 1)[0] + "/" + tid
        started = time.monotonic()
        whole = wait_until(lambda: awake(ctx) and tree_count(ctx, tree, DATA_DIR) >= scene["files"] or None,
                           BRANCH_S, interval=2.0)
        timing["data_s"] = round(timing["held_s"] + (time.monotonic() - started), 2)
        made["tree"] = tree
        timing["said"] = branch_said(ctx, name, str(made.get("id")))
        problems.want(not timing["said"]["missing"], f"{name} is not complete: {timing['said']}")
        during = timing["shell_during_s"]
        timing["shell_max_s"] = max(during) if during else None
        timing["shell_p50_s"] = statistics.median(during) if during else None
        timings[label] = timing
        ctx.write(f"WB02-{label.split()[-1]}-timing.json", json.dumps(timing, indent=1, default=str))
        ctx.record("ws_branch.branch_s", "s", [timing["data_s"]], mib=round(scene["bytes"] / 2**20, 1))
        ctx.record("ws_branch.shell_answer_max_s", "s", [timing["shell_max_s"] or 0])
        problems.want(whole, f"{name}'s tree {tree} holds {tree_count(ctx, tree, DATA_DIR)} of "
                             f"{scene['files']} data files {BRANCH_S} s after the branch")
        problems.want(made.get("name") == name, f"the branch is named {made.get('name')!r}, not {name!r}")
        base = max(timing["shell_baseline_s"])
        stalls = [t for t in during if t > max(SHELL_STALL_S, 4 * base)]
        problems.want(not stalls, f"branching {name} blocked the shell: {len(stalls)} of {len(during)} answers "
                                  f"took over {max(SHELL_STALL_S, 4 * base):.1f} s (longest {max(stalls or [0]):.1f} s, "
                                  f"{base:.2f} s before the branch)")
        problems.want(not timing["shell_errors"], f"the shell did not answer while branching {name}: "
                                                 f"{timing['shell_errors'][:3]}")
    name = f"{CLI_BRANCH} {run}"
    try:
        made, timing = branch_by_cli(ctx, source, name, field)
        made["name"] = made.get("name") or name
        made["tree"] = scene["source"]["tree"].rsplit("/", 1)[0] + "/" + ws_key(made, field)
        timings[CLI_BRANCH] = timing
        timing["said_complete"] = branch_said(ctx, name, str(made.get("id")))
        problems.want(not timing["said_complete"]["missing"], f"{name} is not complete: {timing['said_complete']}")
        problems.want(timing["names_it"], f"`tilo workspace branch` did not say the new workspace's id: "
                                          f"{timing['said']!r}")
        scene["cli_branch"] = made
    except CheckFailed as exc:
        problems.append(str(exc))
    ids = [source["id"]] + [b["id"] for b in branches] + ([scene["cli_branch"]["id"]] if "cli_branch" in scene else [])
    problems.want(len(set(ids)) == len(ids), f"the branches are not workspaces of their own: {ids}")
    res.metrics["branches"] = [{k: b.get(k) for k in ("id", "name", "tree")} for b in branches]
    res.metrics["timings"] = {k: {x: v for x, v in t.items() if x not in ("shell_during_s", "answer")}
                              for k, t in timings.items()}
    scene["branches"] = branches
    if problems:
        problems.append(session_told(ctx))
    problems.judge()


def members_of(ctx, ws: dict) -> dict:
    """The workspace's owner and members, as its governance says and the shell projects it
    (`policy`: owner, members person to role)."""
    row = find_ws(ctx.state(), ws) or {}
    policy = row.get("policy")
    if not isinstance(policy, dict) or not isinstance(policy.get("members"), dict):
        raise Unobservable(f"the shell projects no members of {ws.get('name')}: {str(policy)[:300]}")
    return {"owner": policy.get("owner"), "members": sorted(policy["members"])}


def owner_alone(members: dict, owner: str) -> bool:
    """Whether the person `owner` owns the workspace and nobody else is a member of it (the
    owner is a member as its owner, whether or not the members name them again)."""
    return members.get("owner") == owner and set(members.get("members") or []) <= {owner}


# -- the chat ----------------------------------------------------------------------------------


def chat_rows(ctx, operation: str) -> list[dict]:
    return [r for r in app_rows(ctx, CHAT) if address(r)[1] == operation]


def chat_dir(ctx, ws: dict) -> bool:
    """Whether this device keeps Chat's data for `ws` (an app's data in a workspace)."""
    out = ctx.vm.exec(f'[ -d "$HOME/{WORKSPACE_DATA}/{ws["id"]}/{CHAT_DOCUMENTS}" ] && echo E2E-YES || echo E2E-NO',
                      check=False)
    return "E2E-YES" in out


def chat_says(ctx, ws: dict, text: str, label: str) -> dict:
    """Whether `ws`'s chat holds `text`, Chat opened there: in what Chat's own `read` answers,
    and among what the shell's search lists in front."""
    ae.select_workspace(ctx, ws)
    open_by_id(ctx, CHAT)
    rows = wait_until(lambda: chat_rows(ctx, "read") or None, 30, interval=1.0)
    read = ae.invoke(ctx, rows[0]["provider"], *address(rows[0]), {}, name=f"chat-read-{label}") if rows else None
    listed = [r for r in ctx.vm.actions(text) if text in json.dumps(r)]
    shown = [t.get("name") for t in ctx.vm.targets() if text in str(t.get("name", ""))]
    ctx.write(f"chat-{label}.json", json.dumps({"read_rows": rows, "read": read, "listed": listed, "shown": shown,
                                               "offered": offered(ctx, CHAT)}, indent=1, default=str))
    return {"read": text in json.dumps(read, default=str), "listed": bool(listed), "shown": bool(shown),
            "readable": rows is not None}


def app_window(ctx, app: str) -> dict | None:
    """The shell's mapped window of `app`, if one is: a row of its window listing with the app's id
    and a place on screen (a tab held for a window still to come has none)."""
    try:
        windows = ctx.vm.windows()
    except VmError:
        return None
    return next((w for w in windows if w.get("app_id") == app and w.get("rect")), None)


def open_by_id(ctx, app: str, patience: float = TERMINAL_S) -> None:
    """Open `app` in the workspace in front by its id, as the launcher does, and wait until a window
    of it is mapped on the shell: a window it already had, raised, counts as much as a new one.
    What the app offers is no sign it opened: an app is listed with its `open` row, and with the
    actions its package declares, before any process of it runs (rb95baefd41, r22dca47ed0: Chat
    "offered" those and the open was never sent)."""
    deadline = time.monotonic() + patience
    asked = False
    while not app_window(ctx, app):
        awake(ctx)
        if time.monotonic() > deadline:
            raise CheckFailed(f"{app} shows no window {patience:.0f} s after it was "
                              f"{'asked for' if asked else 'looked for in the catalogue'}: "
                              f"it offers {offered(ctx, app)}; {app_evidence(ctx, app)}")
        if not asked:
            try:
                ctx.vm.open(app)
                asked = True
            except VmError as exc:
                if "unknown_action" not in str(exc):
                    raise
        time.sleep(2)


def holds(heard: dict) -> bool:
    """Whether what `chat_says` heard holds the text: in Chat's read, the listing, or on screen."""
    return any(heard.get(key) for key in ("read", "listed", "shown"))


def post_chat(ctx, ws: dict, text: str) -> None:
    """The person says `text` in `ws`'s chat through Chat's own actions: a channel of its own
    (`new_channel`, Chat's first action, offered before any channel exists), then `send` there."""
    ae.select_workspace(ctx, ws)
    open_by_id(ctx, CHAT)
    made = wait_until(lambda: chat_rows(ctx, "new_channel") or None, 30, interval=1.0)
    if not made:
        raise Unobservable(f"Chat in {ws.get('name')} offers no new_channel; it offers {offered(ctx, CHAT)}; "
                           f"{app_evidence(ctx, CHAT)}; {session_told(ctx)}")
    ae.invoke(ctx, made[0]["provider"], *address(made[0]), {"name": CHAT_CHANNEL}, confirmed=True,
              name="chat-new-channel")
    sends = wait_until(lambda: chat_rows(ctx, "send") or None, 30, interval=1.0)
    if not sends:
        raise Unobservable(f"Chat in {ws.get('name')} offers no send once its channel is made; it offers "
                           f"{offered(ctx, CHAT)}; {app_evidence(ctx, CHAT)}")
    ae.invoke(ctx, sends[0]["provider"], *address(sends[0]), {"body": text}, confirmed=True, name="chat-send")


TASKS_LOG = "/var/tmp/e2e-drawing-threads.log"


def watch_drawing_threads(ctx) -> None:
    """For the run's next 40 minutes, note every process with a thread preparing to draw, by the
    thread's id: the kernel names a crashed thread only by that id and its name."""
    ctx.vm.root_exec(
        "setsid nohup sh -c 'end=$(( $(date +%s) + 2400 )); while [ $(date +%s) -lt $end ]; do "
        "ps -eLo tid=,pid=,comm=,cgroup:90=,args:160= 2>/dev/null | grep -a native-render >> "
        f"{TASKS_LOG}; sleep 0.2; done' </dev/null >/dev/null 2>&1 &", timeout=30, check=False)


def crashed_threads(ctx) -> str:
    """Which processes the drawing threads that crashed were, as [`watch_drawing_threads`] saw them."""
    return ctx.vm.root_exec(
        "journalctl -k --no-pager | grep -ao 'native-render-p\\[[0-9]*\\]: segfault' | grep -o '[0-9]*' "
        f"| sort -u | while read -r t; do grep -a \"^ *$t \" {TASKS_LOG} | tail -n 1; done | cut -c1-300 "
        "| head -n 12", timeout=60, check=False)


# The processes the kernel's crashed drawing threads were, from the boot's journal: the thread id
# it names is a little above its process's id, so the journal's entries of the processes started
# just before it say which app, unit and command each was.
CRASHED_PROCESSES = r"""
import json, re, subprocess
kernel = subprocess.run(["journalctl", "-k", "-b", "--no-pager"], capture_output=True, text=True).stdout
tids = sorted({int(t) for t in re.findall(r"native-render-p\[(\d+)\]: segfault", kernel)})
if not tids:
    print("none")
    raise SystemExit
entries = subprocess.run(["journalctl", "-b", "-o", "json", "--no-pager", "--output-fields",
                          "_PID,_COMM,_CMDLINE,_SYSTEMD_UNIT,_SYSTEMD_USER_UNIT,SYSLOG_IDENTIFIER"],
                         capture_output=True, text=True).stdout.splitlines()
seen = {}
for line in entries:
    try:
        e = json.loads(line)
        pid = int(e.get("_PID", 0))
    except (ValueError, TypeError):
        continue
    seen.setdefault(pid, e)
for tid in tids:
    near = [(pid, e) for pid, e in seen.items() if tid - 60 <= pid <= tid]
    told = "; ".join(f"{pid} {e.get('_COMM') or e.get('SYSLOG_IDENTIFIER')} "
                     f"{e.get('_SYSTEMD_USER_UNIT') or e.get('_SYSTEMD_UNIT')} "
                     f"{str(e.get('_CMDLINE', ''))[:160]}" for pid, e in sorted(near)[-3:])
    print(f"thread {tid}: {told or 'no process of the journal near it'}")
"""


# Every running process that loaded Vulkan: its command, and whether the device-select switch and
# a pinned driver reached it. The crashed ones are gone; the ones of their kind that run say which
# path starts them without the switch.
VULKAN_PROCESSES = r"""
import os
for pid in sorted((p for p in os.listdir("/proc") if p.isdigit()), key=int):
    try:
        maps = open(f"/proc/{pid}/maps").read()
        if "libvulkan" not in maps:
            continue
        env = dict(e.split("=", 1) for e in open(f"/proc/{pid}/environ").read().split("\0") if "=" in e)
        args = open(f"/proc/{pid}/cmdline").read().replace("\0", " ")[:140]
        comm = open(f"/proc/{pid}/comm").read().strip()
        layer = "device_select loaded" if "device_select" in maps else "no device_select"
    except OSError:
        continue
    print(pid, comm, f"NODEVICE_SELECT={env.get('NODEVICE_SELECT', '-')}",
          f"VK_DRIVER_FILES={os.path.basename(env.get('VK_DRIVER_FILES', '-'))}", layer, args)
"""


def crashed_processes(ctx) -> str:
    """Which processes the drawing threads that crashed since boot were."""
    named = ctx.vm.root_exec("python3 -I - <<'PY'\n" + CRASHED_PROCESSES + "\nPY", timeout=180,
                             check=False).strip()[-800:]
    running = ctx.vm.root_exec("python3 -I - <<'PY'\n" + VULKAN_PROCESSES + "\nPY", timeout=120,
                               check=False).strip()[-1500:]
    return f"{named}; the processes running Vulkan now: {running}"


CHAT_PS = "/var/tmp/e2e-chat-processes.log"
CHAT_DETAIL = "/var/tmp/e2e-chat-detail.log"

# Follows the processes in the scopes of the apps APPS names (cgroups, not command lines: a
# sandboxed app's own command need not name it) every 0.3 s for SECONDS: when each scope and process
# came and went and how long it lived (to PS), and what each process was doing at first sight and
# 3, 15 and 60 s old (to DETAIL): its threads and what they wait in, its main thread's kernel stack,
# the kinds of files it holds open, and its display and GPU environment.
SCOPE_WATCH = r"""
import collections, glob, os, time
apps, ps, detail, seconds = APPS, "PS", "DETAIL", SECONDS
out, more = open(ps, "a", buffering=1), open(detail, "a", buffering=1)
def read(path, keep=4000):
    try:
        with open(path, "rb") as f:
            return f.read(keep).decode("utf-8", "replace")
    except OSError as e:
        return f"({e.strerror})"
def snapshot(pid, age, scope):
    d = f"/proc/{pid}"
    threads = collections.Counter()
    for t in glob.glob(f"{d}/task/*"):
        stat = read(f"{t}/stat").rsplit(")", 1)[-1].split()
        threads[(read(f"{t}/comm").strip(), stat[0] if stat else "?", read(f"{t}/wchan").strip())] += 1
    fds = collections.Counter()
    for fd in glob.glob(f"{d}/fd/*"):
        try:
            target = os.readlink(fd)
        except OSError:
            continue
        kind = target.split(":")[0] if target.startswith(("socket:", "pipe:", "anon_inode:")) else target
        if kind.startswith("/home") or kind.startswith("/var/home") or kind.startswith("/run/user"):
            kind = kind.rsplit("/", 1)[0] + "/..."
        fds[kind] += 1
    env = [v for v in read(f"{d}/environ", 65536).split("\0") if v.split("=")[0] in (
        "WAYLAND_DISPLAY", "XDG_RUNTIME_DIR", "NODEVICE_SELECT", "TILO_LAUNCHER_DEBUG_STDERR", "RUST_LOG",
        "WGPU_BACKEND", "WGPU_ADAPTER_NAME", "LIBGL_ALWAYS_SOFTWARE", "MESA_LOADER_DRIVER_OVERRIDE",
        "VK_ICD_FILENAMES", "VK_DRIVER_FILES", "DISPLAY", "TILO_RENDER_NODE")]
    more.write(f"== {time.strftime('%T')} pid {pid} at {age:.1f}s in {scope}: "
               f"{read(d + '/cmdline', 400).replace(chr(0), ' ')[:300]}\n")
    more.write("threads: " + "; ".join(f"{n}x {c} {s} {w}" for (c, s, w), n in threads.most_common(14)) + "\n")
    more.write("stack: " + " | ".join(read(f"{d}/stack").splitlines()[:10]) + "\n")
    more.write("files: " + ", ".join(f"{k} x{n}" for k, n in fds.most_common(16)) + "\n")
    more.write("env: " + " ".join(v[:160] for v in env) + "\n")
scopes, pids, end, passes, beat = {}, {}, time.time() + seconds, 0, 0.0
while time.time() < end:
    now = time.time()
    passes += 1
    # A line a minute: a file with no event in it then says nothing came, not that the watch stopped.
    if now - beat >= 60:
        beat = now
        out.write(f"{time.strftime('%T')} watching: pass {passes}, {len(scopes)} scopes, {len(pids)} processes\n")
    found = {}
    for app in apps:
        for c in glob.glob(f"/sys/fs/cgroup/user.slice/*/*/*/app-tilo-{app}-*.scope") + \
                 glob.glob(f"/sys/fs/cgroup/user.slice/*/*/*/*/app-tilo-{app}-*.scope"):
            found[c] = app
    stamp = time.strftime("%T") + f".{int(now * 10) % 10}"
    for c in found:
        if c not in scopes:
            scopes[c] = now
            out.write(f"{stamp} scope {os.path.basename(c)} came\n")
    for c in [c for c in scopes if c not in found]:
        out.write(f"{stamp} scope {os.path.basename(c)} went after {now - scopes.pop(c):.1f}s\n")
    live = {}
    for c in found:
        for pid in read(f"{c}/cgroup.procs").split():
            if pid.isdigit():
                live[pid] = os.path.basename(c)
    for pid, scope in live.items():
        if pid not in pids:
            pids[pid] = [now, set()]
            stat = read(f"/proc/{pid}/stat").rsplit(")", 1)[-1].split()
            out.write(f"{stamp} pid {pid} came in {scope}, parent {stat[1] if len(stat) > 1 else '?'}: "
                      f"{read(f'/proc/{pid}/comm').strip()} {read(f'/proc/{pid}/cmdline', 300).replace(chr(0), ' ')[:200]}\n")
        age = now - pids[pid][0]
        for mark in (0, 3, 15, 60):
            if age >= mark and mark not in pids[pid][1]:
                pids[pid][1].add(mark)
                snapshot(pid, age, scope)
    for pid in [p for p in pids if p not in live]:
        out.write(f"{stamp} pid {pid} went after {now - pids.pop(pid)[0]:.1f}s\n")
    time.sleep(0.3)
"""


def watch_chat(ctx, seconds: int = 1800) -> None:
    """For the next `seconds`, follow the processes in Chat's scopes, and in Notes' to compare
    with an app whose window comes ([`SCOPE_WATCH`])."""
    script = (SCOPE_WATCH.replace("APPS", repr([CHAT, ae.NOTES])).replace('"PS"', repr(CHAT_PS))
              .replace('"DETAIL"', repr(CHAT_DETAIL)).replace("SECONDS", str(seconds)))
    ctx.vm.root_exec(
        f"cat > /var/tmp/e2e-scope-watch.py <<'PY'\n{script}\nPY\n"
        "setsid nohup python3 -I /var/tmp/e2e-scope-watch.py </dev/null >/var/tmp/e2e-scope-watch.err 2>&1 &",
        timeout=30, check=False)


# What the session logs of an app that did not open and a branch that is not complete: Chat's
# launches, the launch refusals, an app that ended, and each part a branch missed.
TOLD = (r"so\.tilo\.chat|not complete|could not open|launch was refused|launch did not start|"
        r"an app stopped|an app closed|a branch of a workspace|did not reach|did not come|keys did not")


def session_log(ctx, pattern: str, lines: int = 40) -> str:
    """The lines of the account session's own log that match `pattern`: `events.log`, the session's log
    events alone that the measurement stream does not push out, then the full record
    `core.SESSION_LOG` names, each line once."""
    logs = core.SESSION_LOG.rsplit("/", 1)[0]
    return ctx.vm.exec(
        f"cat \"{logs}/events.log\" \"{core.SESSION_LOG}\" 2>/dev/null | awk '!seen[$0]++' "
        "| sed 's/\\x1b\\[[0-9;]*m//g' | grep -av 'FRAME-LIFECYCLE\\|paint-stats\\|HEAP-MILESTONE' "
        f"| grep -aiE {shlex.quote(pattern)} | tail -n {lines} | cut -c1-500", timeout=180, check=False)


def session_said(ctx, pattern: str, lines: int = 40, since: int = 0) -> str:  # noqa: ARG001
    """What the account session logged that matches `pattern`, from its own log files."""
    return session_log(ctx, pattern, lines)


def session_told(ctx, lines: int = 60) -> str:
    """What the session said of the apps that did not open and the branches not complete
    ([`TOLD`]): for a WB01, WB02 or WB05 failure, last in its message."""
    return "the session log said: " + (session_log(ctx, TOLD, lines).strip() or "(nothing of it)")


# The session's log level for this check: its branches and its launches in full. run.py's drop-in
# carries the same (tools/ci/vm-acceptance.sh sets TILO_ACCEPTANCE_SESSION_ENV for it).
SESSION_LOG_LEVEL = "info,tilo_session::workspace_branch=debug,tilo_session::apps=debug"


def ensure_debug_switch(ctx) -> str:
    """Have the running session keep each app's own output and log its branches and launches in
    full: without a `RUST_LOG` it logs errors alone (tilo_startup_metrics::log_directives), so
    none of the warnings this check reads would be there. run.py writes the drop-in before the
    session first starts; when the running session did not get it, WB01 writes it with the same
    writer (session_unit.environment_script), restarts the session, and says every step."""
    running = debug_switch(ctx)
    if "TILO_LAUNCHER_DEBUG_STDERR=1" in running:
        return f"the session started with it: {running.strip()[-500:]}"
    pairs = ["TILO_LAUNCHER_DEBUG_STDERR=1", f"RUST_LOG={SESSION_LOG_LEVEL}"]
    try:
        wrote = ctx.vm.root_exec(session_unit.environment_script(ctx.vm.account, pairs), timeout=90)
    except VmError as exc:
        wrote = f"the writer failed: {exc}"
    since = ctx.vm.exec("date +%s", check=False).strip()
    try:
        ctx.vm.restart_session(timeout=300)
    except VmError as exc:
        said = restart_evidence(ctx, since)
        ctx.write("WB01-restart.txt", said)
        raise CheckFailed(f"the session did not come back after WB01 restarted it with its debug switch: "
                          f"{exc}; {said[-5000:]}") from exc
    ctx.facts.pop("awake_at", None)
    report = (f"run.py's drop-in was not in effect:\n{running.strip()[-600:]}\nWB01's writer said:\n"
              f"{wrote.strip()[-1500:]}\nafter the restart:\n{debug_switch(ctx).strip()[-800:]}")
    ctx.write("WB01-debug-switch.txt", report)
    return report


def restart_evidence(ctx, since: str) -> str:
    """Why the account session did not come back after a restart at `since`: its unit's state and
    journal, the kernel's word on a crash or kill, core dumps, and the session's own log."""
    unit = f"tilo-vm-session@{ctx.vm.account}.service"
    noise = "FRAME-LIFECYCLE|paint-stats|HEAP-MILESTONE|DEVICE-MEMORY|PUMP-PHASE"
    return ctx.vm.root_exec(
        f"echo '== {unit}'; systemctl status --no-pager -n 0 {unit} 2>&1 | head -n 12; "
        f"echo '== its journal since the restart'; journalctl --no-pager -o short-precise -u {unit} "
        f"--since @{since or 0} 2>&1 | sed 's/\\x1b\\[[0-9;]*m//g' | grep -avE '{noise}' | tail -n 60 | cut -c1-400; "
        f"echo '== the kernel'; journalctl -k --no-pager --since @{since or 0} 2>&1 "
        "| grep -aiE 'segfault|oom|killed process|trap|general protection' | tail -n 6; "
        f"echo '== core dumps'; coredumpctl --no-pager list --since @{since or 0} 2>&1 | tail -n 4; "
        "for f in /home/*/.local/state/tilo/session-logs/events.log /var/home/*/.local/state/tilo/session-logs/events.log "
        "/home/*/.local/state/tilo/session-logs/latest.log /var/home/*/.local/state/tilo/session-logs/latest.log; do "
        "[ -f \"$f\" ] || continue; echo \"== $f\"; sed 's/\\x1b\\[[0-9;]*m//g' \"$f\" "
        f"| grep -avE '{noise}' | tail -n 50 | cut -c1-400; done",
        timeout=240, check=False)


def debug_switch(ctx) -> str:
    """What the running session has of the debug switch: the drop-ins, what its unit says, and
    each `tilo-session` process's own environment, with the displays it was started under."""
    try:
        return ctx.vm.root_exec(session_unit.environment_state(ctx.vm.account), timeout=60)
    except VmError as exc:
        return f"(could not read it: {exc})"


def app_output(ctx, ws: dict, app: str) -> str:
    """What `app` itself wrote when opened in `ws` (where [`keep_app_output`] took effect), and the
    end the session saw of it."""
    try:
        ae.select_workspace(ctx, ws)
        open_by_id(ctx, app)
    except (CheckFailed, Unobservable, VmError) as exc:
        return f"(it did not open: {exc})"
    time.sleep(60)
    return "switch: " + debug_switch(ctx)[-600:] + "\n" + ctx.vm.root_exec(
        "find / \\( -path /proc -o -path /sys -o -path /dev \\) -prune -o -name launcher-debug-output.log "
        f"-path '*{app}*' -print 2>/dev/null | head -n 3 | while read -r f; do echo \"== $f\"; "
        "tail -c 5000 \"$f\"; done", timeout=300, check=False) + "\n" + package_servicees(ctx, app)[-3000:]


def package_servicees(ctx, app: str) -> str:
    """`app`'s running processes: each one's threads, what they wait in, and what its scope
    logged, whatever words it used. For an app whose window never came."""
    return ctx.vm.root_exec(
        f"for c in $(find /sys/fs/cgroup -type d -name 'app-tilo-{app}-*.scope' 2>/dev/null); do "
        "echo \"== $c\"; for p in $(cat \"$c\"/cgroup.procs 2>/dev/null); do "
        "echo \"pid $p $(tr '\\0' ' ' < /proc/$p/cmdline | cut -c1-200)\"; "
        "for t in /proc/$p/task/*; do echo \"$(cat $t/comm) $(cut -d' ' -f3 $t/stat) $(cat $t/wchan 2>/dev/null)\"; "
        "done | sort | uniq -c | sort -rn | head -n 25; done; u=$(basename \"$c\"); "
        "journalctl --no-pager -o short-precise _SYSTEMD_USER_UNIT=$u 2>/dev/null | sed 's/\\x1b\\[[0-9;]*m//g' "
        "| grep -avE 'FRAME-LIFECYCLE|paint-stats' | tail -n 60 | cut -c1-300; done",
        timeout=180, check=False) + "\n== its processes by name\n" + ctx.vm.root_exec(
        f"ps -eo pid,ppid,etimes,stat,wchan:20,cgroup:60,args | grep -a '{app.rsplit('.', 1)[-1]}' "
        "| grep -av grep | cut -c1-300 | head -n 20", timeout=60, check=False
    ) + "\n== its scopes' logs, ended ones too\n" + ctx.vm.exec(
        f"journalctl --user --no-pager -o short-precise -u 'app-tilo-{app}-*' 2>&1 "
        "| sed 's/\\x1b\\[[0-9;]*m//g' | grep -avE 'FRAME-LIFECYCLE|paint-stats' | tail -n 80 | cut -c1-300",
        timeout=180, check=False)


def app_evidence(ctx, app: str) -> str:
    """What the session said about `app` lately, and what the shell says of it: for a failure where
    `app` did not open."""
    said = ctx.vm.root_exec(
        "journalctl --no-pager -o cat | sed 's/\\x1b\\[[0-9;]*m//g' "
        f"| grep -a {shlex.quote(app)} | grep -aE 'ERROR|WARN|refus|fail|stopp|denied' "
        "| grep -avE 'FRAME-LIFECYCLE|paint-stats|andlock|realtime events unavailable|no instance route' "
        "| tail -n 30 | cut -c1-500", timeout=180, check=False)
    shown = sorted({str(t.get("name")) for t in ctx.vm.targets()
                    if "/" not in str(t.get("name")) and ("stopped" in str(t.get("name")) or "ended" in str(t.get("name")))})
    return f"the shell shows {shown}; the session said: {said[-6000:]}"


def contents(ctx, scene: dict, ws: dict, label: str) -> tuple[dict, list[str]]:
    """What `ws`, a branch, holds of its source, and what is wrong with it: each part judged on its
    own, so one that cannot be read leaves the others judged."""
    problems = Problems()
    got: dict = {}

    def part(name: str, judge) -> None:
        try:
            judge()
        except (CheckFailed, VmError) as exc:
            problems.append(f"{label}: {name}: {exc}")

    def shell():
        state = ctx.state()
        problems.want(find_ws(state, ws) is not None, f"{label}: the shell no longer shows it")
        got["shell"] = shell_apps(state, ws)
        problems.want(all(a in got["shell"] for a in scene["apps"] if CHAT not in a),
                      f"{label}: its shell shows {got['shell']}, the source's showed {scene['apps']}")

    def notes():
        got["notes"] = note_titles(ctx, ws)
        problems.want(scene["note"] in got["notes"], f"{label}: Notes lists {got['notes']}, not the source's "
                                                     f"{scene['note']!r}")

    def files():
        tree = ws["tree"]
        got["question"] = tree_read(ctx, tree, QUESTION)
        problems.want(got["question"] == scene["question"], f"{label}: {QUESTION} reads {got['question']!r}")
        got["corpus"] = tree_digest(ctx, tree, f"{DATA_DIR}/corpus.bin")
        problems.want(got["corpus"] == scene["corpus"], f"{label}: its corpus.bin is not the source's")
        got["files"] = tree_count(ctx, tree, DATA_DIR)
        problems.want(got["files"] == scene["files"],
                      f"{label}: {got['files']} data files, the source has {scene['files']}")

    def environment():
        terminal, entered = shell_in(ctx, ws, f"terminal-{label.split()[-1]}", ENVIRONMENT, scene["source"]["record"])
        facts = said(terminal.ask(SAY, ae.SHELL_S))
        got["shell"], got["environments"] = facts, entered["known"]
        tid = ws_key(ws, scene["field"])
        problems.want(facts.get("TILO_WORKSPACE") == tid,
                      f"{label}: TILO_WORKSPACE in its {ENVIRONMENT} is {facts.get('TILO_WORKSPACE')!r}, not {tid}")
        problems.want(facts.get("TILO_ENVIRONMENT") == f"{tid}/{entered['record']}",
                      f"{label}: TILO_ENVIRONMENT is {facts.get('TILO_ENVIRONMENT')!r}, not "
                      f"{tid}/{entered['record']}")
        problems.want(facts.get("HOME") == f"{ae.GUEST_TREES}/{tid}",
                      f"{label}: ~ is {facts.get('HOME')!r}, not its own tree {ae.GUEST_TREES}/{tid}")
        problems.want(facts.get("tool") == scene["facts"].get("tool"),
                      f"{label}: the environment's tool says {facts.get('tool')!r}, the source's said "
                      f"{scene['facts'].get('tool')!r}")
        problems.want(facts.get("git") == scene["git"], f"{label}: the checkout says {facts.get('git')!r}, "
                                                        f"the source's said {scene['git']!r}")
        ws["terminal"], ws["record"] = terminal, entered["record"]

    def members():
        got["members"] = members_of(ctx, ws)
        owner = scene["members"]["owner"]
        problems.want(owner_alone(got["members"], owner),
                      f"{label}: its owner and members are {got['members']}, not the source's owner {owner} alone")

    def chat():
        # The chat stayed behind: opened in the branch, Chat holds nothing the source's chat said.
        # (The workspace's own emoji and agents are Chat's to keep but the workspace's, and come
        # along.)
        got["chat_kept"] = chat_dir(ctx, ws)
        if scene["chat"]:
            got["chat"] = chat_says(ctx, ws, scene["chat"], label.split()[-1])
            problems.want(not (got["chat"]["read"] or got["chat"]["listed"] or got["chat"]["shown"]),
                          f"{label}: it holds the source's chat: {got['chat']}")

    for name, judge in (("shell", shell), ("notes", notes), ("files", files), ("environment", environment),
                        ("members", members), ("chat", chat)):
        part(name, judge)
    return got, problems


@check("WB03", AREA, "Each branch holds what the person set up: the note, the files, the environment with "
                     "its tool and TILO_WORKSPACE its own, the shell's windows; its owner is its only member "
                     "and the chat stayed behind")
def branch_contents(ctx, res):
    scene = _scene(ctx)
    branches = _need(scene, "branches", "WB02")
    scene["members"] = members_of(ctx, scene["source"])
    res.metrics["source_members"] = scene["members"]
    problems = Problems()
    for ws in branches + ([scene["cli_branch"]] if "cli_branch" in scene else []):
        got, wrong = contents(ctx, scene, ws, ws["name"])
        res.metrics[ws["name"]] = got
        problems.extend(wrong)
    problems.judge()


def edit(ctx, scene: dict, ws: dict, who: str) -> dict:
    """The person edits `ws`: a note of its own, a line in the question, a file of its own, the
    environment's tool, and an environment of its own. What it wrote."""
    run = scene["run"]
    mark = f"{who}-{run}"
    new_note(ctx, ws, f"Only in {mark}")
    terminal = ws.get("terminal") or shell_in(ctx, ws, f"terminal-{who}-edit", ENVIRONMENT, scene["source"]["record"])[0]
    ws["terminal"] = terminal
    in_shell(ctx, terminal, ws["tree"], f"edit-{who}", f"""
echo 'finding of {mark}' >> ~/{QUESTION}
echo {mark} > ~/{who}-{run}.txt
printf '%s\\n' '#!/bin/bash' 'echo e2e-tool {run} {who}' > ~/{TOOL}
""")
    terminal.call("create-environment", {"name": f"Env {mark}", "confirm": True})
    return {"note": f"Only in {mark}", "line": f"finding of {mark}", "file": f"{who}-{run}.txt",
            "tool": f"e2e-tool {run} {who}", "environment": f"Env {mark}"}


def apart(ctx, scene: dict, phase: str) -> tuple[dict, Problems]:
    """Whether each workspace holds its own edits and none of the others'."""
    problems = Problems()
    seen = {}
    everyone = [scene["source"]] + scene["branches"]
    for ws in everyone:
        mine = scene["edits"].get(ws["id"])
        notes = note_titles(ctx, ws)
        question = tree_read(ctx, ws["tree"], QUESTION) or ""
        shell = ws_shell(ctx, ws, phase)
        envs = ws[f"known:{phase}"]
        tool = said(shell.ask(SAY, ae.SHELL_S)).get("tool")
        seen[ws["name"]] = {"notes": notes, "question": question, "environments": envs, "tool": tool}
        for other in everyone:
            theirs = scene["edits"].get(other["id"])
            if theirs is None:
                continue
            ok = other["id"] == ws["id"]
            what = "its own" if ok else f"{other['name']}'s"
            problems.want((theirs["note"] in notes) == ok, f"{phase}: Notes in {ws['name']} "
                                                           f"{'lacks' if ok else 'lists'} {what} note {theirs['note']!r}")
            problems.want((theirs["line"] in question) == ok, f"{phase}: {QUESTION} in {ws['name']} "
                                                              f"{'lacks' if ok else 'has'} {what} line")
            present = tree_read(ctx, ws["tree"], theirs["file"]) is not None
            problems.want(present == ok, f"{phase}: {ws['name']}'s files {'lack' if ok else 'hold'} {what} "
                                         f"{theirs['file']}")
            problems.want((theirs["environment"] in envs) == ok,
                          f"{phase}: Terminal in {ws['name']} {'lacks' if ok else 'lists'} {what} environment")
        want_tool = mine["tool"] if mine else scene["facts"].get("tool")
        problems.want(tool == want_tool, f"{phase}: the tool in {ws['name']}'s {ENVIRONMENT} says {tool!r}, "
                                         f"not {want_tool!r}")
    return seen, problems


def ws_shell(ctx, ws: dict, phase: str) -> ae.Terminal:
    """A shell in `ws`'s environment for this phase: the one already open, else a new one."""
    key = f"shell:{phase}"
    if ws.get(key) is None:
        ws[key], entered = shell_in(ctx, ws, f"terminal-{phase}-{ws['name'].split()[-1]}", ENVIRONMENT, ws.get("record") or ctx.facts["wsb"]["source"]["record"])
        ws[f"known:{phase}"] = sorted(entered["known"])
    return ws[key]


@check("WB04", AREA, "A branch and its source change apart: an edit in the first branch (a note, a file, the "
                     "environment) shows neither in the source nor the second, and one in the source not in "
                     "the branches")
def independent(ctx, res):
    scene = _scene(ctx)
    branches = _need(scene, "branches", "WB02")
    _need(scene, "members", "WB03")
    # The windows the earlier checks opened go; each workspace gets a Terminal of its own again.
    ctx.reset()
    for ws in [scene["source"]] + branches:
        ws.pop("terminal", None)
    first = branches[0]
    scene["edits"] = {first["id"]: edit(ctx, scene, first, "w1"),
                      scene["source"]["id"]: edit(ctx, scene, scene["source"], "w")}
    seen, problems = apart(ctx, scene, "edited")
    ctx.write("WB04-seen.json", json.dumps(seen, indent=1))
    res.metrics["edits"] = {k: v for k, v in scene["edits"].items()}
    problems.judge()


@check("WB05", AREA, "After the session restarts the branches are there, each still with its own edits alone "
                     "and its owner its only member")
def after_restart(ctx, res):
    scene = _scene(ctx)
    _need(scene, "edits", "WB04")
    # The person closes their windows and restarts the session: what each workspace holds comes back
    # from its own records and files, not from windows that stayed open.
    ctx.reset()
    since = ctx.vm.exec("date +%s", check=False).strip()
    try:
        ctx.vm.restart_session(timeout=180)
    except VmError as exc:
        said = restart_evidence(ctx, since)
        ctx.write("WB05-restart.txt", said)
        raise CheckFailed(f"the session did not come back after its restart: {exc}; {said[-5000:]}") from exc
    started = time.monotonic()
    ctx.facts.pop("awake_at", None)

    ctx.vm.wait_answering(180)
    # The person opens Notes again, as the launcher does, by its id: a session that has just started
    # offers its apps a little later, and a busy one later still.
    deadline = time.monotonic() + 300
    asked = None
    while not note_rows(ctx, "new_note"):
        awake(ctx)
        if time.monotonic() > deadline:
            raise CheckFailed(f"Notes did not open after the restart ({asked or 'never offered'}): it offers "
                              f"{offered(ctx, ae.NOTES)}; {app_evidence(ctx, ae.NOTES)}")
        if asked is None:
            try:
                ctx.vm.open(ae.NOTES)
                asked = "asked for"
            except VmError as exc:
                if "unknown_action" not in str(exc):
                    raise
        time.sleep(2)
    res.metrics["notes_after_restart_s"] = round(time.monotonic() - started, 1)
    res.metrics["answering_after_restart_s"] = round(time.monotonic() - started, 1)
    problems = Problems()
    state = ctx.state()
    for ws in [scene["source"]] + scene["branches"]:
        problems.want(find_ws(state, ws) is not None, f"{ws['name']} is gone after the restart")
        for key in [k for k in ws if k.startswith(("shell:", "known:"))] + ["terminal"]:
            ws.pop(key, None)
    owner = scene["members"]["owner"]
    for ws in scene["branches"]:
        members = members_of(ctx, ws)
        problems.want(owner_alone(members, owner),
                      f"after the restart {ws['name']}'s owner and members are {members}")
    seen, wrong = apart(ctx, scene, "restarted")
    ctx.write("WB05-seen.json", json.dumps(seen, indent=1))
    problems.extend(wrong)
    # Chat works in the first branch after the restart: it still holds none of the source's chat,
    # takes a line of its own, and its records' history is not held back; the source keeps its chat.
    first = scene["branches"][0]
    if not scene["chat"]:
        # The session runs with the launcher keeping its apps' own output (run.py's drop-in): Chat
        # opened once more says, in its data home, why it shows no window.
        problems.append("Chat after the restart is unobservable: the source's chat was never set up (WB01); "
                        f"what Chat itself said, opened again: {app_output(ctx, scene['source'], CHAT)[-4000:]}; "
                        f"the drawing threads that crashed: {crashed_threads(ctx)}")
    else:
        try:
            before = chat_says(ctx, first, scene["chat"], "restarted-branch")
            problems.want(not holds(before), f"after the restart {first['name']}'s Chat holds the source's "
                                             f"chat: {before}")
            line = f"Branch line {scene['run']}"
            post_chat(ctx, first, line)
            heard = wait_until(lambda: holds(chat_says(ctx, first, line, "restarted-branch-line")), 30,
                               interval=2.0)
            problems.want(bool(heard), f"after the restart {first['name']}'s Chat does not hold the line the "
                                       "person said there")
            kept = chat_says(ctx, scene["source"], scene["chat"], "restarted-source")
            problems.want(holds(kept), f"after the restart the source's Chat lost its chat: {kept}")
        except CheckFailed as exc:
            problems.append(f"after the restart Chat in {first['name']}: {exc}")
        blocked = ctx.vm.root_exec(
            f"journalctl --no-pager -o cat --since @{since or 0} | grep -a 'so.tilo.chat' "
            "| grep -a 'does not match its binding' | tail -n 3 | cut -c1-400", timeout=180, check=False).strip()
        problems.want(not blocked, f"after the restart Chat's history is held back: {blocked}")
    if problems:
        problems.append(session_told(ctx))
    problems.judge()


@check("WB06", AREA, "An agent started in the first branch's environment works in that branch alone: it is "
                     "admitted there, finds the branch's notes and not the source's or the sibling's, and its "
                     "environment reads no other workspace's files")
def agent_in_branch(ctx, res):
    scene = _scene(ctx)
    _need(scene, "edits", "WB04")
    first, second = scene["branches"]
    source = scene["source"]
    ws = first
    terminal = ws_shell(ctx, ws, "agent")
    tid = ws_key(ws, scene["field"])
    env = {"name": ws["name"], "workspace": ws, "terminal": terminal, "tree": ws["tree"]}
    ae.account_write(ctx, f"{ws['tree']}/{ae.AGENTS_DIR}/mcp_agent.sh", ae.AGENT.read_bytes())
    agent = ae.Agent(ctx, env, "branch-agent")
    agent.start()
    problems = Problems()
    problems.want(agent.hello.get("workspace") == tid,
                  f"the agent's shell says TILO_WORKSPACE {agent.hello.get('workspace')!r}, not {tid}")
    want = agent.initialize()
    dialog = ae.answer_sheet(ctx, ae.ALLOW_ONCE, "branch-agent-admission")
    reply = agent.reply(want, ae.REPLY_S)
    agent.keep()
    expect(isinstance(reply, dict) and "result" in reply,
           f"initialize was not answered after the person allowed it: {str(reply)[:300]}")
    agent.send("notifications/initialized", notify=True)
    res.metrics["sheet"] = dialog
    # The shell registers the agent in the branch's environment: the session whose environment_id is
    # the TILO_ENVIRONMENT its shell reads (the agent API is the MCP server since a3a6caf4255).
    environment = agent.hello.get("environment", "")
    registered = wait_until(lambda: [a for a in ae.agent_sessions(ctx)
                                 if environment and a.get("environment_id") == environment] or None,
                        30, interval=1.0) or []
    res.metrics["registered"] = [{k: a.get(k) for k in ("key", "name", *ae.WHERE)} for a in registered]
    problems.want(len(registered) == 1, f"the shell registers {len(registered)} agents in {environment!r}: "
                                    f"{[{k: a.get(k) for k in ('key', 'name', *ae.WHERE)} for a in ae.agent_sessions(ctx)]}")
    problems.want(environment.startswith(f"{tid}/"),
                  f"the agent's TILO_ENVIRONMENT {environment!r} is not of its branch {tid}")
    own = scene["edits"][first["id"]]["note"]
    theirs = scene["edits"][source["id"]]["note"]
    found = ae.tool_until(agent, "tilo_find", {"query": own},
                          lambda t: not t.startswith("nothing matches") and own in t, 45)
    problems.want(ae.finds(found, own), f"the agent does not find its branch's note {own!r}: "
                                        f"{ae.text_of(found)[:300]}")
    shared = ae.tool_until(agent, "tilo_find", {"query": scene["note"]},
                           lambda t: not t.startswith("nothing matches") and scene["note"] in t, 30)
    problems.want(ae.finds(shared, scene["note"]), f"the agent does not find the note its branch started with: "
                                                   f"{ae.text_of(shared)[:300]}")
    other = agent.tool("tilo_find", {"query": theirs})
    problems.want(not ae.finds(other, theirs), f"the agent finds the source's later note: {ae.text_of(other)[:300]}")
    apps = ae.tool_until(agent, "tilo_ls", {}, lambda t: "running" in t, 30)
    res.metrics["agent_ls"] = ae.text_of(apps)[:600]
    problems.want(ae.is_error(apps) is False, f"the agent's tilo_ls failed: {ae.text_of(apps)[:300]}")
    # What the agent's own process reaches: its environment's shell, where it runs.
    seen = reach(terminal, [("source", ws_key(source, scene["field"])), ("sibling", ws_key(second, scene["field"]))])
    res.metrics["reach"] = seen
    for name in ("source", "sibling"):
        problems.want(seen["read"].get(name) == "refused",
                      f"the branch's environment reads the {name}'s files: {seen['read'].get(name)!r}")
    problems.want(not {ws_key(source, scene["field"]), ws_key(second, scene["field"])} & set(seen["trees"]),
                  f"the branch's environment lists other workspaces' trees: {seen['trees']}")
    agent.keep()
    agent.quit()
    problems.judge()
