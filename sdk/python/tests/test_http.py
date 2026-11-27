"""A key's client: `/v1` by URL, against a server that answers what each test tells it to."""

from __future__ import annotations

import json
from collections.abc import Callable
from typing import Any

import blake3
import httpx
import pytest

from tilo import Refused, Tilo, resource_proxy, run

WORKSPACE = "6f9619ff-8b86-d011-b42d-00c04fc964ff"
BASE = f"https://api.test/v1/workspaces/{WORKSPACE}"


class Asked:
    """One request the server was asked, as the test reads it."""

    def __init__(self, request: httpx.Request) -> None:
        self.method = request.method
        self.url = str(request.url)
        self.headers = request.headers
        self.body = request.content

    def sent(self) -> dict[str, Any]:
        return json.loads(self.body)


class Server:
    """A server: each request it was asked, and what it answers."""

    def __init__(self) -> None:
        self.asked: list[Asked] = []
        self.reply: Callable[[Asked], httpx.Response] = lambda asked: httpx.Response(200, json={})

    def handler(self, request: httpx.Request) -> httpx.Response:
        asked = Asked(request)
        self.asked.append(asked)
        return self.reply(asked)

    def client(self, retry_deadline: float = 0.0) -> Tilo:
        return Tilo(
            key="tilo_sk_test",
            url="https://api.test/",
            http=httpx.AsyncClient(transport=httpx.MockTransport(self.handler)),
            retry_deadline=retry_deadline,
        )


async def test_lists_by_url_with_the_key() -> None:
    server = Server()
    server.reply = lambda asked: httpx.Response(200, json={"path": "notes", "rows": []})

    listing = await server.client().workspace(WORKSPACE).list_resources(path="notes/Q3 plan")

    assert listing["rows"] == []
    asked = server.asked[0]
    assert asked.method == "GET"
    assert asked.url == f"{BASE}/resources?path=notes%2FQ3+plan"
    assert asked.headers.get("authorization") == "Bearer tilo_sk_test"
    assert asked.headers.get("idempotency-key") is None


async def test_runs_an_action_in_its_group_and_follows_it_until_it_finished() -> None:
    server = Server()

    def reply(asked: Asked) -> httpx.Response:
        if asked.method == "POST":
            return httpx.Response(202, json={"status": "pending", "invocation": "inv-1"})
        return httpx.Response(
            200,
            json={
                "status": "ok",
                "invocation": "inv-1",
                "result": {"renamed": True},
                "link": "https://tilo.test/r/1",
            },
        )

    server.reply = reply
    workspace = server.client().workspace(WORKSPACE)

    async with workspace.invocation("Tidy the plans") as group:
        answer = await run(workspace, {"path": "notes/Q3/4 plan", "action": "rename"}, {"title": "Q4"})
        assert answer["result"] == {"renamed": True}
        assert group.invocations == ["inv-1"]
        assert group.link == "https://tilo.test/r/1"

    action, followed = server.asked
    assert action.url == f"{BASE}/resources/notes/Q3/4%20plan/actions/rename"
    assert action.sent() == {"title": "Q4"}
    assert action.headers.get("idempotency-key") is not None
    assert action.headers.get("tilo-invocation") == group.id
    assert action.headers.get("tilo-invocation-title") == "Tidy the plans"
    assert followed.url == f"{BASE}/invocations/inv-1?wait=60"
    assert followed.headers.get("tilo-invocation") == group.id


async def test_asks_the_account_again_with_the_same_key_while_its_action_waits() -> None:
    server = Server()
    count = 0

    def reply(asked: Asked) -> httpx.Response:
        nonlocal count
        count += 1
        if count < 3:
            return httpx.Response(202, json={"status": "pending", "invocation": "key-ask"})
        return httpx.Response(200, json={"status": "ok", "result": {"key": "tilo_sk_new"}})

    server.reply = reply

    answer = await run(server.client(), {"path": "account/keys", "action": "create"}, {"name": "backend"})

    assert answer["result"] == {"key": "tilo_sk_new"}
    assert [asked.url for asked in server.asked] == [
        "https://api.test/v1/resources/account/keys/actions/create"
    ] * 3
    assert len({asked.headers.get("idempotency-key") for asked in server.asked}) == 1
    assert server.asked[0].headers.get("prefer") == "wait=60"


