"""The wire: MCP over HTTP plus the door's content and key endpoints.

Every call is a `tools/call` of an MCP session, exactly as an agent makes
it, with `_meta.so.tilo.mcp` asking for typed answers and naming the run the
call is a step of. The account is one session (`/mcp`); each workspace is
another (`/w/<workspace>/mcp`).
"""

from __future__ import annotations

import asyncio
import itertools
import inspect
import json
import os
import random
import time
from typing import Any, Mapping

import httpx

from ._errors import RATE_LIMITED, AskPending, ProtocolError, RateLimited, Refused, key_refusal

DEFAULT_DOOR = "https://mcp.tilo.sh"
PROTOCOL_VERSION = "2025-06-18"
SURFACE = "so.tilo.mcp"
SESSION_HEADER = "mcp-session-id"
VERSION = "0.1.0"
# How long a call keeps asking again while the host is busy with the
# person's changes, in seconds.
RETRY_DEADLINE = 60.0


def door_from(door: str | None) -> str:
    return (door or os.environ.get("TILO_API_URL") or DEFAULT_DOOR).rstrip("/")


def key_from(key: str | None) -> str | None:
    return key if key is not None else os.environ.get("TILO_API_KEY")


def messages_in(response: httpx.Response) -> list[Any]:
    """The JSON-RPC messages of a reply: a JSON body, or an event stream."""
    content_type = response.headers.get("content-type", "")
    text = response.text
    if "text/event-stream" in content_type:
        payloads = [
            line[len("data:") :].strip()
            for line in text.splitlines()
            if line.startswith("data:")
        ]
        messages = []
        for payload in payloads:
            if not payload:
                continue
            try:
                messages.append(json.loads(payload))
            except ValueError as error:
                raise ProtocolError(f"the door sent an event that is not JSON: {error}") from error
        return messages
    if not text.strip():
        return []
    try:
        answer = response.json()
    except ValueError as error:
        raise ProtocolError(
            f"the door answered {response.status_code} without JSON: {text[:200]!r}"
        ) from error
    return answer if isinstance(answer, list) else [answer]


def reply_in(messages: list[Any], request_id: int) -> Mapping[str, Any]:
    """The answer to `request_id` among `messages`."""
    for message in reversed(messages):
        if isinstance(message, Mapping) and message.get("id") == request_id:
            return message
    for message in reversed(messages):
        if isinstance(message, Mapping) and "id" in message:
            return message
    raise ProtocolError("the door answered without a reply")


