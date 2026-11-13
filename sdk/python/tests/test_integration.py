"""Against a real API server: `TILO_TEST_API_URL` and `TILO_TEST_KEY` name one.

Nothing here fakes the wire: the key reaches every workspace of its person,
and every call is the one an agent makes.
"""

from __future__ import annotations

import os
import uuid

import pytest

from tilo import AsyncTilo, KeyRevoked, Refused

URL = os.environ.get("TILO_TEST_API_URL")
KEY = os.environ.get("TILO_TEST_KEY")

pytestmark = pytest.mark.skipif(
    not (URL and KEY), reason="TILO_TEST_API_URL and TILO_TEST_KEY name an API server"
)


@pytest.fixture
async def tilo():
    client = AsyncTilo(key=KEY, url=URL)
    yield client
    await client.aclose()


async def test_a_workspace_is_created_read_and_listed(tilo):
    name = f"sdk-test-{uuid.uuid4().hex[:8]}"
    created = await tilo.workspaces.create(name)
    assert created.name == name

    listing = await tilo.workspaces.list()
    assert listing.named(name) is not None

    workspace = tilo.workspace(created.id)
    apps = await workspace.ls()
    assert apps.path == "apps"


async def test_a_file_is_uploaded_and_its_reference_comes_back(tilo):
    workspace = tilo.workspace(
        (await tilo.workspaces.create(f"sdk-upload-{uuid.uuid4().hex[:8]}")).id
    )

    content = await workspace.files.upload(b"a passport, in bytes")

    assert content.startswith("tilo.content:")
    assert await workspace.files.download(content) == b"a passport, in bytes"


async def test_a_key_is_minted_for_one_workspace_and_revoked(tilo):
    created = await tilo.workspaces.create(f"sdk-key-{uuid.uuid4().hex[:8]}")

    minted = await tilo.keys.mint(created.id, name="sdk test agent")

    assert minted.workspace == created.id
    assert minted.token
    keys = await tilo.keys.list()
    assert minted.grant in [key.grant for key in keys]

    await tilo.keys.revoke(minted.grant)
    keys = await tilo.keys.list()
    assert minted.grant not in [key.grant for key in keys]


async def test_the_account_says_what_its_workspaces_keep(tilo):
    created = await tilo.workspaces.create(f"sdk-usage-{uuid.uuid4().hex[:8]}")

    usage = await tilo.usage()

    assert any(row.workspace == created.id for row in usage.workspaces)


async def test_a_deleted_workspace_is_gone_and_its_minted_key_with_it(tilo):
    created = await tilo.workspaces.create(f"sdk-delete-{uuid.uuid4().hex[:8]}")
    minted = await tilo.keys.mint(created.id)

    answer = await tilo.workspaces.delete(created.id)

    assert answer["deleted"] == created.id
    assert minted.grant in answer["revoked_keys"]
    listing = await tilo.workspaces.list()
    assert created.id not in [workspace.id for workspace in listing]
    agent = AsyncTilo(key=minted.token, url=URL)
    try:
        with pytest.raises(KeyRevoked):
            await agent.workspace(created.id).ls()
    finally:
        await agent.aclose()
    with pytest.raises(Refused) as again:
        await tilo.workspaces.delete(created.id)
    assert again.value.code == "not_found"
