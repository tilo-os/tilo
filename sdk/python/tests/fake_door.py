"""An in-process door that speaks the SDK's wire.

It takes MCP over POST with sessions, typed `_meta`, the account resource,
content upload and the key endpoints, and it records what was asked so a
test can assert what the SDK sent.
"""

from __future__ import annotations

import base64
import json
from typing import Any

import httpx

SURFACE = "so.tilo.mcp"

TODO = "notes/Todo"
PDF = "pdf-viewer/application.pdf"

WORKSPACE_ID = "11111111-2222-3333-4444-555555555555"
OTHER_ID = "66666666-7777-8888-9999-aaaaaaaaaaaa"


class FakeDoor:
    def __init__(self) -> None:
        self.key = "tilo_sk_test"
        self.scope = "account"
        self.workspaces = [
            {"id": WORKSPACE_ID, "name": "jane-doe", "host": "eu1"},
            {"id": OTHER_ID, "name": "john-doe", "host": "eu1"},
        ]
        self.hosts = [{"name": "eu1", "default": True}]
        self.fields: dict[str, dict[str, Any]] = {
            TODO: {"title": "Todo", "pinned": False},
            PDF: {
                "name": "application.pdf",
                "page_count": 3,
                "form_values": {"surname": "Doe", "given_name": "Jane"},
            },
        }
        self.kinds = {TODO: "notes.Page", PDF: "pdf-viewer.Document"}
        self.sessions: set[str] = set()
        self.opened = 0
        self.calls: list[dict[str, Any]] = []
        self.edits: list[dict[str, Any]] = []
        self.uploads: list[bytes] = []
        self.contents: dict[str, bytes] = {}
        # The most bytes one tilo_get answers, small so a test sees paging.
        self.piece = 4
        self.upload_sessions: list[str | None] = []
        self.keys: list[dict[str, Any]] = [
            {
                "grant": "g-root",
                "name": "visa-backend",
                "scope": "account",
                "kind": "key",
                "created_at_ms": 1,
                "expires_at_ms": 2,
            }
        ]
        self.asks: dict[str, dict[str, Any]] = {}
        self.polls = 0
        self.stream = False
        # A path mapped to "pending" answers pending until `finish` says so.
        self.polled: dict[str, int] = {}
        self.events: list[dict[str, Any]] = []

    # -- what a test drives ----------------------------------------------

    def transport(self) -> httpx.MockTransport:
        return httpx.MockTransport(self.handle)

    def client(self, door: str = "https://door.test") -> httpx.AsyncClient:
        return httpx.AsyncClient(transport=self.transport(), base_url=door)

    def finish(self, invocation: str, result: Any = {"done": True}) -> None:
        self.finished = getattr(self, "finished", {})
        self.finished[invocation] = result

    def answer_poll(self, key: str = "tilo_sk_new", grant: str = "g-new") -> None:
        self.asked_key = {"key": key, "grant": grant, "name": "cli", "scope": "account"}

    def asked(self, tool: str) -> list[dict[str, Any]]:
        return [call for call in self.calls if call["tool"] == tool]

    # -- HTTP ------------------------------------------------------------

    async def handle(self, request: httpx.Request) -> httpx.Response:
        path = request.url.path
        if request.method == "PUT" and path.endswith("/content"):
            return self.upload(request)
        if path == "/keys/new":
            return self.new_key(request)
        if path == "/keys/poll":
            return self.poll_key(request)
        return self.mcp(request)

    def upload(self, request: httpx.Request) -> httpx.Response:
        if not request.headers.get("authorization"):
            return self.refuse(401, "unauthorized", "send a key")
        self.uploads.append(request.content)
        self.contents[f"tilo.content:{len(request.content)}"] = request.content
        self.upload_sessions.append(request.headers.get("mcp-session-id"))
        return httpx.Response(
            201, json={"content": f"tilo.content:{len(request.content)}", "size": len(request.content)}
        )

    def new_key(self, request: httpx.Request) -> httpx.Response:
        asked = json.loads(request.content or b"{}")
        self.new_key_asked = asked
        return httpx.Response(
            200,
            json={
                "approve_url": "https://mcp.tilo.sh/keys/approve/abc",
                "poll": "poll-secret",
                "interval": 0,
                "expires_in": 300,
            },
        )

    def poll_key(self, request: httpx.Request) -> httpx.Response:
        self.polls += 1
        answer = getattr(self, "asked_key", None)
        if answer is None:
            return httpx.Response(202, json={})
        return httpx.Response(200, json=answer)

    def mcp(self, request: httpx.Request) -> httpx.Response:
        path = request.url.path
        workspace: str | None = None
        if path.startswith("/w/"):
            workspace = path.split("/")[2]
            if not request.url.path.endswith("/mcp"):
                return httpx.Response(404, text="no such endpoint")
        elif path not in ("/mcp", "/mcp/"):
            return httpx.Response(404, text="no such endpoint")
        authorization = request.headers.get("authorization", "")
        if not authorization.startswith("Bearer "):
            return httpx.Response(401, text="sign in to use Tilo")
        if authorization.removeprefix("Bearer ") != self.key:
            return httpx.Response(401, text="sign in to use Tilo")
        if workspace is not None and self.scope == "workspace" and workspace != WORKSPACE_ID:
            return httpx.Response(403, text="this grant does not reach that workspace")
        if request.method == "DELETE":
            self.sessions.discard(request.headers.get("mcp-session-id", ""))
            return httpx.Response(204)
        body = json.loads(request.content or b"{}")
        method = body.get("method")
        if method == "initialize":
            self.opened += 1
            session = f"session-{self.opened}"
            self.sessions.add(session)
            answer = {
                "jsonrpc": "2.0",
                "id": body.get("id"),
                "result": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {"listChanged": True}},
                    "serverInfo": {"name": "tilo", "version": "0"},
                },
            }
            return httpx.Response(200, json=answer, headers={"Mcp-Session-Id": session})
        if body.get("id") is None:
            return httpx.Response(202)
        if request.headers.get("mcp-session-id") not in self.sessions:
            return httpx.Response(404, text="unknown session; initialize again")
        if method != "tools/call":
            return httpx.Response(
                200,
                json={
                    "jsonrpc": "2.0",
                    "id": body.get("id"),
                    "error": {"code": -32601, "message": "Method not found"},
                },
            )
        params = body.get("params") or {}
        surface = (params.get("_meta") or {}).get(SURFACE) or {}
        self.calls.append(
            {
                "tool": params.get("name"),
                "arguments": params.get("arguments") or {},
                "run": surface.get("run"),
                "structured": surface.get("structured"),
                "workspace": workspace,
            }
        )
        answer = self.tool(
            params.get("name", ""), params.get("arguments") or {}, workspace
        )
        result = {"content": [{"type": "text", "text": json.dumps(answer)}]}
        if answer is None:
            result = {"content": [{"type": "text", "text": "not a typed answer"}], "isError": True}
        else:
            result["structuredContent"] = answer
            result["isError"] = answer.get("status") == "failed"
        messages = [{"jsonrpc": "2.0", "id": body.get("id"), "result": result}]
        if self.stream:
            messages.insert(0, {"jsonrpc": "2.0", "method": "notifications/tools/list_changed"})
            body_text = "".join(
                f"event: message\ndata: {json.dumps(message)}\n\n" for message in messages
            )
            return httpx.Response(
                200,
                text=body_text,
                headers={"content-type": "text/event-stream"},
            )
        return httpx.Response(200, json=messages[-1])

    # -- the tools -------------------------------------------------------

    def tool(self, name: str, arguments: dict[str, Any], workspace: str | None) -> Any:
        if workspace is None:
            return self.account(name, arguments)
        if name == "tilo_ls":
            return self.ls(arguments)
        if name == "tilo_read":
            return self.read(arguments)
        if name == "tilo_find":
            return self.find(arguments)
        if name == "tilo_do":
            return self.do(arguments)
        if name == "tilo_status":
            return self.status(arguments)
        if name == "tilo_watch":
            return self.watch(arguments)
        if name == "tilo":
            return self.program(arguments)
        if name == "tilo_get":
            return self.get(arguments)
        return None

    def get(self, arguments: dict[str, Any]) -> Any:
        content = arguments.get("content", "")
        data = self.contents.get(content)
        if data is None:
            return self.failed(
                "", "not_found", f"the workspace has no {content}", "upload it first"
            )
        offset = int(arguments.get("offset") or 0)
        piece = data[offset : offset + self.piece]
        return {
            "content": content,
            "size": len(data),
            "offset": offset,
            "length": len(piece),
            "bytes": base64.b64encode(piece).decode(),
        }

    def account(self, name: str, arguments: dict[str, Any]) -> Any:
        if name == "tilo_ls":
            path = arguments.get("path") or ""
            return {
                "path": path or "account",
                "kind": "account.Account",
                "rows": [
                    {"path": "account/workspaces", "name": "workspaces", "label": "Workspaces", "kind": "account.Workspaces", "has_children": True, "actions": []},
                    {"path": "account/keys", "name": "keys", "label": "Keys", "kind": "account.Keys", "has_children": True, "actions": []},
                ],
                "actions": [],
            }
        if name == "tilo_do":
            return self.account_do(arguments.get("path", ""), arguments.get("args") or {})
        if name == "tilo_status":
            return self.account_status(arguments.get("invocation_id", ""))
        return None

    def account_do(self, path: str, args: dict[str, Any]) -> Any:
        if path == "account/workspaces#list":
            return {
                "status": "ok",
                "path": path,
                "result": {"workspaces": self.workspaces, "hosts": self.hosts},
            }
        if path == "account/workspaces#create":
            created = {
                "id": "99999999-8888-7777-6666-555555555555",
                "name": args.get("name", ""),
                "host": args.get("host") or "eu1",
            }
            self.workspaces.append(created)
            return {"status": "ok", "path": path, "result": {"workspace": created}}
        if path == "account/workspaces#delete":
            return self.failed(
                path,
                "not_supported",
                "the host cannot delete a workspace yet",
                "revoke the keys that reach it with account/keys#revoke",
            )
        if path == "account/keys#list":
            return {"status": "ok", "path": path, "result": {"keys": self.keys}}
        if path == "account/keys#create":
            invocation = "key-abc"
            self.asks[invocation] = {"status": "waiting"}
            return {
                "status": "pending",
                "path": path,
                "invocation_id": invocation,
                "ask": {
                    "invocation_id": invocation,
                    "url": "https://mcp.tilo.sh/keys/approve/abc",
                },
            }
        if path == "account/keys#mint":
            return {
                "status": "ok",
                "path": path,
                "result": {
                    "grant": "g-minted",
                    "name": args.get("name") or "minted",
                    "token": "tilo_sk_minted",
                    "workspace": args.get("workspace"),
                    "mcp_url": f"https://mcp.tilo.sh/w/{args.get('workspace')}/mcp",
                    "expires_at_ms": 9_000_000_000_000,
                },
            }
        if path == "account/keys#revoke":
            return {
                "status": "ok",
                "path": path,
                "result": {"revoked": args.get("grant")},
            }
        return self.failed(path, "not_found", f"the account has no action {path}", "tilo_ls account lists its actions")

    def account_status(self, invocation: str) -> Any:
        asked = self.asks.get(invocation)
        if asked is None:
            return self.failed(
                "account/keys#create",
                "expired",
                "nothing waits under this id",
                "ask again with account/keys#create",
            )
        if asked.get("status") == "waiting":
            return {
                "status": "pending",
                "path": "account/keys#create",
                "invocation_id": invocation,
                "ask": {"invocation_id": invocation, "url": "https://mcp.tilo.sh/keys/approve/abc"},
            }
        return {
            "status": "ok",
            "path": "account/keys#create",
            "result": {
                "key": "tilo_sk_created",
                "grant": "g-created",
                "name": "cli",
                "scope": "account",
                "expires_at_ms": 9_000_000_000_000,
            },
        }

    def ls(self, arguments: dict[str, Any]) -> Any:
        path = arguments.get("path")
        if path is None:
            return {
                "path": "apps",
                "rows": [
                    {"path": "notes", "name": "notes", "label": "Notes", "kind": "app", "state": "running", "has_children": True, "actions": []},
                    {"path": "pdf-viewer", "name": "pdf-viewer", "label": "PDF Viewer", "kind": "app", "state": "installed", "has_children": True, "actions": []},
                    {"path": "terminal", "name": "terminal", "label": "Terminal", "kind": "app", "state": "service", "has_children": True, "actions": []},
                ],
                "actions": [],
            }
        if path == "notes":
            return {
                "path": "notes",
                "kind": "notes",
                "rows": [
                    {"path": TODO, "name": "Todo", "label": "Todo", "kind": "notes.Page", "has_children": True, "actions": []}
                ],
                "actions": [],
            }
        rows = [
            {"path": f"{TODO}/c01e", "name": "Buy milk", "label": "Buy milk", "kind": "notes.Block", "has_children": False, "actions": [{"id": "toggle-checkbox", "label": "Done", "kind": "undoable", "params": [{"name": "id", "ty": "Text", "required": True}]}]},
            {"path": f"{TODO}/c02f", "name": "Buy bread", "label": "Buy bread", "kind": "notes.Block", "has_children": False, "actions": []},
        ]
        limit = arguments.get("limit")
        cursor = arguments.get("cursor")
        if limit and not cursor:
            return {
                "path": path,
                "kind": self.kinds.get(path, "notes.Page"),
                "rows": rows[:1],
                "actions": self.actions_of(path),
                "next_cursor": "1",
            }
        if cursor:
            return {
                "path": path,
                "kind": self.kinds.get(path, "notes.Page"),
                "rows": rows[1:],
                "actions": self.actions_of(path),
            }
        return {
            "path": path,
            "kind": self.kinds.get(path, "notes.Page"),
            "rows": rows,
            "actions": self.actions_of(path),
        }

    def actions_of(self, path: str) -> list[dict[str, Any]]:
        return [
            {"id": "append-paragraph", "label": "Add paragraph", "kind": "undoable", "params": [{"name": "text", "ty": "Text", "required": True}]},
            {"id": "field-edit", "label": "Edit a field", "kind": "undoable", "params": [{"name": "edit", "ty": "Object", "required": True}]},
        ]

    def read(self, arguments: dict[str, Any]) -> Any:
        path = arguments.get("path", "")
        if path not in self.fields:
            return self.failed(path, "not_found", f"no resource {path}", "tilo_ls lists what is here")
        return {
            "path": path,
            "kind": self.kinds.get(path, ""),
            "fields": self.fields[path],
            "children": [],
        }

    def find(self, arguments: dict[str, Any]) -> Any:
        return {
            "query": arguments.get("query", ""),
            "hits": [
                {"path": TODO, "kind": "notes.Page", "label": "Todo", "action": None},
                {"path": f"{TODO}#append-paragraph", "kind": "notes.Page", "label": "Add paragraph", "action": "append-paragraph"},
            ],
        }

    def do(self, arguments: dict[str, Any]) -> Any:
        path = arguments.get("path", "")
        args = arguments.get("args") or {}
        invocation = f"inv-{len(self.calls)}"
        if path.endswith("#field-edit"):
            edit = args.get("edit") or {}
            resource = path.split("#")[0]
            self.edits.append({"resource": resource, "edit": edit})
            self.apply_edit(resource, edit)
            return {"status": "ok", "path": path, "invocation_id": invocation, "result": {"edited": True}, "resources": [resource]}
        if path.endswith("#undo"):
            return {"status": "ok", "path": path, "invocation_id": invocation, "result": {"undone": args.get("steps", 1)}}
        if path == f"{TODO}#append-paragraph":
            if getattr(self, "append_pending", False):
                return {"status": "pending", "path": path, "invocation_id": invocation}
            return {"status": "ok", "path": path, "invocation_id": invocation, "result": {"appended": "c01e"}, "resources": [TODO]}
        if path == "word-processor#export-content":
            letter = b"PK cover letter"
            content = f"tilo.content:{len(letter)}"
            self.contents[content] = letter
            return {"status": "ok", "path": path, "invocation_id": invocation, "resources": [content]}
        if path == f"{PDF}#import-pdf-content":
            return {"status": "ok", "path": path, "invocation_id": invocation, "result": {"imported": args.get("content")}}
        if path == f"{PDF}#read-form-fields":
            return {"status": "ok", "path": path, "invocation_id": invocation, "result": {"fields": list(self.fields[PDF]["form_values"])}}
        if path == f"{TODO}#ask":
            return {
                "status": "pending",
                "path": path,
                "invocation_id": "ask-1",
                "ask": {"invocation_id": "ask-1", "url": "https://mcp.tilo.sh/ask/1"},
            }
        if path.endswith("#remove-me"):
            return {"status": "ok", "path": path, "invocation_id": invocation, "result": None}
        return self.failed(path, "invalid_arguments", f"no action {path}", f"tilo_ls {path.split('#')[0]} lists its actions")

    def apply_edit(self, resource: str, edit: dict[str, Any]) -> None:
        steps = [step for step in edit.get("path") or []]
        if not steps or not isinstance(steps[0], dict):
            return
        field = steps[0].get("field")
        if field is None:
            return
        fields = self.fields.setdefault(resource, {})
        if len(steps) == 1:
            if edit.get("op") == "set":
                fields[field] = edit.get("value")
            return
        entry = fields.get(field)
        key = steps[1].get("key")
        if not isinstance(entry, dict) or key is None:
            return
        if edit.get("op") == "remove-key":
            entry.pop(key, None)
        else:
            entry[key] = edit.get("value")

    def status(self, arguments: dict[str, Any]) -> Any:
        invocation = arguments.get("invocation_id", "")
        if arguments.get("cancel"):
            return {"status": "ok", "path": "notes/Todo#append-paragraph", "result": {"cancelled": invocation}}
        if invocation == "ask-1":
            questions = getattr(self, "ask_answered", False)
            if not questions:
                return {
                    "status": "pending",
                    "path": "notes/Todo#ask",
                    "invocation_id": invocation,
                    "ask": {"invocation_id": invocation, "url": "https://mcp.tilo.sh/ask/1"},
                }
            return {"status": "ok", "path": "notes/Todo#ask", "invocation_id": invocation, "result": {"allowed": True}}
        finished = getattr(self, "finished", {})
        if invocation in finished:
            return {
                "status": "ok",
                "path": "notes/Todo#append-paragraph",
                "invocation_id": invocation,
                "result": finished[invocation],
            }
        self.polled[invocation] = self.polled.get(invocation, 0) + 1
        if self.polled[invocation] >= getattr(self, "polls_until_done", 1):
            return {
                "status": "ok",
                "path": "notes/Todo#append-paragraph",
                "invocation_id": invocation,
                "result": {"appended": "c01e"},
            }
        return {"status": "pending", "path": "notes/Todo#append-paragraph", "invocation_id": invocation}

    def watch(self, arguments: dict[str, Any]) -> Any:
        cursor = int(arguments.get("cursor") or 0)
        page = getattr(self, "watch_page", None)
        events = self.events[cursor:]
        if page is not None:
            events = events[:page]
        return {"events": events, "cursor": str(cursor + len(events))}

    def program(self, arguments: dict[str, Any]) -> Any:
        program = arguments.get("program", "")
        if program == "boom":
            return {"status": "failed", "output": [], "changed": 0, "error": {"code": "compile_error", "message": "unknown name `boom`", "next_steps": ["tilo_ls notes names its resources"]}}
        if program == "preview me":
            return {"status": "preview", "output": ["4 changes in notes"], "changed": 4, "steps": []}
        return {
            "status": "ok",
            "output": ['n1 "Q3 plan"', "1 change in notes"],
            "changed": 1,
            "steps": [{"number": 1, "apps": ["notes"], "changed": 1}],
        }

    def failed(self, path: str, code: str, message: str, next_step: str) -> Any:
        return {
            "status": "failed",
            "path": path,
            "error": {"code": code, "message": message, "next_steps": [next_step]},
        }

    def refuse(self, status: int, code: str, message: str) -> httpx.Response:
        return httpx.Response(
            status, json={"error": {"code": code, "message": message, "next_steps": []}}
        )
