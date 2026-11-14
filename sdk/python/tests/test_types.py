"""The generated types are the apps' resources: a class's fields and its actions."""

from __future__ import annotations

import pytest

from tilo import AsyncTilo, SurfaceChanged

from tilo.types.pdf_viewer import Document, FormFields

from fake_api import PDF, FakeApi

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


async def test_a_generated_class_reads_the_apps_fields(workspace, api):
    api.fields[PDF]["source"] = "tilo.content:abc"

    form = await Document(workspace, PDF).load()

    assert form.page_count == 3
    assert form.source == "tilo.content:abc"
    assert form.form_values["surname"] == "Doe"


async def test_an_assignment_is_a_field_edit(workspace, api):
    form = await Document(workspace, PDF).load()

    form.reading_fraction = 0.5
    await form.flush()

    assert api.fields[PDF]["reading_fraction"] == 0.5
    assert api.asked("POST", "/actions/field-edit")[-1]["body"] == (
        b'{"edit": {"path": [{"field": "reading_fraction"}], "op": "set", "value": 0.5}}'
    )


async def test_a_generated_method_is_the_apps_action(workspace, api):
    form = await Document(workspace, PDF).load()

    answer: FormFields = await form.read_form_fields()

    assert answer == {"fields": ["surname", "given_name"]}
    assert api.asked("POST", "/actions/read-form-fields")


async def test_a_generated_class_names_the_surface_its_types_are_for(workspace, api):
    form = await Document(workspace, PDF).load()

    await form.read_form_fields()

    call = api.asked("POST", "/actions/read-form-fields")[-1]
    assert call["headers"]["tilo-surface"] == Document.__surface__


async def test_types_written_for_another_surface_say_to_write_them_again(
    workspace, api, monkeypatch
):
    monkeypatch.setattr(Document, "__surface__", "so.tilo.pdf-viewer@ffffffffffffffff", raising=False)
    form = await Document(workspace, PDF).load()

    with pytest.raises(SurfaceChanged) as raised:
        await form.read_form_fields()

    assert raised.value.code == "surface_changed"
    assert "tilo sdk gen" in raised.value.message
    assert raised.value.next_steps == [
        "write the types again: tilo sdk gen -w 11111111-2222-3333-4444-555555555555"
    ]


def test_a_listing_describes_each_kinds_actions_once() -> None:
    from tilo._models import Listing

    rename = {"id": "rename", "label": "Rename note", "kind": "undoable", "params": [{"name": "title"}]}
    remove = {"id": "delete", "label": "Delete note", "kind": "undoable", "params": []}
    listing = Listing.of(
        {
            "path": "notes/Notebook",
            "rows": [
                {"path": "notes/Notebook/a", "name": "a", "kind": "notes.Page", "actions": ["rename", "delete"]},
                {"path": "notes/Notebook/b", "name": "b", "kind": "notes.Page", "actions": ["rename"]},
            ],
            "kinds": {"notes.Page": [rename, remove]},
        }
    )
    assert [action["id"] for action in listing.rows[0].actions] == ["rename", "delete"]
    assert [action["id"] for action in listing.rows[1].actions] == ["rename"]
    assert listing.rows[1].actions[0]["params"][0]["name"] == "title"
