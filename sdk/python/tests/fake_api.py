"""An in-process server that speaks Tilo's `/v1`.

It answers the routes `docs/developers/api.md` lists, with the JSON shapes the
API answers with, and records what it was asked so a test can assert what the
SDK sent. `httpx.MockTransport` hands it to a client, so nothing leaves the
process.
"""

from __future__ import annotations

import json
from typing import Any
from urllib.parse import unquote

import httpx

from tilo.types.pdf_viewer import Document

TODO = "notes/Todo"
PDF = "pdf-viewer/application.pdf"

WORKSPACE_ID = "11111111-2222-3333-4444-555555555555"
OTHER_ID = "66666666-7777-8888-9999-aaaaaaaaaaaa"
# A workspace another person made, which this person was invited into.
INVITED_ID = "abcdef01-2345-6789-abcd-ef0123456789"
# The surface the generated PDF Viewer types are written for: the app publishes
# this one until `tilo sdk gen` writes them again.
SURFACE = Document.__surface__


def link_to(path: str) -> str:
    """The link a record's host mints for the record `path` names."""
    record = path.split("#")[0].split("/")[1]
    return f"https://tilo.sh/app/#/@eu1.tilo.sh/w/{WORKSPACE_ID}/so.tilo.notes/{record}"


def frames(events: list[tuple[str, dict[str, Any]]], after: int = 0) -> list[bytes]:
    """`events` as server-sent event frames, the last one naming its cursor."""
    sent = []
    for index, (kind, data) in enumerate(events):
        text = ""
        if index == len(events) - 1:
            text += f"id: {after + len(events)}\n"
        text += f"event: {kind}\ndata: {json.dumps(data)}\n\n"
        sent.append(text.encode())
    return sent


class _Frames(httpx.AsyncByteStream):
    """Event frames as a response's body, one frame at a time."""

    def __init__(self, told: list[bytes]) -> None:
        self.told = list(told)
        self.next = 0

    def __aiter__(self) -> _Frames:
        return self

    async def __anext__(self) -> bytes:
        if self.next >= len(self.told):
            raise StopAsyncIteration
        frame = self.told[self.next]
        self.next += 1
        return frame


