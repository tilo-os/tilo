"""The two transports a call goes over: this computer's daemon, or a key over HTTPS.

Every SDK call is one tool call of Tilo's agent surface (`tilo_ls`,
`tilo_read`, `tilo_do`, …). A transport carries those calls and answers what
the tool answered, or raises the refusal it gave. `DaemonTransport` reaches this
computer's daemon through the Rust extension `tilo._native`; `KeyTransport`
speaks MCP's `tools/call` over HTTPS, the only HTTP the SDK speaks. A program
may bring its own transport: anything with `workspace` and `call`.
"""

from __future__ import annotations

import asyncio
import json
import os
import re
import time
from collections.abc import Mapping
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Protocol, runtime_checkable

if TYPE_CHECKING:
    # The HTTP client belongs to the key transport alone. The daemon transport
    # imports none, so a program that reaches this computer's daemon needs only
    # the package and its extension — including in a runtime that ships the SDK
    # without httpx — and a key transport that cannot find it fails at its first
    # call, where it is installed for.
    import httpx

from . import __version__ as VERSION
from ._errors import (
    INVALID_TOKEN,
    KeyRefused,
    ProtocolError,
    Refused,
    Unreachable,
    key_refusal,
    refusal_of,
)

# Where a client goes when nothing says otherwise: the hosted API server.
DEFAULT_URL = "https://api.tilo.sh"
# The MCP endpoint of a server, under the URL a client was given.
MCP_PATH = "/mcp"
# The protocol version this SDK's MCP client speaks.
PROTOCOL_VERSION = "2025-06-18"
# How long a call keeps asking again while the workspace is briefly not ready,
# in seconds.
RETRY_DEADLINE = 60.0
# The refusals that say the workspace is not ready for the call yet.
# `unavailable` is the one an HTTP 503 without a typed body gets.
RETRY_CODES = frozenset({"not_ready", "unanswered", "unavailable"})


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


def _lane_answered(text: str) -> None:
    """A lane call's answer: nothing, or its refusal raised."""
    body = json.loads(text)
    refused = body.get("refused")
    if refused is not None:
        raise refusal_of(refused)
    if "answer" not in body:
        raise ProtocolError("the daemon answered a call lane in a shape the SDK does not know")


@runtime_checkable
class Transport(Protocol):
    """What carries the SDK's tool calls.

    `workspace` is the one workspace a call acts in when it names none (the
    daemon's served workspace; `None` for a key). `call` runs one tool and
    answers what it answered, raising `Refused`, `AskPending`, `KeyRefused`,
    `Unreachable` or `ProtocolError` as it says. `group` is the invocation
    the call is a step of, `{"id", "title"}`, when one is open.
    """

    @property
    def workspace(self) -> str | None:
        """The workspace a call acts in when it names none."""
        ...

    async def call(
        self,
        workspace: str | None,
        tool: str,
        arguments: Mapping[str, Any],
        group: Mapping[str, Any] | None = None,
    ) -> Any:
        """Run `tool` with `arguments` in `workspace`: what it answered."""
        ...

    async def aclose(self) -> None:
        """Let go of what the transport holds open."""
        ...


def url_from(url: str | None) -> str:
    """The URL a key transport goes to: `url`, `TILO_API_URL`, or the hosted server."""
    return (url or os.environ.get("TILO_API_URL") or DEFAULT_URL).rstrip("/")


def key_from(key: str | None) -> str | None:
    """The key a client sends: `key`, or `TILO_API_KEY`."""
    return key if key is not None else os.environ.get("TILO_API_KEY")


