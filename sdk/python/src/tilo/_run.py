"""A run: an invocation that has not finished, and what to do with it."""

from __future__ import annotations

from typing import Any, AsyncIterator

from ._errors import ProtocolError
from ._endpoint import Endpoint
from ._models import Event
from ._wire import result_of


class Run:
    """An action still running, or one that waits for the person.

    `await run.result()` follows it to its result, `async for event in
    run.output()` follows its events, `await run.cancel()` stops it and
    `await run.undo()` takes back what it changed. With the sync client the
    same calls return their answers directly. Once it is done, `run.link` is
    the link to the record it changed, when it changed one.
    """

    def __init__(self, endpoint: Endpoint, invocation_id: str, path: str | None = None) -> None:
        self.endpoint = endpoint
        self.invocation_id = invocation_id
        self.path = path
        self.link: str | None = None

    def __repr__(self) -> str:
        return f"Run({self.invocation_id!r}, path={self.path!r})"

    @property
    def resource(self) -> str | None:
        """The resource whose action the run is, `app/name` without the action."""
        if not self.path or "#" not in self.path:
            return None
        return self.path.split("#", 1)[0]

    def _handoff(self, value: Any) -> Any:
        return self.endpoint.transport.handoff(value)

    def answer(self, timeout: float | None = None) -> Any:
        """The whole typed answer once the invocation is done."""
        return self._handoff(self._answer(timeout))

    async def _answer(self, timeout: float | None) -> dict[str, Any]:
        answer = await self.endpoint.wait(self.invocation_id, timeout)
        path = answer.get("path")
        if isinstance(path, str) and path:
            self.path = path
        link = answer.get("link")
        if isinstance(link, str) and link:
            self.link = link
        return answer

    def result(self, timeout: float | None = None) -> Any:
        """The action's result; a refusal the host made raises `Refused`."""
        return self._handoff(self._result(timeout))

    async def _result(self, timeout: float | None) -> Any:
        answer = await self._answer(timeout)
        return result_of(answer)

    def cancel(self) -> Any:
        """Stop the invocation."""
        return self._handoff(self.endpoint.status(self.invocation_id, cancel=True))

    def output(self) -> AsyncIterator[Event]:
        """The run's events, from started to succeeded, failed or cancelled."""
        return self._handoff(self._output())

    async def _output(self) -> AsyncIterator[Event]:
        async for event in self.endpoint.watch():
            if event.kind != "run" or event.invocation_id != self.invocation_id:
                continue
            yield event
            if event.phase in ("succeeded", "failed", "cancelled"):
                return

    def undo(self) -> Any:
        """Take the run back: its app's one step, or the whole task."""
        return self._handoff(self._undo())

    async def _undo(self) -> Any:
        if self.path is None:
            await self._answer(None)
        resource = self.resource
        if resource is None:
            raise ProtocolError("this run names no resource, so there is no app step to undo")
        return await self.endpoint.outcome(f"{resource}#undo", {"steps": 1})
