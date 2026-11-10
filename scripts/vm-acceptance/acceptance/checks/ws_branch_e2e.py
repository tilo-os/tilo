"""A workspace set up once and branched, end to end, on one booted tilo box.

A person sets a workspace up the way they work in it: a note, files (a question, a tool in
`~/.local/bin`, a git checkout where the environment has git, and a few hundred megabytes of
data), a Terminal environment, windows on its desk. They branch it per research question or per
customer: through the desk's `workspace.branch` (twice) and `tilo workspace branch` (once). A
branch starts as a copy of everything but the people: its owner is its only member, and the
source's chat stays behind. From then on the branches and their source change apart: an edit in
one (a note, a file, an environment) shows in no other, also after the session restarts. An
agent started in a branch's environment works in that branch alone.

Everything goes through the production paths, as `agents_e2e` does: the desk's own actions
(`new-workspace`, `switch-workspace`, `workspace.branch`), the apps' own actions (Notes'
`new-note` and `read`, Terminal's `create-environment`, `open-in`, `run`, `read`), the person's
`tilo` command, the sheet's own answers, and a scripted MCP client configured as Claude Code
(`guest/mcp_agent.sh`). The harness reads the workspaces' trees through the slot's own account
(`tilo-vm slot exec`).

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

from .. import core
from ..core import CheckFailed, Unobservable, check, expect
from ..vm import VmError, wait_until
from . import agents_e2e as ae

AREA = "ws-branch-e2e"
# The person's workspace, and the branches they make of it: two through the desk's action, one
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
# How long a branch of that may take before the check gives up on it, and how long one answer of
# the desk may take while a branch runs before the branch blocks the desk.
BRANCH_S = 600
# How long an app may take to open a window while the box is busy copying and drawing in software.
TERMINAL_S = 120
DESK_STALL_S = 3.0
# The desk's own Branch workspace entry, as the person reads it, and the action's names.
BRANCH_ENTRY = "Branch workspace"
WORKSPACE_BRANCH = "workspace.branch"
BRANCH_IDS = (WORKSPACE_BRANCH, "branch-workspace", "workspace-branch")
CHAT = "so.tilo.chat"
# The channel every workspace's chat starts with (apps/chat record.rs CHAT_CHANNEL).
CHAT_CHANNEL = "c_chat"
# Chat's composer, as the person's assistive technology names it (chat en-US.ftl chat-composer).
COMPOSER = "Message composer"
# An app's own data in a workspace, on this device (launcher appdir.rs).
APP_ROOMS = ".local/share/tilo-rooms"


# -- the desk ------------------------------------------------------------------------------


def workspace_rows(state: dict) -> list[dict]:
    return (state.get("workspace_tree") or {}).get("workspaces") or []


def find_ws(state: dict, ws: dict) -> dict | None:
    return next((w for w in workspace_rows(state) if w["id"] == ws["id"]), None)


def desk_apps(state: dict, ws: dict) -> list[str]:
    """The apps of the windows on `ws`'s desk, sorted, a tab each."""
    row = find_ws(state, ws) or {}
    return sorted(str(core.tab_app(t)) for s in row.get("strips") or [] for g in s.get("window_groups") or []
                  for t in g.get("tabs") or [])


def close_app_windows(ctx, app: str) -> bool:
    """Close every window of `app` through its group's own close action, as the person does;
    whether they all went (a window whose app ended may offer no close of its own)."""
    gids = {str(g["id"]) for g in core.all_groups(ctx.state())
            if any(app in str(core.tab_app(t)) for t in g.get("tabs") or [])}
    for row in ctx.vm.actions():
        if row["provider"] == "desktop-windows" and row["id"].removeprefix("group.close:") in gids \
                and row["id"].startswith("group.close:"):
            ctx.vm.invoke(row["provider"], row["id"], {"confirm": True} if row.get("params") else None)
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
    """The desk's `workspace.branch`, where `workspace.create` is published: its row."""
    def found():
        rows = [r for r in ctx.vm.actions("branch", programmatic=True)
                if str(r.get("id", "")).split("\x1f")[-1] in BRANCH_IDS
                or r.get("operation") in BRANCH_IDS]
        return rows or None

    rows = wait_until(found, 20, interval=1.0)
    if not rows:
        ctx.write("WB02-branch-rows.json", json.dumps(ctx.vm.actions("branch", programmatic=True), indent=1))
        raise CheckFailed(f"the desk publishes no {WORKSPACE_BRANCH}")
    ctx.write("WB02-branch-action.json", json.dumps(rows, indent=1))
    return rows[0]


