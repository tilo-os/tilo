"""A refusal: what the API, the daemon or the vault answered instead of doing what was asked."""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any


class Refused(Exception):
    """The call was refused, in the API's one refusal shape.

    `error` is the whole body the API answered (`code`, `message`, `next_steps`,
    `retry_after_ms` and the refused action's row where there are any), and
    `status` the HTTP status it came with, 0 for a refusal of the daemon's own
    connection. `code` is stable and typed, what a program branches on
    (`not_permitted`, `key_revoked`, `surface_changed`, …); `message` is for
    people.
    """

    def __init__(self, error: Mapping[str, Any] | None = None, status: int = 0) -> None:
        body = dict(error or {})
        self.error = body
        self.status = status
        super().__init__(str(body.get("message") or body.get("code") or "the call was refused"))

    @property
    def code(self) -> str:
        """What a program branches on: `not_found`, `not_permitted`, `key_revoked`, …"""
        return str(self.error.get("code") or "failed")

    @property
    def message(self) -> str:
        """The API's words for people."""
        return str(self.error.get("message") or "")

    @property
    def next_steps(self) -> list[str]:
        """The calls that would work instead, each a sentence."""
        return [str(step) for step in self.error.get("next_steps") or []]

    @property
    def retry_after_ms(self) -> int | None:
        """How long the refusal asks to wait before the same call works."""
        asked = self.error.get("retry_after_ms")
        return asked if isinstance(asked, int) and asked >= 0 else None

    def __str__(self) -> str:
        text = f"{self.code}: {self.message}" if self.message else self.code
        if self.next_steps:
            text = text + "\n" + "\n".join(self.next_steps)
        return text


def refusal_of(body: Any, status: int = 0) -> Refused:
    """The refusal `body` (`{error: {...}}`) is."""
    error = body.get("error") if isinstance(body, Mapping) else None
    return Refused(error if isinstance(error, Mapping) else {}, status)
