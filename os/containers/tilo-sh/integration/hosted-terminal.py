"""A hosted workspace's Linux commands, in the VM that workspace gets.

A hosted workspace has one VM (docs/hosted-browser.md): its Linux programs
run there, and so does the headless session that draws its Browser. A
Terminal session's command runs in that VM too — ush on the VM host that
holds the VM runs it, as the tenant — and the run it becomes is a structured
record of the workspace: the command, where it ran, how it ended, what it
printed and who ran it. History keeps it as that person's or agent's run.

  --refused   a Linux command from an agent's run, while the provider's plans
              file names no card for the account: refused at once, in words
              that say what would let it through, and nothing runs in the VM;
  --card      the provider puts the account on file, and what the card
              releases runs: the Browser window's next page action, where this
              run opened one, which the free allowance refused a moment ago;
  --terminal  the workspace's Terminal, run as the person's own backend key:
              a file made, the directory read back, `uname` answered in the
              VM, and the run kept as a structured record of its session and
              as a step of History named after the key that ran it;
  --agent     an agent's key, scoped to this one workspace, running a command
              through the `tilo` command (the MCP behind it), and that run in
              History as the agent's;
  --isolated  the Linux session runs as the VM's tenant, not as the
              unprivileged account its Browser session runs as, and neither
              reads the other's files;
  --gui       what a Linux GUI app would take. A hosted workspace's VM runs
              no display server, so there is none to open yet, and this says
              so instead of faking one.

usage: hosted-terminal.py DOOR SERVER_KEY OUT_DIR [--refused|--card|--terminal|--agent|--isolated|--gui]

SERVER_KEY is the startup's own key (`tilo keys create`, named for the backend
that asked for it). OUT_DIR is the hosted Browser's work directory: its
`open.json` names the workspace, and the key its agent was minted, which is
the key scoped to that one workspace. Without it — a run of these steps alone
— the first phase makes a workspace of the startup's own and mints its key,
which is a workspace's Linux commands and no Browser.

HOSTED_TERMINAL_CLI is the `tilo` command `--agent` runs, and
HOSTED_TERMINAL_DOOR the door it points at (default: DOOR).
HOSTED_TERMINAL_PLANS is the plans file the provider writes and the account
HOSTED_TERMINAL_ACCOUNT is named in it (default: the account the local stack
places its workspaces' VMs for). Prints one JSON line, and keeps what it read
in OUT_DIR.
"""

from __future__ import annotations

import asyncio
import json
import os
import re
import subprocess
import sys
import time

from pathlib import Path

from tilo import AsyncTilo, ProtocolError, Refused, Run

import plans

DOOR, SERVER_KEY, OUT = sys.argv[1:4]

# How History names the runs of each key: the name a key is made with is the
# agent it is (front_door's `history_names_a_minted_keys_agent_by_the_name_it
# _was_minted_with`). check.sh asks for the startup's key as this name, and
# the hosted flow mints its agent's as the other.
PERSON_KEY_NAME = "Visa backend"
AGENT_KEY_NAME = "Jane Doe's agent"
CLI = os.environ.get("HOSTED_TERMINAL_CLI", "tilo")
CLI_DOOR = os.environ.get("HOSTED_TERMINAL_DOOR", DOOR)

# The account the stack's relay places its workspaces' VMs for: the one the
# plans file admits by (os/containers/tilo-sh/vm-host.sh). The provider names
# the plans file it writes.
ACCOUNT = os.environ.get("HOSTED_TERMINAL_ACCOUNT", "acct-tilo-sh")
PLANS = os.environ.get("HOSTED_TERMINAL_PLANS", "")
# The guest's unprivileged account, which its Browser session runs as and its
# tenant's commands do not (os/Containerfile.hosted-guest).
BROWSER_USER = "tilo-browser"

# The command an account without a card is refused: `mkdir` is a program of
# the VM's, so the host must wake the VM to run it, and the directory it would
# make is how a later run says whether it ever ran.
REFUSED_MARKER = "refused-marker"
REFUSED_COMMAND = f"mkdir {REFUSED_MARKER}"