def branch_args(row: dict, source: str, name: str) -> dict:
    """{source, name} under the names the action's own parameters give them."""
    params = row.get("params") or {}
    keys = list((params.get("properties") or params) if isinstance(params, dict) else [])
    which = next((k for k in ("source", "workspace", "from") if k in keys), "source")
    called = next((k for k in ("name", "new_name", "title") if k in keys), "name")
    return {which: source, called: name}


def answered_id(value) -> str | None:
    """The workspace id an action or the command answered: a string, or a map naming it."""
    if isinstance(value, (str, int)) and not isinstance(value, bool):
        return str(value)
    if isinstance(value, dict):
        for key in ("workspace", "id", "room", "workspace_id"):
            if isinstance(value.get(key), (str, int)) and not isinstance(value.get(key), bool):
                return str(value[key])
    return None


def by_answer(state: dict, answer: str, before: set) -> dict | None:
    """The new workspace the answer names, by any of its ids."""
    for w in workspace_rows(state):
        if w["id"] not in before and answer in (str(w.get("id")), str(w.get("room"))):
            return w
    return None


class DeskProbe:
    """How long the desk takes to answer, sampled while something else runs: a thread asks for
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


def desk_baseline(ctx, n: int = 10) -> list[float]:
    times = []
    for _ in range(n):
        started = time.monotonic()
        ctx.vm.state()
        times.append(round(time.monotonic() - started, 3))
        time.sleep(0.2)
    return times


# -- Notes, through its own actions ------------------------------------------------------------


def app_rows(ctx, app: str) -> list[dict]:
    return [r for r in ctx.vm.actions(programmatic=True) if r.get("application") == app]


def offered(ctx, app: str) -> str:
    """What `app` offers in front, by operation and provider, for a failure to say."""
    return json.dumps(sorted({(str(r.get("operation")), str(r.get("provider")), str(r.get("resource")))
                              for r in app_rows(ctx, app)}))


def note_rows(ctx, operation: str) -> list[dict]:
    return [r for r in app_rows(ctx, ae.NOTES) if r.get("operation") == operation]


def ensure_notes(ctx, ws: dict) -> None:
    """Notes open in `ws`, in front: the one the branch brought on its desk, else a new one."""
    ae.select_workspace(ctx, ws)
    if wait_until(lambda: len(note_rows(ctx, "new-note")) == 1 or None, 10, interval=1.0):
        return
    ctx.open_app(ae.NOTES)
    if not wait_until(lambda: len(note_rows(ctx, "new-note")) == 1 or None, 30, interval=1.0):
        raise Unobservable(f"{ws.get('name')}: Notes in front offers no single new-note")


def note_titles(ctx, ws: dict) -> list[str]:
    """The titles of the notes in `ws`'s notebook, as Notes' own `read` answers them."""
    ensure_notes(ctx, ws)
    rows = wait_until(lambda: note_rows(ctx, "read"), 30, interval=1.0)
    if not rows:
        raise Unobservable(f"{ws.get('name')}: Notes in front offers no read; it offers {offered(ctx, ae.NOTES)}")
    answer = ae.invoke(ctx, rows[0]["provider"], rows[0]["id"], {}, name=f"notes-read-{ws['id']}")
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
    ae.person_note(ctx, {"name": ws.get("name"), "workspace": ws}, title)


# -- the trees ---------------------------------------------------------------------------------


def long_exec(ctx, script: str, seconds: int) -> str:
    """`slot exec` of a script that may take longer than its default bound: writing or reading
    the hundreds of megabytes of a workspace's data."""
    return ctx.vm.run("slot", "exec", "--timeout", str(seconds), "--", "sh", "-c", script,
                      timeout=seconds + 60, check=False)


