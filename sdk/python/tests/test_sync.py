"""The sync client: the same surface, no `await`, its own event loop."""

from __future__ import annotations

import pytest

from tilo import Key, ProgramAnswer, Refused, Resource, Run, Tilo

from fake_door import PDF, TODO, FakeDoor


@pytest.fixture
def door() -> FakeDoor:
    return FakeDoor()


def test_the_sync_client_reads_acts_and_edits(door):
    with Tilo(key=door.key, door="https://door.test", http=door.client()) as tilo:
        listing = tilo.workspaces.list()
        assert [workspace.name for workspace in listing] == ["jane-doe", "john-doe"]

        page = tilo.workspace("jane-doe").read(TODO)
        assert isinstance(page, Resource)
        assert page.title == "Todo"

        page.title = "Done"
        assert door.fields[TODO]["title"] == "Done"

        result = tilo.workspace("jane-doe").do(f"{TODO}#append-paragraph", text="Buy milk")
        assert result == {"appended": "c01e"}


def test_a_sync_run_group_names_the_run(door):
    with Tilo(key=door.key, door="https://door.test", http=door.client()) as tilo:
        workspace = tilo.workspace("jane-doe")

        with workspace.run("Fill Jane's application") as group:
            workspace.do(f"{PDF}#read-form-fields")

        assert door.asked("tilo_do")[-1]["run"] == {
            "id": group.id,
            "title": "Fill Jane's application",
        }

        undone = group.undo()
        assert isinstance(undone, ProgramAnswer)


def test_a_sync_run_is_followed_to_its_result(door):
    door.append_pending = True

    with Tilo(key=door.key, door="https://door.test", http=door.client()) as tilo:
        run = tilo.workspace("jane-doe").do(f"{TODO}#append-paragraph", text="Buy milk")

        assert isinstance(run, Run)
        assert run.result() == {"appended": "c01e"}


def test_a_sync_watch_is_walked_one_event_at_a_time(door):
    door.events = [
        {"kind": "change", "path": TODO, "change": "c1", "sentence": "one", "time_ms": 1, "undone": False}
    ]

    with Tilo(key=door.key, door="https://door.test", http=door.client()) as tilo:
        stream = tilo.workspace("jane-doe").watch()

        event = next(stream)
        assert event.change == "c1"
        stream.close()


def test_a_sync_refusal_raises_at_the_call(door):
    with Tilo(key=door.key, door="https://door.test", http=door.client()) as tilo:
        with pytest.raises(Refused) as raised:
            tilo.workspace("jane-doe").do(f"{TODO}#nonsense")

    assert raised.value.code == "invalid_arguments"


def test_a_sync_key_is_minted_and_revoked(door):
    with Tilo(key=door.key, door="https://door.test", http=door.client()) as tilo:
        minted = tilo.workspace("jane-doe").mint_mcp()
        assert minted.token == "tilo_sk_minted"

        key: Key = tilo.keys.list()[0]
        assert key.grant == "g-root"
