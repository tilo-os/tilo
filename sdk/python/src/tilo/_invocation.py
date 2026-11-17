"""An invocation: an action that has not finished, and what to do with it."""

from __future__ import annotations

from collections.abc import AsyncIterator, Mapping
from typing import Any

from ._endpoint import Endpoint, result_of
from ._models import Event


class Invocation:
    """An action still running, or one that waits for the person.

    `await invocation.result()` follows it to its result, `async for event in
    invocation.output()` follows its events, `await invocation.cancel()` stops
    it and `await invocation.undo()` takes back what it changed. With the sync
    client the same calls return their answers directly. Once it is done,
    `invocation.link` is the link to the record it changed, when it changed one.
    """

    def __init__(
        self, endpoint: Endpoint, invocation_id: str, path: str | None = None
    ) -> None:
        self.endpoint = endpoint
        self.invocation_id = invocation_id
        self.path = path
        self.link: str | None = None

    def __repr__(self) -> str:
        return f"Invocation({self.invocation_id!r}, path={self.path!r})"

    @property
    def resource(self) -> str | None:
        """The resource whose action the invocation is, `app/name` without the action."""
        if not self.path or "#" not in self.path:
            return None
        return self.path.split("#", 1)[0]

    def _handoff(self, value: Any) -> Any:
        return self.endpoint.client.handoff(value)

    def answer(self, timeout: float | None = None) -> Any:
        """The whole answer once the invocation is done."""
        return self._handoff(self._answer(timeout))

    async def _answer(self, timeout: float | None) -> Mapping[str, Any]:
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
        return result_of(await self._answer(timeout))

    def cancel(self) -> Any:
        """Stop the invocation."""
        return self._handoff(self.endpoint.status(self.invocation_id, cancel=True))

    def output(self) -> AsyncIterator[Event]:
        """The invocation's events, from started to succeeded, failed or cancelled."""
        return self._handoff(self.endpoint.output(self.invocation_id))

    def undo(self) -> Any:
        """Take back what the invocation changed, in its app's own step."""
        return self._handoff(self.endpoint.undo(self.invocation_id))
