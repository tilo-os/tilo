"""The SDK against a real daemon: one workspace, driven through the public surface only.

`tools/ci/sdk-native.sh` starts a daemon in a profile of its own and runs this
suite with `TILO_RUNTIME_PROFILE` naming it, so `AsyncTilo()` reaches it and is
admitted as the person. Anywhere else there is no daemon to reach, so the
suite skips.
"""

from __future__ import annotations

import asyncio
import os

import pytest

from tilo import AsyncTilo, Event, EventWhat, Refused, Workspace

pytestmark = pytest.mark.skipif(
    not os.environ.get("TILO_RUNTIME_PROFILE"),
    reason="needs a daemon: run it through tools/ci/sdk-native.sh, which starts one",
)

TITLE = "Made by the SDK's test"
# The Tasks app's action that makes a task, and the task list it adds it to.
NEW_TASK = "so.tilo.tasks#new_task"
TASK_LIST = "so.tilo.tasks/tl:so.tilo.tasks"
TEXT = "Every call the test makes is a tool call of the agent surface."


async def a_change(workspace: Workspace) -> Event:
    """The next change the workspace tells of."""
    async for event in workspace.watch():
        if event.kind == "change":
            return event
    raise AssertionError("the watch ended without a change")


async def test_the_sdk_drives_the_daemon() -> None:
    async with AsyncTilo() as tilo:
        workspace = tilo.workspace()

        # The workspace lists its apps: one of them makes a record the test reads back.
        listing = await workspace.ls()
        apps = {row.path for row in listing.rows}
        assert apps, "the workspace lists its apps"

        assert "tasks" in apps, f"the workspace runs its Tasks app: {sorted(apps)}"

        # An action makes a task, and a read of the task list sees it.
        await workspace.do(NEW_TASK, title=TITLE)
        read = await workspace.read(TASK_LIST)
        assert TITLE in str(read.fields), read.fields

        # A watch sees the change a call makes while it is open.
        watching = asyncio.create_task(a_change(workspace))
        await asyncio.sleep(1.0)
        await workspace.do(NEW_TASK, title=f"{TITLE} again")
        event = await asyncio.wait_for(watching, 30)
        assert isinstance(event.path, str) and event.path

        # A call of nothing the workspace has is refused, with the code a program branches on.
        with pytest.raises(Refused) as refused:
            await workspace.do("nosuchapp/x#nothing")
        assert isinstance(refused.value.code, str) and refused.value.code

        # The bytes go in and come back out the same.
        data = TEXT.encode() * 1000
        content = await workspace.files.upload(data)
        assert content.startswith("tilo.content:")
        assert await workspace.files.download(content) == data


def made(answer: object) -> str:
    """The id a call that made something names it by: its result's reference, or the resources
    of the answer it kept."""
    raw = getattr(answer, "raw", answer)
    if isinstance(raw, str):
        return raw
    assert isinstance(raw, dict), f"the call names what it made: {raw!r}"
    if isinstance(raw.get("id"), str):
        return raw["id"]
    found = raw.get("resources")
    assert isinstance(found, list) and found, f"the call names what it made: {raw!r}"
    first = found[0]
    return first if isinstance(first, str) else first["id"]


async def changes(workspace: Workspace, into: list[Event]) -> None:
    """Every change the workspace tells of, appended to `into` as it comes."""
    async for event in workspace.watch():
        if event.kind == "change":
            print("watched:", event.raw, flush=True)
            into.append(event)


async def test_a_new_message_and_its_edit_are_told_apart_in_one_conversation() -> None:
    async with AsyncTilo() as tilo:
        workspace = tilo.workspace()
        channel = made(await workspace.do("so.tilo.chat#new_channel", name="sdk-watch"))
        seen: list[Event] = []
        watching = asyncio.create_task(changes(workspace, seen))
        await asyncio.sleep(1.0)

        message = made(await workspace.do(f"so.tilo.chat/{channel}#send", body="first words"))
        await workspace.do(f"so.tilo.chat/{message}#edit", body="second words")

        async def both() -> None:
            while not any(
                message in str(event.raw) for event in seen[1:]
            ) or len([e for e in seen if message in str(e.raw)]) < 2:
                await asyncio.sleep(0.1)

        try:
            await asyncio.wait_for(both(), 30)
        finally:
            watching.cancel()
        told = [event for event in seen if message in str(event.raw)]
        sent, edited = told[0], told[-1]
        # The store says what a change did to its record, next to `change`: the new message
        # reads created and its edit reads edited, in the event's own `what`.
        assert sent.what is EventWhat.CREATED, (
            "the new message's event says it was created",
            sent.raw,
        )
        assert edited.what is EventWhat.EDITED, (
            "the edit's event says it was edited",
            edited.raw,
        )
        items = {r.get("item") for r in sent.resources} & {r.get("item") for r in edited.resources}
        assert items - {None}, ("both name the message they changed", sent.raw, edited.raw)
