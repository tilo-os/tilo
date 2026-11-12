"""What each request the SDK makes says: session, typed answers, run, errors."""

from __future__ import annotations

import pytest

from tilo import AsyncTilo, ProtocolError, RateLimited, Refused, WorkspaceList

from fake_door import WORKSPACE_ID, FakeDoor


@pytest.fixture
def door() -> FakeDoor:
    return FakeDoor()


@pytest.fixture
async def tilo(door: FakeDoor):
    client = AsyncTilo(key=door.key, door="https://door.test", http=door.client())
    yield client
    await client.aclose()


async def test_a_call_opens_a_session_and_asks_for_typed_answers(tilo, door):
    listing = await tilo.workspaces.list()

    assert [workspace.name for workspace in listing] == [
        "jane-doe",
        "john-doe",
        "applicant-jane-doe",
    ]
    assert [workspace.role for workspace in listing] == ["owner", "owner", "editor"]
    assert listing.hosts[0].name == "eu1"
    assert door.opened >= 1
    call = door.asked("tilo_do")[-1]
    assert call["arguments"]["path"] == "account/workspaces#list"
    assert call["structured"] is True
    assert call["workspace"] is None


async def test_a_busy_host_is_asked_again_until_it_answers(tilo, door):
    door.busy = 2

    created = await tilo.workspaces.create("Applicant: Jane Doe")

    assert created.name == "Applicant: Jane Doe"
    assert door.busy == 0
    # The refused calls ran nothing: the door saw the create once.
    assert len(door.asked("tilo_do")) == 1


async def test_a_busy_workspace_entry_is_asked_again(tilo, door):
    door.busy_entering = 2

    await tilo.workspace("jane-doe").ls()

    assert door.busy_entering == 0
    assert len(door.asked("tilo_ls")) == 1


async def test_a_host_busy_past_the_deadline_raises_rate_limited(door):
    door.busy = 1000
    door.busy_wait_ms = 20
    client = AsyncTilo(
        key=door.key, door="https://door.test", http=door.client(), retry_deadline=0.1
    )
    try:
        with pytest.raises(RateLimited) as raised:
            await client.workspaces.create("Applicant: Jane Doe")
    finally:
        await client.aclose()

    assert isinstance(raised.value, Refused)
    assert raised.value.code == "rate_limited"
    assert raised.value.retry_after_ms == 20
    assert door.busy < 1000


async def test_a_workspace_call_goes_to_its_own_session(tilo, door):
    await tilo.workspace("jane-doe").ls()

    calls = [call for call in door.calls if call["tool"] == "tilo_ls"]
    assert calls[-1]["workspace"] == WORKSPACE_ID
    assert calls[-1]["arguments"] == {}


async def test_an_event_stream_reply_is_read_to_its_last_message(tilo, door):
    door.stream = True

    listing = await tilo.workspace("jane-doe").ls()

    assert listing.path == "apps"
    assert [row.name for row in listing.rows] == ["notes", "pdf-viewer", "terminal"]


async def test_a_forgotten_session_is_opened_again(tilo, door):
    workspace = tilo.workspace("jane-doe")
    await workspace.ls()
    opened = door.opened
    door.sessions.clear()

    listing = await workspace.ls("notes")

    assert listing.path == "notes"
    assert door.opened == opened + 1


async def test_a_key_that_is_not_this_door_s_is_a_protocol_error(door):
    other = FakeDoor()
    client = AsyncTilo(key="tilo_sk_other", door="https://door.test", http=other.client())

    with pytest.raises(ProtocolError) as raised:
        await client.workspaces.list()

    assert raised.value.status == 401
    assert "key" in str(raised.value)
    await client.aclose()


async def test_a_call_without_typed_content_is_a_protocol_error(tilo, door, monkeypatch):
    # A door that answers text for an unknown tool: the caller asked typed.
    door.tool = lambda name, arguments, workspace: None

    with pytest.raises(ProtocolError) as raised:
        await tilo.workspace("jane-doe")._call("tilo_nope", {})

    assert "typed" in str(raised.value)


async def test_the_catalog_is_kept_until_a_workspace_is_created(tilo, door):
    first = await tilo.workspaces.list()
    assert tilo.catalog is not None
    assert isinstance(first, WorkspaceList)

    created = await tilo.workspaces.create("new-applicant")
    assert created.name == "new-applicant"
    assert tilo.catalog is None
    assert door.workspaces[-1]["id"] == created.id


async def test_a_workspace_is_named_by_its_id_or_its_name(tilo, door):
    by_name = tilo.workspace("Johnson")
    door.workspaces.append({"id": "22222222-2222-3333-4444-555555555555", "name": "Johnson", "host": "eu1"})

    info = await by_name.info()

    assert info.id == "22222222-2222-3333-4444-555555555555"
    assert info.name == "Johnson"
