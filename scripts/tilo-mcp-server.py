#!/usr/bin/env python3
"""MCP stdio server exposing a running tilo compositor's typed action surface.

Bridges two protocols, both newline-delimited JSON:

  language model  <--JSON-RPC 2.0 over stdio-->  this shim
  this shim       <--TILO_TEST_CONTROL socket-->  compositor

The shim lets a model drive the OS *semantically*: it asks which actions
exist and invokes one by its stable `(provider, id)` address. Nothing here
screenshots, hit-tests or synthesises input.

MCP resources are deliberately lazy. `resources/list` exposes applications,
not the entire action catalogue. Reading an application exposes its current
objects with their full action descriptors nested beneath them. This mirrors
Tilo's application -> resource -> action ownership and keeps unrelated
applications' schemas out of the model's context. Every read rebuilds the
permission-filtered catalogue, so nesting does not turn a stale or revoked
descriptor into authority.

Error handling contract: this shim never invents an action, result or success.
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
SERVER_INFO = {"name": "tilo-actions", "version": "0.1.0"}


def default_socket() -> str | None:
    """`$TILO_TEST_CONTROL`, else `$XDG_RUNTIME_DIR/tilo-session/test-control.sock`.

    Never a shared directory: a well-known name under /tmp can be bound
    first by another account, which then receives every command sent.
    """
    explicit = os.environ.get("TILO_TEST_CONTROL")
    if explicit:
        return explicit
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    if not runtime or not os.path.isabs(runtime):
        return None
    return os.path.join(runtime, "tilo-session", "test-control.sock")


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
                "`TILO_TEST_CONTROL=<path> cargo run --bin shell -- --backend headless`, "
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
        """The whole listing, one bounded page per request. A listing that
        changes between pages is listed again from the start."""
        for _ in range(3):
            listing: dict = {}
            rows: list = []
            cursor = None
            try:
                while True:
                    request = {"op": "actions_enumerate", "query": query}
                    if cursor is not None:
                        request["cursor"] = cursor
                    listing = self.answer(request)
                    rows.extend(listing.get("actions", []))
                    cursor = listing.get("next_cursor")
                    if not cursor:
                        break
            except CompositorError as error:
                if error.code == "stale_cursor":
                    continue
                raise
            listing.update(actions=rows, count=len(rows), truncated=False)
            return listing
        raise CompositorError("stale_cursor", "the listing kept changing while it was paged")

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
        "name": "tilo_enumerate_actions",
        "description": (
            "List the typed actions the running Tilo Desktop exports right now, through the "
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
        "name": "tilo_invoke_action",
        "description": (
            "Invoke exactly one enumerated action by its stable (provider, id) address. The "
            "address is resolved against a freshly built catalogue first, so an id that is "
            "unknown, or hidden from you by the visibility gate, is refused with "
            "`unknown_action` rather than silently doing nothing. Addresses come from "
            "tilo_enumerate_actions; do not guess them."
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
        "name": "tilo_action_status",
        "description": "Read the latest admitted/progress/terminal state and output for a broker invocation receipt id.",
        "inputSchema": {
            "type": "object",
            "properties": {"invocation_id": {"type": "integer", "minimum": 1}},
            "required": ["invocation_id"],
        },
    },
    {
        "name": "tilo_cancel_action",
        "description": "Request cooperative cancellation of a cancellable broker invocation by receipt id.",
        "inputSchema": {
            "type": "object",
            "properties": {"invocation_id": {"type": "integer", "minimum": 1}},
            "required": ["invocation_id"],
        },
    },
    {
        "name": "tilo_state_dump",
        "description": (
            "Return the live workspace tree and chrome state as JSON, so you can observe what "
            "an invocation actually changed. Read-only."
        ),
        "inputSchema": {"type": "object", "properties": {}, "required": []},
    },
]


APPLICATION_MIME = "application/vnd.tilo.application+json"


def _segment(value: str) -> str:
    return urllib.parse.quote(value, safe="")


def _location(action: dict) -> tuple[str, tuple[str, str, str], str]:
    """Return application, object identity, operation for one descriptor.

    Canonical app actions receive explicit hierarchy fields from the shell.
    Palette-only providers do not own Tilo resource trees, so each of their
    entries remains one object with one invokable operation under the provider.
    That fallback preserves reach without pretending dot-separated display ids
    are an application resource protocol.
    """
    provider = str(action.get("provider") or "")
    id_ = str(action.get("id") or "")
    application = action.get("application")
    resource = action.get("resource")
    operation = action.get("operation")
    if all(isinstance(value, str) and value for value in (application, resource, operation)):
        return (
            application,
            (str(action.get("room") or ""), str(action.get("record") or ""), resource),
            operation,
        )
    return provider, ("", "", id_), "invoke"


def application_resource_uri(application: str) -> str:
    return f"tilo://application/{_segment(application)}"


def _catalogue(comp: Compositor) -> list[dict]:
    return [
        action
        for action in comp.enumerate_actions("").get("actions", [])
        if action.get("provider") and action.get("id")
    ]


def list_resources(comp: Compositor) -> dict:
    """List only application roots; descendants are revealed by reads."""
    applications: dict[str, int] = {}
    for action in _catalogue(comp):
        application, _object, _operation = _location(action)
        applications[application] = applications.get(application, 0) + 1
    return {
        "resources": [
            {
                "uri": application_resource_uri(application),
                "name": application,
                "description": (
                    f"Tilo application surface; read to list its objects "
                    f"({count} available action{'s' if count != 1 else ''})"
                ),
                "mimeType": APPLICATION_MIME,
            }
            for application, count in sorted(applications.items())
        ]
    }


def read_resource(comp: Compositor, uri: str) -> dict:
    """Read one lazy hierarchy node from a freshly filtered catalogue."""
    parsed = urllib.parse.urlparse(uri)
    parts = [urllib.parse.unquote(part) for part in parsed.path.split("/") if part]
    if parsed.scheme != "tilo" or parsed.netloc != "application" or len(parts) != 1:
        raise ValueError("unknown Tilo resource URI")
    application = parts[0]
    actions = [action for action in _catalogue(comp) if _location(action)[0] == application]
    if not actions:
        raise CompositorError(
            "unknown_application", "application is unavailable or no longer permitted"
        )

    objects: dict[tuple[str, str, str], list[dict]] = {}
    for action in actions:
        _application, object_key, operation = _location(action)
        descriptor = dict(action)
        descriptor.setdefault("operation", operation)
        objects.setdefault(object_key, []).append(descriptor)
    payload = {
        "type": "application",
        "application": application,
        "objects": [
            {
                "resource": object_key[2],
                "room": object_key[0] or None,
                "record": object_key[1] or None,
                "actions": object_actions,
            }
            for object_key, object_actions in sorted(objects.items())
        ],
    }
    return {
        "contents": [
            {
                "uri": uri,
                "mimeType": APPLICATION_MIME,
                "text": json.dumps(payload, indent=2),
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
        if name == "tilo_enumerate_actions":
            payload = comp.enumerate_actions(str(args.get("query", "")))
        elif name == "tilo_invoke_action":
            provider, id_ = args.get("provider"), args.get("id")
            if not provider or not id_:
                return _err("tilo_invoke_action requires both 'provider' and 'id'.")
            action_args = args.get("args", {})
            if not isinstance(action_args, dict):
                return _err("tilo_invoke_action 'args' must be an object.")
            payload = comp.invoke_action(str(provider), str(id_), action_args)
        elif name in ("tilo_action_status", "tilo_cancel_action"):
            invocation_id = args.get("invocation_id")
            if not isinstance(invocation_id, int) or isinstance(invocation_id, bool) or invocation_id < 1:
                return _err(f"{name} requires a positive integer 'invocation_id'.")
            payload = (
                comp.action_status(invocation_id)
                if name == "tilo_action_status"
                else comp.cancel_action(invocation_id)
            )
        elif name == "tilo_state_dump":
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
    p.add_argument("--socket", default=default_socket(),
                   help="control socket path (default: $TILO_TEST_CONTROL, else "
                        "$XDG_RUNTIME_DIR/tilo-session/test-control.sock)")
    p.add_argument("--selftest", action="store_true",
                   help="connect, enumerate, print the result, exit; no model needed")
    args = p.parse_args()
    if args.socket is None:
        p.error("no control socket: pass --socket or set TILO_TEST_CONTROL "
                "(XDG_RUNTIME_DIR is unset, so there is no private default)")
    comp = Compositor(args.socket)
    return selftest(comp) if args.selftest else serve(comp)


if __name__ == "__main__":
    sys.exit(main())