async def test_raises_the_refusal_it_was_answered() -> None:
    server = Server()
    server.reply = lambda asked: httpx.Response(
        401,
        json={"error": {"code": "key_revoked", "message": "revoked", "revoked_at_ms": 1_794_000_000_000}},
    )

    with pytest.raises(Refused) as failure:
        await server.client().get_api()

    assert failure.value.code == "key_revoked"
    assert failure.value.status == 401
    assert failure.value.error["revoked_at_ms"] == 1_794_000_000_000


async def test_asks_again_while_the_server_is_briefly_unavailable() -> None:
    server = Server()

    def reply(asked: Asked) -> httpx.Response:
        if len(server.asked) == 1:
            return httpx.Response(
                503, json={"error": {"code": "unavailable", "message": "busy", "retry_after_ms": 10}}
            )
        return httpx.Response(200, json={"api_version": "1", "server": "test"})

    server.reply = reply

    info = await server.client(5.0).get_api()

    assert info["api_version"] == "1"
    assert len(server.asked) == 2


async def test_names_the_surface_typed_calls_were_made_for() -> None:
    server = Server()
    server.reply = lambda asked: httpx.Response(200, json={"status": "ok", "result": None})
    page = resource_proxy(
        server.client().workspace(WORKSPACE), "notes/Todo", "so.tilo.notes@0123456789abcdef"
    )

    await page.action("pin")

    assert server.asked[0].url == f"{BASE}/resources/notes/Todo/actions/pin"
    assert server.asked[0].headers.get("tilo-surface") == "so.tilo.notes@0123456789abcdef"


async def test_sends_a_bulk_call_as_a_few_group_calls_of_one_invocation() -> None:
    server = Server()
    server.reply = lambda asked: httpx.Response(
        200,
        json={
            "status": "ok",
            "result": {"applied": len(asked.sent()["actions"])},
        },
    )
    items = [{"title": f"Issue {index}"} for index in range(1200)]

    answers = await server.client().workspace(WORKSPACE).resource("tasks/issues").many("create", items)

    assert [asked.url for asked in server.asked] == [f"{BASE}/resources/tasks/issues/actions/group"] * 3
    actions = [asked.sent()["actions"] for asked in server.asked]
    assert [len(chunk) for chunk in actions] == [500, 500, 200]
    assert [member for chunk in actions for member in chunk] == [
        {"action": "create", "args": args} for args in items
    ]
    assert len({asked.headers.get("tilo-invocation") for asked in server.asked}) == 1
    assert server.asked[0].headers.get("tilo-invocation-title") == "create (1200)"
    assert [answer["result"] for answer in answers] == [
        {"applied": 500},
        {"applied": 500},
        {"applied": 200},
    ]


async def test_puts_a_large_file_up_in_pieces_at_its_blake3_address() -> None:
    server = Server()
    content = bytes(index & 0xFF for index in range(16 * 1024 * 1024 + 17))
    hash = blake3.blake3(content).hexdigest()

    def reply(asked: Asked) -> httpx.Response:
        if (asked.headers.get("content-range") or "").startswith("bytes 0-"):
            return httpx.Response(200, json={"offset": 16 * 1024 * 1024, "size": len(content)})
        return httpx.Response(
            201,
            json={"offset": len(content), "size": len(content), "content": f"tilo.content:{hash}"},
        )

    server.reply = reply

    reference = await server.client().workspace(WORKSPACE).upload(content)

    assert reference == f"tilo.content:{hash}"
    assert [
        [asked.method, asked.url, asked.headers.get("content-range"), len(asked.body)]
        for asked in server.asked
    ] == [
        ["PUT", f"{BASE}/contents/{hash}", f"bytes 0-16777215/{len(content)}", 16 * 1024 * 1024],
        ["PUT", f"{BASE}/contents/{hash}", f"bytes 16777216-16777232/{len(content)}", 17],
    ]


async def test_reads_a_workspaces_events_and_ends_their_stream_when_its_reader_stops() -> None:
    server = Server()
    server.reply = lambda asked: httpx.Response(
        200,
        headers={"content-type": "text/event-stream"},
        content=(
            b': keep-alive\n\nid: c1\nevent: change\ndata: {"path":"notes/Todo","what":"edited"}\n\n'
        ),
    )
    events = server.client().workspace(WORKSPACE).events(path="notes")

    first = await events.__anext__()
    await events.aclose()

    assert first == {
        "id": "c1",
        "type": "change",
        "workspace": WORKSPACE,
        "data": {"path": "notes/Todo", "what": "edited"},
    }
    assert server.asked[0].url == f"{BASE}/events?path=notes"
