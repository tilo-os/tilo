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


class RateLimited(Refused):
    """The host is busy with the person's changes and ran nothing.

    The client asks again by itself, waiting what the host says
    (`retry_after_ms`), until its retry deadline; this is raised only once
    the deadline passed.
    """

    def __init__(
        self,
        message: str = "",
        next_steps: list[str] | None = None,
        retry_after_ms: int | None = None,
    ) -> None:
        super().__init__(RATE_LIMITED, message, next_steps)
        self.retry_after_ms = retry_after_ms


RATE_LIMITED = "rate_limited"


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


class KeyRefused(ProtocolError):
    """The door turned the key away (HTTP 401): it reaches nothing.

    `code` says why, the door's own word: `invalid_token` for a key the door
    never issued (mistyped, or from another door), `key_revoked`
    (`KeyRevoked`) and `key_expired` (`KeyExpired`). `next_steps` are what
    would work, each a sentence.
    """

    def __init__(
        self,
        message: str,
        *,
        code: str = "invalid_token",
        status: int | None = 401,
        next_steps: list[str] | None = None,
    ) -> None:
        super().__init__(message, status=status, code=code)
        self.next_steps = list(next_steps or [])


class KeyRevoked(KeyRefused):
    """The key was revoked: by its person, with the key that minted it, or
    when the workspace it reached was deleted. `revoked_at_ms` says when
    (Unix milliseconds), when the door knows."""

    def __init__(self, message: str = "this key was revoked", *, revoked_at_ms: int | None = None, **rest: Any) -> None:
        super().__init__(message, code=KEY_REVOKED, **rest)
        self.revoked_at_ms = revoked_at_ms


class KeyExpired(KeyRefused):
    """The key expired at `expired_at_ms` (Unix milliseconds), when the door
    knows: a minted key lasts at most 90 days, and a key its person gave an
    expiry ends then."""

    def __init__(self, message: str = "this key expired", *, expired_at_ms: int | None = None, **rest: Any) -> None:
        super().__init__(message, code=KEY_EXPIRED, **rest)
        self.expired_at_ms = expired_at_ms


KEY_REVOKED = "key_revoked"
KEY_EXPIRED = "key_expired"
INVALID_TOKEN = "invalid_token"


def key_refusal(error: Any, status: int | None = 401) -> KeyRefused | None:
    """The `KeyRefused` the door's typed refusal `error` (`{code, message,
    next_steps, revoked_at_ms?, expired_at_ms?}`) is, if it is about the key."""
    if not isinstance(error, dict):
        return None
    code = error.get("code")
    message = str(error.get("message") or code or "")
    steps = [str(step) for step in error.get("next_steps") or []]
    if code == KEY_REVOKED:
        return KeyRevoked(message, revoked_at_ms=_ms(error.get("revoked_at_ms")), status=status, next_steps=steps)
    if code == KEY_EXPIRED:
        return KeyExpired(message, expired_at_ms=_ms(error.get("expired_at_ms")), status=status, next_steps=steps)
    if code == INVALID_TOKEN:
        return KeyRefused(message, code=INVALID_TOKEN, status=status, next_steps=steps)
    return None


def _ms(value: Any) -> int | None:
    return int(value) if isinstance(value, (int, float)) and value >= 0 else None
