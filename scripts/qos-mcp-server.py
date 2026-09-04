#!/usr/bin/env python3
"""MCP stdio server exposing a running nirios compositor's typed action surface.

Bridges two protocols, both newline-delimited JSON:

  language model  <--JSON-RPC 2.0 over stdio-->  this shim
  this shim       <--QOS_TEST_CONTROL socket-->  compositor

The point of the shim is that a model drives the OS *semantically*: it asks
which actions exist and invokes one by its stable `(provider, id)` address.
Nothing here screenshots, hit-tests or synthesises input.

Honesty contract: this shim never invents an action, a result or a success.
If the compositor is unreachable, or answers `{"err": ...}`, the caller gets
that fact -- with the compositor's own error code and message -- as a tool
error. A refusal is a result, not a failure to paper over.

Stdlib only. Run `--selftest` to exercise it without a model in the loop.
"""
from __future__ import annotations

import argparse
import json
import os
import socket
import sys
import urllib.parse

PROTOCOL_VERSIONS = ("2025-06-18", "2025-03-26", "2024-11-05")
DEFAULT_SOCKET = "/tmp/nirios.sock"
SERVER_INFO = {"name": "qos-actions", "version": "0.1.0"}


class CompositorError(RuntimeError):
    """A structured `{"err": {"code", "message"}}` from the compositor."""

    def __init__(self, code: str, message: str):
        super().__init__(f"{code}: {message}")
        self.code = code
        self.message = message


class Compositor:
    """One request, one response, one connection -- the same pattern as
    scripts/headless-examples/_control.py."""

    def __init__(self, socket_path: str, timeout: float = 10.0):
        self.socket_path = socket_path
        self.timeout = timeout

    def call(self, payload: dict) -> dict:
        if not os.path.exists(self.socket_path):
            raise ConnectionError(
                f"no compositor control socket at {self.socket_path}. Start one with "
                "`QOS_TEST_CONTROL=<path> cargo run --bin shell -- --backend headless`, "
                "or pass --socket."
            )
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(self.timeout)
        try:
            s.connect(self.socket_path)
        except OSError as e:
            raise ConnectionError(f"cannot connect to {self.socket_path}: {e}") from e
        try:
            s.sendall(json.dumps(payload).encode() + b"\n")
            buf = b""
            while b"\n" not in buf:
                chunk = s.recv(65536)
                if not chunk:
                    raise ConnectionError("compositor closed the connection mid-reply")
                buf += chunk
        finally:
            s.close()
        resp = json.loads(buf.split(b"\n", 1)[0])
        if "err" in resp:
            err = resp["err"]
            raise CompositorError(err.get("code", "unknown"), err.get("message", ""))
        return resp

    def answer(self, payload: dict) -> dict:
        """Call, then unwrap the `{"answer": ...}` envelope."""
        resp = self.call(payload)
        if "answer" not in resp:
            raise RuntimeError(f"unexpected response shape: {resp}")
        return resp["answer"]

    # --- the three exposed operations ---

    def enumerate_actions(self, query: str = "") -> dict:
        return self.answer({"op": "actions_enumerate", "query": query})

    def invoke_action(self, provider: str, id_: str, args: dict | None = None) -> dict:
        return self.answer(
            {"op": "actions_invoke", "provider": provider, "id": id_, "args": args or {}}
        )

    def action_status(self, invocation_id: int) -> dict:
        return self.answer({"op": "actions_status", "invocation_id": invocation_id})

    def cancel_action(self, invocation_id: int) -> dict:
        return self.answer({"op": "actions_cancel", "invocation_id": invocation_id})

    def state_dump(self) -> dict:
        resp = self.call({"op": "state_dump"})
        return resp.get("state", resp)