# What the workspace's Terminal runs: a file made in the workspace's own
# directory, read back, the directory listed (`refused-marker` absent from it
# is what the refused command never doing anything means), and the machine it
# all happened on. `ls -d` of the guest's own init, because a file only a
# workspace VM has says the command ran in the VM and not on the machine
# holding it.
TERMINAL_COMMAND = (
    "mkdir -p hello"
    " && echo hello-from-the-workspace-vm > hello/note.txt"
    " && ls -l hello"
    " && cat hello/note.txt"
    " && ls -a"
    " && uname -srm"
    " && ls -d /usr/libexec/tilo/hosted-init"
)
# What the agent's key runs through the command line: the machine, and the
# userland the guest image is.
AGENT_COMMAND = "uname -srm && cat /etc/os-release | head -n 2"
# The account the command runs as, and each account reading the other's
# files: the tenant's own file here, the Browser session's home, and the
# Browser user asked to read the tenant's file (which the tenant's sudo can
# ask, being the tenant's own VM).
ISOLATION_COMMAND = (
    "id -un; id -u"
    f" ; echo only-the-tenant > {REFUSED_MARKER}.tenant"
    f" ; chmod 600 {REFUSED_MARKER}.tenant"
    " ; ls /home/tilo-browser"
    f" ; sudo -n -u {BROWSER_USER} -- cat {REFUSED_MARKER}.tenant"
)
# What a Linux GUI app would need: a display server to draw on.
DISPLAY_COMMAND = (
    "ls /tmp/.X11-unix /run/user/1000/wayland-0"
    " ; ps -eo comm | grep -v grep | grep -Ei '^(xorg|xwayland|weston|wayfire|sway)$'"
)


def say(step: str, detail: str = "") -> None:
    print(f"[{time.strftime('%H:%M:%S')}] {step}" + (f": {detail}" if detail else ""), flush=True)


def check(condition: object, what: str) -> None:
    if not condition:
        raise AssertionError(what)
    print(f"    ok  {what}", flush=True)


def write_json(name: str, value: object) -> str:
    path = os.path.join(OUT, name)
    with open(path, "w") as out:
        json.dump(value, out, indent=2, sort_keys=True, default=str)
    return path


async def outcome(call: object) -> object:
    """What an action answered: its result, or the run it waits in, followed
    to its end (a command can take longer than the door waits)."""
    answer = await call
    if isinstance(answer, Run):
        try:
            return await answer.result()
        except Refused as error:
            return {"refused": f"{error.code}: {error.message}", "next": error.next_steps}
        except ProtocolError as error:
            return {"refused": f"{error.code or ''}: {error.message or error}".strip(": ")}
    return answer


def client(key: str) -> AsyncTilo:
    """A client of the door as the key's agent."""
    return AsyncTilo(key=key, door=DOOR)


async def state() -> dict[str, object]:
    """The workspace these commands run in, and the key minted for it.

    The hosted Browser's open names one (`open.json`: the workspace it opened
    a window in, and its agent's key). Without it — a run of these steps on
    their own — a workspace of the startup's own is made, with an agent's key
    for it: a Linux command needs no Browser, and its VM is the same one."""
    for name in ("open.json", "terminal-open.json"):
        path = os.path.join(OUT, name)
        if os.path.isfile(path):
            with open(path) as opened:
                return json.load(opened)
    backend = client(SERVER_KEY)
    try:
        made = await backend.workspaces.create("A workspace's Linux commands")
        minted = await backend.workspace(made.id).keys.mint(name=AGENT_KEY_NAME)
    finally:
        await backend.aclose()
    kept = {"workspace": made.id, "key": minted.token, "made": True}
    write_json("terminal-open.json", kept)
    say("made", f"the workspace {made.id}, with a key for its agent")
    return kept


