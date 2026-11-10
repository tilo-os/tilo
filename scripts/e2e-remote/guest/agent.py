#!/usr/bin/env python3
"""Claude Code, as far as an MCP server can tell, run on a Tilo box by the e2e-remote harness.

    agent.py SCENARIO

It spawns the server command `E2E_CMD` names (a JSON list, base64-encoded: what
`tilo agent connect claude [--device NAME]` registered with `claude mcp add`), speaks
newline-delimited JSON-RPC over its stdio with clientInfo `claude-code`, as Claude Code does,
and prints one line, `E2E-MCP <json>`, with everything it saw. It judges nothing: the harness
reads the transcript. Python standard library only (the guest image's).

Scenarios:
  devices     initialize, tools/list, then `tilo_devices` until it lists the device
              E2E_DEVICE (an endpoint id) as reachable, or E2E_WAIT_S passes
  unanswered  initialize and wait E2E_WAIT_S for an answer that must not come
  stop        initialize, tools/list, `tilo_devices`, `tilo_ls`; then E2E_READY is written
              and `tilo_ls` asked once a second until the connection ends (or E2E_WAIT_S)
  work        initialize, tools/list, `tilo_devices`, `tilo_ls`, Notes opened with
              `notes#open`, a note titled E2E_TITLE made with `notes#new-note` once
              E2E_GO exists, found again with `tilo_find`
  refused     initialize, then what follows the answer
  browser     initialize, tools/list, `tilo_ls`; with E2E_NAVIGATE=1 the Browser that is open
              sent to E2E_URL; the page's form read with `browser/page#read-page-forms`; then
              E2E_READY is written and, once E2E_GO exists, the answers in E2E_ANSWERS (a JSON
              object) placed with `browser/page#fill-page-form`; last the page's fields and
              record read. What it was answered so far is kept in E2E_PROGRESS
"""
from __future__ import annotations

import base64
import json
import os
import select
import subprocess
import sys
import tempfile
import time

PROTOCOL = "2025-06-18"
CLIENT = {"name": "claude-code", "version": "2.1.0"}
INITIALIZE_S = float(os.environ.get("E2E_INITIALIZE_S", "120"))
CALL_S = float(os.environ.get("E2E_CALL_S", "60"))
WAIT_S = float(os.environ.get("E2E_WAIT_S", "60"))
CLOSE_S = 5.0


class Client:
    def __init__(self, command: list[str]):
        self.stderr = tempfile.TemporaryFile()
        self.started = time.monotonic()
        self.proc = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=self.stderr, bufsize=0)
        self.buffer = b""
        self.eof = False
        self.next_id = 0

    def since(self) -> float:
        return round(time.monotonic() - self.started, 2)

    def send(self, message: dict) -> bool:
        try:
            self.proc.stdin.write((json.dumps(message) + "\n").encode())
            self.proc.stdin.flush()
            return True
        except (BrokenPipeError, OSError):
            return False

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
            return json.loads(line)
        except (json.JSONDecodeError, UnicodeDecodeError):
            return {"unparsed": line[:300].decode(errors="replace")}

    def call(self, method: str, params: dict | None = None, timeout: float = CALL_S):
        """The reply to a request: its message, `None` if the connection closed, `"timeout"`."""
        self.next_id += 1
        want = self.next_id
        if not self.send({"jsonrpc": "2.0", "id": want, "method": method,
                          **({"params": params} if params else {})}):
            return None
        deadline = time.monotonic() + timeout
        while True:
            message = self.read(max(0.0, deadline - time.monotonic()))
            if not isinstance(message, dict):
                return message
            if message.get("id") == want:
                return message
            if time.monotonic() >= deadline:
                return "timeout"

    def tool(self, name: str, arguments: dict, timeout: float = CALL_S):
        return self.call("tools/call", {"name": name, "arguments": arguments}, timeout)

    def initialize(self, timeout: float = INITIALIZE_S):
        reply = self.call("initialize", {"protocolVersion": PROTOCOL, "capabilities": {},
                                         "clientInfo": CLIENT}, timeout)
        if isinstance(reply, dict) and "result" in reply:
            self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        return reply

    def close(self) -> dict:
        try:
            self.proc.stdin.close()
        except OSError:
            pass
        try:
            self.proc.stdin = None
            self.proc.communicate(timeout=CLOSE_S)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.communicate()
        finally:
            self.proc.stdout.close()
        self.stderr.seek(0, os.SEEK_END)
        self.stderr.seek(max(0, self.stderr.tell() - 3000))
        try:
            return {"stderr": self.stderr.read().decode(errors="replace"), "exit": self.proc.returncode}
        finally:
            self.stderr.close()


