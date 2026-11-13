"""The one seam every call goes through: a request in, an answer out.

`HttpTransport` sends the calls of Tilo's `/v1` to an API server or to a host
over HTTP. `NativeTransport` sends the very same calls to a Tilo host in
process, through the Rust extension `tilo._native`: one implementation of the
routes, no socket. Nothing above this module knows which one it holds.
"""

from __future__ import annotations

import json as jsonlib
import os
from collections.abc import AsyncIterator, Mapping
from dataclasses import dataclass, field
from typing import Any

import httpx

from ._errors import ProtocolError

VERSION = "0.1.0"
# Where a client goes when nothing says otherwise: the hosted API server.
DEFAULT_URL = "https://api.tilo.sh"
# The kinds of answer a call may read: JSON, or a stream of events.
ACCEPT = "application/json, text/event-stream"


def url_from(url: str | None) -> str:
    """The URL a client goes to: `url`, `TILO_API_URL`, or the hosted server."""
    return (url or os.environ.get("TILO_API_URL") or DEFAULT_URL).rstrip("/")


def key_from(key: str | None) -> str | None:
    """The key a client sends: `key`, or `TILO_API_KEY`."""
    return key if key is not None else os.environ.get("TILO_API_KEY")


class Unreachable(ProtocolError):
    """The server could not be reached: the call may be sent again as it is."""


@dataclass
class Response:
    """One answer: its status, its headers, and its body.

    A streamed answer has no `body`: its bytes arrive through `chunks`.
    """

    status: int
    headers: Mapping[str, str] = field(default_factory=dict)
    body: bytes = b""
    chunks: AsyncIterator[bytes] | None = None

    def header(self, name: str) -> str | None:
        """The header `name`, as the server spelled it."""
        for key, value in self.headers.items():
            if key.lower() == name.lower():
                return value
        return None

    def json(self) -> Any:
        """The body as JSON; a body that is not JSON is a broken wire."""
        if not self.body:
            return None
        try:
            return jsonlib.loads(self.body)
        except ValueError as error:
            raise ProtocolError(
                f"the server answered {self.status} without JSON: {self.body[:200]!r}"
            ) from error


class Transport:
    """What a client sends its calls through.

    `request` is the whole seam: a method, a path, and the body it carries,
    answered by a status, headers and a body (or, for a stream of server-sent
    events, an async iterator of its bytes).
    """

    async def request(
        self,
        method: str,
        path: str,
        *,
        json: Any = None,
        data: bytes | None = None,
        headers: Mapping[str, str] | None = None,
        stream: bool = False,
    ) -> Response:
        """Send one call and answer what came back."""
        raise NotImplementedError

    async def aclose(self) -> None:
        """Let go of whatever this transport keeps open."""
        return None


class HttpTransport(Transport):
    """The API over HTTP: one URL, one key, and httpx."""

    def __init__(
        self,
        url: str | None = None,
        key: str | None = None,
        *,
        http: httpx.AsyncClient | None = None,
    ) -> None:
        self.url = url_from(url)
        self.key = key_from(key)
        self._http = http
        self._owned: httpx.AsyncClient | None = None

    def client(self) -> httpx.AsyncClient:
        """The httpx client these calls go through."""
        if self._http is not None:
            return self._http
        if self._owned is None:
            self._owned = httpx.AsyncClient(
                base_url=self.url,
                timeout=httpx.Timeout(10.0, read=120.0, write=120.0),
            )
        return self._owned

    async def aclose(self) -> None:
        """Close the connections this transport opened."""
        if self._owned is not None:
            await self._owned.aclose()
            self._owned = None

    def url_for(self, path: str) -> httpx.URL:
        """The URL of `path`, as the client sends it.

        The path arrives percent-encoded, segment by segment; it goes on the
        wire exactly as it is, so a resource whose name holds a `/` stays one
        segment.
        """
        base = self.client().base_url
        sent, _, query = path.partition("?")
        root = base.raw_path.partition(b"?")[0].rstrip(b"/")
        url = base.copy_with(raw_path=root + sent.encode())
        return httpx.URL(f"{url}?{query}") if query else url

    async def request(
        self,
        method: str,
        path: str,
        *,
        json: Any = None,
        data: bytes | None = None,
        headers: Mapping[str, str] | None = None,
        stream: bool = False,
    ) -> Response:
        """Send one call over HTTP and answer what came back."""
        sent = {
            "accept": ACCEPT,
            # Whatever httpx asks for, the SDK reads the bytes as they are.
            "accept-encoding": "identity",
            "user-agent": f"tilo-python/{VERSION}",
        }
        if self.key is not None:
            sent["authorization"] = f"Bearer {self.key}"
        if json is not None:
            sent["content-type"] = "application/json"
        sent.update(headers or {})
        content = jsonlib.dumps(json).encode() if json is not None else data
        try:
            request = self.client().build_request(method, self.url_for(path), content=content, headers=sent)
            response = await self.client().send(request, stream=stream)
        except httpx.HTTPError as error:
            raise Unreachable(f"could not reach {self.url} at {path}: {error}") from error
        if stream and response.status_code < 400:
            return Response(
                status=response.status_code,
                headers=dict(response.headers),
                chunks=_closing(response),
            )
        try:
            body = await response.aread()
        except httpx.HTTPError as error:
            raise Unreachable(f"could not finish reading {path}: {error}") from error
        finally:
            await response.aclose()
        return Response(status=response.status_code, headers=dict(response.headers), body=body)


