"""Keys: the account's, the ones minted for a workspace, and asking for one."""

from __future__ import annotations

import pytest

from tilo import AskPending, AsyncTilo, Key, MintedKey, Refused, SignIn, WorkspaceInfo

from fake_api import INVITED_ID, OTHER_ID, WORKSPACE_ID, FakeApi

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
    assert api.asked("GET", "/v1/resources/record:tilo.account:keys")


async def test_a_key_is_minted_for_one_workspace(tilo, api):
    minted = await tilo.keys.mint(WORKSPACE_ID, name="jane's agent")

    assert isinstance(minted, MintedKey)
    assert minted.token == "tilo_sk_minted"
    assert minted.workspace == WORKSPACE_ID
    assert minted.mcp_url.endswith(f"/w/{WORKSPACE_ID}/mcp")
    assert api.asked("POST", "/account/keys/actions/mint")[-1]["body"] == (
        b'{"workspace": "' + WORKSPACE_ID.encode() + b'", "name": "jane\'s agent"}'
    )


async def test_a_workspace_mints_its_own_key(tilo):
    minted = await tilo.workspace("jane-doe").keys.mint(expires_in=3600)

    assert minted.workspace == WORKSPACE_ID


async def test_a_key_is_revoked_and_answers_which(tilo, api):
    answer = await tilo.keys.revoke("g-root")

    assert answer == {"revoked": "g-root"}
    assert api.asked("POST", "/account/keys/g-root/actions/revoke")[-1]["body"] == b"{}"
    assert [key.grant for key in await tilo.keys.list()] == []


async def test_a_workspace_is_deleted_and_gone_from_the_listing(tilo, api):
    answer = await tilo.workspaces.delete("jane-doe")

    assert answer == {"deleted": WORKSPACE_ID, "revoked_keys": ["g-minted"]}
    assert api.asked("POST", "/account/workspaces/actions/delete")[-1]["body"] == (
        b'{"workspace": "' + WORKSPACE_ID.encode() + b'"}'
    )
    listed = await tilo.workspaces.list()
    assert WORKSPACE_ID not in [workspace.id for workspace in listed]


async def test_a_workspace_invited_into_is_not_deleted(tilo):
    with pytest.raises(Refused) as raised:
        await tilo.workspaces.delete(INVITED_ID)

    assert raised.value.code == "not_found"


async def test_a_workspace_is_branched_into_a_new_one_of_the_persons_own(tilo, api):
    branch = await tilo.workspaces.branch(
        "jane-doe",
        "jane-doe, second try",
        labels={"customer_id": "cus_8121"},
        idempotency_key="cus_8121-retry",
    )

    assert isinstance(branch, WorkspaceInfo)
    assert branch.name == "jane-doe, second try"
    assert branch.role == "owner"
    assert branch.labels == {"customer_id": "cus_8121"}
    call = api.asked("POST", "/workspaces/actions/branch")[-1]
    assert call["body"] == (
        b'{"workspace": "' + WORKSPACE_ID.encode() + b'", "name": "jane-doe, second try", '
        b'"labels": {"customer_id": "cus_8121"}}'
    )
    assert call["headers"]["idempotency-key"] == "cus_8121-retry"
    listed = await tilo.workspaces.list()
    assert branch.id in [workspace.id for workspace in listed]


async def test_a_branch_asked_again_with_its_idempotency_key_is_the_same_branch(tilo, api):
    created = await tilo.workspaces.create("Template")
    first = await tilo.workspaces.branch(created, "Acme", idempotency_key="cus_acme")
    again = await tilo.workspaces.branch(created, "Acme", idempotency_key="cus_acme")

    assert again.id == first.id
    assert api.ran["workspaces#branch"] == 1
    assert api.asked("POST", "/workspaces/actions/branch")[-1]["body"] == (
        b'{"workspace": "' + created.id.encode() + b'", "name": "Acme"}'
    )