def text_of(reply) -> str:
    try:
        return "\n".join(b.get("text", "") for b in reply["result"]["content"])
    except (KeyError, TypeError, AttributeError):
        return ""


def ok(reply) -> bool:
    try:
        return reply["result"].get("isError", False) is False
    except (KeyError, TypeError, AttributeError):
        return False


def until(client: Client, name: str, arguments: dict, ready, timeout: float):
    """Call a tool until it succeeds and `ready(text)` holds, or the time is up."""
    deadline = time.monotonic() + timeout
    while True:
        reply = client.tool(name, arguments)
        if ok(reply) and ready(text_of(reply)):
            return reply
        if not isinstance(reply, dict) or time.monotonic() >= deadline:
            return reply
        time.sleep(1.0)


def touch(path: str) -> None:
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as marker:
        marker.write(str(time.time()))


def do(client: Client, path: str, args: dict | None, patience: float):
    """Run an action and wait for how it went: the door answers one that takes longer than a
    few seconds as pending, and `tilo_status` says how it ended."""
    call = {"path": path}
    if args is not None:
        call["args"] = args
    reply = client.tool("tilo_do", call, timeout=patience)
    deadline = time.monotonic() + patience
    while text_of(reply).startswith("pending ") and time.monotonic() < deadline:
        invocation = text_of(reply).split()[1].rstrip(":")
        time.sleep(2.0)
        reply = client.tool("tilo_status", {"invocation_id": invocation}, timeout=patience)
    return reply


def keep(out: dict) -> None:
    """Write what was answered so far where E2E_PROGRESS says, for a run stopped before its end."""
    path = os.environ.get("E2E_PROGRESS")
    if not path:
        return
    path = os.path.expandvars(path)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path + ".part", "w", encoding="utf-8") as kept:
        json.dump(out, kept)
    os.replace(path + ".part", path)


def introduce(client: Client, out: dict) -> bool:
    out["initialize"] = client.initialize()
    out["initialize_at"] = client.since()
    if not isinstance(out["initialize"], dict) or "result" not in out["initialize"]:
        return False
    out["tools_list"] = client.call("tools/list")
    out["devices"] = client.tool("tilo_devices", {})
    out["apps"] = client.tool("tilo_ls", {})
    return True


def devices(client: Client, out: dict) -> None:
    out["initialize"] = client.initialize()
    out["initialize_at"] = client.since()
    if not isinstance(out["initialize"], dict) or "result" not in out["initialize"]:
        return
    out["tools_list"] = client.call("tools/list")
    want = os.environ.get("E2E_DEVICE", "")
    # One line a device: `NAME  ENDPOINT-ID  reachable`, or `not reachable: why`.
    out["devices"] = until(client, "tilo_devices", {}, lambda text: any(
        want in line and line.rstrip().endswith("  reachable") for line in text.splitlines()),
        WAIT_S)
    out["devices_at"] = client.since()
    # The same action the agent takes on the other device, taken here: whether this door
    # carries it out tells a refusal of the other device's door from every door's.
    out["open_here"] = client.tool("tilo_do", {"path": "notes#open"})


def unanswered(client: Client, out: dict) -> None:
    out["initialize"] = client.initialize(WAIT_S)
    out["initialize_at"] = client.since()


def stop(client: Client, out: dict) -> None:
    if not introduce(client, out):
        return
    touch(os.path.expandvars(os.environ["E2E_READY"]))
    out["ready_at"] = client.since()
    deadline = time.monotonic() + WAIT_S
    asks = []
    while time.monotonic() < deadline:
        reply = client.tool("tilo_ls", {}, timeout=10)
        asks.append({"at": client.since(), "reply": reply if not isinstance(reply, dict)
                     else ("ok" if ok(reply) else reply)})
        if reply is None or (isinstance(reply, dict) and "error" in reply):
            break
        time.sleep(1.0)
    out["after_stop"] = asks[-12:]
    out["ended_at"] = client.since()