async def _closing(response: httpx.Response) -> AsyncIterator[bytes]:
    """`response`'s bytes as they arrive, letting the connection go at the end."""
    stream = response.stream
    if not isinstance(stream, httpx.AsyncByteStream):
        raise Unreachable("the answer is not an asynchronous stream")
    pieces = stream.__aiter__()
    try:
        async for piece in pieces:
            yield piece
    except httpx.HTTPError as error:
        raise Unreachable(f"the stream stopped before its end: {error}") from error
    finally:
        close = getattr(pieces, "aclose", None)
        if close is not None:
            await close()
        await response.aclose()


class NativeTransport(Transport):
    """The API served in process, by the Rust extension `tilo._native`."""

    def __init__(self, native: Any) -> None:
        """Wrap `native`, whose `request(method, path, headers, body)` answers
        `(status, headers, body)` with the body's bytes or its iterator."""
        self.native = native

    async def request(
        self,
        method: str,
        path: str,
        *,
        json: Any = None,
        data: bytes | None = None,
        headers: Mapping[str, str] | None = None,
        stream: bool = False,
    ) -> Response:
        """Send one call to the host in process, and answer what it answered."""
        sent = dict(headers or {})
        if json is not None:
            sent["content-type"] = "application/json"
        body = jsonlib.dumps(json).encode() if json is not None else (data or b"")
        status, answered, content = await self.native.request(method, path, sent, body)
        if not isinstance(content, (bytes, bytearray)):
            return Response(status=status, headers=answered, chunks=content)
        return Response(status=status, headers=answered, body=bytes(content))

    async def aclose(self) -> None:
        """Let the host go with this client."""
        close = getattr(self.native, "aclose", None)
        if close is not None:
            await close()


def native_transport(origin: str, key_file: Any = None) -> NativeTransport:
    """The in-process transport of the Tilo host at `origin`, as the key in `key_file`.

    It is the Rust extension `tilo-sdk[native]` installs, which serves the
    same `/v1` routes in process: without it, a client reaches a host only
    over HTTP.
    """
    try:
        from . import _native
    except ImportError as error:
        raise ImportError(
            "AsyncTilo.host() serves a Tilo host in process through the `tilo._native` "
            'extension, which this install does not have: install it with `uv add "tilo-sdk[native]"`, '
            "or reach the host over HTTP instead: AsyncTilo(url=..., key=...)"
        ) from error
    return NativeTransport(
        _native.connect(origin, key_file=None if key_file is None else str(key_file))
    )


@dataclass(frozen=True)
class Sse:
    """One server-sent event: its id (a cursor), its kind, and its data."""

    id: str | None
    event: str
    data: Any


class _Frame:
    """The event being read from a stream's lines, and taking it when it ends."""

    def __init__(self) -> None:
        self.id: str | None = None
        self.event = "message"
        self.data: list[str] = []

    def line(self, text: str) -> Sse | None:
        """Read one line of the stream: a field of the event, or its end."""
        if text == "":
            return self.take()
        if text.startswith(":") or ":" not in text:
            # A comment keeps the connection warm; a bare word says nothing.
            return None
        name, _, value = text.partition(":")
        value = value[1:] if value.startswith(" ") else value
        if name == "id":
            self.id = value
        elif name == "event":
            self.event = value
        elif name == "data":
            self.data.append(value)
        return None

    def take(self) -> Sse | None:
        """The event the lines so far made, and start the next one."""
        if not self.data:
            self.id, self.event, self.data = None, "message", []
            return None
        payload = "\n".join(self.data)
        frame = Sse(id=self.id, event=self.event, data=_json_of(payload))
        self.id, self.event, self.data = None, "message", []
        return frame


def _json_of(payload: str) -> Any:
    try:
        return jsonlib.loads(payload)
    except ValueError as error:
        raise ProtocolError(
            f"the server sent an event that is not JSON: {payload[:200]!r}"
        ) from error


async def events(chunks: AsyncIterator[bytes]) -> AsyncIterator[Sse]:
    """The server-sent events of `chunks`, each as it arrives."""
    buffer = b""
    frame = _Frame()
    try:
        async for piece in chunks:
            buffer += piece
            while True:
                index = buffer.find(b"\n")
                if index < 0:
                    break
                line, buffer = buffer[:index], buffer[index + 1 :]
                sent = frame.line(line.decode("utf-8", "replace").rstrip("\r"))
                if sent is not None:
                    yield sent
        if buffer:
            sent = frame.line(buffer.decode("utf-8", "replace").rstrip("\r"))
            if sent is not None:
                yield sent
        sent = frame.take()
        if sent is not None:
            yield sent
    finally:
        close = getattr(chunks, "aclose", None)
        if close is not None:
            await close()
