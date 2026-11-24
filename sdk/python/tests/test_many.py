"""A bulk call: one all-or-nothing step per call, and every call one invocation group."""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any

from tilo import AsyncTilo, Tilo, made

TRACKER = "tasks/tracker"


class Recording:
    """A carrier that records each tool call and answers as the test says."""

    def __init__(self, answer: Any) -> None:
        self.workspace = "workspace-1"
        self.calls: list[dict[str, Any]] = []
        self._answer = answer

    async def call(
        self,
        workspace: str | None,
        tool: str,
        arguments: Mapping[str, Any],
        group: Mapping[str, Any] | None = None,
    ) -> Any:
        self.calls.append(
            {
                "workspace": workspace,
                "tool": tool,
                "arguments": dict(arguments),
                "group": None if group is None else dict(group),
            }
        )
        return self._answer(tool, arguments) if callable(self._answer) else self._answer

    async def aclose(self) -> None:
        return None


def applied(tool: str, arguments: Mapping[str, Any]) -> Mapping[str, Any]:
    """What the workspace answers a bulk call that applied every member."""
    actions = arguments.get("args", {}).get("actions", [])
    return {"status": "ok", "invocation": "inv-1", "result": {"applied": len(actions)}}


async def test_a_bulk_call_sends_five_hundred_members_a_call() -> None:
    carrier = Recording(applied)
    async with AsyncTilo(carrier=carrier) as tilo:
        items = [{"title": f"Issue {index}"} for index in range(1200)]
        answers = await tilo.workspace().resource(TRACKER).many("create", items)

    assert len(carrier.calls) == 3
    assert all(call["tool"] == "tilo_do" for call in carrier.calls)
    assert all(call["arguments"]["path"] == f"{TRACKER}#group" for call in carrier.calls)
    assert [len(call["arguments"]["args"]["actions"]) for call in carrier.calls] == [500, 500, 200]
    members = [
        member for call in carrier.calls for member in call["arguments"]["args"]["actions"]
    ]
    assert members == [{"action": "create", "args": item} for item in items]
    assert [answer["result"]["applied"] for answer in answers] == [500, 500, 200]


async def test_every_call_of_a_bulk_run_is_one_invocation_group() -> None:
    carrier = Recording(applied)
    async with AsyncTilo(carrier=carrier) as tilo:
        items = [{"title": f"Issue {index}"} for index in range(1200)]
        await tilo.workspace().resource(TRACKER).many("create", items)

    groups = [call["group"] for call in carrier.calls]
    assert groups[0] is not None
    assert all(group == groups[0] for group in groups)


async def test_a_bulk_call_names_an_item_an_earlier_one_makes() -> None:
    carrier = Recording(applied)
    async with AsyncTilo(carrier=carrier) as tilo:
        await tilo.workspace().resource(TRACKER).many(
            "create", [{"title": "Parent"}, {"title": "Child", "parent": made(0)}]
        )

    members = carrier.calls[0]["arguments"]["args"]["actions"]
    assert members[1]["args"]["parent"] == {"$made": {"member": 0, "item": 0}}


async def test_an_empty_bulk_call_sends_nothing() -> None:
    carrier = Recording(applied)
    async with AsyncTilo(carrier=carrier) as tilo:
        assert await tilo.workspace().resource(TRACKER).many("create", []) == []
    assert carrier.calls == []


async def test_a_bulk_call_inside_a_group_joins_it() -> None:
    carrier = Recording(applied)
    async with AsyncTilo(carrier=carrier) as tilo:
        workspace = tilo.workspace()
        async with workspace.invocation("Import the backlog"):
            await workspace.resource(TRACKER).many("create", [{"title": "One"}])

    assert carrier.calls[0]["group"]["title"] == "Import the backlog"


def test_the_sync_client_runs_a_bulk_call_too() -> None:
    carrier = Recording(applied)
    with Tilo(carrier=carrier) as tilo:
        answers = tilo.workspace().resource(TRACKER).many(
            "create", [{"title": "One"}, {"title": "Two"}]
        )

    assert len(carrier.calls) == 1
    assert carrier.calls[0]["arguments"]["args"]["actions"] == [
        {"action": "create", "args": {"title": "One"}},
        {"action": "create", "args": {"title": "Two"}},
    ]
    assert answers[0]["result"] == {"applied": 2}


async def test_a_bulk_call_answered_later_is_waited_for() -> None:
    pending: Mapping[str, Any] = {
        "status": "pending",
        "invocation": "inv-9",
        "result": {"applied": 2},
    }

    def answer(tool: str, arguments: Mapping[str, Any]) -> Mapping[str, Any]:
        if tool == "tilo_status":
            return {"status": "ok", "result": {"applied": 2}}
        return pending

    carrier = Recording(answer)
    async with AsyncTilo(carrier=carrier) as tilo:
        answers = await tilo.workspace().resource(TRACKER).many("create", [{"title": "One"}])

    assert answers[0]["status"] == "ok"
    assert [call["tool"] for call in carrier.calls] == ["tilo_do", "tilo_status"]
