"""Reading a workspace, acting in it, following runs and editing fields."""

from __future__ import annotations

import pytest

from tilo import (
    AskPending,
    AsyncTilo,
    Refused,
    Resource,
    Run,
)

from fake_api import PDF, TODO, WORKSPACE_ID, FakeApi, link_to

URL = "https://api.test"


@pytest.fixture
def api() -> FakeApi:
    return FakeApi()


@pytest.fixture
async def tilo(api: FakeApi):
    client = AsyncTilo(key=api.key, url=URL, http=api.client(URL))
    yield client
    await client.aclose()


@pytest.fixture
def workspace(tilo):
    return tilo.workspace("jane-doe")


async def test_listing_the_apps_and_a_resource_s_children(workspace, api):
    apps = await workspace.ls()
    assert [row.name for row in apps.rows] == ["notes", "pdf-viewer", "terminal"]
    assert apps.rows[0].state == "running"
    assert api.asked("GET", f"/v1/workspaces/{WORKSPACE_ID}/resources")[-1]["query"] == {}

    page = await workspace.ls(TODO)
    assert page.kind == "notes.Page"
    assert [row.name for row in page.rows] == ["Buy milk", "Buy bread"]
    assert page.actions[0]["id"] == "append-paragraph"
    assert api.asked("GET", "/v1/workspaces/")[-1]["query"] == {"path": TODO}


async def test_a_listing_pages_with_its_cursor(workspace, api):
    first = await workspace.ls(TODO, limit=1)
    assert [row.name for row in first.rows] == ["Buy milk"]
    assert first.next_cursor == "1"
    assert api.asked("GET", "/v1/workspaces/")[-1]["query"] == {"path": TODO, "limit": "1"}

    rest = await first.more()
    assert [row.name for row in rest.rows] == ["Buy bread"]
    assert rest.next_cursor is None
    assert rest.more() is None
    assert api.asked("GET", "/v1/workspaces/")[-1]["query"] == {
        "path": TODO,
        "limit": "1",
        "cursor": "1",
    }


async def test_a_listing_is_iterated_page_by_page(workspace):
    first = await workspace.ls(TODO, limit=1)

    names = [row.name async for row in first]

    assert names == ["Buy milk", "Buy bread"]


async def test_a_read_answers_fields_children_and_kind(workspace, api):
    page = await workspace.read(TODO)

    assert isinstance(page, Resource)
    assert page.kind == "notes.Page"
    assert page.title == "Todo"
    assert page.pinned is False
    assert page.fields["title"] == "Todo"
    assert page.children == ()
    assert api.asked("GET", f"/v1/workspaces/{WORKSPACE_ID}/resources/notes/Todo")


async def test_a_path_is_percent_encoded_one_segment_at_a_time(workspace, api):
    api.fields["notes/Q3/4 plan"] = {"title": "Q3/4 plan"}

    page = await workspace.read("notes/Q3%2F4%20plan")

    assert page.title == "Q3/4 plan"
    assert api.asked("GET", "/v1/workspaces/")[-1]["raw_path"].endswith(
        "/resources/notes/Q3%2F4%20plan"
    )


async def test_search_answers_hits(workspace, api):
    hits = await workspace.find("todo", app="notes")

    assert [hit.path for hit in hits] == [TODO, f"{TODO}#append-paragraph"]
    assert hits[1].action == "append-paragraph"
    assert api.asked("GET", "/v1/workspaces/")[-1]["query"] == {"q": "todo", "app": "notes"}


async def test_an_action_answers_its_result(workspace, api):
    result = await workspace.do(f"{TODO}#append-paragraph", text="Buy milk")

    assert result == {"appended": "c01e"}
    call = api.asked("POST", "/v1/workspaces/")[-1]
    assert call["path"] == (
        f"/v1/workspaces/{WORKSPACE_ID}/resources/notes/Todo/actions/append-paragraph"
    )
    assert call["body"] == b'{"text": "Buy milk"}'
    assert "idempotency-key" in call["headers"]
    assert "tilo-surface" not in call["headers"]


async def test_a_pending_action_is_a_run_with_a_result(workspace, api):
    api.append_pending = True

    run = await workspace.do(f"{TODO}#append-paragraph", text="Buy milk")

    assert isinstance(run, Run)
    assert run.run_id.startswith("run-")
    assert await run.result() == {"appended": "c01e"}
    assert run.link == link_to(TODO)
    assert api.asked("GET", f"/v1/workspaces/{WORKSPACE_ID}/runs/")[-1]["query"] == {"wait": "60"}


