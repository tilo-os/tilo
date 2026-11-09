"""What the SDK raises: a refusal, an ask, or a call that broke the wire."""

from __future__ import annotations

import inspect
from typing import Any, Callable


class Refused(Exception):
    """The host refused the call.

    `code` is the app's own code (`invalid_arguments`, `unknown_action`, …),
    `message` its words for people, and `next_steps` the calls that would
    work, each a sentence naming a path.
    """

    def __init__(
        self, code: str, message: str = "", next_steps: list[str] | None = None
    ) -> None:
        super().__init__(message or code)
        self.code = code
        self.message = message or ""
        self.next_steps = list(next_steps or [])

    def __str__(self) -> str:
        text = f"{self.code}: {self.message}" if self.message else self.code
        if self.next_steps:
            text = text + "\n" + "\n".join(self.next_steps)
        return text


class AskPending(Exception):
    """The call waits for the person to allow it.

    `invocation_id` is the invocation they are answering, `url` where they
    answer it when the door names one, and `question` what they are asked.
    `await ask.wait()` follows the invocation until they decide, and returns
    its answer (a `Key` for a key that was created).
    """

    def __init__(
        self,
        url: str | None = None,
        invocation_id: str | None = None,
        *,
        question: Any = None,
        follow: Callable[[], Any] | None = None,
        convert: Callable[[Any], Any] | None = None,
    ) -> None:
        super().__init__(self._describe(url, invocation_id))
        self.url = url
        self.invocation_id = invocation_id
        self.question = question
        self._follow = follow
        self._convert = convert

    @staticmethod
    def _describe(url: str | None, invocation_id: str | None) -> str:
        parts = ["waiting for the person"]
        if invocation_id:
            parts.append(f"invocation {invocation_id}")
        if url:
            parts.append(f"at {url}")
        return ": ".join([parts[0], ", ".join(parts[1:])]) if len(parts) > 1 else parts[0]

    async def wait(self) -> Any:
        """Follow the invocation until the person decides, then its answer."""
        if self._follow is None:
            raise ProtocolError("this ask cannot be waited on: its invocation has no session")
        answer = self._follow()
        if inspect.isawaitable(answer):
            answer = await answer
        return self._convert(answer) if self._convert is not None else answer

    def _then(self, convert: Callable[[Any], Any]) -> AskPending:
        self._convert = convert
        return self


class ProtocolError(Exception):
    """The call itself was malformed, or the door answered something else."""

    def __init__(
        self, message: str, *, status: int | None = None, code: int | str | None = None
    ) -> None:
        super().__init__(message)
        self.message = message
        self.status = status
        self.code = code
