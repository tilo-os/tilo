"""A run: an invocation that has not finished, and what to do with it."""

from __future__ import annotations

from collections.abc import AsyncIterator, Mapping
from typing import Any

from ._endpoint import Endpoint, result_of
from ._models import Event


class Run:
    """An action still running, or one that waits for the person.

    `await run.result()` follows it to its result, `async for event in
    run.output()` follows its events, `await run.cancel()` stops it and
    `await run.undo()` takes back what it changed. With the sync client the
    same calls return their answers directly. Once it is done, `run.link` is
    the link to the record it changed, when it changed one.
    """

    def __init__(self, endpoint: Endpoint, run_id: str, path: str | None = None) -> None:
        self.endpoint = endpoint
        self.run_id = run_id
        self.path = path
        self.link: str | None = None

    def __repr__(self) -> str:
        return f"Run({self.run_id!r}, path={self.path!r})"

    @property
    def resource(self) -> str | None:
        """The resource whose action the run is, `app/name` without the action."""
        if not self.path or "#" not in self.path:
            return None
        return self.path.split("#", 1)[0]

    def _handoff(self, value: Any) -> Any:
        return self.endpoint.client.handoff(value)

    def answer(self, timeout: float | None = None) -> Any:
        """The whole answer once the run is done."""
        return self._handoff(self._answer(timeout))

    async def _answer(self, timeout: float | None) -> Mapping[str, Any]:
        answer = await self.endpoint.wait(self.run_id, timeout)
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
        """Stop the run."""
        return self._handoff(self.endpoint.status(self.run_id, cancel=True))

    def output(self) -> AsyncIterator[Event]:
        """The run's events, from started to succeeded, failed or cancelled."""
        return self._handoff(self.endpoint.output(self.run_id))

    def undo(self) -> Any:
        """Take back what the run changed, in its app's own step."""
        return self._handoff(self.endpoint.undo(self.run_id))
