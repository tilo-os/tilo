"""The SDK against a real daemon: one workspace, driven through the public surface only.

`tools/ci/sdk-native.sh` starts a daemon in a profile of its own and runs this
suite with `TILO_RUNTIME_PROFILE` naming it, so `Tilo()` reaches it and is
admitted as the person. Anywhere else there is no daemon to reach, so the
suite skips.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import os
import uuid
from typing import Any, Mapping

import pytest

from tilo import Refused, Tilo, Workspace, rows, run

pytestmark = pytest.mark.skipif(
    not os.environ.get("TILO_RUNTIME_PROFILE"),
    reason="needs a daemon: run it through tools/ci/sdk-native.sh, which starts one",
)

TITLE = "Made by the SDK's test"
# The Tasks app's action that makes a task, and the task list it adds it to.
NEW_TASK = "so.tilo.tasks#new_task"
TASK_LIST = "so.tilo.tasks/Tasks"
TEXT = "Every call the test makes is an API request of the workspace."
ONCE = uuid.uuid4().hex[:8]


async def act(workspace: Workspace, address: str, args: Mapping[str, Any] | None = None) -> Any:
    """Run the action `address` names (`app/resource#action`) until it finished: what it produced."""
    at = address.rindex("#")
    answer = await run(workspace, {"path": address[:at], "action": address[at + 1 :]}, args or {})
    return answer.get("result") if isinstance(answer, Mapping) else None


async def made(workspace: Workspace, address: str, args: Mapping[str, Any]) -> str:
    """The id a call that made something names it by: its result's reference, or what its answer
    says it made."""
    answer = await run(
        workspace,
        {"path": address[: address.rindex("#")], "action": address[address.rindex("#") + 1 :]},
        args,
    )
    result = answer.get("result") if isinstance(answer, Mapping) else None
    if isinstance(result, str):
        return result
    if isinstance(result, Mapping) and isinstance(result.get("id"), str):
        return result["id"]
    resources = answer.get("resources") if isinstance(answer, Mapping) else None
    first = resources[0] if isinstance(resources, list) and resources else None
    if isinstance(first, str):
        return first
    if isinstance(first, Mapping) and isinstance(first.get("id"), str):
        return first["id"]
    raise AssertionError(f"the call names what it made: {answer!r}")


async def test_the_sdk_drives_the_daemon() -> None:
    async with Tilo() as tilo:
        workspace = tilo.workspace()

        assert await workspace.id()

        # The workspace lists its apps: one of them makes a record the test reads back.
        apps = [row["path"] async for row in rows(workspace)]
        assert apps, "the workspace lists its apps"
        assert "tasks" in apps, f"the workspace runs its Tasks app: {sorted(apps)}"

        # An action makes a task, and a read of the task list sees it.
        await act(workspace, NEW_TASK, {"title": TITLE})
        read = await workspace.resource(TASK_LIST).load()
        assert TITLE in str(read.fields), read.fields

        # A watch sees the change a call makes while it is open.
        events = workspace.events()

        async def a_change() -> Mapping[str, Any]:
            async for event in events:
                if event["type"] == "change":
                    return event
            raise AssertionError("the watch ended without a change")

        watching = asyncio.ensure_future(a_change())
        await asyncio.sleep(1.0)
        await act(workspace, NEW_TASK, {"title": f"{TITLE} again"})
        try:
            event = await asyncio.wait_for(watching, 30)
        finally:
            await events.aclose()
        assert event["data"].get("path") or event["type"] == "change"

        # A call of nothing the workspace has is refused, with the code a program branches on.
        with pytest.raises(Refused) as refused:
            await act(workspace, "nosuchapp/x#nothing")
        assert isinstance(refused.value.code, str) and refused.value.code

        # The bytes go in and come back out the same.
        data = TEXT.encode() * 1000
        content = await workspace.upload(data)
        assert content.startswith("tilo.content:")
        assert await workspace.download(content) == data


async def test_a_new_message_and_its_edit_are_told_apart_in_one_conversation() -> None:
    async with Tilo() as tilo:
        workspace = tilo.workspace()
        channel = await made(workspace, "so.tilo.chat#new_channel", {"name": f"sdk-watch-{ONCE}"})
        events = workspace.events()
        seen: list[Mapping[str, Any]] = []

        async def changes() -> None:
            async for event in events:
                if event["type"] == "change":
                    seen.append(event)

        watching = asyncio.ensure_future(changes())
        await asyncio.sleep(1.0)
        message = await made(workspace, f"so.tilo.chat/{channel}#send", {"body": "first words"})
        await act(workspace, f"so.tilo.chat/{message}#edit", {"body": "second words"})

        async def both() -> None:
            while len([event for event in seen if message in json.dumps(event["data"])]) < 2:
                await asyncio.sleep(0.1)

        try:
            await asyncio.wait_for(both(), 30)
        finally:
            watching.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await watching
        told = [event for event in seen if message in json.dumps(event["data"])]
        sent, edited = told[0]["data"], told[-1]["data"]
        # The store says what a change did to its record, next to `change`: the new message
        # reads created and its edit reads edited, in the event's own `what`.
        assert sent.get("what") == "created", ("the new message's event says it was created", sent)
        assert edited.get("what") == "edited", ("the edit's event says it was edited", edited)
        assert sent.get("change") != edited.get("change")
