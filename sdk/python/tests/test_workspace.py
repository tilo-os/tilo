"""Reading a workspace, acting in it, following runs and editing fields."""

from __future__ import annotations

import pytest

from tilo import (
    AskPending,
    AsyncTilo,
    ProgramAnswer,
    Refused,
    Resource,
    Run,
)

from fake_door import PDF, TODO, FakeDoor, link_to


@pytest.fixture
def door() -> FakeDoor:
    return FakeDoor()


@pytest.fixture
async def tilo(door: FakeDoor):
    client = AsyncTilo(key=door.key, door="https://door.test", http=door.client())
    yield client
    await client.aclose()


@pytest.fixture
def workspace(tilo):
    return tilo.workspace("jane-doe")


async def test_listing_the_apps_and_a_resource_s_children(workspace):
    apps = await workspace.ls()
    assert [row.name for row in apps.rows] == ["notes", "pdf-viewer", "terminal"]
    assert apps.rows[0].state == "running"

    page = await workspace.ls(TODO)
    assert page.kind == "notes.Page"
    assert [row.name for row in page.rows] == ["Buy milk", "Buy bread"]
    assert page.actions[0]["id"] == "append-paragraph"


async def test_a_listing_pages_with_its_cursor(workspace):
    first = await workspace.ls(TODO, limit=1)
    assert [row.name for row in first.rows] == ["Buy milk"]
    assert first.next_cursor == "1"

    rest = await first.more()
    assert [row.name for row in rest.rows] == ["Buy bread"]
    assert rest.next_cursor is None
    assert rest.more() is None


async def test_a_listing_is_iterated_page_by_page(workspace):
    first = await workspace.ls(TODO, limit=1)
    names = [row.name async for row in first]
    assert names == ["Buy milk", "Buy bread"]


async def test_a_read_answers_fields_children_and_kind(workspace):
    page = await workspace.read(TODO)

    assert isinstance(page, Resource)
    assert page.kind == "notes.Page"
    assert page.title == "Todo"
    assert page.pinned is False
    assert page.fields["title"] == "Todo"
    assert page.children == ()


async def test_search_answers_hits(workspace):
    hits = await workspace.find("todo", app="notes")

    assert [hit.path for hit in hits] == [TODO, f"{TODO}#append-paragraph"]
    assert hits[1].action == "append-paragraph"


async def test_an_action_answers_its_result(workspace, door):
    result = await workspace.do(f"{TODO}#append-paragraph", text="Buy milk")

    assert result == {"appended": "c01e"}
    call = door.asked("tilo_do")[-1]
    assert call["arguments"] == {
        "path": f"{TODO}#append-paragraph",
        "args": {"text": "Buy milk"},
    }


async def test_a_refusal_raises_with_the_host_s_words(workspace):
    with pytest.raises(Refused) as raised:
        await workspace.do(f"{TODO}#nonsense")

    assert raised.value.code == "invalid_arguments"
    assert "no action" in raised.value.message
    assert raised.value.next_steps == [f"tilo_ls {TODO} lists its actions"]


async def test_a_pending_action_is_a_run_with_a_result(workspace, door):
    door.append_pending = True

    run = await workspace.do(f"{TODO}#append-paragraph", text="Buy milk")

    assert isinstance(run, Run)
    assert run.invocation_id.startswith("inv-")
    assert await run.result() == {"appended": "c01e"}


async def test_a_run_is_cancelled_and_undone(workspace, door):
    door.append_pending = True
    run = await workspace.do(f"{TODO}#append-paragraph", text="Buy milk")

    stopped = await run.cancel()
    assert stopped["result"] == {"cancelled": run.invocation_id}

    undone = await run.undo()
    assert undone == {"undone": 1}
    assert door.asked("tilo_do")[-1]["arguments"]["path"] == f"{TODO}#undo"


async def test_a_run_output_follows_its_events(workspace, door):
    door.append_pending = True
    run = await workspace.do(f"{TODO}#append-paragraph", text="Buy milk")
    door.events = [
        {"kind": "run", "phase": "started", "invocation_id": run.invocation_id, "path": f"{TODO}#append-paragraph", "run": "Fill", "run_id": "mcp:1", "time_ms": 1},
        {"kind": "change", "path": TODO, "change": "c1", "sentence": "added Buy milk", "by": "Jane", "time_ms": 2, "undone": False},
        {"kind": "run", "phase": "succeeded", "invocation_id": run.invocation_id, "path": f"{TODO}#append-paragraph", "run": "Fill", "run_id": "mcp:1", "time_ms": 3},
    ]

    phases = [event.phase async for event in run.output()]

    assert phases == ["started", "succeeded"]


