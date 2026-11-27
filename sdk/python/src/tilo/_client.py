"""The client: Tilo's API, `/v1`, as the generated operations describe it, carried by this
computer's daemon or by a key.

What is written here is how a request goes out and comes back, what names an invocation group,
and the two things an operation alone does not do: following an action to its end, and putting
a file up in pieces.
"""

from __future__ import annotations

import asyncio
import contextvars
import json
import re
import uuid
from collections.abc import AsyncIterator, Awaitable, Callable, Mapping, Sequence
from typing import Any
from urllib.parse import quote, urlencode

import blake3

from ._errors import Refused, refusal_of
from ._resource import Resource, resource_proxy
from ._transport import (
    Daemon,
    DaemonConnection,
    KeySend,
    Reply,
    Send,
    url_from,
    key_from,
)
from .types.api_v1 import AccountOperations, ActionAnswer, Operation, OPERATIONS, WorkspaceOperations
from ._vault import CallLane, Vault

#: How long a request keeps asking again while the API is briefly unavailable, in seconds.
RETRY_DEADLINE = 60.0
#: The most one request of an upload carries; a larger file goes up in pieces.
PIECE = 16 * 1024 * 1024
_UUID = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$", re.I)

_current_group: contextvars.ContextVar[InvocationGroup | None] = contextvars.ContextVar(
    "tilo_invocation_group", default=None
)


def current_group() -> InvocationGroup | None:
    """The invocation group the current call is made in, when one is open."""
    return _current_group.get()


class InvocationGroup:
    """An invocation group: every request made inside `ws.invocation(title)` names it in
    `Tilo-Invocation` and `Tilo-Invocation-Title`, so the apps record one undo step and History
    shows the title.

    `links` are the records its calls changed, each once; `link` is the one when they changed
    one.
    """

    def __init__(self, workspace: Workspace, title: str, id: str) -> None:
        self.workspace = workspace
        """The workspace its calls are in."""
        self.title = title
        """What History calls it."""
        self.id = id
        """What every request of the group names it by."""
        self.invocations: list[str] = []
        """The invocations its calls answered, in order."""
        self.links: list[str] = []
        """The links to the records its calls changed, each once."""

    @property
    def link(self) -> str | None:
        """The link to the one record its calls changed, or `None`."""
        return self.links[0] if len(self.links) == 1 else None

    async def undo(self) -> ActionAnswer:
        """Take back what its calls changed, in every record."""
        if not self.invocations:
            raise Refused(
                {"code": "not_found", "message": "the invocation group made no call to take back"}
            )
        return await self.workspace.undo_invocation(invocation=self.invocations[-1])

    def note(self, answer: Any) -> None:
        """What one of its calls answered."""
        said = answer if isinstance(answer, Mapping) else {}
        invocation = said.get("invocation")
        if isinstance(invocation, str) and invocation not in self.invocations:
            self.invocations.append(invocation)
        for link in [said.get("link"), *(said.get("links") or [])]:
            if isinstance(link, str) and link and link not in self.links:
                self.links.append(link)


class _InvocationOpening:
    """`ws.invocation(title)`: `async with` groups every call inside into one undo step."""

    def __init__(self, workspace: Workspace, title: str) -> None:
        self.workspace = workspace
        self.title = title
        self.group: InvocationGroup | None = None
        self._token: contextvars.Token[InvocationGroup | None] | None = None

    async def __aenter__(self) -> InvocationGroup:
        slug = re.sub(r"[^a-z0-9]+", "-", self.title.lower()).strip("-")[:40]
        self.group = InvocationGroup(
            self.workspace,
            self.title,
            f"sdk:{slug or 'invocation'}:{uuid.uuid4().hex[:8]}",
        )
        self._token = _current_group.set(self.group)
        return self.group

    async def __aexit__(self, *_: Any) -> None:
        if self._token is not None:
            _current_group.reset(self._token)
            self._token = None


def _address(path: str, params: Mapping[str, Any], query: Sequence[str]) -> str:
    """`path` with `params` in its braces, each segment percent-encoded, and its query."""

    def fill(match: re.Match[str]) -> str:
        name = match.group(1)
        value = params.get(name)
        if value is None:
            raise TypeError(f"{path} names {{{name}}}, and the call gave none")
        return "/".join(quote(part, safe="") for part in str(value).split("/"))

    path = re.sub(r"\{(\w+)\}", fill, path)
    said = urlencode([(name, params[name]) for name in query if params.get(name) is not None])
    return path if said == "" else f"{path}?{said}"