TOOLS = [
    {
        "name": "qos_enumerate_actions",
        "description": (
            "List the typed actions the running qOS desktop exports right now, through the "
            "same provider registry that backs the Ctrl+K command palette. READ-ONLY and "
            "side-effect-free by construction: the returned descriptors carry no invoke "
            "handle, so surveying what is possible can never perform it. Each descriptor is "
            "addressed by a stable (provider, id) pair rather than a screen position. Pass an "
            "empty query to list every provider's default set."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Filter text. Empty lists every provider's default set.",
                }
            },
            "required": [],
        },
    },
    {
        "name": "qos_invoke_action",
        "description": (
            "Invoke exactly one enumerated action by its stable (provider, id) address. The "
            "address is resolved against a freshly built catalogue first, so an id that is "
            "unknown, or hidden from you by the visibility gate, is refused with "
            "`unknown_action` rather than silently doing nothing. Addresses come from "
            "qos_enumerate_actions; do not guess them."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "provider": {"type": "string", "description": "Provider name from the descriptor."},
                "id": {"type": "string", "description": "Action id from the descriptor."},
                "args": {
                    "type": "object",
                    "description": (
                        "Typed argument object matching the descriptor's params/schema. "
                        "Use an empty object for a nullary action."
                    ),
                    "additionalProperties": True,
                },
            },
            "required": ["provider", "id"],
        },
    },
    {
        "name": "qos_action_status",
        "description": "Read the latest admitted/progress/terminal state and output for a broker invocation receipt id.",
        "inputSchema": {
            "type": "object",
            "properties": {"invocation_id": {"type": "integer", "minimum": 1}},
            "required": ["invocation_id"],
        },
    },
    {
        "name": "qos_cancel_action",
        "description": "Request cooperative cancellation of a cancellable broker invocation by receipt id.",
        "inputSchema": {
            "type": "object",
            "properties": {"invocation_id": {"type": "integer", "minimum": 1}},
            "required": ["invocation_id"],
        },
    },
    {
        "name": "qos_state_dump",
        "description": (
            "Return the live workspace tree and chrome state as JSON, so you can observe what "
            "an invocation actually changed. Read-only."
        ),
        "inputSchema": {"type": "object", "properties": {}, "required": []},
    },
]


def action_resource_uri(provider: str, id_: str) -> str:
    """Stable MCP address for one descriptor, with no screen coordinates."""
    return "qos://action/{}/{}".format(
        urllib.parse.quote(provider, safe=""), urllib.parse.quote(id_, safe="")
    )


def list_resources(comp: Compositor) -> dict:
    """Project the permission-filtered action catalogue as standard MCP resources."""
    actions = comp.enumerate_actions("").get("actions", [])
    return {
        "resources": [
            {
                "uri": action_resource_uri(str(action.get("provider", "")), str(action.get("id", ""))),
                "name": str(action.get("title") or action.get("id") or "qOS action"),
                "description": str(action.get("subtitle") or "Typed qOS action descriptor"),
                "mimeType": "application/vnd.qos.action+json",
            }
            for action in actions
            if action.get("provider") and action.get("id")
        ]
    }


def read_resource(comp: Compositor, uri: str) -> dict:
    """Read one descriptor after re-enumerating, so revoked actions fail closed."""
    parsed = urllib.parse.urlparse(uri)
    parts = [urllib.parse.unquote(part) for part in parsed.path.split("/") if part]
    if parsed.scheme != "qos" or parsed.netloc != "action" or len(parts) != 2:
        raise ValueError("unknown qOS resource URI")
    provider, id_ = parts
    actions = comp.enumerate_actions("").get("actions", [])
    descriptor = next(
        (
            action
            for action in actions
            if action.get("provider") == provider and action.get("id") == id_
        ),
        None,
    )
    if descriptor is None:
        raise CompositorError("unknown_action", "resource is unavailable or no longer permitted")
    return {
        "contents": [
            {
                "uri": uri,
                "mimeType": "application/vnd.qos.action+json",
                "text": json.dumps(descriptor, indent=2),
            }
        ]
    }


def call_tool(comp: Compositor, name: str, args: dict) -> dict:
    """Run one tool. Returns an MCP tools/call result.

    Every failure path surfaces as `isError: true` with the underlying code and
    message intact, so the model reads the compositor's own refusal rather than
    a smoothed-over summary.
    """
    try:
        if name == "qos_enumerate_actions":
            payload = comp.enumerate_actions(str(args.get("query", "")))
        elif name == "qos_invoke_action":
            provider, id_ = args.get("provider"), args.get("id")
            if not provider or not id_:
                return _err("qos_invoke_action requires both 'provider' and 'id'.")
            action_args = args.get("args", {})
            if not isinstance(action_args, dict):
                return _err("qos_invoke_action 'args' must be an object.")
            payload = comp.invoke_action(str(provider), str(id_), action_args)
        elif name in ("qos_action_status", "qos_cancel_action"):
            invocation_id = args.get("invocation_id")
            if not isinstance(invocation_id, int) or isinstance(invocation_id, bool) or invocation_id < 1:
                return _err(f"{name} requires a positive integer 'invocation_id'.")
            payload = (
                comp.action_status(invocation_id)
                if name == "qos_action_status"
                else comp.cancel_action(invocation_id)
            )
        elif name == "qos_state_dump":
            payload = comp.state_dump()
        else:
            return _err(f"unknown tool: {name}")
    except CompositorError as e:
        return _err(f"compositor refused the request: {e.code}: {e.message}")
    except (ConnectionError, OSError) as e:
        return _err(f"compositor unreachable: {e}")
    except Exception as e:  # noqa: BLE001 -- surface, never swallow
        return _err(f"{type(e).__name__}: {e}")
    return {"content": [{"type": "text", "text": json.dumps(payload, indent=2)}], "isError": False}


