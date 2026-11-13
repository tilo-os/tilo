"""Keys: the account's, the ones minted for a workspace, and asking for one."""

from __future__ import annotations

import pytest

from tilo import AskPending, AsyncTilo, Key, MintedKey, Refused, SignIn

from fake_api import INVITED_ID, WORKSPACE_ID, FakeApi

URL = "https://api.test"


@pytest.fixture
def api() -> FakeApi:
    return FakeApi()


@pytest.fixture
async def tilo(api: FakeApi):
    client = AsyncTilo(key=api.key, url=URL, http=api.client(URL))
    yield client
    await client.aclose()


async def test_the_account_lists_this_key_and_the_keys_it_minted(tilo, api):
    keys = await tilo.keys.list()

    assert [key.grant for key in keys] == ["g-root"]
    assert keys[0].scope == "account"
    assert keys[0].browser_pages is True
    assert api.asked("GET", "/v1/keys")


async def test_a_key_is_minted_for_one_workspace(tilo, api):
    minted = await tilo.keys.mint(WORKSPACE_ID, name="jane's agent")

    assert isinstance(minted, MintedKey)
    assert minted.token == "tilo_sk_minted"
    assert minted.workspace == WORKSPACE_ID
    assert minted.mcp_url.endswith(f"/w/{WORKSPACE_ID}/mcp")
    assert api.asked("POST", "/v1/keys")[-1]["body"] == (
        b'{"workspace": "' + WORKSPACE_ID.encode() + b'", "name": "jane\'s agent"}'
    )


async def test_a_workspace_mints_its_own_key(tilo):
    minted = await tilo.workspace("jane-doe").keys.mint(expires_in=3600)

    assert minted.workspace == WORKSPACE_ID


async def test_a_key_is_revoked_and_answers_which(tilo, api):
    answer = await tilo.keys.revoke("g-root")

    assert answer == {"revoked": "g-root"}
    assert api.asked("DELETE", "/v1/keys/g-root")
    assert [key.grant for key in await tilo.keys.list()] == []


async def test_a_workspace_is_deleted_and_gone_from_the_listing(tilo, api):
    answer = await tilo.workspaces.delete("jane-doe")

    assert answer == {"deleted": WORKSPACE_ID, "revoked_keys": ["g-minted"]}
    assert api.asked("DELETE", f"/v1/workspaces/{WORKSPACE_ID}")
    listed = await tilo.workspaces.list()
    assert WORKSPACE_ID not in [workspace.id for workspace in listed]


async def test_a_workspace_invited_into_is_not_deleted(tilo):
    with pytest.raises(Refused) as raised:
        await tilo.workspaces.delete(INVITED_ID)

    assert raised.value.code == "not_found"


async def test_a_minted_key_makes_a_client_of_its_own(tilo):
    minted = await tilo.keys.mint(WORKSPACE_ID)

    client = minted.client(url=URL)
    assert client.transport.key == "tilo_sk_minted"
    await client.aclose()


async def test_asking_for_a_key_shows_a_link_and_waits_for_approval(tilo, api):
    with pytest.raises(AskPending) as raised:
        await tilo.keys.create("visa-backend", workspace=WORKSPACE_ID)

    ask = raised.value
    assert ask.url == "https://mcp.tilo.sh/keys/approve/abc"
    assert ask.invocation_id == "abc"
    assert api.new_key_asked == {"name": "visa-backend", "workspace": WORKSPACE_ID}

    api.asked_key = {"key": "tilo_sk_new", "grant": "g-new", "name": "visa-backend", "scope": "account"}
    key = await ask.wait()

    assert isinstance(key, Key)
    assert key.token == "tilo_sk_new"
    assert key.grant == "g-new"
    assert api.polls_of_key >= 1


async def test_a_terminal_sign_in_shows_a_link_and_polls_for_the_key(tilo, api):
    sign_in = await tilo.sign_in(name="cli")

    assert isinstance(sign_in, SignIn)
    assert sign_in.url == "https://mcp.tilo.sh/keys/approve/abc"
    assert api.new_key_asked == {"name": "cli"}

    api.asked_key = {"key": "tilo_sk_new", "grant": "g-new", "name": "cli", "scope": "account"}
    key = await sign_in.wait()

    assert key.token == "tilo_sk_new"


async def test_the_account_says_what_its_workspaces_kept_and_ran(tilo, api):
    usage = await tilo.usage()

    assert api.asked("GET", "/v1/account")
    assert usage.host == "api.test"
    assert usage.plan == "free"
    assert [row.workspace for row in usage.workspaces] == [WORKSPACE_ID, "other"]
    assert usage.workspaces[0].stored_bytes == 2048
    assert usage.vm_seconds == 90
    assert usage.running == ["other"]


async def test_a_workspace_invites_another_person_with_a_link(tilo, api):
    invitation = await tilo.workspace("jane-doe").members.invite(role="editor")

    assert invitation.link.startswith("https://tilo.sh/i/")
    assert invitation.role == "editor"
    assert invitation.email is None and not invitation.sent
    assert api.asked("POST", "/v1/workspaces/")[-1]["path"].endswith("/members/invitations")
    assert api.asked("POST", "/members/invitations")[-1]["body"] == b'{"role": "editor"}'


async def test_an_emailed_invitation_says_it_was_not_sent(tilo):
    invitation = await tilo.workspace("jane-doe").members.invite(email="rosa@example.com")

    assert invitation.role == "viewer"
    assert invitation.email == "rosa@example.com"
    assert invitation.sent is False and invitation.next


async def test_an_invitation_above_editor_is_refused(tilo):
    with pytest.raises(Refused) as raised:
        await tilo.workspace("jane-doe").members.invite(role="moderator")

    assert raised.value.code == "invalid_arguments"


async def test_a_workspace_one_was_invited_into_is_listed_with_its_role_and_reached_by_name(tilo):
    listing = await tilo.workspaces.list()
    invited = listing.named("applicant-jane-doe")

    assert invited is not None
    assert invited.id == INVITED_ID
    assert invited.role == "editor"
    assert listing.named(INVITED_ID).role == "editor"
    assert (await tilo.workspace("applicant-jane-doe").info()).id == INVITED_ID


async def test_the_members_of_a_workspace_with_their_roles(tilo):
    members = await tilo.workspace("jane-doe").members.list()

    assert [(member.name, member.role) for member in members] == [("Rosa Ortiz", "editor")]


async def test_a_key_for_one_workspace_lists_that_workspace_alone(api):
    api.scope = "workspace"
    client = AsyncTilo(key=api.key, url=URL, http=api.client(URL))
    try:
        listing = await client.workspaces.list()
        assert [workspace.id for workspace in listing] == [WORKSPACE_ID]
        with pytest.raises(Refused) as raised:
            await client.workspaces.create("Mallory")
        assert raised.value.code == "beyond_scope"
        with pytest.raises(Refused) as minting:
            await client.keys.mint(WORKSPACE_ID)
        assert minting.value.code == "beyond_scope"
    finally:
        await client.aclose()
