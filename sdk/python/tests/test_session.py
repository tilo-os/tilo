"""What each call says and what its answer raises: versions, keys, retries, errors."""

from __future__ import annotations

from pathlib import Path
import re

import httpx
import pytest

import tilo as tilo_sdk
from tilo import (
    AsyncTilo,
    HttpTransport,
    KeyExpired,
    KeyRefused,
    KeyRevoked,
    ProtocolError,
    Refused,
    Unreachable,
    UnsupportedVersion,
)

from fake_api import WORKSPACE_ID, FakeApi

URL = "https://api.test"


@pytest.fixture
def api() -> FakeApi:
    return FakeApi()


def client(api: FakeApi, key: str | None = None, **options) -> AsyncTilo:
    """A client of the fake API, through `api`'s own transport."""
    return AsyncTilo(
        key=api.key if key is None else key, url=URL, http=api.client(URL), **options
    )


@pytest.fixture
async def tilo(api: FakeApi):
    made = client(api)
    yield made
    await made.aclose()


async def test_every_call_carries_the_key_and_asks_for_json_or_events(tilo, api):
    await tilo.workspaces.list()

    call = api.asked("POST", "/workspaces/actions/list")[-1]
    assert call["headers"]["authorization"] == f"Bearer {api.key}"
    assert call["headers"]["accept"] == "application/json, text/event-stream"


async def test_the_key_and_url_come_from_the_environment(api, monkeypatch):
    monkeypatch.setenv("TILO_API_URL", URL)
    monkeypatch.setenv("TILO_API_KEY", api.key)
    made = AsyncTilo(http=api.client(URL))
    try:
        assert made.transport.url == URL
        assert made.transport.key == api.key
    finally:
        await made.aclose()


async def test_a_server_of_another_major_version_is_refused(api):
    api.version = "2"
    made = client(api)
    try:
        with pytest.raises(UnsupportedVersion) as raised:
            await made.workspaces.list()
    finally:
        await made.aclose()

    assert raised.value.version == "2"
    assert "tilo-sdk" in str(raised.value)


async def test_a_transient_refusal_is_asked_again_with_the_same_idempotency_key(tilo, api):
    api.busy = 2

    created = await tilo.workspaces.create("Applicant: Jane Doe")

    assert created.name == "Applicant: Jane Doe"
    assert created.role == "owner"
    assert api.busy == 0
    assert api.ran["workspaces#create"] == 1
    keys = [call["headers"]["idempotency-key"] for call in api.asked("POST", "/workspaces/actions/create")]
    assert len(keys) == 3
    assert len(set(keys)) == 1


async def test_a_transient_refusal_past_the_deadline_is_raised(api):
    api.busy = 1000
    made = client(api, retry_deadline=0.1)
    try:
        with pytest.raises(Refused) as raised:
            await made.workspaces.create("Applicant: Jane Doe")
    finally:
        await made.aclose()

    assert raised.value.code == "unavailable"
    assert api.busy < 1000


async def test_an_unreachable_server_is_asked_again_as_the_same_call(api):
    seen: list[str] = []

    async def once_down(request: httpx.Request) -> httpx.Response:
        seen.append(request.headers.get("idempotency-key", ""))
        if len(seen) == 1:
            raise httpx.ConnectError("connection refused")
        return await api.handle(request)

    made = AsyncTilo(
        key=api.key,
        url=URL,
        http=httpx.AsyncClient(transport=httpx.MockTransport(once_down), base_url=URL),
    )
    try:
        created = await made.workspaces.create("Applicant: Jane Doe")
    finally:
        await made.aclose()

    assert created.name == "Applicant: Jane Doe"
    assert len(seen) == 2
    assert seen[0] == seen[1]


async def test_an_unreachable_endpoint_raises_exported_unreachable(api):
    assert "Unreachable" in tilo_sdk.__all__
    assert tilo_sdk.Unreachable is Unreachable

    async def down(_: httpx.Request) -> httpx.Response:
        raise httpx.ConnectError("connection refused")

    http = httpx.AsyncClient(transport=httpx.MockTransport(down), base_url=URL)
    transport = HttpTransport(URL, api.key, http=http)
    made = AsyncTilo(key=api.key, url=URL, http=http, retry_deadline=0)
    try:
        with pytest.raises(Unreachable) as direct:
            await transport.request("GET", "/v1")
        with pytest.raises(Unreachable) as raised:
            await made.workspaces.list()
    finally:
        await made.aclose()
        await http.aclose()

    assert type(direct.value) is tilo_sdk.Unreachable
    assert isinstance(direct.value, ProtocolError)
    assert type(raised.value) is tilo_sdk.Unreachable
    assert isinstance(raised.value, ProtocolError)


