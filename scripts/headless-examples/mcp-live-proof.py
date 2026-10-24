#!/usr/bin/env python3
"""Drive a live compositor the way a language model does: over MCP.

`action-api-demo.py` exercises the socket. This exercises the layer a model
speaks to — `scripts/tilo-mcp-server.py` — against a compositor over MCP
stdio framing, without fixtures.

What it checks, in order:

  1. `initialize` negotiates and names the server.
  2. `tools/list` offers the five tools, including receipt status/cancel.
  3. MCP resources reveal the surface lazily: the root contains applications
     only; reading one application returns its objects with their actions
     nested beneath them. The full catalogue never lands in the model's
     initial context, and the tree follows the ownership apps already publish.
  4. `tilo_enumerate_actions` can still search the live surface explicitly.
  5. `tilo_invoke_action` moves state — read back with an
     independent `state_dump`, not from the tool's own reply.
  6. A refusal reaches the model as `isError: true` carrying the
     compositor's own code, rather than being smoothed into a success.

Exits non-zero if any of those fail.

Usage:
  ./mcp-live-proof.py
"""
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import time

REPO = pathlib.Path(__file__).resolve().parents[2]
# The nightly ignored-test lane points these at the binaries Cargo built for the run.
SHELL = pathlib.Path(os.environ.get("TILO_SHELL_BINARY", REPO / "target" / "debug" / "shell"))
CONTROL = pathlib.Path(os.environ.get("TILO_CONTROL_BINARY", REPO / "target" / "debug" / "tilo-control"))
SHIM = REPO / "scripts" / "tilo-mcp-server.py"
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from _control import runtime_socket_path  # noqa: E402

SOCKET = runtime_socket_path("mcp-live-proof.sock")
PROTOCOL = "2025-06-18"

failures: list[str] = []


def check(label: str, ok: bool, detail: str = "") -> bool:
    print(f"  {'PASS' if ok else 'FAIL'}  {label}" + (f" — {detail}" if detail else ""))
    if not ok:
        failures.append(label)
    return ok


def rpc(proc: subprocess.Popen, payload: dict) -> None:
    assert proc.stdin is not None
    proc.stdin.write(json.dumps(payload) + "\n")
    proc.stdin.flush()


def read_reply(proc: subprocess.Popen, want_id: int):
    """Read lines until the reply with `want_id` arrives.

    Notifications carry no id, so they are skipped rather than mistaken
    for the answer to the request in flight.
    """
    assert proc.stdout is not None
    while True:
        line = proc.stdout.readline()
        if not line:
            return None
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if msg.get("id") == want_id:
            return msg


def tool_text(reply) -> str:
    try:
        return reply["result"]["content"][0]["text"]
    except (KeyError, IndexError, TypeError):
        return ""


def resource_text(reply) -> dict:
    try:
        return json.loads(reply["result"]["contents"][0]["text"])
    except (KeyError, IndexError, TypeError, json.JSONDecodeError):
        return {}


def overlays() -> dict:
    """Read chrome overlays straight off the socket.

    Deliberately not through the shim: an effect confirmed by the same
    layer that claimed to cause it is not confirmed at all.
    """
    out = subprocess.run(
        [str(CONTROL), "--socket", SOCKET, "state-dump"],
        capture_output=True,
        text=True,
        timeout=30,
    )
    if out.returncode != 0:
        return {}
    return json.loads(out.stdout)["chrome"]["overlays"]


