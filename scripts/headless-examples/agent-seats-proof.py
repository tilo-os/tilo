#!/usr/bin/env python3
"""Two agents at the session's agent door, each admitted as itself.

The session's agent door is how a program of this login — an AI agent in a
terminal — reaches this desktop's actions as an agent, never as the person.
This proof drives two such clients at once, through a headless session of its
own, and reads every claim back off the session rather than off the clients:

  1. asking    each client opens with `initialize` and waits: nothing it asked
               for runs until the person answers the sheet the door puts up.
  2. seats     the person answers each sheet, once per client; each agent is
               admitted as a member and shows on the desk as its own presence,
               a seat of its own beside the person's runs.
  3. acting    each admitted client is served as itself: its own tools answer,
               and what the desk offers a person's own socket is not what it is
               offered.
  4. stopping  stopping one agent's seat ends that connection alone; the other
               agent still answers, and the stopped one does not.

Usage:
  ./agent-seats-proof.py
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

REPO = pathlib.Path(__file__).resolve().parents[2]
# The nightly step points this at the binary Cargo built for the run.
SHELL = pathlib.Path(os.environ.get("TILO_SHELL_BINARY", REPO / "target" / "debug" / "tilo-session"))
PROTOCOL = "2025-06-18"
# What each client calls itself: the door proposes each as this agent id.
CLIENTS = [("Claude Code", "claude-code"), ("Codex", "codex")]

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


class Agent:
    """One MCP client at the agent door, on its own connection."""

    def __init__(self, path: pathlib.Path, name: str) -> None:
        self.name = name
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.settimeout(120)
        self.sock.connect(str(path))
        self.file = self.sock.makefile("rwb")
        self.next_id = 1
        self.served = None
        self.send("initialize", {
            "protocolVersion": PROTOCOL,
            "capabilities": {},
            "clientInfo": {"name": name, "version": "1"},
        })

    def send(self, method: str, params: dict) -> int:
        request = {"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params}
        self.next_id += 1
        self.file.write(json.dumps(request).encode() + b"\n")
        self.file.flush()
        return request["id"]

    def notify(self, method: str) -> None:
        self.file.write(json.dumps({"jsonrpc": "2.0", "method": method}).encode() + b"\n")
        self.file.flush()

    def reply(self):
        """The answer to the request in flight, or None at the connection's end."""
        while True:
            line = self.file.readline()
            if not line:
                return None
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                continue
            if "id" in message:
                return message

    def hears(self, timeout: float = 0.0) -> bool:
        """Whether the door has sent anything back on this connection yet."""
        ready, _, _ = select.select([self.sock], [], [], timeout)
        return bool(ready)

    def answered(self, timeout: float = 120.0) -> bool:
        """Whether the door answered its `initialize`, waiting up to `timeout`."""
        if self.served is not None:
            return "error" not in self.served
        self.sock.settimeout(timeout)
        self.served = self.reply()
        return self.served is not None and "error" not in self.served

    def call(self, tool: str, arguments: dict):
        """(text, failed) of one tool call, once the door answers it.

        A connection the person stopped answers nothing: writing to it fails,
        which is the same answer as no reply at all."""
        try:
            self.send("tools/call", {"name": tool, "arguments": arguments})
            message = self.reply()
        except OSError:
            return "the connection is over", True
        if message is None:
            return "", True
        try:
            result = message["result"]
        except KeyError:
            return json.dumps(message), True
        try:
            text = result["content"][0]["text"]
        except (KeyError, IndexError, TypeError):
            text = ""
        return text, bool(result.get("isError"))

    def close(self) -> None:
        try:
            self.sock.close()
        except OSError:
            pass


class Desk:
    """A headless session on a scratch home of its own, driven at its socket."""

    def __init__(self) -> None:
        self.dir = pathlib.Path(tempfile.mkdtemp(prefix="tilo-agent-seats-proof-"))
        self.runtime = self.dir / "run"
        self.home = self.dir / "home"
        self.identity = self.dir / "identity"
        for path in (self.runtime, self.home, self.identity):
            path.mkdir(parents=True, exist_ok=True)
        self.identity.chmod(0o700)
        self.control = self.runtime / "control.sock"
        self.door = self.runtime / "agent.sock"
        self.env = {
            **os.environ,
            "HOME": str(self.home),
            "TILO_TEST_CONTROL": str(self.control),
            # The door a developer names, so this proof never meets the login's own.
            "TILO_AGENT_DOOR": str(self.door),
            "TILO_IDENTITY_ROOT": str(self.identity),
            "XDG_RUNTIME_DIR": str(self.runtime),
            "XDG_DATA_HOME": str(self.dir / "data"),
            "XDG_CACHE_HOME": str(self.dir / "cache"),
            "XDG_STATE_HOME": str(self.dir / "state"),
            "XDG_CONFIG_HOME": str(self.dir / "config"),
        }
        self.env.pop("DISPLAY", None)
        self.env.pop("WAYLAND_DISPLAY", None)
        self.session = None

    def start(self) -> None:
        self.control.unlink(missing_ok=True)
        self.door.unlink(missing_ok=True)
        self.session = subprocess.Popen(
            [str(SHELL), "--backend", "headless", "--seed-apps"],
            env=self.env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
        )
        if not until(lambda: self.control.exists(), 60):
            if self.session.poll() is not None:
                raise SystemExit(f"the session exited at startup: {self.session.stderr.read()}")
            raise SystemExit("the session never answered on its socket")

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
        """The agents the desk shows, as its bar seats them."""
        return (self.call({"op": "state_dump"}).get("state") or {}).get("agents") or []

    def answer(self, choice: int = 1) -> bool:
        """Answer the sheet up as the person's own keyboard does.

        `choice` counts down from the first row: the door's sheet offers
        "Allow once", "Allow for this session" and "Always allow".
        """
        for _ in range(choice):
            self.call({"op": "key", "key": "ArrowDown"})
        self.call({"op": "key", "key": "Enter"})
        return True

    def stop_row(self) -> dict | None:
        """The Stop the person's own door lists on the desk, if it lists one."""
        rows = (self.call({"op": "actions_enumerate", "query": "stop"}).get("answer")
                or {}).get("actions") or []
        return next((row for row in rows
                     if row.get("resource") == "session:desk"
                     and row.get("operation") == "stop-agent"), None)