async def test_a_run_is_followed_with_the_wait_it_was_given(workspace, api):
    api.append_pending = True
    api.polls_until_done = 3
    run = await workspace.do(f"{TODO}#append-paragraph", text="Buy milk")

    answer = await run.answer(timeout=0.5)

    assert answer["status"] == "ok"
    holds = [call["query"]["wait"] for call in api.asked("GET", "/runs/")]
    assert holds[-1] in ("0", "1")


async def test_a_run_is_cancelled_and_undone(workspace, api):
    api.append_pending = True
    run = await workspace.do(f"{TODO}#append-paragraph", text="Buy milk")

    stopped = await run.cancel()
    assert stopped["status"] == "cancelled"
    assert api.cancelled == [run.run_id]

    undone = await run.undo()
    assert undone == {"status": "ok", "undone": [TODO]}
    assert api.undone == [run.run_id]


async def test_a_run_output_follows_its_events(workspace, api):
    api.append_pending = True
    run = await workspace.do(f"{TODO}#append-paragraph", text="Buy milk")

    phases = [event.phase async for event in run.output()]

    assert phases == ["started", "succeeded"]
    assert api.asked("GET", f"/v1/workspaces/{WORKSPACE_ID}/runs/{run.run_id}/output")


async def test_an_ask_raises_with_its_link_and_waits_for_the_answer(workspace, api):
    with pytest.raises(AskPending) as raised:
        await workspace.do(f"{TODO}#ask")

    ask = raised.value
    assert ask.url == "https://tilo.sh/ask/1"
    assert ask.invocation_id == "ask-1"

    api.ask_answered = True
    answer = await ask.wait()
    assert answer["result"] == {"allowed": True}


async def test_calls_inside_a_run_carry_its_id_and_title(workspace, api):
    async with workspace.run("Fill Jane's application") as group:
        await workspace.do(f"{TODO}#append-paragraph", text="Buy milk")
        await workspace.do(f"{PDF}#read-form-fields")

    calls = api.asked("POST", "/v1/workspaces/")[-2:]
    assert [call["headers"]["tilo-run"] for call in calls] == [group.id, group.id]
    assert calls[0]["headers"]["tilo-run-title"] == "Fill Jane's application"
    assert len(group.runs) == 2


async def test_a_run_hands_over_the_link_to_the_record_it_changed(workspace, api):
    async with workspace.run("Fill Jane's application") as group:
        await workspace.do(f"{TODO}#append-paragraph", text="Buy milk")
        await workspace.do(f"{PDF}#read-form-fields")
        await workspace.do(f"{TODO}#append-paragraph", text="Buy bread")

    assert group.links == [link_to(TODO)]
    assert group.link == link_to(TODO)


async def test_a_run_across_records_hands_over_each_link_once(workspace):
    async with workspace.run("Fill Jane's application") as group:
        await workspace.do(f"{TODO}#append-paragraph", text="Buy milk")
        await workspace.do(
            f"{PDF}#field-edit",
            edit={"path": [{"field": "form_values"}, {"key": "surname"}], "op": "set", "value": "Roe"},
        )

    assert group.links == [link_to(TODO), link_to(PDF)]
    assert group.link is None


async def test_any_path_has_a_link_and_a_bad_one_is_refused(workspace, api):
    assert await workspace.link(PDF) == link_to(PDF)
    assert api.asked("GET", "/v1/workspaces/")[-1]["query"] == {"path": PDF}

    await workspace.link(TODO, mode="desktop")
    assert api.asked("GET", "/v1/workspaces/")[-1]["query"] == {"path": TODO, "mode": "desktop"}

    with pytest.raises(Refused):
        await workspace.link("notes/Nothing")


async def test_a_run_group_is_undone_as_its_run(workspace, api):
    async with workspace.run("Fill Jane's application") as group:
        await workspace.do(f"{TODO}#append-paragraph", text="Buy milk")
        undone = await group.undo()

    assert undone["status"] == "ok"
    assert api.undone == [group.runs[-1]]


async def test_a_program_answers_its_output_and_steps(workspace, api):
    answer = await workspace.program("notes.notebook.notes")

    assert answer.status == "ok"
    assert list(answer.output) == ['n1 "Q3 plan"', "1 change in notes"]
    assert answer.steps[0]["apps"] == ["notes"]
    assert api.asked("POST", "/programs")[-1]["body"] == b'{"program": "notes.notebook.notes"}'


async def test_a_program_that_previews_answers_preview(workspace):
    answer = await workspace.program("preview me")

    assert answer.status == "preview"
    assert answer.changed == 4


async def test_a_program_that_fails_raises_refused(workspace):
    with pytest.raises(Refused) as raised:
        await workspace.program("boom")

    assert raised.value.code == "failed"
    assert "unknown name" in raised.value.message