def main() -> int:
    for path, what in ((SHELL, "compositor"), (CONTROL, "tilo-control"), (SHIM, "MCP shim")):
        if not path.exists():
            print(f"FAIL: {what} missing at {path}", file=sys.stderr)
            return 1

    try:
        os.unlink(SOCKET)
    except FileNotFoundError:
        pass

    # Isolate the whole home, not just the workspace tree: record
    # documents live under `XDG_DATA_HOME`, so `TILO_STATE_PATH` alone
    # leaves runs sharing documents with each other and with whatever is
    # already on this machine.
    state_dir = tempfile.mkdtemp(prefix="tilo-mcp-proof-")
    env = {
        **os.environ,
        "TILO_TEST_CONTROL": SOCKET,
        "TILO_STATE_PATH": os.path.join(state_dir, "state.json"),
        "XDG_DATA_HOME": os.path.join(state_dir, "data"),
        "XDG_CACHE_HOME": os.path.join(state_dir, "cache"),
        "XDG_STATE_HOME": os.path.join(state_dir, "state"),
        "XDG_CONFIG_HOME": os.path.join(state_dir, "config"),
        # Its own runtime directory: the desktop this runs beside keeps its
        # sockets and session files.
        "XDG_RUNTIME_DIR": tempfile.mkdtemp(prefix="tilo-rt-"),
    }
    env.pop("DISPLAY", None)
    env.pop("WAYLAND_DISPLAY", None)

    compositor = subprocess.Popen(
        [str(SHELL), "--backend", "headless"],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    shim = None
    try:
        # Startup is not instant on a cold GPU; wait for the socket
        # itself rather than guessing a sleep.
        deadline = time.time() + 40.0
        while time.time() < deadline:
            if os.path.exists(SOCKET):
                break
            if compositor.poll() is not None:
                print("FAIL: compositor exited during startup", file=sys.stderr)
                return 1
            time.sleep(0.3)
        else:
            print("FAIL: control socket never appeared", file=sys.stderr)
            return 1
        time.sleep(1.0)

        shim = subprocess.Popen(
            [sys.executable, str(SHIM), "--socket", SOCKET],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            bufsize=1,
        )

        print("\n[1] initialize")
        rpc(shim, {
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": PROTOCOL, "capabilities": {},
                       "clientInfo": {"name": "mcp-live-proof", "version": "1"}},
        })
        reply = read_reply(shim, 1)
        ok = bool(reply) and "result" in reply
        check("the shim initializes", ok,
              json.dumps(reply["result"]["serverInfo"]) if ok else "no reply")
        rpc(shim, {"jsonrpc": "2.0", "method": "notifications/initialized"})

        print("\n[2] tools/list")
        rpc(shim, {"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
        reply = read_reply(shim, 2)
        names = sorted(t["name"] for t in reply["result"]["tools"]) if reply else []
        check("all five tools are offered",
              names == ["tilo_action_status", "tilo_cancel_action", "tilo_enumerate_actions",
                        "tilo_invoke_action", "tilo_state_dump"],
              ", ".join(names))

        print("\n[3] resources are lazy: application -> object -> action")
        rpc(shim, {"jsonrpc": "2.0", "id": 3, "method": "resources/list"})
        reply = read_reply(shim, 3)
        resources = (reply or {}).get("result", {}).get("resources", [])
        app_only = bool(resources) and all(
            resource.get("mimeType") == "application/vnd.tilo.application+json"
            and resource.get("uri", "").startswith("tilo://application/")
            for resource in resources
        )
        check("the root loads applications, not action descriptors", app_only,
              f"{len(resources)} application surface(s)")

        app = resources[0] if resources else {}
        rpc(shim, {"jsonrpc": "2.0", "id": 4, "method": "resources/read",
                   "params": {"uri": app.get("uri", "")}})
        application = resource_text(read_reply(shim, 4))
        objects = application.get("objects", [])
        nested_actions = [action for object_ in objects for action in object_.get("actions", [])]
        check("reading one application nests actions under their objects",
              application.get("type") == "application"
              and bool(objects) and bool(nested_actions),
              f"{len(objects)} object(s), {len(nested_actions)} action(s)")

        print("\n[4] enumerate through the shim")
        rpc(shim, {
            "jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": {"name": "tilo_enumerate_actions", "arguments": {"query": ""}},
        })
        reply = read_reply(shim, 5)
        try:
            payload = json.loads(tool_text(reply))
            count = payload.get("count", 0)
        except json.JSONDecodeError:
            count = 0
        check("a live surface reaches the model", count > 0, f"{count} actions")

        print("\n[5] invoke, and read the effect back independently")
        before = overlays().get("collaborators")
        rpc(shim, {
            "jsonrpc": "2.0", "id": 6, "method": "tools/call",
            "params": {"name": "tilo_invoke_action",
                       "arguments": {"provider": "actions", "id": "collaborators.open"}},
        })
        reply = read_reply(shim, 6)
        check("the invocation is not an error",
              reply is not None and not reply["result"].get("isError"),
              tool_text(reply)[:60].replace("\n", " "))
        time.sleep(0.6)
        after = overlays().get("collaborators")
        check("live state actually moved", before != after, f"{before} -> {after}")

        print("\n[6] a refusal reaches the model intact")
        rpc(shim, {
            "jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": {"name": "tilo_invoke_action",
                       "arguments": {"provider": "actions", "id": "not.a.real.action"}},
        })
        reply = read_reply(shim, 7)
        text = tool_text(reply)
        check("the model is told it failed",
              reply is not None and reply["result"].get("isError") is True)
        check("the compositor's own reason survives", "unknown_action" in text,
              text[:80].replace("\n", " "))

        print()
        if failures:
            print(f"FAILED: {len(failures)} check(s): {', '.join(failures)}")
            return 1
        print("A model can drive this compositor over MCP. Every claim read back live.")
        return 0
    finally:
        if shim is not None:
            shim.terminate()
            try:
                shim.wait(timeout=5)
            except subprocess.TimeoutExpired:
                shim.kill()
        compositor.terminate()
        try:
            compositor.wait(timeout=5)
        except subprocess.TimeoutExpired:
            compositor.kill()
        try:
            os.unlink(SOCKET)
        except FileNotFoundError:
            pass
        shutil.rmtree(state_dir, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