async def _all(reply: Reply) -> bytes:
    """Every piece of `reply`'s body."""
    pieces: list[bytes] = []
    while True:
        piece = await reply.next()
        if piece is None:
            return b"".join(pieces)
        pieces.append(piece)


async def _answer_of(reply: Reply, operation: Operation) -> Any:
    """What a successful answer carries, as `operation` answers it."""
    if operation.answer == "bytes":
        return await _all(reply)
    text = (await _all(reply)).decode("utf-8", "replace")
    return json.loads(text) if text.strip() else {}


def _retry_after_ms(reply: Reply) -> int:
    for name, value in reply.headers:
        if name.lower() == "retry-after":
            try:
                return int(float(value) * 1000)
            except ValueError:
                return 0
    return 0


async def _refusal_of_reply(reply: Reply) -> tuple[Refused, int]:
    """The refusal an answer that is not a success is: its body's, else its status."""
    text = (await _all(reply)).decode("utf-8", "replace")
    try:
        body = json.loads(text)
    except ValueError:
        body = None
    if isinstance(body, Mapping) and body.get("error") is not None:
        return refusal_of(body, reply.status), _retry_after_ms(reply)
    return (
        Refused(
            {
                "code": "unavailable" if reply.status == 503 else "failed",
                "message": f"the API answered {reply.status}: {text[:200]}",
            },
            reply.status,
        ),
        _retry_after_ms(reply),
    )


def _event_of(frame: str, workspace: str) -> dict[str, Any] | None:
    """One server-sent event's frame; `None` for a comment that keeps the stream alive."""
    id = ""
    type = "message"
    data: list[str] = []
    for line in re.split(r"\r?\n", frame):
        if line == "" or line.startswith(":"):
            continue
        at = line.find(":")
        field = line if at < 0 else line[:at]
        value = "" if at < 0 else line[at + 1 :].removeprefix(" ")
        if field == "id":
            id = value
        elif field == "event":
            type = value
        elif field == "data":
            data.append(value)
    if not data:
        return None
    parsed = json.loads("\n".join(data))
    if type == "error":
        raise refusal_of(parsed)
    return {"id": id, "type": type, "workspace": workspace, "data": parsed}


def stream_of(
    open_reply: Callable[[], Awaitable[tuple[Reply, str]]],
) -> AsyncIterator[dict[str, Any]]:
    """The server-sent events of a request `open_reply` makes, each the event's `id`, `type`,
    `workspace` and `data`. An `error` event raises its refusal. Ending the iterator closes the
    stream at once, even while it waits on a quiet one."""

    async def events() -> AsyncIterator[dict[str, Any]]:
        reply: Reply | None = None
        buffer = b""
        try:
            reply, workspace = await open_reply()
            while True:
                piece = await reply.next()
                if piece is None:
                    return
                buffer += piece
                while b"\n\n" in buffer:
                    frame, buffer = buffer.split(b"\n\n", 1)
                    event = _event_of(frame.decode("utf-8", "replace"), workspace)
                    if event is not None:
                        yield event
        finally:
            close = getattr(reply, "aclose", None) if reply is not None else None
            if close is not None:
                await close()

    return events()


