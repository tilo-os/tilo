"""How the client's requests reach Tilo's API: through this computer's daemon, or by URL with a key.

Either way a request is `(method, target, headers, body)` and its answer a status, its headers
and its body as it comes; the generated client is written against that alone.

The daemon's connection also carries what exists only there: the vault's lane (a web call with a
kept key, a release, a find) and an app's call lane.
"""

from __future__ import annotations

from collections.abc import Awaitable, Callable, Mapping, Sequence
from dataclasses import dataclass
from typing import Any, Protocol, TYPE_CHECKING, runtime_checkable

from . import __version__
from ._errors import Refused, refusal_of

if TYPE_CHECKING:
    import httpx

# The SDK's version, as the hosted server reads it in the user agent.
VERSION = __version__
# The server a key reaches when none is named.
DEFAULT_URL = "https://api.tilo.sh"
# The extension this computer's daemon is reached through.
NATIVE_MODULE = "tilo._native"

# One request to the API, `target` its path and query (`/v1/...`).
Send = Callable[
    [str, str, Sequence[tuple[str, str]], bytes | None],
    Awaitable["Reply"],
]


@runtime_checkable
class Reply(Protocol):
    """What the API answered one request with."""

    @property
    def status(self) -> int:
        """The status it answered with."""

    @property
    def headers(self) -> Sequence[tuple[str, str]]:
        """Its headers, each as `(name, value)`."""

    async def next(self) -> bytes | None:
        """The next piece of the body; `None` once it ended."""


@dataclass(frozen=True)
class CallFrame:
    """One 20 ms Opus frame of a call (48 kHz), untouched, and the account that said it.

    `skipped` is how many frames of this stream the device dropped just before
    this one: a sender leaves their place in its timeline (RTP sequence and
    timestamp), so the receiver conceals the gap.
    """

    speaker: str
    opus: bytes
    skipped: int = 0


@dataclass(frozen=True)
class CallAsk:
    """Where the device asks the app to be in the outside call: in `call`, showing
    `status` to its people about who is on the workspace's side, or in none
    (`call` is `None`)."""

    call: str | None
    status: str | None


@runtime_checkable
class DaemonConnection(Protocol):
    """A connection to the daemon, as the extension hands it over (or an app brings its own)."""

    workspace: str | None
    """The workspace this connection acts in when a call names none."""

    async def request(
        self,
        method: str,
        target: str,
        headers: list[list[str]],
        body: bytes | None = None,
    ) -> Any:
        """One `/v1` request, answered as this connection's own identity."""

    async def web_call(self, workspace: str | None, args: str) -> str:
        """A web call made with a key the vault keeps: JSON text."""

    async def release(self, workspace: str | None, item: str) -> str:
        """A vault item's secret, released to this process: JSON text."""

    async def find(self, workspace: str | None, app: str, purpose: str) -> str:
        """The vault item tagged for an app and a purpose: JSON text."""

    async def carry_call(self, pair: str) -> str:
        """Carry an outside call into a workspace call, or let go: JSON text."""

    async def report_call(self, report: str) -> str:
        """Who is in the outside calls, and where the app stands: JSON text."""

    def hear_call(self, speaker: str, opus: bytes) -> str | None:
        """Hand over one Opus frame another account said: `None`, or the refusal's JSON text."""

    def said_call(self) -> str:
        """The frames the workspace's call said since the last time: JSON text."""


def url_from(url: str | None) -> str:
    """The URL a key's requests go to: `url`, `TILO_API_URL`, or the hosted server."""
    import os

    return (url or os.environ.get("TILO_API_URL") or DEFAULT_URL).rstrip("/")


def key_from(key: str | None) -> str | None:
    """The key requests go with: `key`, or `TILO_API_KEY`."""
    import os

    return key if key is not None else os.environ.get("TILO_API_KEY")


def answered(answered: Any) -> Any:
    """What one call of the daemon's own lanes answered: its answer, or its refusal raised."""
    if isinstance(answered, Mapping) and answered.get("refused") is not None:
        raise refusal_of(answered["refused"])
    return answered.get("answer") if isinstance(answered, Mapping) else None


class _NativeReply:
    """A reply of the extension, its headers as pairs."""

    def __init__(self, reply: Any) -> None:
        self._reply = reply

    @property
    def status(self) -> int:
        return int(self._reply.status)

    @property
    def headers(self) -> list[tuple[str, str]]:
        return [
            (str(pair[0]), str(pair[1]))
            for pair in self._reply.headers
            if isinstance(pair, (list, tuple)) and len(pair) >= 2
        ]

    async def next(self) -> bytes | None:
        piece = await self._reply.next()
        return None if piece is None else bytes(piece)


