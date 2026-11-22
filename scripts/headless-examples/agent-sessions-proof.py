#!/usr/bin/env python3
"""Two agents at the session's MCP server, each admitted as itself.

The session's MCP server is how a program of this login — an AI agent in a
terminal — reaches this desktop's actions as an agent, never as the person.
This proof drives two such clients at once, through a headless session of its
own, and reads every claim back off the session rather than off the clients:

  1. asking    each client opens with `initialize` and waits: nothing it asked
               for runs until the person answers the sheet the MCP server puts up.
  2. admitted  the person answers each sheet, once per client; each agent is
               admitted as an agent session of its own, called as the workspace
               names the agent it admits the client as (`Claude Code`), acting
               for the person, and shows on the shell as its own presence, a
               session of its own beside the person's runs.
  3. acting    each admitted client is served as itself: its own tools answer,
               and what the shell offers a person's own socket is not what it is
               offered.
  4. faces     each agent's face on the bar opens its own card: pressed where
               the bar publishes it, and pressed by the name a reader is told.
  5. stopping  the person presses Stop on one agent's card by name, as a screen
               reader activates it: that connection alone ends, and the other
               agent still answers.
  6. taking    Take over on the other agent's card ends its work here: the
               person is left with nobody admitted, and the agent's connection is
               over.
  7. revoking  Revoke on an MCP-admitted agent's card ends its work here, and
               the next client of that program has to be answered for.

Usage:
  ./agent-sessions-proof.py
"""
import json
import os
import pathlib
import select
import shutil
import socket
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from real_apps import Agent, address, catalogue  # noqa: E402

REPO = pathlib.Path(__file__).resolve().parents[2]
# The one place that picks a session's private runtime directory.
SESSION_LAUNCHER = REPO / "dist" / "dev" / "nested-session.sh"
# tools/ci/session-proofs.sh points this at the session build the run made.
SHELL = pathlib.Path(os.environ.get("TILO_SHELL_BINARY", REPO / "target" / "debug" / "tilo-session"))
# What each client calls itself. The workspace names the agent it is admitted as after the
# client, so the shell calls the session by that name too.
CLIENTS = ["Claude Code", "Codex"]

failures: list[str] = []


def check(label: str, ok: bool, detail: str = "") -> bool:
    print(f"  {'PASS' if ok else 'FAIL'}  {label}" + (f" — {detail}" if detail else ""))
    if not ok:
        failures.append(label)
    return ok


def until(check, timeout: float = 40.0):
    """True once `check()` answers so, within `timeout`; else False."""
    end = time.time() + timeout
    while time.time() < end:
        if check():
            return True
        time.sleep(0.2)
    return False


def open_card(shell: "Shell", name: str) -> None:
    """Open an agent's card from its face on the bar, as the person's own press does."""
    face = shell.targets().get(f"{name}, agent")
    if face is not None:
        x, y, width, height = face
        shell.call({"op": "click_at", "x": x + width / 2, "y": y + height / 2})
    until(lambda: f"{name}’s profile" in shell.targets(), 10)


def close_card(shell: "Shell") -> None:
    """Leave the card up: the roster's place takes it back."""
    if any(card.endswith("’s profile") for card in shell.targets()):
        shell.call({"op": "key", "key": "Escape"})
        until(lambda: not any(card.endswith("’s profile") for card in shell.targets()), 10)


def runtime_directory(state: pathlib.Path) -> pathlib.Path:
    """The private runtime directory a session on `state` uses.

    The launcher picks it (a login's XDG_RUNTIME_DIR, the user's /run/user
    session, or a short directory of its own) and prints it, so no second copy of
    that rule lives here. A socket path must fit the kernel's limit, which is
    why it is never left to whatever depth this proof's scratch home sits at."""
    printed = subprocess.run(
        [str(SESSION_LAUNCHER), "--print-runtime", "--state", str(state)],
        capture_output=True, text=True, check=True,
    )
    runtime = printed.stdout.strip()
    if not runtime:
        raise SystemExit("the session launcher named no runtime directory")
    runtime = pathlib.Path(runtime)
    runtime.mkdir(mode=0o700, parents=True, exist_ok=True)
    return runtime


