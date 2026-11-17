"""A bulk call: one all-or-nothing step per call, and every call one invocation group."""

from __future__ import annotations

import json

import pytest

from tilo import AsyncTilo, Tilo

from tilo.types.tasks import Issues

from fake_api import TASKS, FakeApi

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


async def test_create_many_sends_a_few_bulk_calls_of_one_invocation(workspace, api):
    issues = await Issues(workspace, TASKS).load()
    items = [{"title": f"Issue {index}", "status": "todo"} for index in range(1200)]

    answers = await issues.create_many(items)

    calls = api.asked("POST", "/actions/group")
    assert len(calls) == 3
    assert len({call["headers"]["tilo-invocation"] for call in calls}) == 1
    assert len({call["headers"]["tilo-invocation-title"] for call in calls}) == 1
    members = [
        member for call in calls for member in json.loads(call["body"])["actions"]
    ]
    assert members == [{"action": "create", "args": item} for item in items]
    assert [answer["result"]["applied"] for answer in answers] == [500, 500, 200]


async def test_an_empty_bulk_call_sends_nothing(workspace, api):
    issues = await Issues(workspace, TASKS).load()
    sent = len(api.requests)

    assert await issues.create_many([]) == []
    assert len(api.requests) == sent


async def test_a_plain_bulk_call_is_a_step_of_the_invocation_it_is_in(workspace, api):
    issues = await Issues(workspace, TASKS).load()

    async with workspace.invocation("Import the backlog"):
        await issues.many("create", [{"title": "One"}, {"title": "Two"}])

    calls = api.asked("POST", "/actions/group")
    assert len(calls) == 1
    # The caller's own invocation names it: the bulk call does not open a second one.
    assert calls[0]["headers"]["tilo-invocation-title"] == "Import the backlog"


def test_the_sync_client_runs_a_bulk_call_too(api):
    with Tilo(key=api.key, url=URL, http=api.client(URL)) as tilo:
        issues = Issues(tilo.workspace("jane-doe"), TASKS).load()
        answers = issues.create_many([{"title": "One"}, {"title": "Two"}])

    calls = api.asked("POST", "/actions/group")
    assert len(calls) == 1
    assert len({call["headers"]["tilo-invocation"] for call in calls}) == 1
    assert answers[0]["result"] == {"applied": 2}
