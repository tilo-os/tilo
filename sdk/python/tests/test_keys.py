"""Keys: the account's, the ones minted for a workspace, and asking for one."""

from __future__ import annotations

import pytest

from tilo import AskPending, AsyncTilo, Key, KeyExpired, KeyRefused, KeyRevoked, MintedKey, Refused, SignIn

from fake_door import INVITED_ID, WORKSPACE_ID, FakeDoor


@pytest.fixture
def door() -> FakeDoor:
    return FakeDoor()


@pytest.fixture
async def tilo(door: FakeDoor):
    client = AsyncTilo(key=door.key, door="https://door.test", http=door.client())
    yield client
    await client.aclose()


async def test_the_account_lists_this_key_and_the_keys_it_minted(tilo, door):
    keys = await tilo.keys.list()

    assert [key.grant for key in keys] == ["g-root"]
    assert keys[0].scope == "account"
    assert door.asked("tilo_do")[-1]["arguments"]["path"] == "account/keys#list"


async def test_a_key_is_minted_for_one_workspace(tilo, door):
    minted = await tilo.keys.mint(WORKSPACE_ID, name="jane's agent")

    assert isinstance(minted, MintedKey)
    assert minted.token == "tilo_sk_minted"
    assert minted.workspace == WORKSPACE_ID
    assert minted.mcp_url.endswith(f"/w/{WORKSPACE_ID}/mcp")
    assert door.asked("tilo_do")[-1]["arguments"]["args"] == {
        "workspace": WORKSPACE_ID,
        "name": "jane's agent",
    }


async def test_a_workspace_mints_its_own_key(tilo):
    minted = await tilo.workspace("jane-doe").keys.mint()

    assert minted.workspace == WORKSPACE_ID


async def test_mint_mcp_is_the_same_key(tilo):
    minted = await tilo.workspace("jane-doe").mint_mcp(expires_in=3600)

    assert minted.grant == "g-minted"


async def test_a_key_is_revoked_and_answers_which(tilo):
    answer = await tilo.keys.revoke("g-root")

    assert answer == {"revoked": "g-root"}


async def test_a_workspace_is_deleted_and_gone_from_the_listing(tilo, door):
    answer = await tilo.workspaces.delete("jane-doe")

    assert answer == {"deleted": WORKSPACE_ID, "revoked_keys": ["g-minted"]}
    assert door.asked("tilo_do")[-1]["arguments"]["args"] == {"workspace": WORKSPACE_ID}
    listed = await tilo.workspaces.list()
    assert WORKSPACE_ID not in [workspace.id for workspace in listed]


async def test_a_workspace_invited_into_is_not_deleted(tilo):
    with pytest.raises(Refused) as raised:
        await tilo.workspaces.delete(INVITED_ID)

    assert raised.value.code == "not_found"


@pytest.mark.parametrize(
    ("key", "raised", "code"),
    [
        ("tilo_sk_revoked", KeyRevoked, "key_revoked"),
        ("tilo_sk_expired", KeyExpired, "key_expired"),
        ("tilo_sk_never", KeyRefused, "invalid_token"),
    ],
)
async def test_a_key_the_door_turns_away_says_why(door, key, raised, code):
    client = AsyncTilo(key=key, door="https://door.test", http=door.client())
    try:
        with pytest.raises(raised) as refused:
            await client.workspaces.list()
    finally:
        await client.aclose()

    assert type(refused.value) is raised
    assert refused.value.code == code
    assert refused.value.status == 401
    if raised is KeyRevoked:
        assert refused.value.revoked_at_ms == 1_794_000_000_000
        assert refused.value.next_steps == ["ask the person for a new key"]
    if raised is KeyExpired:
        assert refused.value.expired_at_ms == 1_794_000_000_000


async def test_asking_for_a_key_raises_with_the_link_and_waits(tilo, door):
    with pytest.raises(AskPending) as raised:
        await tilo.keys.create("visa-backend", workspace=WORKSPACE_ID)

    ask = raised.value
    assert ask.url == "https://mcp.tilo.sh/keys/approve/abc"
    assert ask.invocation_id == "key-abc"

    door.asks["key-abc"]["status"] = "approved"
    key = await ask.wait()

    assert isinstance(key, Key)
    assert key.token == "tilo_sk_created"
    assert key.grant == "g-created"
    assert key.scope == "account"


async def test_a_terminal_sign_in_shows_a_link_and_polls_for_the_key(tilo, door):
    sign_in = await tilo.sign_in(name="cli")

    assert isinstance(sign_in, SignIn)
    assert sign_in.url == "https://mcp.tilo.sh/keys/approve/abc"
    assert door.new_key_asked == {"name": "cli"}

    door.answer_poll()
    key = await sign_in.wait()

    assert key.token == "tilo_sk_new"
    assert door.polls >= 1


async def test_a_minted_key_makes_a_client_of_its_own(tilo):
    minted = await tilo.keys.mint(WORKSPACE_ID)

    client = minted.client()
    assert client.transport.key == "tilo_sk_minted"
    await client.aclose()


async def test_the_account_says_what_its_workspaces_kept_and_ran(tilo, door):
    usage = await tilo.usage()

    assert door.asked("tilo_read")[-1]["arguments"] == {"path": "account/usage"}
    assert usage.host == "door.test"
    assert usage.plan == "free"
    assert [row.workspace for row in usage.workspaces] == [WORKSPACE_ID, "other"]
    assert usage.workspaces[0].stored_bytes == 2048
    assert usage.vm_seconds == 90
    assert usage.running == ["other"]


async def test_a_workspace_invites_another_person_with_a_link(tilo, door):
    invitation = await tilo.workspace("jane-doe").members.invite(role="editor")

    assert invitation.link.startswith("https://tilo.sh/i/")
    assert invitation.role == "editor"
    assert invitation.email is None and not invitation.sent
    assert door.asked("tilo_do")[-1]["arguments"]["path"] == "workspace/members#invite"
    assert door.asked("tilo_do")[-1]["arguments"]["args"] == {"role": "editor"}


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