class DaemonTransport:
    """The daemon's native endpoint, through the `tilo._native` extension."""

    def __init__(self, daemon: Any) -> None:
        self.daemon = daemon

    @classmethod
    async def connect(
        cls, socket: str | None = None, workspace: str | None = None
    ) -> DaemonTransport:
        """Connect to this computer's daemon, as the person or the app it started.

        `socket` names the daemon's endpoint when it is not the one this
        computer's login keeps; `workspace` the one workspace the connection
        acts in, when the launcher did not say.
        """
        try:
            from . import _native
        except ImportError as error:
            raise ImportError(
                "reaching this computer's daemon needs the `tilo._native` extension: "
                'install it with `uv add "tilo-sdk[native]"`, or pass a key'
            ) from error
        return cls(await _native.connect(socket, workspace))

    @property
    def workspace(self) -> str | None:
        """The workspace the daemon serves this connection, once it named one."""
        return self.daemon.workspace

    async def call(
        self,
        workspace: str | None,
        tool: str,
        arguments: Mapping[str, Any],
        group: Mapping[str, Any] | None = None,
    ) -> Any:
        """One tool call as action frames to the daemon: its typed answer, or `Refused`."""
        # The native part answers JSON text: `{"answer": ...}` ran, and
        # `{"refused": {"error": {...}}}` did not.
        text = await self.daemon.call(
            workspace,
            tool,
            json.dumps(dict(arguments)),
            None if group is None else json.dumps(dict(group)),
        )
        body = json.loads(text)
        refused = body.get("refused")
        if refused is not None:
            raise refusal_of(refused)
        return body.get("answer")

    async def web_call(
        self, workspace: str | None, arguments: Mapping[str, Any]
    ) -> Any:
        """Make the web call `arguments` names with a key the vault keeps.

        One action frame to the vault, answered for the process this connection
        is: an app's held connection has the vault add the key to the request,
        and is asked for its person's grant once per service and item. A plain
        program's call goes through `tilo_do`, as every program's does.
        """
        body = json.loads(
            await self.daemon.web_call(workspace, json.dumps(dict(arguments)))
        )
        refused = body.get("refused")
        if refused is not None:
            raise refusal_of(refused)
        return body.get("answer")

    async def release(self, workspace: str | None, item: str) -> str:
        """The secret of the vault item `item`, released to this app's process.

        One action frame to the vault, not a tool: the vault releases a secret
        only to the process an app's connection was handed to, for an item its
        grant covers, and refuses every agent. The secret is answered to the
        caller alone and kept nowhere.
        """
        body = json.loads(await self.daemon.release(workspace, item))
        refused = body.get("refused")
        if refused is not None:
            raise refusal_of(refused)
        answer = body.get("answer")
        secret = answer.get("secret") if isinstance(answer, Mapping) else None
        if not isinstance(secret, str):
            raise ProtocolError("the vault answered a release with no secret")
        return secret

    async def carry_call(self, pair: Mapping[str, Any] | None) -> None:
        """Carry the outside call `pair` names into the workspace call, or let go (`None`)."""
        _lane_answered(
            await self.daemon.carry_call(json.dumps(None if pair is None else dict(pair)))
        )

    async def report_call(self, report: Mapping[str, Any]) -> None:
        """Say who is in the outside calls this connection carries, and where it stands."""
        _lane_answered(await self.daemon.report_call(json.dumps(dict(report))))

    def hear_call(self, speaker: str, opus: bytes) -> None:
        """Hand the workspace's call one frame an outside person said; nothing answers it."""
        refused = self.daemon.hear_call(speaker, bytes(opus))
        if refused is not None:
            raise refusal_of(json.loads(refused))

    def take_call(self) -> tuple[list[CallFrame], CallAsk | None]:
        """What the workspace's call said since the last time, and a new ask, if there is one."""
        body = json.loads(self.daemon.said_call())
        refused = body.get("refused")
        if refused is not None:
            raise refusal_of(refused)
        answer = body.get("answer") or {}
        frames = [
            CallFrame(
                speaker=str(frame["speaker"]),
                opus=bytes(frame["opus"]),
                skipped=int(frame.get("skipped", 0)),
            )
            for frame in answer.get("frames", [])
        ]
        asked = answer.get("asked")
        return frames, None if asked is None else CallAsk(
            call=asked.get("call"), status=asked.get("status")
        )

    async def aclose(self) -> None:
        """Close the connection to the daemon."""
        close = getattr(self.daemon, "aclose", None)
        if close is not None:
            await close()


