"""The sync client: the same surface, no `await`, its own event loop."""

from __future__ import annotations

import pytest

from tilo import Key, Refused, Resource, Run, Tilo

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

        with workspace.run("Fill Jane's application") as group:
            workspace.do(f"{PDF}#read-form-fields")

        call = api.asked("POST", "/actions/read-form-fields")[-1]
        assert call["headers"]["tilo-run"] == group.id
        assert call["headers"]["tilo-run-title"] == "Fill Jane's application"

        undone = group.undo()
        assert undone["status"] == "ok"
        assert api.undone == [group.runs[-1]]


def test_a_sync_run_is_followed_to_its_result(api):
    api.append_pending = True

    with Tilo(key=api.key, url=URL, http=api.client(URL)) as tilo:
        run = tilo.workspace("jane-doe").do(f"{TODO}#append-paragraph", text="Buy milk")

        assert isinstance(run, Run)
        assert run.result() == {"appended": "c01e"}


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