def _err(message: str) -> dict:
    return {"content": [{"type": "text", "text": message}], "isError": True}


def handle(comp: Compositor, req: dict) -> dict | None:
    """Dispatch one JSON-RPC request. Returns None for notifications."""
    method, req_id = req.get("method"), req.get("id")
    if method == "initialize":
        want = (req.get("params") or {}).get("protocolVersion")
        version = want if want in PROTOCOL_VERSIONS else PROTOCOL_VERSIONS[0]
        return _ok(req_id, {
            "protocolVersion": version,
            "capabilities": {
                "tools": {"listChanged": True},
                "resources": {"listChanged": True},
            },
            "serverInfo": SERVER_INFO,
        })
    if method in ("notifications/initialized", "notifications/cancelled"):
        return None
    if method == "ping":
        return _ok(req_id, {})
    if method == "tools/list":
        return _ok(req_id, {"tools": TOOLS})
    if method == "resources/list":
        try:
            return _ok(req_id, list_resources(comp))
        except (CompositorError, ConnectionError, OSError, ValueError) as e:
            return _rpc_error(req_id, -32002, str(e))
    if method == "resources/read":
        try:
            uri = str(((req.get("params") or {}).get("uri") or ""))
            return _ok(req_id, read_resource(comp, uri))
        except (CompositorError, ConnectionError, OSError, ValueError) as e:
            return _rpc_error(req_id, -32002, str(e))
    if method == "tools/call":
        params = req.get("params") or {}
        return _ok(req_id, call_tool(comp, params.get("name", ""), params.get("arguments") or {}))
    if req_id is None:
        return None
    return {"jsonrpc": "2.0", "id": req_id,
            "error": {"code": -32601, "message": f"method not found: {method}"}}


def _ok(req_id, result: dict) -> dict:
    return {"jsonrpc": "2.0", "id": req_id, "result": result}


def _rpc_error(req_id, code: int, message: str) -> dict:
    return {"jsonrpc": "2.0", "id": req_id, "error": {"code": code, "message": message}}


def serve(comp: Compositor) -> int:
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError as e:
            _emit({"jsonrpc": "2.0", "id": None,
                   "error": {"code": -32700, "message": f"parse error: {e}"}})
            continue
        resp = handle(comp, req)
        if resp is not None:
            _emit(resp)
    return 0


def _emit(obj: dict) -> None:
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def selftest(comp: Compositor) -> int:
    print(f"socket: {comp.socket_path}", file=sys.stderr)
    try:
        answer = comp.enumerate_actions("")
    except (CompositorError, ConnectionError, OSError) as e:
        print(f"FAIL: {e}", file=sys.stderr)
        return 1
    actions = answer.get("actions", [])
    print(f"OK: {answer.get('count')} action(s) enumerated", file=sys.stderr)
    for a in actions[:20]:
        print(f"  {a.get('provider')}/{a.get('id')}  [{a.get('kind')}]  {a.get('title')}",
              file=sys.stderr)
    if len(actions) > 20:
        print(f"  ... and {len(actions) - 20} more", file=sys.stderr)
    return 0


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--socket", default=os.environ.get("QOS_TEST_CONTROL", DEFAULT_SOCKET),
                   help="control socket path (default: $QOS_TEST_CONTROL, else %(default)s)")
    p.add_argument("--selftest", action="store_true",
                   help="connect, enumerate, print the result, exit; no model needed")
    args = p.parse_args()
    comp = Compositor(args.socket)
    return selftest(comp) if args.selftest else serve(comp)


if __name__ == "__main__":
    sys.exit(main())