class FakeApi:
    """A host's API, answering `/v1` for one person's key."""

    def __init__(self) -> None:
        self.key = "tilo_sk_test"
        # `account` reaches every workspace, `workspace` only WORKSPACE_ID.
        self.scope = "account"
        # The person's own workspaces, then one they were invited into: each
        # with the part they take in it.
        self.workspaces = [
            {"id": WORKSPACE_ID, "name": "jane-doe", "host": "eu1", "role": "owner"},
            {"id": OTHER_ID, "name": "john-doe", "host": "eu1", "role": "owner"},
            {
                "id": INVITED_ID,
                "name": "applicant-jane-doe",
                "host": "eu1",
                "role": "editor",
            },
        ]
        self.fields: dict[str, dict[str, Any]] = {
            TODO: {"title": "Todo", "pinned": False},
            PDF: {
                "name": "application.pdf",
                "page_count": 3,
                "form_values": {"surname": "Doe", "given_name": "Jane"},
            },
        }
        self.kinds = {TODO: "notes.Page", PDF: "pdf-viewer.Document"}
        # Each app's published surface, by app id.
        self.surfaces = {"pdf-viewer": SURFACE}
        self.keys: list[dict[str, Any]] = [
            {
                "grant": "g-root",
                "name": "visa-backend",
                "scope": "account",
                "kind": "key",
                "created_at_ms": 1,
                "expires_at_ms": None,
                "browser_pages": True,
            }
        ]
        # Keys this API turns away, with its typed refusal of each.
        self.denied: dict[str, dict[str, Any]] = {
            "tilo_sk_revoked": {
                "code": "key_revoked",
                "message": "this key was revoked on 2026-11-09",
                "next_steps": ["ask the person for a new key"],
                "revoked_at_ms": 1_794_000_000_000,
            },
            "tilo_sk_expired": {
                "code": "key_expired",
                "message": "this key expired on 2026-11-09",
                "next_steps": ["ask the person for a new key, or mint one again"],
                "expired_at_ms": 1_794_000_000_000,
            },
            "tilo_sk_never": {
                "code": "invalid_token",
                "message": "this is not a key or token this API issued",
                "next_steps": [],
            },
        }
        # What a test drives.
        self.requests: list[dict[str, Any]] = []
        self.version = "1"
        # How many calls the host refuses as busy before answering them, and
        # how long it says to wait.
        self.busy = 0
        self.busy_status = 429
        self.busy_wait_ms = 5
        # Runs that have not finished: each with the polls it was asked for.
        self.polls: dict[str, int] = {}
        self.polls_until_done = 1
        self.finished: dict[str, dict[str, Any]] = {}
        # Runs still waiting for the person, by the run id an action answered.
        self.asks: dict[str, dict[str, Any]] = {}
        self.ask_answered = False
        self.append_pending = False
        self.cancelled: list[str] = []
        self.undone: list[str] = []
        # Files: the uploads received, and the bytes content names.
        self.uploads: list[bytes] = []
        self.contents: dict[str, bytes] = {}
        # What the workspace's stream tells, and how many it tells at a time.
        self.events: list[tuple[str, dict[str, Any]]] = []
        self.page: int | None = None
        self.output: list[tuple[str, dict[str, Any]]] = []
        # The device flow for a key: what was asked, and the key it answers.
        self.new_key_asked: dict[str, Any] = {}
        self.asked_key: dict[str, Any] | None = None
        self.polls_of_key = 0
        # How many times an action actually ran, by path.
        self.ran: dict[str, int] = {}

    # -- what a test drives ----------------------------------------------

    def transport(self) -> httpx.MockTransport:
        return httpx.MockTransport(self.handle)

    def client(self, url: str = "https://api.test") -> httpx.AsyncClient:
        return httpx.AsyncClient(transport=self.transport(), base_url=url)

    def asked(self, method: str, path: str) -> list[dict[str, Any]]:
        """The calls the SDK made of `method` whose path holds `path`."""
        return [
            call
            for call in self.requests
            if call["method"] == method and path in call["path"]
        ]

    def finish(self, run: str, result: Any = {"appended": "c01e"}) -> None:
        """Answer `run` as the next look at it would."""
        self.finished[run] = {
            "status": "ok",
            "run": run,
            "path": f"{TODO}#append-paragraph",
            "result": result,
            "link": link_to(TODO),
        }

    # -- HTTP ------------------------------------------------------------

    async def handle(self, request: httpx.Request) -> httpx.Response:
        body = await request.aread()
        call = {
            "method": request.method,
            "path": request.url.path,
            "raw_path": request.url.raw_path.decode(),
            "query": dict(request.url.params),
            "headers": {name.lower(): value for name, value in request.headers.items()},
            "body": body,
        }
        self.requests.append(call)
        if request.url.path in ("/keys/new", "/keys/poll"):
            return self.device(request, call)
        authorization = request.headers.get("authorization", "")
        if not authorization.startswith("Bearer "):
            return self.refuse(401, "invalid_token", "send a key: Authorization: Bearer tilo_sk_…")
        presented = authorization.removeprefix("Bearer ")
        if presented in self.denied:
            return httpx.Response(
                401, json={"error": self.denied[presented]}, headers=self.headers()
            )
        if presented != self.key:
            return self.refuse(401, "invalid_token", "this is not a key this API issued")
        if self.busy > 0:
            self.busy -= 1
            return self.refuse(
                self.busy_status,
                "rate_limited" if self.busy_status == 429 else "unavailable",
                "the host takes your changes more slowly; nothing ran",
                retry_after_ms=self.busy_wait_ms,
            )
        return self.route(request, call)

    def route(self, request: httpx.Request, call: dict[str, Any]) -> httpx.Response:
        path = call["path"]
        if path == "/v1":
            return self.answer(200, {"api_version": "1", "server": "fake-api"})
        if path == "/v1/account":
            return self.answer(
                200,
                {
                    "host": "api.test",
                    "standing": {
                        "plan": "free",
                        "account": "ab" * 32,
                        "usage": {
                            "workspaces": [
                                {
                                    "workspace": WORKSPACE_ID,
                                    "vm_seconds": 0,
                                    "running": False,
                                    "stored_bytes": 2048,
                                },
                                {"workspace": "other", "vm_seconds": 90, "running": True},
                            ]
                        },
                    },
                },
            )
        if path == "/v1/workspaces":
            if request.method == "GET":
                return self.answer(200, {"workspaces": self.reached()})
            return self.create_workspace(call)
        if path == "/v1/keys":
            if request.method == "GET":
                return self.answer(200, {"keys": self.keys})
            return self.mint_key(call)
        if path.startswith("/v1/keys/"):
            return self.revoke_key(unquote(path.removeprefix("/v1/keys/")))
        if not path.startswith("/v1/workspaces/"):
            return self.refuse(404, "not_found", "the API has no such route")
        rest = path.removeprefix("/v1/workspaces/")
        workspace, _, after = rest.partition("/")
        workspace = unquote(workspace)
        if not after:
            return self.delete_workspace(workspace)
        if workspace != WORKSPACE_ID and self.scope == "workspace":
            return self.refuse(403, "beyond_scope", "this key does not reach that workspace")
        if workspace not in [row["id"] for row in self.workspaces]:
            return self.refuse(404, "not_found", "no such workspace")
        return self.workspace_route(request, call, workspace, after)

    def workspace_route(
        self, request: httpx.Request, call: dict[str, Any], workspace: str, after: str
    ) -> httpx.Response:
        if after == "resources" and request.method == "GET":
            return self.listing(call)
        if after.startswith("resources/"):
            return self.resource_route(request, call, after.removeprefix("resources/"))
        if after == "search":
            return self.search(call)
        if after == "links":
            return self.link(call)
        if after == "members":
            return self.answer(
                200,
                {
                    "workspace": WORKSPACE_ID,
                    "members": [
                        {
                            "person": "cd" * 32,
                            "name": "Rosa Ortiz",
                            "role": "editor",
                            "administrator": False,
                        }
                    ],
                    "invitations": [],
                },
            )
        if after == "members/invitations":
            return self.invite(call)
        if after == "programs":
            return self.program(call)
        if after == "uploads":
            self.uploads.append(call["body"])
            content = f"tilo.content:{len(call['body'])}"
            self.contents[content] = call["body"]
            return self.answer(201, {"content": content, "size": len(call["body"])})
        if after.startswith("content/"):
            content = unquote(after.removeprefix("content/"))
            data = self.contents.get(content)
            if data is None:
                return self.refuse(404, "not_found", f"the workspace has no {content}")
            return httpx.Response(200, content=data, headers=self.headers())
        if after == "events":
            return self.stream(self.events, call)
        if after.startswith("runs/"):
            return self.run_route(request, after.removeprefix("runs/"))
        return self.refuse(404, "not_found", "the API has no such route")

    # -- what a test drives -------------------------------------------------

    def reached(self) -> list[dict[str, Any]]:
        """The workspaces the key reaches: its own, or the one it was made for."""
        if self.scope == "workspace":
            return [row for row in self.workspaces if row["id"] == WORKSPACE_ID]
        return self.workspaces

    def create_workspace(self, call: dict[str, Any]) -> httpx.Response:
        asked = json.loads(call["body"] or b"{}")
        if self.scope == "workspace":
            return self.refuse(403, "beyond_scope", "this key reaches one workspace")
        made = {
            "id": "99999999-8888-7777-6666-555555555555",
            "name": asked.get("name", ""),
            "host": "eu1",
            "role": "owner",
        }
        self.workspaces.append(made)
        self.ran["workspaces#create"] = self.ran.get("workspaces#create", 0) + 1
        return self.answer(201, made)

    def delete_workspace(self, workspace: str) -> httpx.Response:
        own = [
            row
            for row in self.workspaces
            if row["id"] == workspace and row["role"] == "owner"
        ]
        if not own:
            return self.refuse(
                404, "not_found", "no such workspace of yours", ["GET /v1/workspaces names them"]
            )
        self.workspaces = [row for row in self.workspaces if row["id"] != workspace]
        return self.answer(200, {"deleted": workspace, "revoked_keys": ["g-minted"]})

    def mint_key(self, call: dict[str, Any]) -> httpx.Response:
        asked = json.loads(call["body"] or b"{}")
        workspace = asked.get("workspace")
        if workspace is None:
            return self.refuse(400, "invalid_arguments", "`workspace` is required")
        if self.scope == "workspace":
            return self.refuse(403, "beyond_scope", "this key reaches one workspace")
        return self.answer(
            201,
            {
                "grant": "g-minted",
                "name": asked.get("name") or "minted",
                "token": "tilo_sk_minted",
                "workspace": workspace,
                "mcp_url": f"https://mcp.tilo.sh/w/{workspace}/mcp",
                "expires_at_ms": 9_000_000_000_000,
            },
        )

    def revoke_key(self, grant: str) -> httpx.Response:
        self.keys = [key for key in self.keys if key["grant"] != grant]
        return self.answer(200, {"revoked": grant})

    def listing(self, call: dict[str, Any]) -> httpx.Response:
        query = call["query"]
        path = query.get("path")
        if path is None:
            return self.answer(
                200,
                {
                    "path": "apps",
                    "rows": [
                        {"path": "notes", "name": "notes", "label": "Notes", "kind": "app", "state": "running", "has_children": True, "actions": []},
                        {"path": "pdf-viewer", "name": "pdf-viewer", "label": "PDF Viewer", "kind": "app", "state": "installed", "has_children": True, "actions": []},
                        {"path": "terminal", "name": "terminal", "label": "Terminal", "kind": "app", "state": "service", "has_children": True, "actions": []},
                    ],
                    "actions": [],
                },
            )
        if path == "notes":
            return self.answer(
                200,
                {
                    "path": "notes",
                    "kind": "notes",
                    "rows": [
                        {"path": TODO, "name": "Todo", "label": "Todo", "kind": "notes.Page", "has_children": True, "actions": []}
                    ],
                    "actions": [],
                },
            )
        rows = [
            {"path": f"{TODO}/c01e", "name": "Buy milk", "label": "Buy milk", "kind": "notes.Block", "has_children": False, "actions": [{"id": "toggle-checkbox", "label": "Done", "kind": "undoable", "params": [{"name": "id", "ty": "text", "required": True}]}]},
            {"path": f"{TODO}/c02f", "name": "Buy bread", "label": "Buy bread", "kind": "notes.Block", "has_children": False, "actions": []},
        ]
        limit = query.get("limit")
        cursor = query.get("cursor")
        if limit and not cursor:
            return self.answer(
                200,
                {
                    "path": path,
                    "kind": self.kinds.get(path, "notes.Page"),
                    "rows": rows[:1],
                    "actions": self.actions_of(path),
                    "next_cursor": "1",
                },
            )
        if cursor:
            return self.answer(
                200,
                {
                    "path": path,
                    "kind": self.kinds.get(path, "notes.Page"),
                    "rows": rows[1:],
                    "actions": self.actions_of(path),
                },
            )
        return self.answer(
            200,
            {
                "path": path,
                "kind": self.kinds.get(path, "notes.Page"),
                "rows": rows,
                "actions": self.actions_of(path),
            },
        )

    def actions_of(self, path: str) -> list[dict[str, Any]]:
        return [
            {"id": "append-paragraph", "label": "Add paragraph", "kind": "undoable", "params": [{"name": "text", "ty": "text", "required": True}]},
            {"id": "field-edit", "label": "Edit a field", "kind": "undoable", "params": [{"name": "edit", "ty": "object", "required": True}]},
        ]

    def resource_route(self, request: httpx.Request, call: dict[str, Any], rest: str) -> httpx.Response:
        if request.method == "GET":
            path = "/".join(unquote(part) for part in rest.split("/"))
            if path not in self.fields:
                return self.refuse(
                    404, "not_found", f"no resource {path}", ["GET /v1/workspaces/{workspace}/resources lists what is here"]
                )
            return self.answer(
                200,
                {
                    "path": path,
                    "kind": self.kinds.get(path, ""),
                    "fields": self.fields[path],
                    "children": [],
                },
            )
        resource, _, action = rest.rpartition("/actions/")
        path = "/".join(unquote(part) for part in resource.split("/"))
        action = unquote(action)
        return self.run_action(call, path, action)

    def run_action(self, call: dict[str, Any], path: str, action: str) -> httpx.Response:
        at = f"{path}#{action}"
        surface = call["headers"].get("tilo-surface")
        app = path.split("/")[0]
        if surface is not None and surface != self.surfaces.get(app):
            return self.refuse(
                409,
                "surface_changed",
                f"these types were made for {surface}; the workspace's app publishes {self.surfaces.get(app)}",
                [f"write the types again: tilo sdk gen -w {WORKSPACE_ID}"],
            )
        args = json.loads(call["body"] or b"{}")
        if action == "field-edit":
            self.apply_edit(path, args.get("edit") or {})
            self.ran[at] = self.ran.get(at, 0) + 1
            return self.done(at, {"edited": True}, [path], link_to(path))
        if at == f"{TODO}#append-paragraph":
            self.ran[at] = self.ran.get(at, 0) + 1
            if self.append_pending:
                return self.pending(at)
            return self.done(at, {"appended": "c01e"}, [TODO], link_to(TODO))
        if at == "word-processor#export-content":
            letter = b"PK cover letter"
            content = f"tilo.content:{len(letter)}"
            self.contents[content] = letter
            return self.done(at, None, [content])
        if at == f"{PDF}#import-pdf-content":
            return self.done(at, {"imported": args.get("content")})
        if at == f"{PDF}#read-form-fields":
            return self.done(at, {"fields": list(self.fields[PDF]["form_values"])})
        if at == f"{PDF}#history":
            return self.done(at, {"versions": []})
        if at == f"{TODO}#undo":
            return self.done(at, {"undone": 1})
        if at == f"{TODO}#ask":
            run = f"ask-{len(self.asks) + 1}"
            self.asks[run] = {"path": at}
            return httpx.Response(
                202,
                json={
                    "status": "pending",
                    "run": run,
                    "ask": {"url": "https://tilo.sh/ask/1"},
                },
                headers=self.headers(location=f"/v1/workspaces/{WORKSPACE_ID}/runs/{run}"),
            )
        if at == f"{TODO}#remove-me":
            return self.done(at, None)
        if at == "pdf-viewer#open":
            return self.done(at, None)
        return self.refuse(
            422,
            "invalid_arguments",
            f"no action {at}",
            [f"GET /v1/workspaces/{WORKSPACE_ID}/resources?path={path} lists its actions"],
        )

    def apply_edit(self, resource: str, edit: dict[str, Any]) -> None:
        steps = list(edit.get("path") or [])
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

    def done(
        self,
        path: str,
        result: Any,
        resources: list[str] | None = None,
        link: str | None = None,
    ) -> httpx.Response:
        answer: dict[str, Any] = {
            "status": "ok",
            "run": f"run-{len(self.requests)}",
            "path": path,
            "resources": resources or [],
        }
        if result is not None:
            answer["result"] = result
        if link is not None:
            answer["link"] = link
        return self.answer(200, answer)

    def pending(self, path: str) -> httpx.Response:
        run = f"run-{len(self.requests)}"
        self.polls[run] = 0
        return httpx.Response(
            202,
            json={"status": "pending", "run": run},
            headers=self.headers(location=f"/v1/workspaces/{WORKSPACE_ID}/runs/{run}"),
        )

    def run_route(self, request: httpx.Request, rest: str) -> httpx.Response:
        run, _, after = rest.partition("/")
        if after == "cancel":
            self.cancelled.append(run)
            self.finished[run] = {"status": "cancelled", "run": run, "path": f"{TODO}#append-paragraph"}
            return self.answer(200, self.finished[run])
        if after == "undo":
            self.undone.append(run)
            return self.answer(200, {"status": "ok", "undone": [TODO]})
        if after == "output":
            return self.stream(self.output_for(run), None)
        if after:
            return self.refuse(404, "not_found", "the API has no such route")
        finished = self.finished.get(run)
        if finished is not None:
            return self.answer(200, finished)
        if run in self.asks:
            if not self.ask_answered:
                return self.answer(202, {"status": "pending", "run": run, "ask": {"url": "https://tilo.sh/ask/1"}})
            return self.answer(
                200,
                {"status": "ok", "run": run, "path": f"{TODO}#ask", "result": {"allowed": True}},
            )
        if run in self.polls:
            self.polls[run] += 1
            if self.polls[run] >= self.polls_until_done:
                self.finish(run)
                return self.answer(200, self.finished[run])
            return self.answer(202, {"status": "pending", "run": run})
        return self.refuse(404, "not_found", "no such run")

    def output_for(self, run: str) -> list[tuple[str, dict[str, Any]]]:
        if self.output:
            return self.output
        return [
            ("run", {"phase": "started", "run": run, "path": f"{TODO}#append-paragraph", "title": "Fill Jane's application", "time_ms": 1}),
            ("run", {"phase": "succeeded", "run": run, "path": f"{TODO}#append-paragraph", "title": "Fill Jane's application", "time_ms": 2}),
        ]

    def search(self, call: dict[str, Any]) -> httpx.Response:
        query = call["query"]
        if not query.get("q"):
            return self.refuse(400, "invalid_arguments", "`q` is required")
        return self.answer(
            200,
            {
                "query": query["q"],
                "hits": [
                    {"path": TODO, "kind": "notes.Page", "label": "Todo"},
                    {"path": f"{TODO}#append-paragraph", "kind": "notes.Page", "label": "Add paragraph", "action": "append-paragraph"},
                ],
            },
        )

    def link(self, call: dict[str, Any]) -> httpx.Response:
        path = call["query"].get("path")
        if path is None:
            return self.refuse(400, "invalid_arguments", "`path` is required")
        if path not in self.fields:
            return self.refuse(404, "not_found", f"{path} names nothing", ["GET /v1/workspaces/…/search finds it"])
        return self.answer(200, {"path": path, "link": link_to(path)})

    def invite(self, call: dict[str, Any]) -> httpx.Response:
        asked = json.loads(call["body"] or b"{}")
        if asked.get("role") not in ("viewer", "commenter", "editor"):
            return self.refuse(
                422, "invalid_arguments", "`role` is viewer, commenter or editor", ["send {role: viewer}"]
            )
        invited: dict[str, Any] = {
            "workspace": WORKSPACE_ID,
            "invite": "ab" * 32,
            "role": asked["role"],
            "link": "https://tilo.sh/i/AAAAAAAAAAAAAAAA#key",
            "expires_at_ns": 7,
        }
        if "email" in asked:
            invited |= {"email": asked["email"], "sent": False, "next": "send the link yourself"}
        return self.answer(201, invited)

    def program(self, call: dict[str, Any]) -> httpx.Response:
        asked = json.loads(call["body"] or b"{}")
        source = asked.get("program", "")
        if source == "boom":
            return self.answer(
                200,
                {"status": "failed", "output": ["unknown name `boom`"], "changed": 0, "steps": []},
            )
        if source == "preview me":
            return self.answer(
                200,
                {"status": "preview", "output": ["4 changes in notes"], "changed": 4, "steps": []},
            )
        if source == "undo task":
            return self.answer(
                200,
                {"status": "ok", "output": ["undid 1 step"], "changed": 1, "steps": []},
            )
        return self.answer(
            200,
            {
                "status": "ok",
                "output": ['n1 "Q3 plan"', "1 change in notes"],
                "changed": 1,
                "steps": [{"number": 1, "apps": ["notes"], "changed": 1}],
            },
        )

    def device(self, request: httpx.Request, call: dict[str, Any]) -> httpx.Response:
        if call["path"] == "/keys/new":
            self.new_key_asked = json.loads(call["body"] or b"{}")
            return self.answer(
                200,
                {
                    "approve_url": "https://mcp.tilo.sh/keys/approve/abc",
                    "poll": "poll-secret",
                    "interval": 0,
                    "expires_in": 300,
                },
            )
        self.polls_of_key += 1
        if self.asked_key is None:
            return self.answer(202, {})
        return self.answer(200, self.asked_key)

    # -- answers ---------------------------------------------------------

    def headers(self, **extra: str) -> dict[str, str]:
        return {"tilo-api-version": self.version, **extra}

    def answer(self, status: int, body: dict[str, Any]) -> httpx.Response:
        return httpx.Response(status, json=body, headers=self.headers())

    def refuse(
        self,
        status: int,
        code: str,
        message: str,
        next_steps: list[str] | None = None,
        retry_after_ms: int | None = None,
    ) -> httpx.Response:
        error: dict[str, Any] = {"code": code, "message": message}
        if next_steps:
            error["next_steps"] = list(next_steps)
        if retry_after_ms is not None:
            error["retry_after_ms"] = retry_after_ms
        return httpx.Response(status, json={"error": error}, headers=self.headers())

    def stream(
        self, told: list[tuple[str, dict[str, Any]]], call: dict[str, Any] | None
    ) -> httpx.Response:
        told = list(told)
        after = 0
        if call is not None:
            # Resuming after the cursor, one page at a time when a test says so.
            after = int(call["headers"].get("last-event-id") or 0)
            told = told[after:]
            if self.page is not None:
                told = told[: self.page]
        return httpx.Response(
            200,
            stream=_Frames(frames(told, after)),
            headers={"content-type": "text/event-stream", **self.headers()},
        )