def _typescript_value_exports(index_ts: Path) -> set[str]:
    """Value names re-exported from `sdk/typescript/src/index.ts`."""
    text = re.sub(r"//[^\n]*|/\*.*?\*/", "", index_ts.read_text(encoding="utf-8"), flags=re.S)
    exports: set[str] = set()
    for match in re.finditer(r"\bexport\s*\{([^}]*)\}", text):
        for entry in match.group(1).split(","):
            item = entry.strip()
            if not item or item.startswith("type "):
                continue
            if " as " in item:
                _, item = item.split(" as ", 1)
            exports.add(item.strip())
    return exports


def test_both_sdks_export_the_shared_error_classes():
    shared_errors = {
        "Refused",
        "SurfaceChanged",
        "AskPending",
        "ProtocolError",
        "KeyRefused",
        "KeyRevoked",
        "KeyExpired",
        "UnsupportedVersion",
        "Unreachable",
    }

    assert shared_errors <= set(tilo_sdk.__all__)
    for name in ("ProtocolError", "UnsupportedVersion", "Unreachable"):
        assert issubclass(getattr(tilo_sdk, name), ProtocolError)

    ts_index = Path(__file__).resolve().parents[2] / "typescript" / "src" / "index.ts"
    ts_exports = _typescript_value_exports(ts_index)
    assert shared_errors <= ts_exports


async def test_a_new_logical_call_names_an_idempotency_key_of_its_own(tilo, api):
    await tilo.workspaces.create("One")
    await tilo.workspaces.create("Two")

    keys = [call["headers"].get("idempotency-key") for call in api.asked("POST", "/workspaces/actions/create")]
    assert all(keys)
    assert keys[0] != keys[1]


async def test_reads_name_no_idempotency_key(tilo, api):
    await tilo.workspace("jane-doe").ls()

    assert "idempotency-key" not in api.asked("GET", "/resources")[-1]["headers"]


async def test_a_refusal_raises_with_the_api_s_words(tilo, api):
    with pytest.raises(Refused) as raised:
        await tilo.workspace("jane-doe").do("notes/Todo#nonsense")

    assert raised.value.code == "invalid_arguments"
    assert "no action" in raised.value.message
    assert raised.value.next_steps == [
        f"GET /v1/workspaces/{WORKSPACE_ID}/resources?path=notes/Todo lists its actions"
    ]


@pytest.mark.parametrize(
    ("key", "raised", "code"),
    [
        ("tilo_sk_revoked", KeyRevoked, "key_revoked"),
        ("tilo_sk_expired", KeyExpired, "key_expired"),
        ("tilo_sk_never", KeyRefused, "invalid_token"),
    ],
)
async def test_a_key_the_api_turns_away_says_why(api, key, raised, code):
    made = client(api, key=key)
    try:
        with pytest.raises(raised) as refused:
            await made.workspaces.list()
    finally:
        await made.aclose()

    assert type(refused.value) is raised
    assert refused.value.code == code
    assert refused.value.status == 401
    if raised is KeyRevoked:
        assert refused.value.revoked_at_ms == 1_794_000_000_000
        assert refused.value.next_steps == ["ask the person for a new key"]
    if raised is KeyExpired:
        assert refused.value.expired_at_ms == 1_794_000_000_000


async def test_a_refusal_that_is_not_the_error_shape_is_a_protocol_error(api):
    async def plain(_: httpx.Request) -> httpx.Response:
        return httpx.Response(500, text="boom")

    made = AsyncTilo(
        key=api.key,
        url=URL,
        http=httpx.AsyncClient(transport=httpx.MockTransport(plain), base_url=URL),
        retry_deadline=0,
    )
    try:
        with pytest.raises(ProtocolError) as raised:
            await made.workspaces.list()
    finally:
        await made.aclose()

    assert raised.value.status == 500
    assert "boom" in str(raised.value)


async def test_the_catalog_is_kept_until_a_workspace_is_created(tilo):
    first = await tilo.workspaces.list()
    assert tilo.catalog is not None
    assert len(first) == 3

    created = await tilo.workspaces.create("new-applicant")
    assert created.name == "new-applicant"
    assert tilo.catalog is None


async def test_a_workspace_is_named_by_its_id_or_its_name(tilo, api):
    api.workspaces.append(
        {"id": "22222222-2222-3333-4444-555555555555", "name": "Johnson", "host": "eu1"}
    )

    info = await tilo.workspace("Johnson").info()

    assert info.id == "22222222-2222-3333-4444-555555555555"
    assert info.name == "Johnson"
    assert info.host == "eu1"