async def test_a_watch_answers_events_with_their_cursor(workspace, api):
    api.events = [
        ("change", {"path": TODO, "change": "c1", "sentence": 'added "Buy milk"', "by": "Jane", "title": "Fill", "group": "sdk:fill:1", "time_ms": 10, "undone": False})
    ]
    stream = workspace.watch(TODO)

    event = await anext(stream)

    assert event.kind == "change"
    assert event.sentence == 'added "Buy milk"'
    assert event.by == "Jane"
    assert event.title == "Fill"
    assert event.group == "sdk:fill:1"
    assert api.asked("GET", "/v1/workspaces/")[-1]["query"] == {"path": TODO}
    await stream.aclose()


async def test_a_watch_asks_again_after_its_last_event_id(workspace, api):
    api.events = [
        ("change", {"path": TODO, "change": "c1", "sentence": "one", "time_ms": 1, "undone": False}),
        ("presence", {"here": [{"who": "u1", "name": "Jane", "kind": "person"}]}),
    ]
    api.page = 1
    stream = workspace.watch()

    first = await anext(stream)
    second = await anext(stream)

    assert first.change == "c1"
    assert second.kind == "presence"
    assert second.here[0].name == "Jane"
    assert api.asked("GET", "/v1/workspaces/")[-1]["headers"]["last-event-id"] == "1"
    await stream.aclose()


async def test_an_upload_answers_its_content_reference(workspace, api):
    content = await workspace.files.upload(b"passport bytes")

    assert content == "tilo.content:14"
    assert api.uploads == [b"passport bytes"]
    assert api.asked("POST", "/uploads")[-1]["headers"]["content-type"] == "application/octet-stream"

    imported = await workspace.do(f"{PDF}#import-pdf-content", content=content, name="passport.pdf")
    assert imported == {"imported": content}


async def test_a_download_answers_the_bytes(workspace, api):
    content = await workspace.files.upload(b"cover letter")

    assert await workspace.files.download(content) == b"cover letter"
    assert api.asked("GET", f"/v1/workspaces/{WORKSPACE_ID}/content/")


async def test_an_action_that_makes_a_file_answers_its_reference(workspace):
    letter = await workspace.do("word-processor#export-content", format="docx")

    assert letter == "tilo.content:15"
    assert await workspace.files.download(letter) == b"PK cover letter"


async def test_a_download_of_unknown_content_is_refused(workspace):
    with pytest.raises(Refused) as refused:
        await workspace.files.download("tilo.content:nothing")

    assert refused.value.code == "not_found"


async def test_a_field_is_read_from_the_snapshot_and_written_as_a_field_edit(workspace, api):
    page = await workspace.read(TODO)
    assert page.title == "Todo"

    page.title = "Done"
    await page.flush()

    assert api.fields[TODO]["title"] == "Done"
    call = api.asked("POST", "/actions/field-edit")[-1]
    assert call["body"] == b'{"edit": {"path": [{"field": "title"}], "op": "set", "value": "Done"}}'


async def test_a_map_field_is_edited_one_key_down(workspace, api):
    document = await workspace.read(PDF)
    assert document.form_values["surname"] == "Doe"

    document.form_values["surname"] = "Roe"
    await document.flush()

    assert api.fields[PDF]["form_values"]["surname"] == "Roe"
    assert api.asked("POST", "/actions/field-edit")[-1]["body"] == (
        b'{"edit": {"path": [{"field": "form_values"}, {"key": "surname"}], '
        b'"op": "set", "value": "Roe"}}'
    )


async def test_a_map_entry_is_taken_away(workspace, api):
    document = await workspace.read(PDF)

    await document.remove("form_values", "given_name")

    assert api.fields[PDF]["form_values"] == {"surname": "Doe"}
    assert b'"op": "remove-key"' in api.asked("POST", "/actions/field-edit")[-1]["body"]


async def test_a_field_edit_can_name_an_item(workspace, api):
    page = await workspace.read(TODO)

    await page.edit("blocks", {"item": "c01e"}, "text", value="Buy oat milk")

    assert api.asked("POST", "/actions/field-edit")[-1]["body"] == (
        b'{"edit": {"path": [{"field": "blocks"}, {"item": "c01e"}, {"key": "text"}], '
        b'"op": "set", "value": "Buy oat milk"}}'
    )


async def test_a_method_call_is_the_action(workspace, api):
    document = await workspace.read(PDF)

    result = await document.read_form_fields()

    assert result == {"fields": ["surname", "given_name"]}
    assert api.asked("POST", "/actions/read-form-fields")


async def test_a_resource_is_read_before_its_fields(workspace):
    resource = workspace.resource(TODO)

    with pytest.raises(AttributeError) as raised:
        resource.title

    assert "load()" in str(raised.value)

    await resource.load()
    assert resource.title == "Todo"
