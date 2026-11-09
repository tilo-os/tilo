"""One endpoint the client calls: the account, or one workspace.

Every call here is a `tools/call` of that endpoint's session, with the run
the call is a step of when one is open.
"""

from __future__ import annotations

import asyncio
import contextvars
import time
from typing import Any, AsyncIterator, Mapping

from ._errors import AskPending, ProtocolError
from ._models import Event
from ._wire import Session, Transport, answer_of


class RunGroup:
    """A run a group of calls is attributed to: one undo step per app."""

    def __init__(self, link: Link, title: str, id: str) -> None:
        self.link = link
        self.title = title
        self.id = id
        self.invocations: list[str] = []

    def note(self, invocation_id: str | None) -> None:
        if invocation_id:
            self.invocations.append(invocation_id)

    def undo(self) -> Any:
        """Take the run's changes back, each app one step at a time."""
        return self.link.transport.handoff(self.link.program("undo task"))

    async def __aenter__(self) -> RunGroup:
        return self

    async def __aexit__(self, *_: Any) -> None:
        return None


CURRENT: contextvars.ContextVar[RunGroup | None] = contextvars.ContextVar(
    "tilo_run_group", default=None
)


class Link:
    """The account's session or one workspace's, and what this call joins."""

    def __init__(self, transport: Transport, workspace: str | None = None) -> None:
        self.transport = transport
        self.workspace = workspace

    @property
    def session(self) -> Session:
        return self.transport.session(self.workspace)

    def run_meta(self) -> Mapping[str, Any] | None:
        if self.transport.sync:
            group = self.transport.run_group
        else:
            group = CURRENT.get()
        if group is None:
            return None
        return {"id": group.id, "title": group.title}

    async def call(
        self,
        tool: str,
        arguments: Mapping[str, Any] | None = None,
        *,
        grouped: bool = True,
    ) -> dict[str, Any]:
        """One typed call, refused or asked as the wire says."""
        run = self.run_meta() if grouped else None
        answer = answer_of(await self.session.call(tool, arguments or {}, run))
        status = answer.get("status")
        invocation = answer.get("invocation_id")
        if status == "pending":
            ask = answer.get("ask")
            if isinstance(ask, Mapping):
                invocation_id = str(answer.get("invocation_id") or ask.get("invocation_id") or "")
                url = ask.get("url")
                raise AskPending(
                    url if isinstance(url, str) else None,
                    invocation_id or None,
                    question=ask,
                    follow=lambda: self.wait(invocation_id),
                )
        if isinstance(invocation, str):
            group = self.transport.run_group if self.transport.sync else CURRENT.get()
            if group is not None:
                group.note(invocation)
        return answer

    async def outcome(self, path: str, args: Mapping[str, Any]) -> Any:
        """Run one action: its result, or the `Run` that follows it."""
        answer = await self.call("tilo_do", {"path": path, "args": dict(args)})
        return self.follow(answer, path)

    def follow(self, answer: Mapping[str, Any], path: str | None = None) -> Any:
        from ._run import Run

        status = answer.get("status")
        if status == "pending":
            return Run(
                link=self,
                invocation_id=str(answer.get("invocation_id") or ""),
                path=path,
            )
        if status == "ok":
            return answer.get("result")
        return answer

    async def status(self, invocation_id: str, cancel: bool = False) -> dict[str, Any]:
        """`tilo_status`: how an invocation went, or stop it."""
        arguments: dict[str, Any] = {"invocation_id": invocation_id}
        if cancel:
            arguments["cancel"] = True
        return answer_of(
            await self.session.call("tilo_status", arguments, None)
        )

    async def wait(
        self, invocation_id: str, timeout: float | None = None
    ) -> dict[str, Any]:
        """Follow an invocation until it is done, a `Run` aside."""
        delay = 0.2
        deadline = None if timeout is None else time.monotonic() + timeout
        while True:
            answer = await self.status(invocation_id)
            if answer.get("status") != "pending":
                return answer
            if deadline is not None and time.monotonic() >= deadline:
                raise ProtocolError(
                    f"invocation {invocation_id} is still running after {timeout}s"
                )
            await asyncio.sleep(delay)
            delay = min(delay * 2, 1.0)

    async def program(self, source: str, confirm: bool | None = None) -> Any:
        """The `tilo` tool: a tilolang program run as this run's task."""
        from ._models import ProgramAnswer

        arguments: dict[str, Any] = {"program": source}
        if confirm is not None:
            arguments["confirm"] = confirm
        answer = await self.call("tilo", arguments)
        status = answer.get("status")
        if status == "pending":
            return self.follow(answer)
        return ProgramAnswer.of(answer)

    async def watch(
        self, path: str | None = None, wait_ms: int | None = None
    ) -> AsyncIterator[Event]:
        """Each change, run event and presence change, its cursor kept."""
        cursor: str | None = None
        while True:
            arguments: dict[str, Any] = {}
            if cursor is not None:
                arguments["cursor"] = cursor
            if path is not None:
                arguments["path"] = path
            if wait_ms is not None:
                arguments["wait_ms"] = wait_ms
            answer = await self.call("tilo_watch", arguments, grouped=False)
            next_cursor = answer.get("cursor")
            if isinstance(next_cursor, str):
                cursor = next_cursor
            for raw in answer.get("events") or []:
                if isinstance(raw, Mapping):
                    yield Event.of(raw)