async def test_a_workspace_branches_itself(tilo, api):
    branch = await tilo.workspace("john-doe").branch("john-doe, audit")

    assert branch.name == "john-doe, audit"
    call = api.asked("POST", "/workspaces/actions/branch")[-1]
    assert call["body"] == b'{"workspace": "' + OTHER_ID.encode() + b'", "name": "john-doe, audit"}'
    assert call["headers"]["idempotency-key"]


async def test_a_workspace_invited_into_is_not_branched(tilo):
    with pytest.raises(Refused) as raised:
        await tilo.workspaces.branch(INVITED_ID, "mine now")

    assert raised.value.code == "not_found"


async def test_a_workspace_runs_commands_unattended_once_the_person_allows_it(tilo, api):
    workspace = tilo.workspace("jane-doe")
    with pytest.raises(AskPending) as raised:
        await workspace.unattended()

    ask = raised.value
    assert ask.invocation_id == f"unattended-{WORKSPACE_ID}"
    call = api.asked("POST", "/actions/unattended")[-1]
    assert call["path"] == (
        f"/v1/workspaces/{WORKSPACE_ID}/resources/account/workspaces/actions/unattended"
    )
    assert call["body"] == b'{"workspace": "' + WORKSPACE_ID.encode() + b'", "on": true}'
    listed = await tilo.workspaces.list()
    assert not listed.named(WORKSPACE_ID).unattended, "off until they answer"

    api.unattended_allowed = True
    assert await ask.wait() == {"workspace": WORKSPACE_ID, "unattended": True}
    assert (await tilo.workspaces.list()).named(WORKSPACE_ID).unattended


async def test_a_workspace_the_person_did_not_let_stays_asking(tilo, api):
    with pytest.raises(AskPending) as raised:
        await tilo.workspace("jane-doe").unattended()

    api.unattended_allowed = False
    with pytest.raises(Refused) as refused:
        await raised.value.wait()
    assert "did not allow" in refused.value.message
    assert not (await tilo.workspaces.list()).named(WORKSPACE_ID).unattended


async def test_running_commands_unattended_is_turned_off_without_asking(tilo, api):
    answer = await tilo.workspace("jane-doe").unattended(on=False)

    assert answer == {"workspace": WORKSPACE_ID, "unattended": False}
    assert api.asked("POST", "/actions/unattended")[-1]["body"] == (
        b'{"workspace": "' + WORKSPACE_ID.encode() + b'", "on": false}'
    )


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

    assert api.asked("GET", "/v1/resources/account/usage")
    assert usage.host == "api.test"
    assert usage.plan == "free"
    assert [row.workspace for row in usage.workspaces] == [WORKSPACE_ID, "other"]
    assert usage.workspaces[0].stored_bytes == 2048
    assert usage.vm_seconds == 90
    assert usage.running == ["other"]


async def test_a_workspace_invites_another_person_with_a_link(tilo, api):
    invitation = await tilo.workspace("jane-doe").members.invite(role="editor")

    assert invitation.link.startswith("https://tilo.sh/#/w/")
    assert invitation.role == "editor"
    assert invitation.email is None and not invitation.sent
    asked = api.asked("POST", "/resources/workspace/invitations/actions/invite")[-1]
    assert asked["body"] == b'{"role": "editor"}'


async def test_an_emailed_invitation_says_it_was_not_sent(tilo):
    invitation = await tilo.workspace("jane-doe").members.invite(email="rosa@example.com")

    assert invitation.role == "viewer"
    assert invitation.email == "rosa@example.com"
    assert invitation.sent is False and invitation.next


async def test_an_invitation_to_no_place_is_refused(tilo):
    with pytest.raises(Refused) as raised:
        await tilo.workspace("jane-doe").members.invite(role="superuser")

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
        with pytest.raises(Refused) as branching:
            await client.workspaces.branch(WORKSPACE_ID, "Mallory")
        assert branching.value.code == "not_permitted"
    finally:
        await client.aclose()