async def test_an_ask_raises_with_its_link_and_waits_for_the_answer(workspace, door):
    with pytest.raises(AskPending) as raised:
        await workspace.do(f"{TODO}#ask")

    ask = raised.value
    assert ask.url == "https://mcp.tilo.sh/ask/1"
    assert ask.invocation_id == "ask-1"

    door.ask_answered = True
    answer = await ask.wait()
    assert answer["result"] == {"allowed": True}


async def test_calls_inside_a_run_carry_its_id_and_title(workspace, door):
    async with workspace.run("Fill Jane's application") as group:
        await workspace.do(f"{TODO}#append-paragraph", text="Buy milk")
        await workspace.do(f"{PDF}#read-form-fields")

    calls = door.asked("tilo_do")[-2:]
    assert [call["run"]["id"] for call in calls] == [group.id, group.id]
    assert calls[0]["run"]["title"] == "Fill Jane's application"
    assert len(group.invocations) == 2


async def test_a_run_hands_over_the_link_to_the_record_it_changed(workspace, door):
    async with workspace.run("Fill Jane's application") as group:
        await workspace.do(f"{TODO}#append-paragraph", text="Buy milk")
        await workspace.do(f"{PDF}#read-form-fields")
        await workspace.do(f"{TODO}#append-paragraph", text="Buy bread")

    assert group.links == [link_to(TODO)]
    assert group.link == link_to(TODO)


async def test_a_run_across_records_hands_over_each_link_once(workspace, door):
    async with workspace.run("Fill Jane's application") as group:
        await workspace.do(f"{TODO}#append-paragraph", text="Buy milk")
        await workspace.do(f"{PDF}#field-edit", edit={"path": "form_values.surname", "op": "set", "value": "Roe"})

    assert group.links == [link_to(TODO), link_to(PDF)]
    assert group.link is None


async def test_a_finished_run_names_its_link(workspace, door):
    door.append_pending = True
    run = await workspace.do(f"{TODO}#append-paragraph", text="Buy milk")
    assert isinstance(run, Run)
    assert run.link is None

    await run.result()

    assert run.link == link_to(TODO)


async def test_any_path_has_a_link_and_a_bad_one_is_refused(workspace, door):
    assert await workspace.link(PDF) == link_to(PDF)
    assert door.asked("tilo_link")[-1]["arguments"] == {"path": PDF}

    await workspace.link(TODO, mode="desktop")
    assert door.asked("tilo_link")[-1]["arguments"] == {"path": TODO, "mode": "desktop"}

    with pytest.raises(Refused):
        await workspace.link("notes/Nothing")


async def test_a_conversation_is_read_from_the_chat_record(workspace, door):
    chain = "derived:messages@derived:default@tab:0000000000000000"
    door.records[("so.tilo.chat", chain)] = {"messages": {"m1": {"channel": "general", "text": "hi"}}}

    read = await workspace.record("so.tilo.chat", chain, parts=True, after="r0")

    assert read["records"][0]["value"]["messages"]["m1"]["text"] == "hi"
    assert door.asked("tilo_read")[-1]["arguments"] == {
        "path": f"record:so.tilo.chat:{chain}",
        "parts": True,
        "after": "r0",
    }
    absent = await workspace.record("so.tilo.chat")
    assert absent["records"] == [{"record": "", "absent": True}]


async def test_a_run_group_is_undone_as_one_task(workspace, door):
    async with workspace.run("Fill Jane's application") as group:
        await workspace.do(f"{TODO}#append-paragraph", text="Buy milk")
        undone = await group.undo()

    assert isinstance(undone, ProgramAnswer)
    assert undone.status == "ok"
    assert door.asked("tilo")[-1]["arguments"] == {"program": "undo task"}


async def test_a_program_answers_its_output_and_steps(workspace, door):
    answer = await workspace.program("notes.notebook.notes")

    assert answer.status == "ok"
    assert list(answer.output) == ['n1 "Q3 plan"', "1 change in notes"]
    assert answer.steps[0]["apps"] == ["notes"]
    assert door.asked("tilo")[-1]["arguments"] == {"program": "notes.notebook.notes"}


async def test_a_program_that_previews_answers_preview(workspace):
    answer = await workspace.program("preview me")

    assert answer.status == "preview"
    assert answer.changed == 4


async def test_a_program_that_fails_raises_refused(workspace):
    with pytest.raises(Refused) as raised:
        await workspace.program("boom")

    assert raised.value.code == "compile_error"
    assert raised.value.next_steps == ["tilo_ls notes names its resources"]