class KeyTransport:
    """A key's MCP session with a hosted API server, over HTTPS."""

    def __init__(
        self,
        key: str,
        url: str | None = None,
        *,
        http: httpx.AsyncClient | None = None,
        retry_deadline: float | None = None,
    ) -> None:
        self.key = key
        self.url = url_from(url)
        self.retry_deadline = RETRY_DEADLINE if retry_deadline is None else retry_deadline
        self._http = http
        self._owned: httpx.AsyncClient | None = None
        # The MCP session the server opened, and whether one was opened at all.
        self._session: str | None = None
        self._opened = False
        self._next_id = 0

    @property
    def workspace(self) -> str | None:
        """A key names its workspaces in each call: it serves none by default."""
        return None

    def client(self) -> httpx.AsyncClient:
        """The httpx client these calls go through."""
        import httpx

        if self._http is not None:
            return self._http
        if self._owned is None:
            self._owned = httpx.AsyncClient(
                timeout=httpx.Timeout(10.0, read=60.0, write=60.0)
            )
        return self._owned

    async def aclose(self) -> None:
        """Close the HTTP client this transport made, if it made one."""
        if self._owned is not None:
            await self._owned.aclose()
            self._owned = None

    async def call(
        self,
        workspace: str | None,
        tool: str,
        arguments: Mapping[str, Any],
        group: Mapping[str, Any] | None = None,
    ) -> Any:
        """One tool call to the server's MCP endpoint as the key: its typed answer, or `Refused`."""
        deadline = time.monotonic() + self.retry_deadline
        attempt = 0
        # A session the server no longer knows is opened again once.
        reopened = False
        while True:
            status: int | None = None
            try:
                await self._open()
                named = self._session
                status, headers, text = await self._post(
                    workspace, tool, arguments, group
                )
                if status == 404 and named is not None and not reopened:
                    self._session = None
                    self._opened = False
                    reopened = True
                    continue
                return self._answer(status, text, tool)
            except Refused as error:
                if status != 503 and error.code not in RETRY_CODES:
                    raise
                refused = error
            except Unreachable:
                raise
            # The workspace is not ready yet: ask again, no sooner than it asked.
            delay = (
                refused.retry_after_ms / 1000
                if getattr(refused, "retry_after_ms", None) is not None
                else min(0.05 * 2**attempt, 2.0)
            )
            if time.monotonic() + delay > deadline:
                raise refused
            attempt += 1
            await asyncio.sleep(delay)

    def _headers(self) -> dict[str, str]:
        sent = {
            "authorization": f"Bearer {self.key}",
            "content-type": "application/json",
            "accept": "application/json",
            "user-agent": f"tilo-python/{VERSION}",
        }
        if self._session is not None:
            sent["mcp-session-id"] = self._session
        return sent

    async def _post(
        self,
        workspace: str | None,
        tool: str,
        arguments: Mapping[str, Any],
        group: Mapping[str, Any] | None,
    ) -> tuple[int, Mapping[str, str], str]:
        args = dict(arguments)
        if workspace is not None:
            args["workspace"] = workspace
        meta: dict[str, Any] = {"structured": True}
        if group is not None:
            meta["invocation"] = dict(group)
        self._next_id += 1
        request = {
            "jsonrpc": "2.0",
            "id": self._next_id,
            "method": "tools/call",
            "params": {
                "name": tool,
                "arguments": args,
                "_meta": {"so.tilo.mcp": meta},
            },
        }
        return await self._send(request)

    async def _open(self) -> None:
        """Open the MCP session, the first time only."""
        if self._opened:
            return
        request = {
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "tilo-sdk", "version": VERSION},
            },
        }
        status, headers, text = await self._send(request)
        if status >= 400:
            self._answer(status, text, "initialize")
        self._session = _header(headers, "mcp-session-id")
        self._opened = True

    async def _send(self, request: Mapping[str, Any]) -> tuple[int, Mapping[str, str], str]:
        import httpx

        try:
            response = await self.client().post(
                f"{self.url}{MCP_PATH}", json=request, headers=self._headers()
            )
        except httpx.HTTPError as error:
            raise Unreachable(f"could not reach {self.url}: {error}") from error
        headers = dict(response.headers)
        text = response.text
        if response.status_code in (401, 403):
            raise _key_refusal(response.status_code, headers, text)
        return response.status_code, headers, text

    def _answer(self, status: int, text: str, tool: str) -> Any:
        """The tool's answer, or the refusal it is."""
        body = _json(text)
        if status >= 400:
            error = body.get("error") if isinstance(body, Mapping) else None
            if isinstance(error, Mapping):
                raise refusal_of(body)
            raise Refused(
                "unavailable" if status == 503 else "failed",
                f"the API answered {status}: {text[:200]}",
            )
        if not isinstance(body, Mapping):
            raise ProtocolError(f"the API answered no JSON-RPC: {text[:200]!r}")
        error = body.get("error")
        if error is not None:
            said = error.get("message") if isinstance(error, Mapping) else error
            raise Refused("failed", str(said or "the API refused the call"))
        result = body.get("result")
        if not isinstance(result, Mapping):
            raise ProtocolError(f"the API answered no result: {text[:200]!r}")
        failed = result.get("isError") is True
        structured = result.get("structuredContent")
        if structured is not None:
            # A program's run is its answer however it ended: `tilo` says a
            # failed program's words in the answer itself.
            if failed and tool != "tilo":
                raise refusal_of(structured)
            return structured
        content = result.get("content")
        said = ""
        if isinstance(content, list) and content and isinstance(content[0], Mapping):
            said = str(content[0].get("text") or "")
        raise Refused(
            "failed" if failed else "not_found", said or "the API answered nothing"
        )