async def find_run(workspace: object, run: str) -> tuple[str, dict]:
    """The Terminal session record holding the run `run`, and the run as it is
    recorded: whichever session of the app the host opened."""
    listing = await workspace.ls("terminal")  # type: ignore[attr-defined]
    paths = [str(row.path) for row in listing.rows]
    check(paths, f"the workspace has a Terminal session ({paths})")
    for path in paths:
        typed = await workspace.resource(path).load()  # type: ignore[attr-defined]
        for recorded in typed.fields.get("runs") or []:
            if str(recorded.get("run")) == run:
                return path, recorded
    raise AssertionError(f"the run {run} is on no Terminal session of the workspace ({paths})")


async def output_of(workspace: object, recorded: dict) -> bytes | None:
    """What a recorded run printed, when the host kept it in the content."""
    output = recorded.get("output")
    if isinstance(output, str) and output.startswith("tilo.content:"):
        return await workspace.files.download(output)  # type: ignore[attr-defined]
    return None


async def history_named(workspace: object, name: str) -> list[dict]:
    """History's versions of the Terminal's changes, those named after `name`:
    the agent the key is, for the person it runs for."""
    history = await workspace.do("terminal#history", last=100)  # type: ignore[attr-defined]
    write_json(f"history-{re.sub(r'[^a-z0-9]+', '-', name.lower())}.json", history)
    versions = (history or {}).get("versions") if isinstance(history, dict) else None
    return [version for version in versions or [] if (version.get("actor") or {}).get("name") == name]


async def refused() -> dict[str, object]:
    """The plans rule: a Linux action needs a card on file."""
    kept = await state()
    workspace = str(kept["workspace"])
    agent = client(str(kept["key"]))
    try:
        aws = agent.workspace(workspace)
        say("the agent asks the workspace's Terminal for a command, in its own run")
        began = time.perf_counter()
        # In a run, as an agent's commands go: the shell fails at once with
        # the refusal, where a person's own command would wait for the card.
        async with aws.run("A Linux command the account cannot pay for yet"):
            answer = await outcome(aws.do("terminal#run", command=REFUSED_COMMAND))
        seconds = time.perf_counter() - began
        say("answered", f"{seconds:.2f} s: {json.dumps(answer, default=str)[:400]}")
        check(isinstance(answer, dict), f"the refusal is the run's answer ({answer!r})")
        refusal = answer.get("refusal") or {}
        message = str(refusal.get("message") or "")
        steps = refusal.get("next") or []
        check(
            refusal.get("reason") == "no_payment_method",
            f"a Linux action without a card is refused with no_payment_method ({refusal})",
        )
        check("Linux machine" in message, f"the refusal says it needs a Linux machine ({message!r})")
        check("card" in message.lower(), f"the refusal says a card continues ({message!r})")
        check(
            any(step.get("kind") == "add_payment_method" for step in steps),
            f"the refusal names adding a card as the way through ({steps})",
        )
        check(
            any(str(step.get("url") or "").startswith("http") for step in steps),
            f"and where to add it ({steps})",
        )
        check(answer.get("exit") != 0, f"the command ended refused (exit {answer.get('exit')})")
        output = str(answer.get("output") or "")
        check(
            "Linux machine" in output,
            f"what the caller reads is the refusal, not a run ({output.strip()[:300]})",
        )
        # The refused attempt is part of the record too: History holds what
        # was asked and that it did not run.
        path, recorded = await find_run(aws, str(answer.get("run")))
        check(recorded.get("exit") != 0, f"the session records the refused run ({path})")
        write_json("refused.json", {"answer": answer, "record": recorded})
        return {
            "seconds": round(seconds, 2),
            "reason": refusal.get("reason"),
            "message": message,
            "next": steps,
            "exit": answer.get("exit"),
            "record": recorded.get("exit"),
        }
    finally:
        await agent.aclose()


