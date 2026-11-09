"""The wire: MCP over HTTP plus the door's content and key endpoints.

Every call is a `tools/call` of an MCP session, exactly as an agent makes
it, with `_meta.so.tilo.mcp` asking for typed answers and naming the run the
call is a step of. The account is one session (`/mcp`); each workspace is
another (`/w/<workspace>/mcp`).
"""

from __future__ import annotations

import itertools
import inspect
import json
import os
from typing import Any, Mapping

import httpx

from ._errors import AskPending, ProtocolError, Refused

DEFAULT_DOOR = "https://mcp.tilo.sh"
PROTOCOL_VERSION = "2025-06-18"
SURFACE = "so.tilo.mcp"
SESSION_HEADER = "mcp-session-id"
VERSION = "0.1.0"


def door_from(door: str | None) -> str:
    return (door or os.environ.get("TILO_DOOR") or DEFAULT_DOOR).rstrip("/")


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
            raise ProtocolError(
                self.transport.refusal(response), status=response.status_code
            )
        reply_in(messages_in(response), payload["id"])
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
        """One `tools/call`, answered typed."""
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
            raise ProtocolError(self.transport.refusal(response), status=response.status_code)
        reply = reply_in(messages_in(response), payload["id"])
        error = reply.get("error")
        if isinstance(error, Mapping):
            raise ProtocolError(
                str(error.get("message") or "the door refused the call"),
                code=error.get("code"),
            )
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
    ) -> None:
        self.door = door_from(door)
        self.key = key_from(key)
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


def answer_of(answer: Mapping[str, Any]) -> Mapping[str, Any]:
    """The typed answer, or the refusal it is."""
    status = answer.get("status")
    if status == "failed":
        error = answer.get("error")
        if not isinstance(error, Mapping):
            raise Refused("failed", "" if error is None else str(error))
        raise Refused(
            str(error.get("code") or "failed"),
            str(error.get("message") or ""),
            [str(step) for step in error.get("next_steps") or []],
        )
    return answer