def _json(text: str) -> Any:
    try:
        return json.loads(text)
    except ValueError:
        return None


def _header(headers: Mapping[str, str], name: str) -> str | None:
    for key, value in headers.items():
        if key.lower() == name.lower():
            return value
    return None


_AUTH_PARAM = re.compile(r'(\w+)\s*=\s*"((?:[^"\\]|\\.)*)"')


def _auth_param(header: str, name: str) -> str | None:
    for key, value in _AUTH_PARAM.findall(header or ""):
        if key.lower() == name.lower():
            return value.replace('\\"', '"').replace("\\\\", "\\")
    return None


def _key_refusal(status: int, headers: Mapping[str, str], text: str) -> KeyRefused:
    """The key refusal an HTTP 401 or 403 is.

    The body may be the API's typed refusal; failing that, the
    `www-authenticate` header says why in its own `error` and
    `error_description` parameters.
    """
    body = _json(text)
    error = body.get("error") if isinstance(body, Mapping) else None
    if isinstance(error, Mapping):
        typed = key_refusal(error)
        if typed is not None:
            return typed
        return KeyRefused(
            str(error.get("message") or ""),
            code=str(error.get("code") or INVALID_TOKEN),
            next_steps=[str(step) for step in error.get("next_steps") or []],
        )
    header = _header(headers, "www-authenticate") or ""
    code = _auth_param(header, "error") or INVALID_TOKEN
    said = _auth_param(header, "error_description") or f"the API refused the key ({status})"
    return key_refusal({"code": code, "message": said}) or KeyRefused(said, code=code)


__all__ = [
    "Transport",
    "DaemonTransport",
    "KeyTransport",
    "RETRY_DEADLINE",
    "VERSION",
    "key_from",
    "url_from",
]
