#!/usr/bin/env python3
"""An MCP client the way Claude Code runs one, for the acceptance suite to run in the guest.

    mcp_client.py session   # connect, initialize, tools/list, list apps, a new Notes note, a refusal
    mcp_client.py deny      # connect and initialize only (the person refuses)

It spawns `tilo-runtime-mcp --connect` (override with ACC_MCP_BIN) with no profile, as
`claude mcp add tilo -- tilo-runtime-mcp --connect` does, speaks newline-delimited JSON-RPC
over its stdio, and prints one line, `ACC-MCP <json>`, with everything it saw. It judges
nothing: the harness reads the transcript. Python standard library only (the guest image's).

Inside a Tilo session `--connect` relays to the session's agent door, and the door asks the
person before it answers `initialize`, so `initialize` may take as long as the person does.
"""
from __future__ import annotations

import json
import os
import select
import subprocess
import sys
import time
import tempfile

BIN = os.environ.get("ACC_MCP_BIN", "tilo-runtime-mcp")
INITIALIZE_S = float(os.environ.get("ACC_MCP_INITIALIZE_S", "90"))
CALL_S = float(os.environ.get("ACC_MCP_CALL_S", "30"))
PROTOCOL = "2025-06-18"
# The note the agent makes, found again by its title (ACC_MCP_TITLE from the harness).
TITLE = os.environ.get("ACC_MCP_TITLE", "Acceptance agent note")
# How long an app's directory may take to appear on the door after it opens.
LISTED_S = 20.0


def daemons() -> set[int]:
    """Pids of standalone runtime daemons: a `--connect` in a session must never start one.

    Read from /proc (what `pgrep -x` matches, the process name), so the client needs no
    process tools beyond the standard library."""
    found = set()
    for entry in os.scandir("/proc"):
        if not entry.name.isdigit():
            continue
        try:
            with open(f"/proc/{entry.name}/comm", encoding="utf-8") as comm:
                if comm.read().strip() == "tilo-runtime":
                    found.add(int(entry.name))
        except OSError:
            continue  # it exited, or is not ours to read
    return found