async def card() -> dict[str, object]:
    """The provider puts the account on file, and what the card releases runs.

    The card is the account's, and one file says so: the plans file the VM
    host admits Linux actions by and the relay counts page actions by. So the
    Browser's page action the free allowance refused a moment ago runs here
    too, when this run opened a Browser window to hold it to."""
    kept = await state()
    check(PLANS and os.path.isfile(PLANS), f"the plans file is where the provider writes it ({PLANS})")
    plans.add_account(PLANS, ACCOUNT)
    say("the provider put the account on file", f"{ACCOUNT} in {PLANS}, with a card")
    report: dict[str, object] = {"account": ACCOUNT, "plans": PLANS}
    windows = Path(OUT, "open.json")
    if windows.is_file():
        workspace = str(kept["workspace"])
        agent = client(str(kept["key"]))
        try:
            aws = agent.workspace(workspace)
            listing = await aws.ls("browser")
            open_windows = [str(row.path) for row in listing.rows if "@" in str(row.path)]
            check(open_windows, f"the workspace's Browser window is open ({open_windows})")
            page = f"browser@{open_windows[0].split('/', 1)[0].split('@', 1)[1]}/page#read-page"
            began = time.perf_counter()
            answer = await outcome(aws.do(page))
            seconds = time.perf_counter() - began
            refused = isinstance(answer, dict) and "refused" in answer
            check(
                not refused,
                f"the page action the allowance refused runs once the card is on file "
                f"({seconds:.2f} s): {json.dumps(answer, default=str)[:300]}",
            )
            report["page_action_seconds"] = round(seconds, 2)
        finally:
            await agent.aclose()
    else:
        say("no Browser", "this run opened none, so only the Linux command it releases is held (the next step)")
    return report


async def terminal() -> dict[str, object]:
    """The workspace's Terminal, and the command that never ran in it."""
    kept = await state()
    workspace = str(kept["workspace"])
    person = client(SERVER_KEY)
    try:
        ows = person.workspace(workspace)
        say("the person's own key runs a command in the workspace's Terminal")
        began = time.perf_counter()
        answer = await outcome(ows.do("terminal#run", command=TERMINAL_COMMAND))
        seconds = time.perf_counter() - began
        check(isinstance(answer, dict), f"the run answers what it did ({answer!r})")
        output = str(answer.get("output") or "")
        say("answered", f"{seconds:.2f} s, exit {answer.get('exit')}: {output.strip()[:400]}")
        check(answer.get("exit") == 0, f"the command ended well (exit {answer.get('exit')})")
        check("note.txt" in output, "the file it made is listed")
        check("hello-from-the-workspace-vm" in output, "and what it wrote reads back")
        check("Linux " in output, f"the machine it ran on answered uname ({output.strip()[:200]})")
        check(
            "/usr/libexec/tilo/hosted-init" in output,
            "it ran in the workspace's VM, where the guest's own init is",
        )
        check(
            REFUSED_MARKER not in output,
            "the command the account could not pay for made nothing in that directory",
        )
        run = str(answer.get("run"))
        check(run, f"the run has an id of its own ({run!r})")

        say("the run is a structured record of the Terminal's session")
        path, recorded = await find_run(ows, run)
        say("recorded", f"{path}: {json.dumps(recorded, default=str)[:600]}")
        check(
            "hello-from-the-workspace-vm" in str(recorded.get("command"))
            and "uname -srm" in str(recorded.get("command")),
            f"the record holds the command line ({recorded.get('command')})",
        )
        check(str(recorded.get("cwd") or ""), f"and where it ran ({recorded.get('cwd')})")
        check(recorded.get("exit") == 0, f"and how it ended ({recorded.get('exit')})")
        started, ended = recorded.get("started_ms"), recorded.get("ended_ms")
        check(
            isinstance(started, int) and isinstance(ended, int) and ended >= started,
            f"and when it ran ({started} → {ended})",
        )
        actor = str(recorded.get("actor") or "")
        check(actor.startswith("agent:"), f"and who ran it, by their key ({actor})")
        check(isinstance(recorded.get("items"), list), "its parsed items are listed")
        check("tool" in recorded, f"and the tool it runs ({recorded.get('tool')!r})")
        kept = await output_of(ows, recorded)
        if kept is not None:
            check(
                b"hello-from-the-workspace-vm" in kept,
                f"the output it kept holds what it printed ({len(kept)} bytes)",
            )
        write_json("terminal-run.json", {"session": path, "run": recorded})

        say("History keeps the run as the key's, for the person it ran for")
        versions = await history_named(ows, PERSON_KEY_NAME)
        say("history", f"{len(versions)} versions by {PERSON_KEY_NAME}")
        check(versions, f"History names the run after the key that ran it ({PERSON_KEY_NAME})")
        first = versions[0]
        check(
            first.get("actor", {}).get("kind") == "agent",
            f"the version is an agent's ({first.get('actor')})",
        )
        check(
            first.get("actor", {}).get("colour"),
            f"and it has presence's colour for that agent ({first.get('actor')})",
        )
        check(
            any(str(version.get("run") or "") == PERSON_KEY_NAME for version in versions),
            "the run goes by the key's name, as a run with no title does "
            f"({[version.get('run') for version in versions]})",
        )
        return {
            "seconds": round(seconds, 2),
            "run": run,
            "session": path,
            "exit": recorded.get("exit"),
            "actor": actor,
            "history_versions": len(versions),
        }
    finally:
        await person.aclose()


