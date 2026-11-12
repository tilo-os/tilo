"""The generated types are the apps' resources: a class's fields and its actions."""

from __future__ import annotations

import pytest

from tilo import AsyncTilo
from tilo.types.pdf_viewer import Document, FormFields

from fake_door import PDF, FakeDoor


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


async def test_a_generated_class_reads_the_apps_fields(workspace, door):
    door.fields[PDF]["source"] = "tilo.content:abc"

    form = await Document(workspace, PDF).load()

    assert form.page_count == 3
    assert form.source == "tilo.content:abc"
    assert form.form_values["surname"] == "Doe"


async def test_an_assignment_is_a_field_edit(workspace, door):
    form = await Document(workspace, PDF).load()

    form.reading_fraction = 0.5
    await form.flush()

    assert door.edits[-1] == {
        "resource": PDF,
        "edit": {"path": [{"field": "reading_fraction"}], "op": "set", "value": 0.5},
    }
    assert door.fields[PDF]["reading_fraction"] == 0.5


async def test_a_generated_method_is_the_apps_action(workspace, door):
    form = await Document(workspace, PDF).load()

    answer: FormFields = await form.read_form_fields()

    assert answer == {"fields": ["surname", "given_name"]}
    assert door.asked("tilo_do")[-1]["arguments"]["path"] == f"{PDF}#read-form-fields"