class Tilo(AccountOperations):
    """The account, and where every request goes.

    `Tilo()` reaches this computer's daemon through the `tilo._native` extension, which answers
    `/v1` in process as whoever connected; `Tilo(key=...)` reaches `/v1` by URL. Its methods are
    the API's account operations; `workspace(...)` names one workspace.
    """

    def __init__(
        self,
        key: str | None = None,
        url: str | None = None,
        *,
        socket: str | None = None,
        workspace: str | None = None,
        daemon: DaemonConnection | None = None,
        http: Any = None,
        send: Send | None = None,
        retry_deadline: float = RETRY_DEADLINE,
    ) -> None:
        """Where the requests go.

        `daemon` is a connection of your own; `send` makes requests itself. Else a key (`key`, or
        `TILO_API_KEY`) reaches the API server at `url` (`TILO_API_URL`, `https://api.tilo.sh`)
        over HTTPS. Else the client reaches this computer's daemon through the `tilo._native`
        extension, connecting as whoever runs this program: `socket` names the daemon's endpoint
        when it is not the one this computer's login keeps, and `workspace` the one workspace the
        connection acts in when the launcher said nothing.

        `retry_deadline` is how long, in seconds, a request keeps asking again while the API is
        briefly unavailable (60 by default; 0 raises its refusal at once).
        """
        self.retry_deadline = retry_deadline
        named = key_from(key)
        if daemon is not None:

            async def opened() -> DaemonConnection:
                return daemon

            self.daemon: Daemon | None = Daemon(opened)
        elif named is None:
            self.daemon = Daemon.native(socket, workspace)
        else:
            self.daemon = None
        self._keysend: KeySend | None = None
        if send is not None:
            self._send_request = send
        elif self.daemon is not None:
            self._send_request = self.daemon.send
        else:
            self._keysend = KeySend(url_from(url), named or "", http)
            self._send_request = self._keysend

    def workspace(self, ref: str | None = None) -> Workspace:
        """One workspace, by its id or its name; with none, the one the daemon's connection
        acts in."""
        return Workspace(self, ref)

    async def aclose(self) -> None:
        """End the requests of a key's client still open."""
        if self._keysend is not None:
            await self._keysend.aclose()

    async def __aenter__(self) -> Tilo:
        return self

    async def __aexit__(self, *_: Any) -> None:
        await self.aclose()

    async def _send(
        self,
        operation: Operation,
        params: Mapping[str, Any],
        body: Any,
        headers: Mapping[str, str],
    ) -> Any:
        return await _answer_of(await self._request(operation, params, body, headers), operation)

    def _stream(
        self,
        operation: Operation,
        params: Mapping[str, Any],
        body: Any,
        headers: Mapping[str, str],
    ) -> AsyncIterator[dict[str, Any]]:
        async def open_reply() -> tuple[Reply, str]:
            return await self._request(operation, params, body, headers), ""

        return stream_of(open_reply)

    async def _request(
        self,
        operation: Operation,
        params: Mapping[str, Any],
        body: Any,
        headers: Mapping[str, str],
    ) -> Reply:
        """One request for `operation`: its answer, or the refusal it raises.

        A request names the invocation group it is made in, and a mutating one an
        `Idempotency-Key`, so asking again while the API is briefly unavailable runs it once.
        """
        sent: dict[str, str] = {
            "accept": {"json": "application/json", "events": "text/event-stream", "bytes": "*/*"}[
                operation.answer
            ]
        }
        group = current_group()
        if group is not None:
            sent["tilo-invocation"] = group.id
            sent["tilo-invocation-title"] = group.title
        if operation.mutating:
            sent["idempotency-key"] = str(uuid.uuid4())
        content: bytes | None = None
        if operation.body == "json":
            content = json.dumps(body if body is not None else {}).encode()
            sent["content-type"] = "application/json"
        elif operation.body == "bytes":
            content = bytes(body)
            sent["content-type"] = "application/octet-stream"
        for name, value in headers.items():
            sent[name.lower()] = value
        target = _address(operation.path, params, operation.query)
        deadline = asyncio.get_running_loop().time() + self.retry_deadline
        attempt = 0
        while True:
            reply = await self._send_request(operation.method, target, list(sent.items()), content)
            if 200 <= reply.status < 300:
                return reply
            refusal, asked = await _refusal_of_reply(reply)
            wait = max(
                asked,
                refusal.retry_after_ms or 0,
                min(50 * 2**attempt, 2_000),
            )
            if (
                reply.status != 503
                or asyncio.get_running_loop().time() + wait / 1000 > deadline
            ):
                raise refusal
            await asyncio.sleep(wait / 1000)
            attempt += 1