class _HttpReply:
    """An `httpx` response, its body read as it comes."""

    def __init__(self, response: httpx.Response) -> None:
        self._response = response
        self._pieces = response.aiter_bytes()

    @property
    def status(self) -> int:
        return self._response.status_code

    @property
    def headers(self) -> list[tuple[str, str]]:
        return list(self._response.headers.multi_items())

    async def next(self) -> bytes | None:
        try:
            return await self._pieces.__anext__()
        except StopAsyncIteration:
            await self.aclose()
            return None

    async def aclose(self) -> None:
        """Close the response and the connection it reads."""
        await self._response.aclose()


class Daemon:
    """This computer's daemon, made the first time a request needs it and again after one failed."""

    def __init__(self, connect: Callable[[], Awaitable[DaemonConnection]]) -> None:
        self._connect = connect
        self._opened: DaemonConnection | None = None
        # The connection once made, for what a live call hands it without waiting.
        self.ready: DaemonConnection | None = None

    @classmethod
    def native(cls, socket: str | None = None, workspace: str | None = None) -> Daemon:
        """This computer's daemon, through the extension: the connection the launcher handed an
        app it started, else `socket`, else the daemon's own."""

        async def connect() -> DaemonConnection:
            try:
                from . import _native
            except ImportError as error:
                raise ImportError(
                    "reaching this computer's daemon needs the `tilo._native` extension: install "
                    "a `tilo-sdk` wheel that carries it, or give the client a key "
                    "(Tilo(key=...)) to reach Tilo by URL"
                ) from error
            return await _native.connect(socket, workspace)

        return cls(connect)

    async def opened(self) -> DaemonConnection:
        """The connection, made once."""
        if self._opened is None:
            try:
                self._opened = await self._connect()
            except BaseException:
                self._opened = None
                raise
            self.ready = self._opened
        return self._opened

    async def send(
        self,
        method: str,
        target: str,
        headers: Sequence[tuple[str, str]],
        body: bytes | None,
    ) -> Reply:
        """One request through the daemon, its answer's body streaming as it comes."""
        connection = await self.opened()
        reply = await connection.request(method, target, [list(pair) for pair in headers], body)
        return _NativeReply(reply)

    def aclose(self) -> None:
        """Let go of the connection: the extension closes it with the process."""


class KeySend:
    """A request with a key, by URL: `Authorization: Bearer <key>` at `url`."""

    def __init__(
        self,
        url: str,
        key: str,
        http: httpx.AsyncClient | None = None,
    ) -> None:
        self.url = url.rstrip("/")
        self.key = key
        self._http = http
        self._owned: httpx.AsyncClient | None = None

    def client(self) -> httpx.AsyncClient:
        """The `httpx` client these requests go through."""
        import httpx

        if self._http is not None:
            return self._http
        if self._owned is None:
            self._owned = httpx.AsyncClient(timeout=httpx.Timeout(10.0, read=60.0, write=60.0))
        return self._owned

    async def __call__(
        self,
        method: str,
        target: str,
        headers: Sequence[tuple[str, str]],
        body: bytes | None,
    ) -> Reply:
        """One request at `url + target`, the key in its `Authorization`."""
        request = self.client().build_request(
            method,
            f"{self.url}{target}",
            headers=[
                *headers,
                ("authorization", f"Bearer {self.key}"),
                ("user-agent", f"tilo-python/{VERSION}"),
            ],
            content=body,
        )
        response = await self.client().send(request, stream=True)
        return _HttpReply(response)

    async def aclose(self) -> None:
        """Close the HTTP client this transport made, if it made one."""
        if self._owned is not None:
            await self._owned.aclose()
            self._owned = None


def lane_refusal(text: str) -> Refused | None:
    """The refusal a lane's JSON text answers, when it is one."""
    import json

    try:
        body = json.loads(text)
    except ValueError:
        return None
    refused = body.get("refused") if isinstance(body, Mapping) else None
    return refusal_of(refused) if refused is not None else None


__all__ = [
    "CallAsk",
    "CallFrame",
    "Daemon",
    "DaemonConnection",
    "DEFAULT_URL",
    "KeySend",
    "NATIVE_MODULE",
    "Reply",
    "Send",
    "VERSION",
    "answered",
    "key_from",
    "url_from",
]