class Client:
    def __init__(self):
        # Diagnostics must not fill a pipe while we wait for a protocol reply.
        self.stderr = tempfile.TemporaryFile()
        try:
            self.proc = subprocess.Popen([BIN, "--connect"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                         stderr=self.stderr, bufsize=0)
        except BaseException:
            self.stderr.close()
            raise
        self.buffer = b""
        self.eof = False
        self.next_id = 0
        self.trace: list[dict] = []

    def send(self, message: dict) -> None:
        self.trace.append({"to": "server", "message": message})
        self.proc.stdin.write((json.dumps(message) + "\n").encode())
        self.proc.stdin.flush()

    def read(self, timeout: float):
        """The next JSON line, `None` at EOF, or `"timeout"`."""
        deadline = time.monotonic() + timeout
        while b"\n" not in self.buffer and not self.eof:
            ready, _, _ = select.select([self.proc.stdout], [], [], max(0, deadline - time.monotonic()))
            if not ready:
                return "timeout"
            chunk = os.read(self.proc.stdout.fileno(), 65536)
            self.eof = not chunk
            self.buffer += chunk
        if not self.buffer:
            return None
        line, separator, rest = self.buffer.partition(b"\n")
        self.buffer = rest if separator else b""
        try:
            message = json.loads(line)
        except (json.JSONDecodeError, UnicodeDecodeError):
            message = {"unparsed": line[:300].decode(errors="replace")}
        self.trace.append({"from": "server", "message": message})
        return message

    def call(self, method: str, params: dict | None = None, timeout: float = CALL_S):
        """The reply to a request: its message, `None` if the connection closed, `"timeout"`."""
        self.next_id += 1
        want = self.next_id
        self.send({"jsonrpc": "2.0", "id": want, "method": method, **({"params": params} if params else {})})
        deadline = time.monotonic() + timeout
        while True:
            message = self.read(max(0.0, deadline - time.monotonic()))
            if not isinstance(message, dict):
                return message
            if message.get("id") == want:
                return message
            if time.monotonic() >= deadline:
                return "timeout"

    def tool(self, name: str, arguments: dict):
        return self.call("tools/call", {"name": name, "arguments": arguments})

    def close(self) -> str:
        try:
            self.proc.stdin.close()
        except OSError:
            pass
        try:
            # Drain stdout too: a server may finish by writing notifications.
            self.proc.stdin = None
            self.proc.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.communicate()
        finally:
            self.proc.stdout.close()
        self.stderr.seek(0, os.SEEK_END)
        self.stderr.seek(max(0, self.stderr.tell() - 1500))
        try:
            return self.stderr.read().decode(errors="replace")
        finally:
            self.stderr.close()


def text_of(reply) -> str:
    try:
        return "\n".join(b.get("text", "") for b in reply["result"]["content"])
    except (KeyError, TypeError):
        return ""


def failed(reply) -> bool | None:
    """Whether a tool reply is an error; None when it is no tool result at all."""
    try:
        value = reply["result"].get("isError", False)
    except (KeyError, TypeError, AttributeError):
        return None
    return value if isinstance(value, bool) else None


def tool_until(client: Client, name: str, arguments: dict, ready, timeout: float = LISTED_S):
    """Call a tool until it succeeds and `ready(text)` holds, or the time is up: an app
    publishes its directory a moment after it opens. The last reply either way."""
    deadline = time.monotonic() + timeout
    while True:
        reply = client.tool(name, arguments)
        if failed(reply) is False and ready(text_of(reply)):
            return reply
        if not isinstance(reply, dict) or time.monotonic() >= deadline:
            return reply
        time.sleep(1.0)


def initialize(client: Client):
    return client.call("initialize", {"protocolVersion": PROTOCOL, "capabilities": {},
                                      "clientInfo": {"name": "acceptance-agent", "version": "1"}},
                       timeout=INITIALIZE_S)


def session(out: dict) -> None:
    client = Client()
    out["daemons_before"] = sorted(daemons())
    try:
        out["initialize"] = initialize(client)
        if not isinstance(out["initialize"], dict) or "result" not in out["initialize"]:
            return
        client.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        out["tools_list"] = client.call("tools/list")
        # The apps, one line each: `notes  Notes  running` for the open notebook.
        out["list_apps"] = tool_until(client, "tilo_ls", {}, lambda text: any(
            line.startswith("notes ") and line.endswith("running") for line in text.splitlines()))
        out["apps_text"] = text_of(out["list_apps"])
        # A new note through the action Notes publishes, then found by its title.
        out["new_note_params"] = client.tool("tilo_ls", {"path": "notes#new-note"})
        out["new_note"] = client.tool("tilo_do", {"path": "notes#new-note", "args": {"title": TITLE}})
        out["found_note"] = tool_until(client, "tilo_find", {"query": TITLE, "app": "notes"},
                                       lambda text: TITLE in text)
        # A refusal must reach the model as isError, carrying the reason.
        out["refusal"] = client.tool("tilo_do", {"path": "notes#not-a-real-action"})
    finally:
        out["daemons_after"] = sorted(daemons())
        out["stderr"] = client.close()
        out["trace_len"] = len(client.trace)


def deny(out: dict) -> None:
    client = Client()
    try:
        out["initialize"] = initialize(client)
        # The door closes a refused connection: the next read is EOF, not another reply.
        out["after_refusal"] = client.read(5.0)
    finally:
        out["stderr"] = client.close()


def main() -> int:
    scenario = sys.argv[1] if len(sys.argv) > 1 else "session"
    out: dict = {"scenario": scenario, "bin": BIN}
    try:
        {"session": session, "deny": deny}[scenario](out)
    except Exception as exc:  # noqa: BLE001 - the harness reads what went wrong
        out["client_error"] = f"{type(exc).__name__}: {exc}"
    print("ACC-MCP " + json.dumps(out))
    return 0


if __name__ == "__main__":
    sys.exit(main())