class Session:
    """One MCP session: `path` names the account or one workspace."""

    def __init__(self, transport: Transport, path: str) -> None:
        self.transport = transport
        self.path = path
        self.id: str | None = None
        self._ids = itertools.count(1)

    async def open(self) -> None:
        payload = {
            "jsonrpc": "2.0",
            "id": next(self._ids),
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "Tilo SDK", "version": VERSION},
            },
        }
        response = await self._send(payload)
        if response.status_code >= 400:
            raise self.transport.refused(response)
        error = reply_in(messages_in(response), payload["id"]).get("error")
        if isinstance(error, Mapping):
            raise refusal_of(error)
        session = response.headers.get(SESSION_HEADER)
        if not session:
            raise ProtocolError("the door answered initialize without a session id")
        self.id = session
        await self._send(
            {"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}
        )

    async def close(self) -> None:
        if self.id is None:
            return
        session, self.id = self.id, None
        try:
            await self.transport.request("DELETE", self.path, session=session)
        except (httpx.HTTPError, ProtocolError):
            pass

    async def call(
        self, tool: str, arguments: Mapping[str, Any], run: Mapping[str, Any] | None = None
    ) -> dict[str, Any]:
        """One `tools/call`, answered typed.

        A call the host refused because it is busy with the person's changes
        ran nothing, so it is asked again after the wait the host said, with
        jitter, until the transport's retry deadline; then `RateLimited`.
        """
        deadline = time.monotonic() + self.transport.retry_deadline
        attempt = 0
        while True:
            try:
                answer = await self._call(tool, arguments, run)
                busy = rate_limited(answer)
                if busy is None:
                    return answer
            except RateLimited as refused:
                busy = refused
            delay = max(
                (busy.retry_after_ms or 0) / 1000.0, min(0.05 * 2**attempt, 2.0)
            ) * random.uniform(1.0, 1.5)
            if time.monotonic() + delay > deadline:
                raise busy
            attempt += 1
            await asyncio.sleep(delay)

    async def _call(
        self, tool: str, arguments: Mapping[str, Any], run: Mapping[str, Any] | None
    ) -> dict[str, Any]:
        if self.id is None:
            await self.open()
        payload = {
            "jsonrpc": "2.0",
            "id": next(self._ids),
            "method": "tools/call",
            "params": {
                "name": tool,
                "arguments": dict(arguments),
                "_meta": {SURFACE: self.transport.surface(run)},
            },
        }
        response = await self._send(payload)
        if response.status_code == 404:
            # The door forgot the session (it idles out): open another one.
            self.id = None
            await self.open()
            payload["id"] = next(self._ids)
            response = await self._send(payload)
        if response.status_code >= 400:
            raise self.transport.refused(response)
        reply = reply_in(messages_in(response), payload["id"])
        error = reply.get("error")
        if isinstance(error, Mapping):
            raise refusal_of(error)
        result = reply.get("result")
        if not isinstance(result, Mapping):
            raise ProtocolError("the door answered a call without a result")
        return self.transport.structured(result)

    async def _send(self, payload: Mapping[str, Any]) -> httpx.Response:
        return await self.transport.request("POST", self.path, body=payload, session=self.id)


class Transport:
    """The door and this client's key: where every session is opened."""

    def __init__(
        self,
        door: str | None = None,
        key: str | None = None,
        http: httpx.AsyncClient | None = None,
        retry_deadline: float | None = None,
    ) -> None:
        self.door = door_from(door)
        self.key = key_from(key)
        self.retry_deadline = RETRY_DEADLINE if retry_deadline is None else retry_deadline
        self._http = http
        self._owned: httpx.AsyncClient | None = None
        self._sessions: dict[str | None, Session] = {}
        # The sync client runs on its own loop and hands back answers, not
        # awaitables; the async client hands the awaitables back.
        self.sync = False
        self.loop: Any = None
        # The run a group of the sync client's calls is a step of: it has no
        # contextvars across threads, so it names the run here.
        self.run_group: Any = None

    def handoff(self, value: Any) -> Any:
        """`value` as this client's caller takes it: awaited, or walked here."""
        if not self.sync:
            return value
        if inspect.isawaitable(value):
            return self.loop.run(value)
        if hasattr(value, "__anext__"):
            from ._sync import SyncIterator

            return SyncIterator(value, self.loop)
        return value

    def session(self, workspace: str | None = None) -> Session:
        """The session for the account (`None`) or one workspace."""
        if workspace not in self._sessions:
            path = "/mcp" if workspace is None else f"/w/{workspace}/mcp"
            self._sessions[workspace] = Session(self, path)
        return self._sessions[workspace]

    def client(self) -> httpx.AsyncClient:
        if self._http is not None:
            return self._http
        if self._owned is None:
            self._owned = httpx.AsyncClient(
                base_url=self.door,
                timeout=httpx.Timeout(10.0, read=120.0, write=120.0),
            )
        return self._owned

    async def aclose(self) -> None:
        for session in list(self._sessions.values()):
            await session.close()
        if self._owned is not None:
            await self._owned.aclose()
            self._owned = None

    def surface(self, run: Mapping[str, Any] | None) -> dict[str, Any]:
        surface: dict[str, Any] = {"structured": True}
        if run is not None:
            surface["run"] = dict(run)
        return surface

    def refused(self, response: httpx.Response) -> Exception:
        """What the door's refusal `response` raises: `KeyRevoked`,
        `KeyExpired` or `KeyRefused` for a key it turned away, typed as it
        said; else a `ProtocolError` with its words."""
        if response.status_code == 401:
            try:
                body = response.json()
            except ValueError:
                body = None
            typed = key_refusal(body.get("error") if isinstance(body, dict) else None, response.status_code)
            if typed is not None:
                return typed
        return ProtocolError(self.refusal(response), status=response.status_code)

    def refusal(self, response: httpx.Response) -> str:
        text = response.text.strip() or response.reason_phrase
        if response.status_code in (401, 403):
            if self.key is None:
                return (
                    f"no key: pass key=... or set TILO_API_KEY ({text})"
                )
            return f"this key may not do that: {text}"
        if response.status_code == 404:
            return f"the door has no such session or workspace: {text}"
        return f"the door answered {response.status_code}: {text}"

    def structured(self, result: Mapping[str, Any]) -> dict[str, Any]:
        """The typed answer of a tool result, or why there is none."""
        content = result.get("structuredContent")
        if isinstance(content, dict):
            return content
        text = "".join(
            part.get("text", "")
            for part in result.get("content", [])
            if isinstance(part, Mapping)
        )
        raise ProtocolError(
            "the door answered text, not a typed value"
            + (f": {text.strip()[:400]}" if text.strip() else "")
        )

    async def request(
        self,
        method: str,
        path: str,
        *,
        body: Mapping[str, Any] | None = None,
        data: bytes | None = None,
        session: str | None = None,
    ) -> httpx.Response:
        headers = {
            "accept": "application/json, text/event-stream",
            "user-agent": f"tilo-python/{VERSION}",
        }
        if self.key is not None:
            headers["authorization"] = f"Bearer {self.key}"
        if session is not None:
            headers[SESSION_HEADER] = session
        try:
            if body is not None:
                return await self.client().request(
                    method,
                    path,
                    content=json.dumps(body).encode(),
                    headers={**headers, "content-type": "application/json"},
                )
            return await self.client().request(method, path, content=data, headers=headers)
        except httpx.HTTPError as error:
            raise ProtocolError(f"could not reach the door at {self.door}: {error}") from error


def refusal_of(error: Mapping[str, Any]) -> Exception:
    """A JSON-RPC error as the SDK raises it: `RateLimited` when its data
    says the host is busy, else a `ProtocolError`."""
    message = str(error.get("message") or "the door refused the call")
    data = error.get("data")
    if isinstance(data, Mapping) and data.get("code") == RATE_LIMITED:
        return RateLimited(message, retry_after_ms=_wait_of(data))
    return ProtocolError(message, code=error.get("code"))


def _wait_of(fields: Mapping[str, Any]) -> int | None:
    wait = fields.get("retry_after_ms")
    return int(wait) if isinstance(wait, (int, float)) and wait >= 0 else None


def rate_limited(answer: Mapping[str, Any]) -> RateLimited | None:
    """The `RateLimited` a typed answer is, if it is one."""
    error = answer.get("error")
    if answer.get("status") != "failed" or not isinstance(error, Mapping):
        return None
    if error.get("code") != RATE_LIMITED:
        return None
    return RateLimited(
        str(error.get("message") or ""),
        [str(step) for step in error.get("next_steps") or []],
        _wait_of(error),
    )


def answer_of(answer: Mapping[str, Any]) -> Mapping[str, Any]:
    """The typed answer, or the refusal it is."""
    status = answer.get("status")
    if status == "failed":
        error = answer.get("error")
        if not isinstance(error, Mapping):
            raise Refused("failed", "" if error is None else str(error))
        busy = rate_limited(answer)
        if busy is not None:
            raise busy
        typed = key_refusal(dict(error), None)
        if typed is not None:
            raise typed
        raise Refused(
            str(error.get("code") or "failed"),
            str(error.get("message") or ""),
            [str(step) for step in error.get("next_steps") or []],
        )
    return answer


CONTENT_PREFIX = "tilo.content:"


def result_of(answer: Mapping[str, Any]) -> Any:
    """What an action answered: its value, or, when it gave none and made a
    file instead (Writer's `export-content`), that file's `tilo.content:`
    reference, which `ws.files.download` fetches. Several files answer a
    list of references."""
    result = answer.get("result")
    if result is not None:
        return result
    made = [
        resource
        for resource in answer.get("resources") or []
        if isinstance(resource, str) and resource.startswith(CONTENT_PREFIX)
    ]
    if len(made) == 1:
        return made[0]
    return made or None
