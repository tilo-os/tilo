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

import blake3
import httpx

from openapi_contract import Contract
from tilo.types.pdf_viewer import Document
from tilo.types.tasks import Tracker

contract = Contract()

TODO = "notes/Todo"
PDF = "pdf-viewer/application.pdf"
TASKS = "tasks/Tracker"

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
            TASKS: {"issues": {}},
        }
        self.kinds = {
            TODO: "notes.Page",
            PDF: "pdf-viewer.Document",
            TASKS: "tasks.tracker",
        }
        # Each app's published surface, by app id.
        self.surfaces = {"pdf-viewer": SURFACE, "tasks": Tracker.__surface__}
        # The bulk calls the SDK made: each resource, and the members it sent.
        self.groups: list[dict[str, Any]] = []
        # The keys each workspace admitted, as its governance keeps their grants.
        self.keys: list[dict[str, Any]] = [
            {
                "id": "g-root",
                "person": "ab" * 32,
                "device": "cd" * 32,
                "name": "visa-backend",
                "created_ms": 1,
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
        # How many calls the server says it is briefly unavailable for before
        # answering them.
        self.busy = 0
        # Runs that have not finished: each with the polls it was asked for.
        self.polls: dict[str, int] = {}
        self.polls_until_done = 1
        self.finished: dict[str, dict[str, Any]] = {}
        # Invocations still waiting for the person, by the id an action answered.
        self.asks: dict[str, dict[str, Any]] = {}
        self.ask_answered = False
        self.append_pending = False
        # Whether a bulk call is answered later, as a workspace that asks its person first does.
        self.group_pending = False
        self.cancelled: list[str] = []
        self.undone: list[str] = []
        # Every invocation the workspace lists, oldest first, as its host keeps them.
        self.invocations: list[dict[str, Any]] = []
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
        # The branches made, by the idempotency key each was asked with.
        self.branches: dict[str, dict[str, Any]] = {}
        # Whether the person answers a workspace's ask to invocation commands
        # unattended: `None` while they have not, then whether they allowed it.
        self.unattended_allowed: bool | None = None

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

    def finish(self, invocation: str, result: Any = {"appended": "c01e"}) -> None:
        """Answer `invocation` as the next look at it would."""
        self.finished[invocation] = {
            "status": "ok",
            "invocation": invocation,
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
        response = self.answer_of(request, call)
        # What the SDK sent and what this fake answers have to be what
        # `docs/api/openapi.json` describes.
        contract.check(call, response)
        return response

    def answer_of(self, request: httpx.Request, call: dict[str, Any]) -> httpx.Response:
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
                503,
                "unavailable",
                "the server is briefly unavailable; nothing ran",
            )
        return self.route(request, call)

    def route(self, request: httpx.Request, call: dict[str, Any]) -> httpx.Response:
        path = call["path"]
        if path == "/v1":
            return self.answer(200, {"api_version": "1", "server": "fake-api"})
        if path == "/v1/resources/account/usage":
            return self.answer(
                200,
                {"path": "account/usage", "fields": {
                    "host": "api.test",
                    "subscription": {
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
                }},
            )
        if path.startswith("/v1/resources/account/") and request.method == "POST":
            return self.account(call, path.removeprefix("/v1/resources/account/"))
        if not path.startswith("/v1/workspaces/"):
            return self.refuse(404, "not_found", "the API has no such route")
        rest = path.removeprefix("/v1/workspaces/")
        workspace, _, after = rest.partition("/")
        workspace = unquote(workspace)
        if workspace != WORKSPACE_ID and self.scope == "workspace":
            return self.refuse(403, "beyond_scope", "this key does not reach that workspace")
        if workspace not in [row["id"] for row in self.workspaces]:
            return self.refuse(404, "not_found", "no such workspace")
        return self.workspace_route(request, call, workspace, after)

    def workspace_route(
        self, request: httpx.Request, call: dict[str, Any], workspace: str, after: str
    ) -> httpx.Response:
        if after == "resources/account/workspaces/actions/unattended":
            return self.unattended(call, json.loads(call["body"] or b"{}").get("workspace"))
        if after == "resources" and request.method == "GET":
            return self.listing(call)
        if after.startswith("resources/"):
            return self.resource_route(request, call, after.removeprefix("resources/"))
        if after == "views":
            return self.view(call)
        if after == "programs":
            return self.program(call)
        if after.startswith("contents/"):
            hash = unquote(after.removeprefix("contents/"))
            if request.method == "PUT":
                body = call["body"]
                self.uploads.append(body)
                self.contents[hash] = body
                return self.answer(
                    201,
                    {
                        "content": f"tilo.content:{hash}",
                        "offset": len(body),
                        "size": len(body),
                    },
                )
            data = self.contents.get(hash)
            if data is None:
                return self.refuse(404, "not_found", f"the workspace has no {hash}")
            return httpx.Response(
                200,
                content=data,
                headers={"content-type": "application/octet-stream", **self.headers()},
            )
        if after == "events":
            return self.stream(self.events, call)
        if after.startswith("invocations/"):
            return self.invocation_route(request, after.removeprefix("invocations/"))
        return self.refuse(404, "not_found", "the API has no such route")

    # -- what a test drives -------------------------------------------------

    def account(self, call: dict[str, Any], route: str) -> httpx.Response:
        """An action of the account (`workspaces/actions/create`), answered as
        every action is."""
        resource, _, action = route.partition("/actions/")
        asked = json.loads(call["body"] or b"{}")
        if (resource, action) == ("workspaces", "list"):
            labels = asked.get("labels") or {}
            return self.ok(call, {"workspaces": [
                row for row in self.reached()
                if all((row.get("labels") or {}).get(name) == value for name, value in labels.items())
            ]})
        if (resource, action) == ("workspaces", "create"):
            return self.create_workspace(call)
        if (resource, action) == ("workspaces", "branch"):
            return self.branch_workspace(call, asked.get("workspace", ""))
        if (resource, action) == ("workspaces", "delete"):
            return self.delete_workspace(call, asked.get("workspace", ""))
        if (resource, action) == ("keys", "mint"):
            return self.mint_key(call)
        if action == "revoke" and resource.startswith("keys/"):
            return self.revoke_key(call, resource.removeprefix("keys/"))
        return self.refuse(404, "not_found", f"the account has no {resource}#{action}")

    def ok(self, call: dict[str, Any], result: dict[str, Any]) -> httpx.Response:
        """`result`, as a finished action answers it."""
        route = call["path"].removeprefix("/v1/resources/")
        resource, _, action = route.partition("/actions/")
        return self.answer(200, {"status": "ok", "path": f"{resource}#{action}", "result": result})

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
        return self.ok(call, {"workspace": made})

    def branch_workspace(self, call: dict[str, Any], source: str) -> httpx.Response:
        """A branch of `source`, one of the person's own; the same branch
        again for an idempotency key already answered."""
        asked = json.loads(call["body"] or b"{}")
        if self.scope == "workspace":
            return self.refuse(
                403,
                "not_permitted",
                "this key reaches one workspace and no account",
                ["account paths need a key reaching all your workspaces"],
            )
        if not any(row["id"] == source and row["role"] == "owner" for row in self.workspaces):
            return self.refuse(
                404, "not_found", "no such workspace of yours", ["account/workspaces#list names them"]
            )
        retried = self.branches.get(call["headers"].get("idempotency-key", ""))
        if retried is not None:
            return self.ok(call, {"workspace": retried})
        made: dict[str, Any] = {
            "id": f"bbbbbbbb-0000-0000-0000-{len(self.branches) + 1:012d}",
            "name": asked.get("name", ""),
            "host": "eu1",
            "role": "owner",
        }
        if asked.get("labels"):
            made["labels"] = asked["labels"]
        self.workspaces.append(made)
        self.branches[call["headers"].get("idempotency-key", "")] = made
        self.ran["workspaces#branch"] = self.ran.get("workspaces#branch", 0) + 1
        return self.ok(call, {"workspace": made})

    def unattended(self, call: dict[str, Any], workspace: str) -> httpx.Response:
        """Running commands unattended in `workspace`: off at once, on only
        once the person allows it, an invocation waiting for them meanwhile."""
        row = next(row for row in self.workspaces if row["id"] == workspace)
        if row["role"] != "owner":
            return self.refuse(
                403, "not_permitted", "the workspace refused: that workspace is not one of your own"
            )
        if not json.loads(call["body"] or b"{}").get("on"):
            row.pop("unattended", None)
            return self.answer(
                200,
                {"status": "ok", "path": "workspace/unattended#set",
                 "result": {"workspace": workspace, "unattended": False}},
            )
        invocation = f"unattended-{workspace}"
        self.asks[invocation] = {"path": "workspace/unattended#set", "workspace": workspace}
        return self.answer(
            202,
            {"status": "pending", "invocation": invocation, "path": "workspace/unattended#set", "ask": {}},
        )

    def delete_workspace(self, call: dict[str, Any], workspace: str) -> httpx.Response:
        own = [
            row
            for row in self.workspaces
            if row["id"] == workspace and row["role"] == "owner"
        ]
        if not own:
            return self.refuse(
                404, "not_found", "no such workspace of yours", ["account/workspaces#list names them"]
            )
        self.workspaces = [row for row in self.workspaces if row["id"] != workspace]
        return self.ok(call, {"deleted": workspace, "revoked_keys": ["g-minted"]})

    def mint_key(self, call: dict[str, Any]) -> httpx.Response:
        asked = json.loads(call["body"] or b"{}")
        workspace = asked.get("workspace")
        if workspace is None:
            return self.refuse(400, "invalid_arguments", "`workspace` is required")
        if self.scope == "workspace":
            return self.refuse(403, "beyond_scope", "this key reaches one workspace")
        return self.ok(
            call,
            {
                "grant": "g-minted",
                "name": asked.get("name") or "minted",
                "token": "tilo_sk_minted",
                "workspace": workspace,
                "mcp_url": f"https://mcp.tilo.sh/w/{workspace}/mcp",
                "expires_at_ms": 9_000_000_000_000,
            },
        )

    def revoke_key(self, call: dict[str, Any], grant: str) -> httpx.Response:
        self.keys = [key for key in self.keys if key["id"] != grant]
        return self.ok(call, {"revoked": grant})

    def listing(self, call: dict[str, Any]) -> httpx.Response:
        query = call["query"]
        path = query.get("path")
        if path == "record:tilo.grants":
            rows = [
                {"path": f"record:tilo.grants:{key['id']}", "name": key["id"], "label": key["id"], "kind": "tilo.grants", "actions": []}
                for key in self.keys
            ]
            return self.answer(200, {"path": path, "rows": rows, "actions": []})
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
                        {"path": TODO, "name": "Todo", "id": "notes.page:todo", "key": "notes.notebook/pages/todo", "label": "Todo", "kind": "notes.Page", "has_children": True, "actions": []}
                    ],
                    "actions": [],
                },
            )
        rows = [
            {"path": f"{TODO}/c01e", "name": "Buy milk", "id": "notes.block:c01e", "key": "notes.page/body/blocks/c01e", "label": "Buy milk", "kind": "notes.Block", "has_children": False, "actions": ["toggle-checkbox"]},
            {"path": f"{TODO}/c02f", "name": "Buy bread", "id": "notes.block:c02f", "key": "notes.page/body/blocks/c02f", "label": "Buy bread", "kind": "notes.Block", "has_children": False, "actions": []},
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
                    "kinds": self.listing_kinds(),
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
                    "kinds": self.listing_kinds(),
                },
            )
        return self.answer(
            200,
            {
                "path": path,
                "kind": self.kinds.get(path, "notes.Page"),
                "rows": rows,
                "actions": self.actions_of(path),
                "kinds": self.listing_kinds(),
            },
        )

    def listing_kinds(self) -> dict[str, Any]:
        """The actions a row of each kind offers, named once for the listing."""
        return {
            "notes.Block": [
                {"id": "toggle-checkbox", "label": "Done", "kind": "undoable", "params": [{"name": "id", "ty": "text", "required": True}]}
            ]
        }

    def actions_of(self, path: str) -> list[dict[str, Any]]:
        return [
            {"id": "append-paragraph", "label": "Add paragraph", "kind": "undoable", "params": [{"name": "text", "ty": "text", "required": True}]},
            {"id": "field-edit", "label": "Edit a field", "kind": "undoable", "params": [{"name": "edit", "ty": "object", "required": True}]},
        ]

    def resource_route(self, request: httpx.Request, call: dict[str, Any], rest: str) -> httpx.Response:
        if request.method == "GET":
            path = "/".join(unquote(part) for part in rest.split("/"))
            granted = [key for key in self.keys if path == f"record:tilo.grants:{key['id']}"]
            if granted:
                return self.answer(200, {"path": path, "fields": granted[0]})
            if path == "record:tilo.invocations:":
                return self.answer(
                    200,
                    {"path": path, "fields": self.record_of_invocations()},
                )
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
        if at == "search#find":
            hits = [
                {"path": TODO, "kind": "notes.Page", "label": "Todo"},
                {"path": f"{TODO}#append-paragraph", "kind": "notes.Page", "label": "Add paragraph", "action": "append-paragraph"},
            ]
            return self.done(at, {"query": args.get("query", ""), "hits": hits})
        if action == "share-link":
            if path not in self.fields:
                return self.refuse(404, "not_found", f"{path} names nothing")
            return self.done(at, {"url": link_to(path)})
        if at == "workspace/members#list":
            row = {"person": "cd" * 32, "name": "Rosa Ortiz", "role": "editor"}
            return self.done(at, [row | {"administrator": False}], [path], link_to(path))
        if at == "workspace/invitations#invite":
            return self.invite(at, args)
        if action == "group":
            members = args.get("actions")
            if not isinstance(members, list):
                return self.refuse(
                    422, "invalid_arguments", "`actions` is a list of invocations", []
                )
            for member in members:
                at = f"{path}#{member.get('action')}"
                self.ran[at] = self.ran.get(at, 0) + 1
            self.groups.append({"path": path, "actions": members})
            if self.group_pending:
                return self.pending(f"{path}#group")
            return self.done(
                f"{path}#group", {"applied": len(members)}, [path], link_to(path)
            )
        if action.startswith("set-") and "value" in args:
            # A settings section's setter: its field takes the whole value.
            field = action.removeprefix("set-").replace("-", "_")
            self.apply_edit(path, {"path": [{"field": field}], "op": "set", "value": args["value"]})
            return self.done(at, {"set": field}, [path])
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
            hash = blake3.blake3(letter).hexdigest()
            content = f"tilo.content:{hash}"
            self.contents[hash] = letter
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
            invocation = f"ask-{len(self.asks) + 1}"
            self.asks[invocation] = {"path": at}
            return httpx.Response(
                202,
                json={
                    "status": "pending",
                    "invocation": invocation,
                    "ask": {"url": "https://tilo.sh/ask/1"},
                },
                headers=self.headers(location=f"/v1/workspaces/{WORKSPACE_ID}/invocations/{invocation}"),
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

    def note_invocation(self, invocation: str, path: str, status: str, changes: int) -> None:
        """Keep `invocation` as the workspace's host lists it."""
        self.invocations.append(
            {
                "id": invocation,
                "calls": [invocation],
                "by": {"name": "Visa backend", "kind": "agent"},
                "status": status,
                "started_ms": len(self.invocations) + 1,
                "action": path,
                "changes": changes,
                "undone": 0,
                "undoable": changes > 0,
                "recorded": changes > 0,
                "remembered": True,
            }
        )

    def listed_as(self, invocation: str, status: str) -> None:
        for listed in self.invocations:
            if listed["id"] == invocation:
                listed["status"] = status
                if status == "undone":
                    listed["undone"] = listed["changes"]
                    listed["undoable"] = False

    def record_of_invocations(self) -> dict[str, Any]:
        """`record:tilo.invocations:`: the workspace's runs, newest first, as the
        record read answers them."""
        newest = list(reversed(self.invocations))
        return {
            "items": newest,
            "total": len(newest),
            "waiting": sum(1 for invocation in newest if invocation["status"] == "waiting"),
        }

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
            "invocation": f"invocation-{len(self.requests)}",
            "path": path,
            "resources": resources or [],
        }
        if result is not None:
            answer["result"] = result
        if link is not None:
            answer["link"] = link
        self.note_invocation(answer["invocation"], path, "ended", len(resources or []))
        return self.answer(200, answer)

    def pending(self, path: str) -> httpx.Response:
        invocation = f"invocation-{len(self.requests)}"
        self.polls[invocation] = 0
        self.note_invocation(invocation, path, "running", 0)
        return httpx.Response(
            202,
            json={"status": "pending", "invocation": invocation},
            headers=self.headers(location=f"/v1/workspaces/{WORKSPACE_ID}/invocations/{invocation}"),
        )

    def invocation_route(self, request: httpx.Request, rest: str) -> httpx.Response:
        invocation, _, after = rest.partition("/")
        if after == "cancel":
            self.cancelled.append(invocation)
            self.listed_as(invocation, "cancelled")
            self.finished[invocation] = {"status": "cancelled", "invocation": invocation, "path": f"{TODO}#append-paragraph"}
            return self.answer(200, self.finished[invocation])
        if after == "undo":
            self.undone.append(invocation)
            self.listed_as(invocation, "undone")
            return self.answer(200, {"status": "ok", "undone": [TODO]})
        if after == "output":
            return self.stream(self.output_for(invocation), None)
        if after:
            return self.refuse(404, "not_found", "the API has no such route")
        finished = self.finished.get(invocation)
        if finished is not None:
            return self.answer(200, finished)
        if invocation in self.asks and "workspace" in self.asks[invocation]:
            if self.unattended_allowed is None:
                return self.answer(202, {"status": "pending", "invocation": invocation, "ask": {}})
            if not self.unattended_allowed:
                return self.refuse(403, "refused", "the workspace refused: the person did not allow it")
            workspace = self.asks[invocation]["workspace"]
            for row in self.workspaces:
                if row["id"] == workspace:
                    row["unattended"] = True
            return self.answer(
                200,
                {"status": "ok", "invocation": invocation, "path": "workspace/unattended#set",
                 "result": {"workspace": workspace, "unattended": True}},
            )
        if invocation in self.asks:
            if not self.ask_answered:
                return self.answer(202, {"status": "pending", "invocation": invocation, "ask": {"url": "https://tilo.sh/ask/1"}})
            return self.answer(
                200,
                {"status": "ok", "invocation": invocation, "path": f"{TODO}#ask", "result": {"allowed": True}},
            )
        if invocation in self.polls:
            self.polls[invocation] += 1
            if self.polls[invocation] >= self.polls_until_done:
                self.finish(invocation)
                return self.answer(200, self.finished[invocation])
            return self.answer(202, {"status": "pending", "invocation": invocation})
        return self.refuse(404, "not_found", "no such invocation")

    def output_for(self, invocation: str) -> list[tuple[str, dict[str, Any]]]:
        if self.output:
            return self.output
        return [
            ("invocation", {"phase": "started", "invocation": invocation, "path": f"{TODO}#append-paragraph", "title": "Fill Jane's application", "time_ms": 1}),
            ("invocation", {"phase": "succeeded", "invocation": invocation, "path": f"{TODO}#append-paragraph", "title": "Fill Jane's application", "time_ms": 2}),
        ]

    def view(self, call: dict[str, Any]) -> httpx.Response:
        path = call["query"].get("path")
        if path is None:
            return self.refuse(400, "invalid_arguments", "`path` is required")
        if path not in self.fields:
            return self.refuse(404, "not_found", f"{path} names nothing")
        view: dict[str, Any] = {
            "path": path,
            "app": "so.tilo.notes",
            "shown": "resource",
            "text": 'document\n  static_text "Todo"\n  button "Rename" {notes.rename}',
            "tree": [
                {"role": "static_text", "name": "Todo", "rect": [0, 0, 40, 20]},
                {"role": "button", "name": "Rename", "rect": [0, 24, 60, 24], "action": "notes.rename"},
            ],
            "nodes": 2,
            "omitted": 0,
            "viewport": [1280, 760],
            "window_origin": [0, 40],
            "revision": "abc",
        }
        if call["query"].get("picture") == "true":
            view["picture"] = "tilo.content:look"
        return self.answer(200, view)

    def invite(self, at: str, asked: dict[str, Any]) -> httpx.Response:
        roles = ("viewer", "commenter", "editor", "moderator", "administrator", "owner")
        if asked.get("role") not in roles:
            return self.refuse(
                422,
                "invalid_arguments",
                "`role` is one of the workspace's places",
                ["send {role: viewer}"],
            )
        invited: dict[str, Any] = {
            "workspace": WORKSPACE_ID,
            "invite": "ab" * 32,
            "role": asked["role"],
            "link": f"https://tilo.sh/#/w/{WORKSPACE_ID}?invite=AAAA",
            "expires_at_ns": 7,
        }
        return self.done(at, invited, [], "")

    def program(self, call: dict[str, Any]) -> httpx.Response:
        asked = json.loads(call["body"] or b"{}")
        source = asked.get("program", "")
        if source == "boom":
            return self.answer(
                200,
                {"status": "failed", "output": ["unknown name `boom`"], "changed": 0, "steps": []},
            )
        if source == "delete everything":
            return self.answer(
                200,
                {
                    "status": "waiting",
                    "output": [],
                    "changed": 0,
                    "steps": [],
                    "text": "waiting for your person's yes: ws-1/rec-1",
                },
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
    ) -> httpx.Response:
        error: dict[str, Any] = {"code": code, "message": message}
        if next_steps:
            error["next_steps"] = list(next_steps)
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