def main() -> int:
    if not SHELL.exists():
        print(f"FAIL: no session binary at {SHELL}", file=sys.stderr)
        return 1

    desk = Desk()
    agents: list[Agent] = []
    try:
        # A development desk opens on first-use setup; the throwaway identity a
        # development session opens its desk with is minted here, so the agents
        # are admitted into a workspace rather than into Setup.
        print("\n[0] a desk of its own")
        desk.start()
        minted = desk.call({"op": "provision_test_identity"})
        desk.stop()
        check("a development desk mints its throwaway identity",
              bool((minted.get("answer") or {}).get("created")), json.dumps(minted)[:120])
        desk.start()
        check("the desk opens with a workspace in front", bool(desk.workspace()),
              json.dumps(desk.workspace())[:80])
        check("and its agent door answers", until(lambda: desk.door.exists()))

        print("\n[1] two agents ask, and nothing they ask for runs yet")
        for name, _ in CLIENTS:
            agents.append(Agent(desk.door, name))
        # The sheet is up: the person's own keyboard is what answers it. The
        # clients are still waiting on their `initialize` until then.
        check("no client is served before the person answers",
              not any(agent.hears(1.0) for agent in agents))

        # Each answer admits that client alone: the workspace takes it in with
        # the Observer preset's grants, through its governance where this
        # device holds it (as the settings sheet does) and as the desk's own
        # membership where it does not, which is what a scratch desk is.
        print("\n[2] the person admits each agent, and both are members")
        desk.answer(choice=1)
        check("the first answer serves one client", until(
            lambda: any(agent.hears() for agent in agents), 30)
            and sum(agent.hears() for agent in agents) == 1,
              "the other is still waiting on its own sheet")
        desk.answer(choice=1)
        if not until(lambda: all(agent.hears() for agent in agents), 15):
            # The sheet for the second client came up after the keys went by.
            desk.answer(choice=1)
        check("each client is served its initialize", until(lambda: all(
            agent.hears() for agent in agents), 15) and all(agent.answered() for agent in agents),
              "the door answered both")
        for agent, (_, expected) in zip(agents, CLIENTS):
            agent.notify("notifications/initialized")
            listed = desk.grants()
            check(f"the workspace's policy names {expected}", any(
                grant.startswith(f"agent-member:{expected}:") for grant in listed),
                ", ".join(listed)[:160])
        seated = {agent.get("key") for agent in desk.agents()}
        check("the desk seats both agents, each as itself",
              seated == {expected for _, expected in CLIENTS}, ", ".join(sorted(map(str, seated))))
        check("each seat is the person's own to end",
              all(agent.get("seat") for agent in desk.agents()), json.dumps(desk.agents())[:160])

        print("\n[3] each agent acts as itself")
        for agent, (name, _) in zip(agents, CLIENTS):
            agent.send("tools/list", {})
            listed = (agent.reply() or {}).get("result") or {}
            tools = sorted(tool.get("name", "") for tool in listed.get("tools") or [])
            check(f"{name} is served the agent's own tools",
                  "tilo_do" in tools and "tilo_read" in tools, ", ".join(tools)[:120])
            text, failed = agent.call("tilo_ls", {})
            check(f"{name}'s listing answers its own work", not failed,
                  text.splitlines()[0][:100] if text else "")
        # What the desk offers the person's own door as a write is not what an
        # agent is offered: the seat's Stop is the person's.
        stop = desk.stop_row()
        check("the person's own door lists the seat's Stop", stop is not None)
        text, failed = agents[0].call("tilo_do", {"path": "session#stop-agent",
                                                  "args": {"agent": CLIENTS[1][1]}})
        check("an agent is not offered it", failed, text.splitlines()[0][:100] if text else "")
        # The same door speaks the control protocol to a program inside an app this
        # session started, and to nothing else: a program of this login's own is told so.
        with socket.socket(socket.AF_UNIX) as outsider:
            outsider.settimeout(30)
            outsider.connect(str(desk.door))
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

        print("\n[4] stopping one agent's seat ends that connection alone")
        # The desk's own Stop: the person's door names the agent, as their press on the
        # agent's presence does.
        if stop is not None:
            desk.call({"op": "actions_invoke", "provider": stop["provider"], "id": stop["id"],
                       "args": {"agent": CLIENTS[1][1]}})
        stopped, other = agents[1], agents[0]
        text, failed = stopped.call("tilo_ls", {})
        check("the stopped agent's connection is over", failed,
              text.splitlines()[0][:100] if text else "")
        text, failed = other.call("tilo_ls", {})
        check("the other agent still answers", not failed,
              text.splitlines()[0][:100] if text else "")
        left = [agent.get("key") for agent in desk.agents()]
        check("the desk keeps the other agent's seat alone", left == [CLIENTS[0][1]],
              ", ".join(map(str, left)))
    finally:
        for agent in agents:
            agent.close()
        desk.stop()
        shutil.rmtree(desk.dir, ignore_errors=True)

    print()
    if failures:
        print(f"FAILED: {len(failures)} check(s): {', '.join(failures)}")
        return 1
    print("Two agents work here, each admitted as itself. Every claim read off the session.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