class Workspace(WorkspaceOperations):
    """One workspace, named by its id or its name; named by neither, the one the daemon's
    connection acts in.

    Its methods are the API's workspace operations, each naming this workspace.
    """

    def __init__(self, client: Tilo, ref: str | None = None) -> None:
        self.client = client
        """The client its requests go through."""
        self.ref = ref
        """What it was named by: an id, a name, or `None` for the daemon's own."""
        self.vault = Vault(self)
        """The person's vault, as this workspace's calls reach it."""
        self.call_lane = CallLane(self)
        """The call lane of an app's own connection."""
        self._id: str | None = None
        self._resolving: asyncio.Task[str] | None = None

    async def id(self) -> str:
        """Its id: the one it was named by, the daemon's own, or the one the account lists by
        its name."""
        if self._id is not None:
            return self._id
        if self._resolving is None:
            self._resolving = asyncio.ensure_future(self._resolve())
        try:
            self._id = await self._resolving
        except BaseException:
            self._resolving = None
            raise
        return self._id

    async def _resolve(self) -> str:
        if self.ref is not None and _UUID.match(self.ref):
            return self.ref
        served = None
        if self.client.daemon is not None:
            served = (await self.client.daemon.opened()).workspace
        if self.ref is None or self.ref == served:
            if served is None:
                raise Refused(
                    {"code": "invalid_request", "message": "name a workspace: a key's client serves none"}
                )
            return served
        listed = await run(self.client, {"path": "account/workspaces", "action": "list"})
        named = (listed.get("result") or {}).get("workspaces") if isinstance(listed, Mapping) else None
        found = next(
            (workspace for workspace in named or [] if isinstance(workspace, Mapping) and workspace.get("name") == self.ref),
            None,
        )
        if not isinstance(found, Mapping) or not isinstance(found.get("id"), str):
            raise Refused(
                {
                    "code": "not_found",
                    "message": f"no workspace {self.ref}",
                    "next_steps": ["account/workspaces#list names them"],
                }
            )
        return str(found["id"])

    async def _send(
        self,
        operation: Operation,
        params: Mapping[str, Any],
        body: Any,
        headers: Mapping[str, str],
    ) -> Any:
        reply = await self.client._request(
            operation, {**params, "workspace": await self.id()}, body, headers
        )
        answer = await _answer_of(reply, operation)
        group = current_group()
        if group is not None and group.workspace is self:
            group.note(answer)
        return answer

    def _stream(
        self,
        operation: Operation,
        params: Mapping[str, Any],
        body: Any,
        headers: Mapping[str, str],
    ) -> AsyncIterator[dict[str, Any]]:
        async def open_reply() -> tuple[Reply, str]:
            workspace = await self.id()
            reply = await self.client._request(
                operation, {**params, "workspace": workspace}, body, headers
            )
            return reply, workspace

        return stream_of(open_reply)

    def resource(self, path: str, surface: Any = None) -> ResourceType:
        """A resource as a typed object: its fields, its actions and its edits. `surface` is the
        app's version generated types were made from, which every call through it names."""
        return resource_proxy(self, path, surface)

    def invocation(self, title: str) -> _InvocationOpening:
        """`async with ws.invocation(title)`: every call inside names it, so the apps record one
        undo step and History shows `title`."""
        return _InvocationOpening(self, title)

    async def upload(self, data: Any) -> str:
        """Put a file's bytes into the workspace: the `tilo.content:` reference an action takes
        it by.

        The bytes are addressed by their BLAKE3 hash, so a file goes up once whatever it is
        called; one larger than a request carries goes up in pieces, each continuing where the
        put stands.
        """
        content = _bytes_of(data)
        hash = blake3.blake3(content).hexdigest()
        offset = 0
        while True:
            end = min(offset + PIECE, len(content))
            range = (
                {}
                if offset == 0 and end == len(content)
                else {"content-range": f"bytes {offset}-{end - 1}/{len(content)}"}
            )
            put = await self.put_content(hash, content[offset:end], headers=range)
            reference = put.get("content") if isinstance(put, Mapping) else None
            if isinstance(reference, str) and reference:
                return reference
            stands = put.get("offset") if isinstance(put, Mapping) else None
            if not isinstance(stands, int) or stands <= offset:
                raise Refused(
                    {
                        "code": "upload_failed",
                        "message": f"the put stands at {stands} after a piece from {offset}",
                    }
                )
            offset = stands

    async def download(self, reference: str) -> bytes:
        """The bytes a `tilo.content:` reference names."""
        return await self.get_content(reference.removeprefix("tilo.content:"))


def _bytes_of(data: Any) -> bytes:
    """A file's bytes: the bytes themselves, a file object, or a path."""
    if isinstance(data, bytes):
        return data
    if isinstance(data, (bytearray, memoryview)):
        return bytes(data)
    if hasattr(data, "read"):
        return data.read()
    with open(data, "rb") as file:
        return file.read()


async def run(
    where: Tilo | Workspace,
    params: Mapping[str, str],
    args: Mapping[str, Any] | None = None,
    headers: Mapping[str, str] | None = None,
) -> ActionAnswer:
    """Run `action` on the resource `path` until it is no longer pending: its answer, `result`
    what the action produced. A refusal raises.

    In a workspace a pending action is followed by its invocation; the account's is asked again
    with the same `Idempotency-Key`, which asks about the same invocation.
    """
    arguments = dict(args or {})
    if isinstance(where, Workspace):
        answer = await where.run_action(
            params["path"], params["action"], arguments, headers=dict(headers or {})
        )
        while (
            isinstance(answer, Mapping)
            and answer.get("status") == "pending"
            and isinstance(answer.get("invocation"), str)
        ):
            answer = await where.get_invocation(answer["invocation"], wait="60")
        return answer
    again: dict[str, str] = {
        "prefer": "wait=60",
        "idempotency-key": str(uuid.uuid4()),
    }
    for name, value in (headers or {}).items():
        again[name.lower()] = value
    while True:
        answer = await where.run_account_action(
            params["path"], params["action"], arguments, headers=again
        )
        if not isinstance(answer, Mapping) or answer.get("status") != "pending":
            return answer