def tree_read(ctx, tree: str, rel: str) -> str | None:
    return ae.slot_read(ctx, f"{tree}/{rel}")


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
    out = ctx.vm.exec(f"python3 -c {shlex.quote(RUNTIME_LIST)} {shlex.quote(str(ws.get('room')))}", check=False)
    line = next((l for l in out.splitlines() if l.startswith("E2E-RUNTIME ")), None)
    try:
        reply = json.loads(line[len("E2E-RUNTIME "):]) if line else {}
    except json.JSONDecodeError:
        reply = {}
    if reply.get("result") != "environments":
        raise CheckFailed(f"the runtime did not list {ws.get('name')}'s environments: {(line or out)[-400:]}")
    return [{"record": e.get("key"), "name": e.get("name")} for e in reply.get("environments") or []]


def terminal_in(ctx, ws: dict, label: str) -> ae.Terminal:
    """A Terminal session in `ws`: one on its desk already (a branch's desk opens the source's
    windows again), else a Terminal the person opens there."""
    ae.select_workspace(ctx, ws)

    def mine():
        rows = [r for r in ae.terminal_rows(ctx) if r.get("room") in (None, "", ws.get("room"))]
        return sorted({ae.identity(r) for r in rows}, key=str) or None

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


# What a shell in an environment says about itself and its tree, a field a line.
SAY = ("for v in TILO_WORKSPACE TILO_ENVIRONMENT HOME; do printf '%s %s %s\\n' {M}-ENV \"$v\" \"${!v:-none}\"; "
       "done; printf '%s %s\\n' {M}-TOOL \"$(bash ~/" + TOOL + " 2>&1 | head -1 | tr ' ' _)\"; "
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
                     "and a checkout in it, windows on its desk, a few hundred MB of data")