async def test_a_watch_keeps_its_cursor_and_answers_events(workspace, door):
    door.events = [
        {"kind": "change", "path": TODO, "change": "c1", "sentence": 'added "Buy milk"', "by": "Jane", "time_ms": 10, "undone": False}
    ]
    stream = workspace.watch(TODO)

    event = await anext(stream)

    assert event.kind == "change"
    assert event.sentence == 'added "Buy milk"'
    assert event.by == "Jane"
    assert door.asked("tilo_watch")[-1]["arguments"]["path"] == TODO
    await stream.aclose()


async def test_a_watch_asks_again_from_its_cursor(workspace, door):
    door.events = [
        {"kind": "change", "path": TODO, "change": "c1", "sentence": "one", "time_ms": 1, "undone": False},
        {"kind": "presence", "here": [{"who": "u1", "name": "Jane", "kind": "person"}]},
    ]
    door.watch_page = 1
    stream = workspace.watch()

    first = await anext(stream)
    second = await anext(stream)

    assert first.change == "c1"
    assert second.kind == "presence"
    assert second.here[0].name == "Jane"
    assert door.asked("tilo_watch")[-1]["arguments"]["cursor"] == "1"
    await stream.aclose()


async def test_an_upload_answers_its_content_reference(workspace, door):
    content = await workspace.files.upload(b"passport bytes")

    assert content == "tilo.content:14"
    assert door.uploads == [b"passport bytes"]

    imported = await workspace.do(f"{PDF}#import-pdf-content", content=content, name="passport.pdf")
    assert imported == {"imported": content}


async def test_a_download_gathers_every_piece(workspace, door):
    content = await workspace.files.upload(b"cover letter")

    assert await workspace.files.download(content) == b"cover letter"
    assert [call["arguments"]["offset"] for call in door.asked("tilo_get")] == [0, 4, 8]


async def test_an_action_that_makes_a_file_answers_its_reference(workspace, door):
    letter = await workspace.do("word-processor#export-content", format="docx")

    assert letter == "tilo.content:15"
    assert await workspace.files.download(letter) == b"PK cover letter"


async def test_a_download_of_unknown_content_is_refused(workspace):
    with pytest.raises(Refused) as refused:
        await workspace.files.download("tilo.content:nothing")

    assert refused.value.code == "not_found"


async def test_a_field_is_read_from_the_snapshot_and_written_as_a_field_edit(workspace, door):
    page = await workspace.read(TODO)
    assert page.title == "Todo"

    page.title = "Done"
    await page.flush()

    assert door.edits == [
        {"resource": TODO, "edit": {"path": [{"field": "title"}], "op": "set", "value": "Done"}}
    ]
    assert door.fields[TODO]["title"] == "Done"


async def test_a_map_field_is_edited_one_key_down(workspace, door):
    document = await workspace.read(PDF)
    assert document.form_values["surname"] == "Doe"

    document.form_values["surname"] = "Roe"
    await document.flush()

    assert door.edits[-1] == {
        "resource": PDF,
        "edit": {
            "path": [{"field": "form_values"}, {"key": "surname"}],
            "op": "set",
            "value": "Roe",
        },
    }
    assert door.fields[PDF]["form_values"]["surname"] == "Roe"


async def test_a_map_entry_is_taken_away(workspace, door):
    document = await workspace.read(PDF)

    await document.remove("form_values", "given_name")

    assert door.edits[-1]["edit"]["op"] == "remove-key"
    assert door.fields[PDF]["form_values"] == {"surname": "Doe"}


async def test_a_field_edit_can_name_an_item(workspace, door):
    page = await workspace.read(TODO)

    await page.edit("blocks", {"item": "c01e"}, "text", value="Buy oat milk")

    assert door.edits[-1]["edit"]["path"] == [
        {"field": "blocks"},
        {"item": "c01e"},
        {"key": "text"},
    ]


async def test_a_method_call_is_the_action(workspace, door):
    document = await workspace.read(PDF)

    result = await document.read_form_fields()

    assert result == {"fields": ["surname", "given_name"]}
    assert door.asked("tilo_do")[-1]["arguments"]["path"] == f"{PDF}#read-form-fields"


async def test_a_resource_is_read_before_its_fields(tilo, workspace):
    resource = workspace.resource(TODO)

    with pytest.raises(AttributeError) as raised:
        resource.title

    assert "load()" in str(raised.value)

    await resource.load()
    assert resource.title == "Todo"