def run_cli(workspace: str, key: str, command: str) -> tuple[int, str, str]:
    """`tilo -w WORKSPACE do terminal#run command=…`, with the key in the
    environment as the command takes it."""
    env = dict(os.environ, TILO_API_KEY=key, TILO_DOOR=CLI_DOOR)
    ran = subprocess.run(
        [CLI, "--door", CLI_DOOR, "-w", workspace, "do", "terminal#run", f"command={command}"],
        capture_output=True,
        text=True,
        timeout=600,
        env=env,
    )
    return ran.returncode, ran.stdout, ran.stderr


async def agent() -> dict[str, object]:
    """An agent's key, through the `tilo` command, and its History."""
    kept = await state()
    workspace = str(kept["workspace"])
    key = str(kept["key"])
    say("the agent's key runs a command in the VM through the tilo command")
    began = time.perf_counter()
    code, out, err = run_cli(workspace, key, AGENT_COMMAND)
    seconds = time.perf_counter() - began
    say("answered", f"{seconds:.2f} s, exit {code}: {out.strip()[:400]}")
    check(code == 0, f"the command answered the run ({code}): {err.strip()[-400:]}")
    check(out.strip().startswith("ok"), f"the run succeeded, as the command reads it ({out.strip()[:200]})")
    check("Linux " in out, f"what the command printed comes back ({out.strip()[:300]})")
    check("Fedora" in out, "and it is the guest image's own userland")
    agent_client = client(key)
    try:
        aws = agent_client.workspace(workspace)
        # A run the door stopped waiting for comes back pending; the same
        # invocation is followed with the key it was asked with.
        pending = re.search(r"pending ([0-9a-f-]{8,})", out)
        if pending:
            say("followed", f"the command answered pending {pending.group(1)}")
            answer = await aws.status(pending.group(1))
            check("succeeded" in json.dumps(answer), f"the run it started ended well ({answer})")
        say("History keeps it as the agent's run")
        versions = await history_named(aws, AGENT_KEY_NAME)
        say("history", f"{len(versions)} versions by {AGENT_KEY_NAME}")
        check(versions, f"History names the run after the agent's key ({AGENT_KEY_NAME})")
        first = versions[0]
        check(
            first.get("actor", {}).get("kind") == "agent",
            f"the version is the agent's ({first.get('actor')})",
        )
        check(
            any(str(version.get("run") or "") == AGENT_KEY_NAME for version in versions),
            "the run goes by the agent's name "
            f"({[version.get('run') for version in versions]})",
        )
        person_versions = await history_named(aws, PERSON_KEY_NAME)
        check(
            person_versions,
            f"and the run the person's key made is still theirs ({PERSON_KEY_NAME})",
        )
        check(
            first.get("actor", {}).get("name")
            != person_versions[0].get("actor", {}).get("name"),
            "the two keys' runs are attributed apart",
        )
        return {
            "seconds": round(seconds, 2),
            "exit": code,
            "pending": pending.group(1) if pending else None,
            "history_versions": len(versions),
        }
    finally:
        await agent_client.aclose()