def work(client: Client, out: dict) -> None:
    if not introduce(client, out):
        return
    title = os.environ["E2E_TITLE"]
    out["open_notes"] = do(client, "notes#open", None, 60)
    out["notes_listing"] = until(client, "tilo_ls", {"path": "notes"},
                                 lambda text: "#new-note" in text, 30)
    go = os.path.expandvars(os.environ["E2E_GO"])
    touch(os.path.expandvars(os.environ["E2E_READY"]))
    deadline = time.monotonic() + WAIT_S
    while not os.path.exists(go) and time.monotonic() < deadline:
        time.sleep(0.5)
    out["go"] = os.path.exists(go)
    out["new_note"] = do(client, "notes#new-note", {"title": title}, 60)
    # The door's "nothing matches <title>" names the title too: that is not the note.
    out["found_note"] = until(client, "tilo_find", {"query": title, "app": "notes"},
                              lambda text: title in text and not text.startswith("nothing matches"), 30)


def page_of(client: Client, out: dict, patience: float = 60.0) -> str | None:
    """The Browser's page as this door names it: by what it is, else by its key. A Browser that
    was only now opened takes a moment to publish it. None when the door names no page."""
    out["page_listings"] = {}
    deadline = time.monotonic() + patience
    while True:
        for path in ("browser/page", "browser/browser.page"):
            reply = client.tool("tilo_ls", {"path": path})
            out["page_listings"][path] = reply
            # A Browser that is not running is answered with a note, not a refusal.
            if ok(reply) and "is installed but not running" not in text_of(reply):
                return path
        if time.monotonic() >= deadline:
            return None
        time.sleep(1.0)


def browser(client: Client, out: dict) -> None:
    keep(out)
    admitted = introduce(client, out)
    keep(out)
    if not admitted:
        return
    # A step that acts on a page is put to the person at the desktop first: it answers once
    # they have, so it is waited for as long as the door waits for them.
    asked_s = float(os.environ.get("E2E_ASKED_S", "240"))
    page = page_of(client, out)
    out["page"] = page
    keep(out)
    if page is None:
        return
    if os.environ.get("E2E_NAVIGATE") == "1":
        out["navigate"] = do(client, f"{page}#navigate", {"url": os.environ["E2E_URL"]}, asked_s)
        keep(out)
    out["forms"] = until(client, "tilo_do", {"path": f"{page}#read-page-forms"},
                         lambda text: "form1" in text and "Given names" in text, asked_s)
    keep(out)
    go = os.path.expandvars(os.environ["E2E_GO"])
    touch(os.path.expandvars(os.environ["E2E_READY"]))
    out["ready_at"] = client.since()
    deadline = time.monotonic() + WAIT_S
    while not os.path.exists(go) and time.monotonic() < deadline:
        time.sleep(0.2)
    out["go"] = os.path.exists(go)
    out["fill_from"] = client.since()
    out["fill"] = do(client, f"{page}#fill-page-form",
                     {"form": "form1", "answers": json.loads(os.environ["E2E_ANSWERS"])}, asked_s)
    out["fill_until"] = client.since()
    keep(out)
    out["fields"] = do(client, f"{page}#read-page-fields", None, asked_s)
    out["record"] = do(client, f"{page}#read-page-record", None, asked_s)


def refused(client: Client, out: dict) -> None:
    out["initialize"] = client.initialize(WAIT_S)
    out["initialize_at"] = client.since()
    out["after"] = client.read(5.0)


def main() -> int:
    scenario = sys.argv[1]
    command = json.loads(base64.b64decode(os.environ["E2E_CMD"]))
    out: dict = {"scenario": scenario, "command": command}
    client = None
    try:
        client = Client(command)
        {"devices": devices, "unanswered": unanswered, "stop": stop, "work": work,
         "refused": refused, "browser": browser}[scenario](client, out)
    except Exception as exc:  # noqa: BLE001 - the harness reads what went wrong
        out["client_error"] = f"{type(exc).__name__}: {exc}"
    finally:
        if client is not None:
            out["server"] = client.close()
    print("E2E-MCP " + json.dumps(out))
    return 0


if __name__ == "__main__":
    sys.exit(main())
