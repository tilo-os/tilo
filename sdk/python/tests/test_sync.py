"""The sync client: the same surface, no `await`, its own event loop."""

from __future__ import annotations

import pytest

from tilo import AskPending, Invocation, Key, Refused, Resource, Tilo

from fake_api import PDF, TODO, WORKSPACE_ID, FakeApi

URL = "https://api.test"


@pytest.fixture
def api() -> FakeApi:
    return FakeApi()


def test_the_sync_client_reads_acts_and_edits(api):
    with Tilo(key=api.key, url=URL, http=api.client(URL)) as tilo:
        listing = tilo.workspaces.list()
        assert [workspace.name for workspace in listing] == [
            "jane-doe",
            "john-doe",
            "applicant-jane-doe",
        ]

        page = tilo.workspace("jane-doe").read(TODO)
        assert isinstance(page, Resource)
        assert page.title == "Todo"

        page.title = "Done"
        assert api.fields[TODO]["title"] == "Done"

        result = tilo.workspace("jane-doe").do(f"{TODO}#append-paragraph", text="Buy milk")
        assert result == {"appended": "c01e"}


def test_a_sync_run_group_names_the_run(api):
    with Tilo(key=api.key, url=URL, http=api.client(URL)) as tilo:
        workspace = tilo.workspace("jane-doe")

        with workspace.invocation("Fill Jane's application") as group:
            workspace.do(f"{PDF}#read-form-fields")

        call = api.asked("POST", "/actions/read-form-fields")[-1]
        assert call["headers"]["tilo-invocation"] == group.id
        assert call["headers"]["tilo-invocation-title"] == "Fill Jane's application"

        undone = group.undo()
        assert undone["status"] == "ok"
        assert api.undone == [group.invocations[-1]]


def test_a_sync_run_is_followed_to_its_result(api):
    api.append_pending = True

    with Tilo(key=api.key, url=URL, http=api.client(URL)) as tilo:
        invocation = tilo.workspace("jane-doe").do(f"{TODO}#append-paragraph", text="Buy milk")

        assert isinstance(invocation, Invocation)
        assert invocation.result() == {"appended": "c01e"}


def test_a_sync_watch_is_walked_one_event_at_a_time(api):
    api.events = [
        ("change", {"path": TODO, "change": "c1", "sentence": "one", "time_ms": 1, "undone": False})
    ]

    with Tilo(key=api.key, url=URL, http=api.client(URL)) as tilo:
        stream = tilo.workspace("jane-doe").watch()

        event = next(stream)
        assert event.change == "c1"
        stream.close()


def test_a_sync_refusal_raises_at_the_call(api):
    with Tilo(key=api.key, url=URL, http=api.client(URL)) as tilo:
        with pytest.raises(Refused) as raised:
            tilo.workspace("jane-doe").do(f"{TODO}#nonsense")

    assert raised.value.code == "invalid_arguments"


def test_a_sync_key_is_minted_and_listed(api):
    with Tilo(key=api.key, url=URL, http=api.client(URL)) as tilo:
        minted = tilo.workspace("jane-doe").keys.mint()
        assert minted.token == "tilo_sk_minted"
        assert minted.workspace == WORKSPACE_ID

        key: Key = tilo.keys.list()[0]
        assert key.grant == "g-root"


def test_a_sync_listing_of_invocations_is_walked_to_the_oldest(api):
    with Tilo(key=api.key, url=URL, http=api.client(URL)) as tilo:
        workspace = tilo.workspace("jane-doe")
        workspace.do(f"{TODO}#append-paragraph", text="Buy milk")
        workspace.do(f"{TODO}#append-paragraph", text="Buy bread")
        listed_invocations = workspace.invocations(limit=1)
        assert listed_invocations.total == 2
        assert [listed.id for listed in listed_invocations] == [
            listed["id"] for listed in reversed(api.invocations)
        ]


def test_a_sync_branch_answers_the_new_workspace(api):
    with Tilo(key=api.key, url=URL, http=api.client(URL)) as tilo:
        created = tilo.workspaces.create("Template")
        branch = tilo.workspaces.branch(created, "Acme")
        again = tilo.workspace("jane-doe").branch("jane-doe, second try")

        assert branch.name == "Acme"
        assert again.name == "jane-doe, second try"
        assert api.asked("POST", "/workspaces/actions/branch")[0]["body"] == (
            b'{"workspace": "' + created.id.encode() + b'", "name": "Acme"}'
        )


def test_a_sync_ask_waits_where_the_sync_client_runs(api):
    with Tilo(key=api.key, url=URL, http=api.client(URL)) as tilo:
        with pytest.raises(AskPending) as raised:
            tilo.keys.create("nightly", workspace=WORKSPACE_ID)
        ask = raised.value
        assert api.new_key_asked == {"name": "nightly", "workspace": WORKSPACE_ID}

        api.asked_key = {
            "key": "tilo_sk_new",
            "grant": "g-new",
            "name": "nightly",
            "scope": "account",
        }
        key = ask.wait()

        assert isinstance(key, Key)
        assert key.token == "tilo_sk_new"
        assert api.polls_of_key >= 1
