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

from tilo import AsyncTilo, Event, Refused, Workspace

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