class Shell:
    """A headless session on a scratch home of its own, driven at its socket."""

    def __init__(self) -> None:
        self.dir = pathlib.Path(tempfile.mkdtemp(prefix="tilo-agent-sessions-proof-"))
        self.state = self.dir / "state"
        self.home = self.dir / "home"
        self.identity = self.dir / "identity"
        for path in (self.home, self.identity):
            path.mkdir(parents=True, exist_ok=True)
        self.identity.chmod(0o700)
        # The shell's state is this scratch directory's, as a launcher's state
        # is a session's: the runtime directory its sockets go in comes from
        # there, from the one place that picks it.
        self.runtime = runtime_directory(self.state)
        self.control = self.runtime / "control.sock"
        # Where the session binds its MCP server, as it prints it once it started.
        self.mcp: pathlib.Path | None = None
        self.env = {
            **os.environ,
            "HOME": str(self.home),
            "TILO_TEST_CONTROL": str(self.control),
            "TILO_IDENTITY_ROOT": str(self.identity),
            "XDG_RUNTIME_DIR": str(self.runtime),
            "XDG_DATA_HOME": str(self.dir / "data"),
            "XDG_CACHE_HOME": str(self.dir / "cache"),
            "XDG_STATE_HOME": str(self.state),
            "XDG_CONFIG_HOME": str(self.dir / "config"),
        }
        self.env.pop("DISPLAY", None)
        self.env.pop("WAYLAND_DISPLAY", None)
        self.session = None

    def start(self) -> None:
        self.control.unlink(missing_ok=True)
        self.session = subprocess.Popen(
            [
                str(SHELL),
                "--backend",
                "headless",
                "--seed-apps",
                # A session's own state, as a login's has: what the person says ahead of time
                # about a program is kept there, which is what Revoke takes back.
                "--state",
                str(self.state),
            ],
            env=self.env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        if not until(lambda: self.control.exists(), 60):
            if self.session.poll() is not None:
                raise SystemExit(f"the session exited at startup: {self.session.stderr.read()}")
            raise SystemExit("the session never answered on its socket")
        self.mcp = self.printed("TILO_MCP_SERVER")

    def printed(self, name: str, timeout: float = 60.0) -> pathlib.Path | None:
        """The path the session printed as `name=...` once it bound it, as a launcher reads it."""
        stdout = self.session.stdout.fileno()
        seen = b""
        end = time.time() + timeout
        while (left := end - time.time()) > 0 and select.select([stdout], [], [], left)[0]:
            chunk = os.read(stdout, 4096)
            if not chunk:
                break
            seen += chunk
            for line in seen.decode(errors="replace").split("\n")[:-1]:
                if line.startswith(f"{name}="):
                    return pathlib.Path(line.split("=", 1)[1])
        return None

    def stop(self) -> None:
        if self.session is None:
            return
        self.session.terminate()
        try:
            self.session.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.session.kill()
            self.session.wait()
        self.session = None

    def call(self, request: dict, timeout: float = 120.0) -> dict:
        with socket.socket(socket.AF_UNIX) as client:
            client.settimeout(timeout)
            client.connect(str(self.control))
            client.sendall(json.dumps(request).encode() + b"\n")
            answer = b""
            while not answer.endswith(b"\n"):
                chunk = client.recv(1 << 20)
                if not chunk:
                    break
                answer += chunk
        return json.loads(answer)

    def workspace(self) -> dict:
        tree = (self.call({"op": "state_dump"}).get("state") or {}).get("workspace_tree") or {}
        return (tree.get("workspaces") or [{}])[0]

    def grants(self) -> list[str]:
        """Every grant of the workspace in front, by its id."""
        workspace = self.workspace()
        policy = workspace.get("policy") or {}
        return [grant.get("id", "") for grant in policy.get("grants") or []]

    def agents(self) -> list[dict]:
        """The agents the shell shows, as its bar lists them."""
        return (self.call({"op": "state_dump"}).get("state") or {}).get("agents") or []

    def targets(self) -> dict[str, list[float]]:
        """The shell's named controls, each with where it stands."""
        return {target["name"]: target["rect"]
                for target in self.call({"op": "list_targets"}).get("targets") or []}

    def answer(self, choice: int = 1) -> bool:
        """Answer the sheet up as the person's own keyboard does.

        `choice` counts down from the first row: the server's sheet offers
        "Allow once", "Allow for this session" and "Always allow".
        """
        for _ in range(choice):
            self.call({"op": "key", "key": "ArrowDown"})
        self.call({"op": "key", "key": "Enter"})
        return True

    def stop_row(self) -> dict | None:
        """The Stop the person's own MCP server lists on the shell, if it lists one."""
        return next((row for row in catalogue(self.call, "stop")
                     if address(row) == ("session:agents", "stop-agent")), None)


def main() -> int:
    for path, what in ((SHELL, "session binary"), (SESSION_LAUNCHER, "session launcher")):
        if not path.exists():
            print(f"FAIL: no {what} at {path}", file=sys.stderr)
            return 1

    shell = Shell()
    agents: list[Agent] = []
    try:
        # A development shell opens on first-use setup; the throwaway identity a
        # development session opens its shell with is minted here, so the agents
        # are admitted into a workspace rather than into Setup.
        print("\n[0] a shell of its own")
        shell.start()
        minted = shell.call({"op": "provision_test_identity"})
        shell.stop()
        check("a development shell mints its throwaway identity",
              bool((minted.get("answer") or {}).get("created")), json.dumps(minted)[:120])
        shell.start()
        check("the shell opens with a workspace in front", bool(shell.workspace()),
              json.dumps(shell.workspace())[:80])
        check("and its MCP server answers", shell.mcp is not None and shell.mcp.exists(),
              str(shell.mcp))

        print("\n[1] two agents ask, and nothing they ask for runs yet")
        for name in CLIENTS:
            agents.append(Agent(shell.mcp, name))
        # The sheet is up: the person's own keyboard is what answers it. The
        # clients are still waiting on their `initialize` until then.
        check("no client is served before the person answers",
              not any(agent.hears(1.0) for agent in agents))

        # Each answer admits that client alone, as an agent of its own acting
        # for the person: what it may do is the person's grants capped at an
        # agent's, in a run its admission starts, so the workspace's policy
        # gains no grant of its own for it.
        print("\n[2] the person admits each agent, each as an agent of its own")
        grants_before = shell.grants()
        shell.answer(choice=1)
        check("the first answer serves one client", until(
            lambda: any(agent.hears() for agent in agents), 30)
            and sum(agent.hears() for agent in agents) == 1,
              "the other is still waiting on its own sheet")
        shell.answer(choice=1)
        if not until(lambda: all(agent.hears() for agent in agents), 15):
            # The sheet for the second client came up after the keys went by.
            shell.answer(choice=1)
        check("each client is served its initialize", until(lambda: all(
            agent.hears() for agent in agents), 15) and all(agent.answered() for agent in agents),
              "the server answered both")
        for agent in agents:
            agent.notify("notifications/initialized")
        check("admitting them grants nothing beyond the person's own",
              shell.grants() == grants_before, ", ".join(shell.grants())[:160])
        sessions = {agent.get("name") for agent in shell.agents()}
        check("the shell shows both agent sessions, each as itself",
              sessions == set(CLIENTS), ", ".join(sorted(map(str, sessions))))

        print("\n[3] each agent acts as itself")
        for agent, name in zip(agents, CLIENTS):
            agent.send("tools/list", {})
            listed = (agent.reply() or {}).get("result") or {}
            tools = sorted(tool.get("name", "") for tool in listed.get("tools") or [])
            check(f"{name} is served the agent's own tools",
                  "tilo_do" in tools and "tilo_read" in tools, ", ".join(tools)[:120])
            text, failed = agent.call("tilo_ls", {})
            check(f"{name}'s listing answers its own work", not failed,
                  text.splitlines()[0][:100] if text else "")
        # What the shell offers the person's own MCP server as a write is not what an
        # agent is offered: the session's Stop is the person's.
        stop = shell.stop_row()
        check("the person's own MCP server lists the session's Stop", stop is not None)
        text, failed = agents[0].call("tilo_do", {"path": "session#stop-agent",
                                                  "args": {"agent": shell.agents()[-1].get("key")}})
        check("an agent is not offered it", failed, text.splitlines()[0][:100] if text else "")
        # The same server speaks the control protocol to a program inside an app this
        # session started, and to nothing else: a program of this login's own is told so.
        with socket.socket(socket.AF_UNIX) as outsider:
            outsider.settimeout(30)
            outsider.connect(str(shell.mcp))
            outsider.sendall(b'{"op":"state_dump"}\n')
            reply = b""
            while not reply.endswith(b"\n"):
                chunk = outsider.recv(1 << 16)
                if not chunk:
                    break
                reply += chunk
        check("a control connection outside the session's apps is refused",
              (json.loads(reply or b"{}").get("err") or {}).get("code") == "not_allowed",
              reply.decode()[:140])

        print("\n[4] each agent's face on the bar opens its own card")
        sessions = {session.get("name"): session.get("key") for session in shell.agents()}
        for name in sessions:
            face = shell.targets().get(f"{name}, agent")
            opened = None
            if face is not None:
                x, y, width, height = face
                shell.call({"op": "click_at", "x": x + width / 2, "y": y + height / 2})
                until(lambda: f"{name}’s profile" in shell.targets(), 10)
                opened = [card.removesuffix("’s profile")
                          for card in shell.targets() if card.endswith("’s profile")]
            check(f"{name}'s face opens its own card", opened == [name],
                  f"opened {opened} at {face}")
            close_card(shell)
        # A reader is told the same face, and presses it where a person does: the name it
        # publishes is the face, not the bar behind it.
        for name in sessions:
            shell.call({"op": "click_named", "name": f"{name}, agent"})
            until(lambda: f"{name}’s profile" in shell.targets(), 10)
            opened = [card.removesuffix("’s profile")
                      for card in shell.targets() if card.endswith("’s profile")]
            check(f"a reader's {name} opens the same card", opened == [name], f"opened {opened}")
            close_card(shell)

        print("\n[5] the person stops one agent from its card, by name")
        stopped_name = next(iter(sessions))
        open_card(shell, stopped_name)
        check("its card offers Stop", "profile-agent-stop" in shell.targets())
        # The name a reader activates it by, as a person's pointer press reaches the same button.
        pressed = shell.call({"op": "click_named", "name": "Stop"})
        check("Stop is a control a reader can name", "ok" in pressed, json.dumps(pressed)[:120])
        keys = list(CLIENTS)
        gone = keys.index(stopped_name)
        stopped, other = agents[gone], agents[1 - gone]
        until(lambda: len(shell.agents()) < 2, 10)
        text, failed = stopped.call("tilo_ls", {})
        check("the stopped agent's connection is over", failed,
              text.splitlines()[0][:100] if text else "")
        text, failed = other.call("tilo_ls", {})
        check("the other agent still answers", not failed,
              text.splitlines()[0][:100] if text else "")
        left = [agent.get("name") for agent in shell.agents()]
        check("the shell keeps the other agent's session alone", left == [keys[1 - gone]],
              ", ".join(map(str, left)))

        print("\n[6] the person takes over the other agent's work")
        # It works in this shell alone: nothing of it is left admitted, and its
        # connection is over once the person takes its work on.
        open_card(shell, next(iter([name for name in sessions if name != stopped_name])))
        taken_at = shell.targets().get("profile-agent-take-over")
        check("its card offers Take over", taken_at is not None)
        pressed = shell.call({"op": "click_named", "name": "Take over"})
        check("Take over is a control a reader can name", "ok" in pressed, json.dumps(pressed)[:120])
        until(lambda: not shell.agents(), 10)
        check("the shell sessions nobody after taking the work on", not shell.agents(),
              json.dumps(shell.agents())[:160])
        text, failed = other.call("tilo_ls", {})
        check("the agent it was taken from stopped working", failed,
              text.splitlines()[0][:100] if text else "")

        print("\n[7] the person revokes what let an agent in")
        # A client of the same program, admitted as its own connection: Revoke takes back its
        # work here, and the next client of that program is asked about, as the first was.
        again = Agent(shell.mcp, "Claude Code")
        agents.append(again)
        shell.answer(choice=1)
        check("a client of the program is admitted",
              until(lambda: again.hears(), 30) and again.answered(),
              json.dumps(again.served)[:120])
        again.notify("notifications/initialized")
        sessions = [agent.get("key") for agent in shell.agents()]
        check("the shell shows the agent it let in", len(sessions) == 1, ", ".join(sessions))
        open_card(shell, shell.agents()[0]["name"])
        check("its card offers Revoke", "profile-agent-revoke" in shell.targets())
        pressed = shell.call({"op": "click_named", "name": "profile-agent-revoke"})
        check("Revoke is a control a reader can press", "ok" in pressed, json.dumps(pressed)[:120])
        until(lambda: not shell.agents(), 10)
        text, failed = again.call("tilo_ls", {})
        check("the revoked agent's connection is over", failed,
              text.splitlines()[0][:100] if text else "")
        check("the shell sessions nobody after the revoke", not shell.agents(),
              json.dumps(shell.agents())[:160])
        # What let it in is not the previous answer: the next client of that program is asked
        # about, and served once the person answers.
        next_client = Agent(shell.mcp, "Claude Code")
        agents.append(next_client)
        check("the next client of the program waits for the person",
              not next_client.hears(1.0))
        shell.answer(choice=1)
        check("the person's answer admits it",
              until(lambda: next_client.hears(), 30) and next_client.answered(),
              json.dumps(next_client.served)[:120])
        next_client.notify("notifications/initialized")
    finally:
        for agent in agents:
            agent.close()
        shell.stop()
        # The runtime directory the launcher named for this shell's state is
        # this run's, like the scratch home it belongs to.
        shutil.rmtree(shell.runtime, ignore_errors=True)
        shutil.rmtree(shell.dir, ignore_errors=True)

    print()
    if failures:
        print(f"FAILED: {len(failures)} check(s): {', '.join(failures)}")
        return 1
    print("Two agents work here, each admitted as itself. Every claim read off the session.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
