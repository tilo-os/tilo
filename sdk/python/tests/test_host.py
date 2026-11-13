"""A host served in process: the same routes, through tilo._native."""

from __future__ import annotations

import sys
import types

import httpx
import pytest

from tilo import AsyncTilo

from fake_api import WORKSPACE_ID, FakeApi


class FakeNative:
    """What `tilo._native` will hand the SDK: a host answering its own routes."""

    def __init__(self, api: FakeApi) -> None:
        self.api = api
        self.calls: list[tuple[str, str]] = []

    async def request(self, method, path, headers, body):
        self.calls.append((method, path))
        # The host's own key authenticates the call: nothing travels with it.
        request = httpx.Request(
            method,
            "https://host.test" + path,
            headers={**headers, "authorization": f"Bearer {self.api.key}"},
            content=body,
        )
        response = await self.api.handle(request)
        return response.status_code, dict(response.headers), await response.aread()


def module(native: FakeNative) -> types.ModuleType:
    made = types.ModuleType("tilo._native")
    made.Host = lambda key_file: native  # type: ignore[attr-defined]
    return made


async def test_host_without_the_extension_says_what_to_install(monkeypatch):
    monkeypatch.setitem(sys.modules, "tilo._native", None)

    with pytest.raises(ImportError) as raised:
        AsyncTilo.host("/home/jane/.config/tilo/key.json")

    assert "tilo-sdk[native]" in str(raised.value)


async def test_host_serves_the_same_routes_in_process(monkeypatch):
    api = FakeApi()
    native = FakeNative(api)
    monkeypatch.setitem(sys.modules, "tilo._native", module(native))

    client = AsyncTilo.host("/home/jane/.config/tilo/key.json")
    try:
        listing = await client.workspaces.list()
        apps = await client.workspace(WORKSPACE_ID).ls()
    finally:
        await client.aclose()

    assert [workspace.name for workspace in listing] == [
        "jane-doe",
        "john-doe",
        "applicant-jane-doe",
    ]
    assert apps.path == "apps"
    assert native.calls[0] == ("GET", "/v1/workspaces")
    assert native.calls[-1] == (
        "GET",
        f"/v1/workspaces/{WORKSPACE_ID}/resources",
    )