async def isolated() -> dict[str, object]:
    """The Linux session's account, and the Browser session's, apart."""
    kept = await state()
    workspace = str(kept["workspace"])
    person = client(SERVER_KEY)
    try:
        ows = person.workspace(workspace)
        say("a command runs as the tenant, and neither it nor the Browser user reads the other's files")
        began = time.perf_counter()
        answer = await outcome(ows.do("terminal#run", command=ISOLATION_COMMAND))
        seconds = time.perf_counter() - began
        check(isinstance(answer, dict), f"the run answers what it did ({answer!r})")
        output = str(answer.get("output") or "")
        say("answered", f"{seconds:.2f} s: {output.strip()[:600]}")
        lines = [line.strip() for line in output.splitlines() if line.strip()]
        check(lines[:2] == ["tilo", "1000"], f"the session runs as the VM's tenant ({lines[:2]})")
        check(
            "Permission denied" in output,
            f"and may not read the Browser session's home ({output.strip()[-300:]})",
        )
        # The tenant's own command ends refused at the reads that were
        # refused, which is what proves them: a listing would be a leak.
        check(answer.get("exit") != 0, f"both reads were refused (exit {answer.get('exit')})")
        say("and the Browser user may not read the tenant's file")
        other = await outcome(
            ows.do(
                "terminal#run",
                command=(
                    f"ls /home/tilo-browser && echo leaked"
                    f" ; sudo -n -u tilo-browser -- cat {REFUSED_MARKER}.tenant && echo leaked"
                ),
            )
        )
        text = str((other or {}).get("output") or "")
        say("answered", text.strip()[:400])
        check("leaked" not in text, f"neither account read the other's files ({text.strip()[-300:]})")
        check("Permission denied" in text, "both reads are refused for the other's account")
        write_json("isolation.json", {"tenant": answer, "browser_user": other})
        return {
            "seconds": round(seconds, 2),
            "session": lines[:2],
            "browser_user": "tilo-browser",
            "account": ACCOUNT,
        }
    finally:
        await person.aclose()


async def gui() -> dict[str, object]:
    """Whether a hosted workspace has a Linux GUI app to open at all."""
    kept = await state()
    workspace = str(kept["workspace"])
    person = client(SERVER_KEY)
    try:
        ows = person.workspace(workspace)
        answer = await outcome(ows.do("terminal#run", command=DISPLAY_COMMAND))
        output = str((answer or {}).get("output") or "")
        say("answered", output.strip()[:400])
        servers = [
            line
            for line in output.splitlines()
            if line.strip()
            and "No such file" not in line
            and "cannot access" not in line
        ]
        check(
            not servers,
            "the workspace's VM runs no display server, so no Linux app's window can be "
            f"opened there yet ({servers}). A hosted workspace has Browser's window and no "
            "other: whichever brings a Linux desktop into a workspace brings its windows to "
            "the desk, and this step opens one and holds its window to appearing and closing.",
        )
        write_json("gui.json", {"answer": answer, "display_servers": servers})
        return {"display_servers": servers}
    finally:
        await person.aclose()


async def main() -> int:
    os.makedirs(OUT, exist_ok=True)
    phase = sys.argv[4] if len(sys.argv) > 4 else "--all"
    report: dict[str, object] = {"workspace": (await state())["workspace"]}
    previous = os.path.join(OUT, "terminal.json")
    if os.path.isfile(previous):
        with open(previous) as saved:
            report.update(json.load(saved))
    phases = {
        "--refused": refused,
        "--card": card,
        "--terminal": terminal,
        "--agent": agent,
        "--isolated": isolated,
        "--gui": gui,
    }
    if phase == "--all":
        for name, one in phases.items():
            report[name.lstrip("-")] = await one()
    else:
        if phase not in phases:
            raise SystemExit(f"usage: hosted-terminal.py DOOR SERVER_KEY OUT [{'|'.join(phases)}]")
        report[phase.lstrip("-")] = await phases[phase]()
    write_json("terminal.json", report)
    print(json.dumps(report), flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