def set_up(ctx, res):
    ctx.facts["wsb:why"] = "WB01 failed"
    ctx.reset()
    run = uuid.uuid4().hex[:8]
    ws = ae.create_workspace(ctx, f"{SOURCE} {run}")
    ws["name"] = f"{SOURCE} {run}"
    note = f"Sources {run}"
    new_note(ctx, ws, note)
    # The workspace's chat says something, which its branches leave behind; the person closes
    # Chat again, so the desk keeps Notes and Terminal.
    chat = f"Chat line {run}"
    problems = Problems()
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
    # The person's files and their environment's tool, written from the environment's shell as
    # they would: a question, a tool on PATH, a git checkout where the environment has git.
    terminal.bash(
        f"mkdir -p ~/{DATA_DIR} ~/.local/bin && printf '%s\\n' 'Question {run}' 'first line of thought' "
        f"> ~/{QUESTION} && printf '%s\\n' '#!/bin/bash' 'echo e2e-tool {run} source' > ~/{TOOL} "
        f"&& chmod +x ~/{TOOL}")
    terminal.bash(
        f"if command -v git >/dev/null; then mkdir -p ~/{CHECKOUT} && cd ~/{CHECKOUT} && git init -q "
        f"&& git config user.email p@e2e.example && git config user.name Person && echo tools > README "
        f"&& git add README && git commit -qm 'checkout {run}'; fi")
    lines = terminal.ask(SAY, ae.SHELL_S)
    facts = said(lines)
    res.metrics["source_shell"] = facts
    problems.want(facts.get("tool") == f"e2e-tool {run} source",
                  f"the tool the person wrote does not run in {ENVIRONMENT}: {facts.get('tool')!r}")
    problems.want(facts.get("question") == f"Question {run}",
                  f"the question the person wrote does not read back: {facts.get('question')!r}")
    probe = f".e2e-probe-{run}"
    terminal.bash(f"echo source > ~/{probe}")
    tree = ae.find_tree(ctx, probe)
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
    # Which of the desk's ids names the workspace to its environments.
    field = next((f for f in ("id", "room") if ws_key(ws, f) == ws["tilo_workspace"]), None)
    problems.want(field is not None, f"TILO_WORKSPACE {ws['tilo_workspace']!r} is neither the desk's id "
                                     f"{ws.get('id')!r} nor room {ws.get('room')!r} of the workspace")
    problems.want(tree.endswith("/" + ws["tilo_workspace"]),
                  f"the workspace's tree {tree} is not named for {ws['tilo_workspace']}")
    state = ctx.state()
    apps = desk_apps(state, ws)
    problems.want(ae.NOTES in apps and ae.TERMINAL in apps, f"the desk of {ws['name']} shows {apps}")
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
    res.metrics.update(workspace={k: ws.get(k) for k in ("id", "room", "name", "tilo_workspace", "tree", "record")},
                       desk=apps, bytes=size, git=facts.get("git"))
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
    """Branch `source` into `name` through the desk's `workspace.branch`, as the person's desk
    asks for it, timing it to the branch's data in its tree and sampling the desk meanwhile.
    The new workspace, and what was measured."""
    before = {w["id"] for w in workspace_rows(ctx.state())}
    row = branch_action(ctx)
    args = branch_args(row, str(source["id"]), name)
    baseline = desk_baseline(ctx)
    started = time.monotonic()
    with DeskProbe(ctx) as probe:
        answer = ae.invoke(ctx, row["provider"], row["id"], args, confirmed=True,
                           name=f"branch-{name.split()[-1].lower()}", timeout=BRANCH_S)
        answered = time.monotonic() - started
        new_id = answered_id(answer)
        if new_id is None:
            raise CheckFailed(f"{WORKSPACE_BRANCH} answered no workspace id: {str(answer)[:300]}")
        # The branch goes on the desk once what it holds is copied.
        made = wait_until(lambda: awake(ctx) and by_answer(ctx.state(), new_id, before), BRANCH_S, interval=0.5)
        if made is None:
            raise CheckFailed(f"the desk shows no workspace {new_id} {WORKSPACE_BRANCH} answered")
        ctx.wait_state(lambda s: ae.held_here(s, made["id"]), 120, f"{name} held on this device")
        held = time.monotonic() - started
    return made, {"answered_s": round(answered, 2), "held_s": round(held, 2), "answer": answer,
                  "desk_baseline_s": baseline, "desk_during_s": probe.times, "desk_errors": probe.errors}


