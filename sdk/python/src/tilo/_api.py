"""Tilo's `/v1`: the calls, the headers they carry, and what their answers mean.

Every call a client makes goes through [`Api.call`], which names the API's
version on the way out, sends an `Idempotency-Key` with every call that
changes something, asks a transient refusal again until its deadline, and
raises the refusal an error answer is.
"""

from __future__ import annotations

import asyncio
import random
import time
import uuid
from collections.abc import Mapping
from typing import Any
from urllib.parse import urlencode

from ._errors import (
    SURFACE_CHANGED,
    ProtocolError,
    Refused,
    SurfaceChanged,
    Unreachable,
    UnsupportedVersion,
    key_refusal,
)
from ._transport import Response, Transport

# The API's major version this SDK speaks, and the header every answer names.
API_MAJOR = "1"
VERSION_HEADER = "tilo-api-version"
# The header a mutating call says which logical call it is in.
IDEMPOTENCY_HEADER = "idempotency-key"
# The methods that change something, and so carry an `Idempotency-Key`.
MUTATING = frozenset({"POST", "PUT", "PATCH", "DELETE"})
# How long a call keeps asking again while the server is briefly unavailable
# or unreachable, in seconds.
RETRY_DEADLINE = 60.0
# The longest a call's answer is held on the server, in seconds.
MAX_WAIT = 60


class Api:
    """One transport, and what every call through it does the same way."""

    def __init__(self, transport: Transport, retry_deadline: float | None = None) -> None:
        self.transport = transport
        self.retry_deadline = RETRY_DEADLINE if retry_deadline is None else retry_deadline

    async def call(
        self,
        method: str,
        path: str,
        *,
        query: Mapping[str, Any] | None = None,
        json: Any = None,
        data: bytes | None = None,
        headers: Mapping[str, str] | None = None,
        stream: bool = False,
        mutating: bool | None = None,
    ) -> Response:
        """Send one call: a refusal raises, a transient one is asked again.

        A call that changes something carries one `Idempotency-Key`, and every
        retry of it carries the same one, so the host runs it at most once.
        """
        sent = dict(headers or {})
        changes = (method in MUTATING) if mutating is None else mutating
        if changes:
            sent.setdefault(IDEMPOTENCY_HEADER, uuid.uuid4().hex)
        url = _with_query(path, query)
        deadline = time.monotonic() + self.retry_deadline
        attempt = 0
        while True:
            try:
                response = await self.transport.request(
                    method, url, json=json, data=data, headers=sent, stream=stream
                )
                transient = retryable_refusal(response)
                if transient is None:
                    break
            except Unreachable as error:
                transient = error
            delay = _wait_seconds(attempt)
            if time.monotonic() + delay > deadline:
                raise transient
            attempt += 1
            await asyncio.sleep(delay)
        check_version(response)
        refusal = refusal_of(response)
        if refusal is not None:
            raise refusal
        return response

    async def json(
        self,
        method: str,
        path: str,
        *,
        query: Mapping[str, Any] | None = None,
        json: Any = None,
        data: bytes | None = None,
        headers: Mapping[str, str] | None = None,
        mutating: bool | None = None,
    ) -> Any:
        """One call, its answer as JSON: an object, or `None` for an empty one."""
        response = await self.call(
            method,
            path,
            query=query,
            json=json,
            data=data,
            headers=headers,
            mutating=mutating,
        )
        return response.json()


def _with_query(path: str, query: Mapping[str, Any] | None) -> str:
    pairs = [
        (name, value)
        for name, value in (query or {}).items()
        if value is not None and value != ""
    ]
    if not pairs:
        return path
    return f"{path}?{urlencode(pairs)}"


def check_version(response: Response) -> None:
    """Refuse an answer of another major version of the API."""
    version = response.header(VERSION_HEADER)
    if version is None:
        return
    major = version.split(".")[0].strip()
    if major != API_MAJOR:
        raise UnsupportedVersion(version, API_MAJOR)


def error_of(response: Response) -> Mapping[str, Any] | None:
    """The refusal body `response` carries, if it carries one."""
    if response.status < 400:
        return None
    try:
        body = response.json()
    except ProtocolError:
        return None
    error = body.get("error") if isinstance(body, Mapping) else None
    return error if isinstance(error, Mapping) else None


def refusal_of(response: Response) -> Exception | None:
    """What a refusal answer raises: `None` when it answered something."""
    if response.status < 400:
        return None
    error = error_of(response)
    if error is None:
        return ProtocolError(
            f"the API answered {response.status}: {response.body[:200]!r}",
            status=response.status,
        )
    return refusal_of_error(dict(error), response.status)


def refusal_of_error(error: Mapping[str, Any], status: int | None = None) -> Exception:
    """What the API's refusal body `error` raises, at `status`."""
    code = str(error.get("code") or "failed")
    message = str(error.get("message") or "")
    steps = [str(step) for step in error.get("next_steps") or []]
    if status == 401:
        typed = key_refusal(dict(error), status)
        if typed is not None:
            return typed
    if code == SURFACE_CHANGED:
        return SurfaceChanged(message, steps)
    return Refused(code, message, steps)


def retryable_refusal(response: Response) -> Refused | None:
    """The refusal a call may ask again as it is, when the server is briefly unavailable."""
    if response.status != 503:
        return None
    error = error_of(response) or {}
    code = str(error.get("code") or "unavailable")
    message = str(error.get("message") or "")
    steps = [str(step) for step in error.get("next_steps") or []]
    return Refused(code, message, steps)


def _wait_seconds(attempt: int) -> float:
    """How long to wait before asking a transient refusal's call again."""
    return min(0.05 * 2**attempt, 2.0) * random.uniform(1.0, 1.5)
