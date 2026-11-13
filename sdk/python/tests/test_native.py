"""The native part against a real Tilo host: `TILO_KEY_FILE` names a key file of
a device of the person (the `api-on-relay` harness writes one).

Nothing stands between the test and the host: `tilo._native` is the API in
process with the device's own key, dialling the relay as the person's devices
do. Without `TILO_KEY_FILE` this is skipped; with it, a missing extension fails.
"""

from __future__ import annotations

import asyncio
import json
import os
import uuid
from collections.abc import AsyncIterator

import pytest

KEY_FILE = os.environ.get("TILO_KEY_FILE")

pytestmark = pytest.mark.skipif(
    not KEY_FILE, reason="TILO_KEY_FILE names a key file of a device of the person"
)

JSON = {"content-type": "application/json"}


@pytest.fixture
def host():
    from tilo._native import Host

    return Host(KEY_FILE)


async def whole(host, method: str, path: str, body: object = None, headers=None):
    payload = b"" if body is None else json.dumps(body).encode()
    status, answered, reply = await host.request(method, path, {**JSON, **(headers or {})}, payload)
    assert isinstance(reply, bytes), f"{path} answered a stream"
    return status, answered, json.loads(reply) if reply else None


async def test_the_key_reaches_the_host_as_the_person(host):
    status, headers, info = await whole(host, "GET", "/v1")
    assert status == 200, info
    assert headers["content-type"].startswith("application/json")
    assert info["key"]["scope"] == "all", info


async def test_a_key_file_is_also_its_contents():
    from tilo._native import Host

    with open(KEY_FILE, encoding="utf-8") as file:
        contents = json.load(file)
    status, _, info = await whole(Host(contents), "GET", "/v1")
    assert status == 200, info


async def test_a_bad_key_file_is_refused():
    from tilo._native import Host

    with pytest.raises(ValueError):
        Host({"host": "http://127.0.0.1:1", "name": "x", "key": "zz", "cert": ""})
    with pytest.raises(TypeError):
        Host(42)


async def test_a_workspace_is_made_once_and_its_apps_answer(host):
    name = f"native-{uuid.uuid4().hex[:8]}"
    retried = {"idempotency-key": name}
    status, _, made = await whole(host, "POST", "/v1/workspaces", {"name": name}, retried)
    assert status == 201, made
    _, _, again = await whole(host, "POST", "/v1/workspaces", {"name": name}, retried)
    assert again["id"] == made["id"], "a retry makes no second workspace"

    status, _, listed = await whole(host, "GET", "/v1/workspaces")
    assert status == 200, listed
    assert made["id"] in json.dumps(listed)

    workspace = made["id"]
    status, _, opened = await whole(
        host, "POST", f"/v1/workspaces/{workspace}/resources/notes/actions/open", {}
    )
    assert status == 200, opened
    status, _, apps = await whole(host, "GET", f"/v1/workspaces/{workspace}/resources")
    assert status == 200, apps
    assert apps["rows"], apps


async def test_a_workspaces_events_stream_as_they_happen(host):
    _, _, made = await whole(host, "POST", "/v1/workspaces", {"name": "native-events"})
    workspace = made["id"]
    status, headers, events = await host.request(
        "GET", f"/v1/workspaces/{workspace}/events", {}, b""
    )
    assert status == 200
    assert headers["content-type"].startswith("text/event-stream")
    assert isinstance(events, AsyncIterator)

    async def first_event() -> str:
        seen = b""
        async for chunk in events:
            assert isinstance(chunk, bytes)
            seen += chunk
            if b"\n\n" in seen and b"event:" in seen:
                return seen.decode()
        raise AssertionError(f"the stream ended after {seen!r}")

    reading = asyncio.ensure_future(first_event())
    status, _, opened = await whole(
        host, "POST", f"/v1/workspaces/{workspace}/resources/notes/actions/open", {}
    )
    assert status == 200, opened
    event = await asyncio.wait_for(reading, timeout=60)
    assert "data:" in event, event