def branch_said(ctx, name: str, room: str) -> dict:
    """What the desk told the person when the branch `name` was made ("<name> is ready", or "<name>
    is not complete" with what did not come along), as its notifications name it, and what the
    runtime that keeps the environments recorded of the branch into `room`."""
    def told():
        names = [str(t.get("name")) for t in ctx.vm.targets() if name in str(t.get("name", ""))]
        return [n for n in names if "is ready" in n or "is not complete" in n] or None

    notices = wait_until(told, 15, interval=0.5) or []
    audit = ctx.vm.exec("cat \"$HOME\"/.local/share/ush/audit/* \"$HOME\"/.local/share/ush/*/audit/* 2>/dev/null "
                        f"| grep -a branch-workspace | grep -a {shlex.quote(room)} | tail -n 2 | cut -c1-600",
                        check=False)
    return {"ready": [n for n in notices if "is ready" in n], "missing": [n for n in notices if "is not complete" in n],
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
    # The branch goes on the desk once what it holds is copied; the command says its id at once.
    made = wait_until(lambda: awake(ctx) and next((w for w in workspace_rows(ctx.state())
                                    if w["id"] not in before and (w.get("name") == name or str(w.get("room")) in text)),
                                   None), BRANCH_S, interval=1.0)
    if made is None:
        raise CheckFailed(f"`tilo workspace branch` said {text[-300:]!r}; the desk shows no workspace {name!r}")
    ctx.wait_state(lambda s: ae.held_here(s, made["id"]), 120, f"{name} held on this device")
    named = [w for w in (str(made.get("id")), str(made.get("room"))) if w in text]
    return made, {"seconds": seconds, "said": text[-300:], "names_it": bool(named)}


@check("WB02", AREA, "The person branches the workspace twice through the desk's workspace.branch and once "
                     "with `tilo workspace branch`; each branch is timed, and none blocks the desk")
def branch(ctx, res):
    scene = _scene(ctx)
    source, run, field = scene["source"], scene["run"], scene["field"]
    problems = Problems()
    # The desk offers the person its Branch workspace entry.
    entries = ctx.vm.actions("Branch")
    ctx.write("WB02-entries.json", json.dumps(entries, indent=1))
    offered = [r for r in entries if str(r.get("title", "")).lower() in (BRANCH_ENTRY.lower(), "branch a workspace")]
    res.metrics["entry"] = [{k: r.get(k) for k in ("provider", "id", "title")} for r in offered][:5]
    problems.want(bool(offered), f"the desk offers no {BRANCH_ENTRY!r} entry")
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
        timing["said"] = branch_said(ctx, name, str(made.get("room")))
        problems.want(not timing["said"]["missing"], f"{name} is not complete: {timing['said']}")
        during = timing["desk_during_s"]
        timing["desk_max_s"] = max(during) if during else None
        timing["desk_p50_s"] = statistics.median(during) if during else None
        timings[label] = timing
        ctx.write(f"WB02-{label.split()[-1]}-timing.json", json.dumps(timing, indent=1, default=str))
        ctx.record("ws_branch.branch_s", "s", [timing["data_s"]], mib=round(scene["bytes"] / 2**20, 1))
        ctx.record("ws_branch.desk_answer_max_s", "s", [timing["desk_max_s"] or 0])
        problems.want(whole, f"{name}'s tree {tree} holds {tree_count(ctx, tree, DATA_DIR)} of "
                             f"{scene['files']} data files {BRANCH_S} s after the branch")
        problems.want(made.get("name") == name, f"the branch is named {made.get('name')!r}, not {name!r}")
        base = max(timing["desk_baseline_s"])
        stalls = [t for t in during if t > max(DESK_STALL_S, 3 * base)]
        problems.want(not stalls, f"branching {name} blocked the desk: {len(stalls)} of {len(during)} answers "
                                  f"took over {max(DESK_STALL_S, 3 * base):.1f} s (longest {max(stalls or [0]):.1f} s, "
                                  f"{base:.2f} s before the branch)")
        problems.want(not timing["desk_errors"], f"the desk did not answer while branching {name}: "
                                                 f"{timing['desk_errors'][:3]}")
    name = f"{CLI_BRANCH} {run}"
    try:
        made, timing = branch_by_cli(ctx, source, name, field)
        made["name"] = made.get("name") or name
        made["tree"] = scene["source"]["tree"].rsplit("/", 1)[0] + "/" + ws_key(made, field)
        timings[CLI_BRANCH] = timing
        timing["said_complete"] = branch_said(ctx, name, str(made.get("room")))
        problems.want(not timing["said_complete"]["missing"], f"{name} is not complete: {timing['said_complete']}")
        problems.want(timing["names_it"], f"`tilo workspace branch` did not say the new workspace's id: "
                                          f"{timing['said']!r}")
        scene["cli_branch"] = made
    except CheckFailed as exc:
        problems.append(str(exc))
    ids = [source["id"]] + [b["id"] for b in branches] + ([scene["cli_branch"]["id"]] if "cli_branch" in scene else [])
    problems.want(len(set(ids)) == len(ids), f"the branches are not workspaces of their own: {ids}")
    res.metrics["branches"] = [{k: b.get(k) for k in ("id", "room", "name", "tree")} for b in branches]
    res.metrics["timings"] = {k: {x: v for x, v in t.items() if x not in ("desk_during_s", "answer")}
                              for k, t in timings.items()}
    scene["branches"] = branches
    problems.judge()


def members_of(ctx, ws: dict) -> dict:
    """The workspace's owner and members, as its governance says and the desk projects it
    (`policy`: owner, members person to role)."""
    row = find_ws(ctx.state(), ws) or {}
    policy = row.get("policy")
    if not isinstance(policy, dict) or not isinstance(policy.get("members"), dict):
        raise Unobservable(f"the desk projects no members of {ws.get('name')}: {str(policy)[:300]}")
    return {"owner": policy.get("owner"), "members": sorted(policy["members"])}


def owner_alone(members: dict, owner: str) -> bool:
    """Whether the person `owner` owns the workspace and nobody else is a member of it (the
    owner is a member as its owner, whether or not the members name them again)."""
    return members.get("owner") == owner and set(members.get("members") or []) <= {owner}


# -- the chat ----------------------------------------------------------------------------------


def chat_rows(ctx, operation: str) -> list[dict]:
    return [r for r in app_rows(ctx, CHAT) if r.get("operation") == operation]


def chat_dir(ctx, ws: dict) -> bool:
    """Whether this device keeps Chat's data for `ws` (an app's data in a workspace)."""
    out = ctx.vm.exec(f'[ -d "$HOME/{APP_ROOMS}/{ws["room"]}/{CHAT}" ] && echo E2E-YES || echo E2E-NO',
                      check=False)
    return "E2E-YES" in out


def chat_says(ctx, ws: dict, text: str, label: str) -> dict:
    """Whether `ws`'s chat holds `text`, Chat opened there: in what Chat's own `read` answers,
    and among what the desk's search lists in front."""
    ae.select_workspace(ctx, ws)
    if not app_rows(ctx, CHAT):
        ctx.open_app(CHAT)
    rows = wait_until(lambda: chat_rows(ctx, "read") or None, 15, interval=1.0)
    read = ae.invoke(ctx, rows[0]["provider"], rows[0]["id"], {}, name=f"chat-read-{label}") if rows else None
    listed = [r for r in ctx.vm.actions(text) if text in json.dumps(r)]
    shown = [t.get("name") for t in ctx.vm.targets() if text in str(t.get("name", ""))]
    ctx.write(f"chat-{label}.json", json.dumps({"read_rows": rows, "read": read, "listed": listed, "shown": shown,
                                               "offered": offered(ctx, CHAT)}, indent=1, default=str))
    return {"read": text in json.dumps(read, default=str), "listed": bool(listed), "shown": bool(shown),
            "readable": rows is not None}


def post_chat(ctx, ws: dict, text: str) -> None:
    """The person says `text` in `ws`'s chat, in its first channel, through Chat's own post."""
    ae.select_workspace(ctx, ws)
    ctx.open_app(CHAT)
    rows = wait_until(lambda: chat_rows(ctx, "post") or None, 10, interval=1.0)
    if rows:
        ae.invoke(ctx, rows[0]["provider"], rows[0]["id"], {"channel": CHAT_CHANNEL, "text": text},
                  confirmed=True, name="chat-post")
        return
    # Chat offers a person no post of its own until the channel holds a message: they write in the
    # channel's composer and press Enter.
    if not wait_until(lambda: any(t.get("name") == COMPOSER for t in ctx.vm.targets()) or None, 30, interval=1.0):
        said = ctx.vm.exec(
            f"sed 's/\\x1b\\[[0-9;]*m//g' \"{core.SESSION_LOG}\" | grep -ai 'chat' "
            "| grep -avE 'FRAME-LIFECYCLE|paint-stats' | tail -n 40 | cut -c1-300; "
            "journalctl --user --no-pager -n 400 2>/dev/null | grep -ai 'chat' | tail -n 40 | cut -c1-300", check=False)
        gpu = ctx.vm.root_exec("journalctl --no-pager -o cat | sed 's/\\x1b\\[[0-9;]*m//g' | grep -a 'so.tilo.chat' "
                               "| grep -aiE 'gpu|vulkan|icd|adapter|render|device_select|egl' | tail -n 12 | cut -c1-400",
                               timeout=180, check=False)
        crash = ctx.vm.root_exec("journalctl -k --no-pager | grep -ai 'segfault\\|trap\\|general protection' | tail -n 8; "
                                 "coredumpctl --no-pager list 2>&1 | tail -n 5; "
                                 "coredumpctl --no-pager info 2>&1 | grep -aE '^ +#|Signal|Executable|Command' | head -n 60",
                                 timeout=180, check=False)
        # The kernel names a crashed library by its object in the image's store: which file it is.
        objects = sorted(set(re.findall(r" in ([0-9a-f]{64,128})\[", crash)))
        for digest in objects[:3]:
            crash += "\n" + ctx.vm.root_exec(
                f"o=$(find /sysroot /composefs /var/lib -xdev -name '{digest[2:]}*' 2>/dev/null | head -1); "
                "echo \"object $o\"; [ -n \"$o\" ] || exit 0; n=$(stat -c %s \"$o\"); "
                "find /usr /opt -xdev -type f -size ${n}c 2>/dev/null | while read -r f; do "
                "cmp -s \"$f\" \"$o\" && echo \"crashed in $f\"; done; true", timeout=300, check=False)
        said = crash + "\n" + said
        ctx.write("WB01-chat-said.txt", said)
        raise Unobservable(f"Chat in {ws.get('name')} shows no {COMPOSER!r} and offers no post; it offers "
                           f"{offered(ctx, CHAT)}; Chat's crash: {crash[-2000:]}; its drawing: {gpu[-2500:]}")
    ae.press(ctx, COMPOSER)
    ctx.vm.input("type", text)
    ctx.vm.key("Return")


def app_evidence(ctx, app: str) -> str:
    """What the session said about `app` lately, and what the desk says of it: for a failure where
    `app` did not open."""
    said = ctx.vm.exec(
        f"sed 's/\\x1b\\[[0-9;]*m//g' \"{core.SESSION_LOG}\" | grep -a {shlex.quote(app)} "
        "| grep -avE 'FRAME-LIFECYCLE|paint-stats|landlock|Landlock' | tail -n 25 | cut -c1-400", check=False)
    shown = sorted({str(t.get("name")) for t in ctx.vm.targets()
                    if "/" not in str(t.get("name")) and ("stopped" in str(t.get("name")) or "ended" in str(t.get("name")))})
    return f"the desk shows {shown}; the session said: {said[-2500:]}"


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

    def desk():
        state = ctx.state()
        problems.want(find_ws(state, ws) is not None, f"{label}: the desk no longer shows it")
        got["desk"] = desk_apps(state, ws)
        problems.want(all(a in got["desk"] for a in scene["apps"] if CHAT not in a),
                      f"{label}: its desk shows {got['desk']}, the source's showed {scene['apps']}")

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

    for name, judge in (("desk", desk), ("notes", notes), ("files", files), ("environment", environment),
                        ("members", members), ("chat", chat)):
        part(name, judge)
    return got, problems


@check("WB03", AREA, "Each branch holds what the person set up: the note, the files, the environment with "
                     "its tool and TILO_WORKSPACE its own, the desk's windows; its owner is its only member "
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
    terminal.bash(f"echo 'finding of {mark}' >> ~/{QUESTION} && echo {mark} > ~/{who}-{run}.txt "
                  f"&& printf '%s\\n' '#!/bin/bash' 'echo e2e-tool {run} {who}' > ~/{TOOL}")
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
    ctx.vm.restart_session(timeout=180)
    started = time.monotonic()
    ctx.facts.pop("awake_at", None)

    def catalogue():
        awake(ctx)
        try:
            return any(a["provider"] == "applications" and a["id"] == ae.TERMINAL for a in ctx.vm.actions("Terminal"))
        except VmError:
            return False

    # The desk opens every workspace's windows again, front workspace first.
    if not wait_until(catalogue, 300, interval=2.0):
        chrome = (ctx.state().get("chrome") or {})
        raise CheckFailed(f"the session listed no applications within 300 s of its restart (locked: "
                          f"{chrome.get('session_locked')}); {app_evidence(ctx, 'tilo-launcher')}")
    res.metrics["catalogue_after_restart_s"] = round(time.monotonic() - started, 1)
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
    ae.slot_write(ctx, f"{ws['tree']}/{ae.AGENTS_DIR}/mcp_agent.sh", ae.AGENT.read_bytes())
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
