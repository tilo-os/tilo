"""One workspace: what is in it, what to do there, and who is there."""

from __future__ import annotations

import base64
import builtins
from typing import Any, AsyncIterator, Mapping

from ._errors import ProtocolError, Refused
from ._link import CURRENT, Link, RunGroup
from ._models import (
    Event,
    Hit,
    Invitation,
    Listing,
    Member,
    MintedKey,
    ProgramAnswer,
    WorkspaceInfo,
)
from ._resource import Resource

RUN_PREFIX = "sdk:"


class Files:
    """File bytes in and out of the workspace, named `tilo.content:<hash>`.

    Upload first, then an action takes the reference; an action that makes a
    file (Writer's `export-content`) answers one to download.
    """

    def __init__(self, workspace: Workspace) -> None:
        self.workspace = workspace

    def upload(self, path_or_bytes: Any, name: str | None = None) -> Any:
        """Upload a file (a path or bytes) and answer its `tilo.content:<hash>`.

        The door names content by its hash, so `name` is a caller's own
        label, kept beside the reference rather than sent.
        """
        return self.workspace.client.handoff(
            self.workspace._upload(_bytes_of(path_or_bytes))
        )

    def download(self, content: str) -> Any:
        """The bytes of `content`, a `tilo.content:<hash>` in this workspace."""
        return self.workspace.client.handoff(self.workspace._download(content))


def _bytes_of(data: Any) -> bytes:
    if isinstance(data, bytes):
        return data
    if isinstance(data, bytearray):
        return bytes(data)
    if hasattr(data, "read"):
        return data.read()
    return open(data, "rb").read()


class WorkspaceKeys:
    """The keys that reach this one workspace."""

    def __init__(self, workspace: Workspace) -> None:
        self.workspace = workspace

    def mint(self, name: str | None = None, expires_in: int | None = None) -> Any:
        """A key for an agent to act in this workspace, narrower than your own."""
        return self.workspace.client.handoff(self.workspace._mint(name, expires_in))


class Members:
    """The people in this workspace, and bringing someone else in.

    Inviting takes a key you created reaching all your workspaces, into one
    you administer, and invites as an editor at most.
    """

    PATH = "workspace/members"

    def __init__(self, workspace: Workspace) -> None:
        self.workspace = workspace

    def invite(self, role: str = "viewer", email: str | None = None) -> Any:
        """An `Invitation`: the link that brings one person in as `role`."""
        return self.workspace.client.handoff(self._invite(role, email))

    async def _invite(self, role: str, email: str | None) -> Invitation:
        args: dict[str, Any] = {"role": role}
        if email is not None:
            args["email"] = email
        result = await self.workspace._outcome(f"{self.PATH}#invite", args)
        if not isinstance(result, Mapping):
            raise ProtocolError("the door invited without answering a link")
        return Invitation.of(result)

    def list(self) -> Any:
        """The `Member`s of the workspace, each with their role."""
        return self.workspace.client.handoff(self._list())

    async def _list(self) -> builtins.list[Member]:
        result = await self.workspace._outcome(f"{self.PATH}#list", {})
        rows = result.get("members") if isinstance(result, Mapping) else None
        return [Member.of(row) for row in rows or [] if isinstance(row, Mapping)]


class RunOpening:
    """`ws.run(title)`: `async with` groups, `with` does too for the sync client."""

    def __init__(self, workspace: Workspace, title: str) -> None:
        self.workspace = workspace
        self.title = title
        self.group: RunGroup | None = None
        self._token: Any = None
        self._previous: Any = None

    def _begin(self) -> RunGroup:
        if self.group is None:
            self.group = RunGroup(self.workspace._link(), self.title, _run_id(self.title))
        transport = self.workspace.client.transport
        if transport.sync:
            self._previous = transport.run_group
            transport.run_group = self.group
        else:
            self._token = CURRENT.set(self.group)
        return self.group

    def _close(self) -> None:
        transport = self.workspace.client.transport
        if transport.sync:
            transport.run_group = self._previous
        elif self._token is not None:
            CURRENT.reset(self._token)
            self._token = None

    async def __aenter__(self) -> RunGroup:
        await self.workspace._resolve()
        return self._begin()

    async def __aexit__(self, *_: Any) -> None:
        self._close()

    def __enter__(self) -> RunGroup:
        self.workspace.client.handoff(self.workspace._resolve())
        return self._begin()

    def __exit__(self, *_: Any) -> None:
        self._close()


def _run_id(title: str) -> str:
    import uuid

    slug = "-".join(part for part in "".join(
        character.lower() if character.isalnum() else " " for character in title
    ).split() if part)[:40]
    return f"{RUN_PREFIX}{slug or 'run'}:{uuid.uuid4().hex[:8]}"


class Workspace:
    """One workspace, named by its id or by its name.

    Its calls answer directly with the sync client, and are awaitable with
    the async one.
    """

    def __init__(self, client: Any, ref: str) -> None:
        self.client = client
        self.ref = ref
        self._info: WorkspaceInfo | None = None
        self.files = Files(self)
        self.keys = WorkspaceKeys(self)
        self.members = Members(self)

    # -- identity --------------------------------------------------------

    def id(self) -> Any:
        """Its id; the sync client answers it, the async client returns it."""
        return self.client.handoff(self._id())

    async def _id(self) -> str:
        return (await self._resolve()).id

    async def _resolve(self) -> WorkspaceInfo:
        if self._info is None:
            self._info = await self.client._workspace_info(self.ref)
        return self._info

    def info(self) -> Any:
        return self.client.handoff(self._resolve())

    def _link(self) -> Link:
        if self._info is None:
            raise ProtocolError(
                f"the workspace {self.ref!r} is not resolved yet: use an awaited call first"
            )
        return Link(self.client.transport, self._info.id)

    # -- reading ---------------------------------------------------------

    def ls(
        self, path: str | None = None, *, kind: str | None = None, limit: int | None = None
    ) -> Any:
        """The apps, an app's resources, or one resource's children and actions."""
        return self.client.handoff(self._ls(path, kind, limit))

    async def _ls(
        self,
        path: str | None,
        kind: str | None,
        limit: int | None,
        cursor: str | None = None,
    ) -> Listing:
        arguments: dict[str, Any] = {}
        if path is not None:
            arguments["path"] = path
        if kind is not None:
            arguments["kind"] = kind
        if limit is not None:
            arguments["limit"] = limit
        if cursor is not None:
            arguments["cursor"] = cursor
        answer = await self._call("tilo_ls", arguments)
        return Listing.of(
            answer,
            self.client,
            lambda next_cursor: self._ls(path, kind, limit, next_cursor),
        )

    def read(self, path: str) -> Any:
        """A resource read whole: its fields, its children and its actions."""
        return Resource(self, path).load()

    def resource(self, path: str) -> Resource:
        """A resource as a typed object: fields, actions and field edits."""
        return Resource(self, path)

    def find(self, query: str, app: str | None = None, limit: int | None = None) -> Any:
        """Search the workspace by words, best first."""
        return self.client.handoff(self._find(query, app, limit))

    async def _find(self, query: str, app: str | None, limit: int | None) -> list[Hit]:
        arguments: dict[str, Any] = {"query": query}
        if app is not None:
            arguments["app"] = app
        if limit is not None:
            arguments["limit"] = limit
        answer = await self._call("tilo_find", arguments)
        return [
            Hit.of(hit)
            for hit in answer.get("hits") or []
            if isinstance(hit, Mapping)
        ]

    # -- acting ----------------------------------------------------------

    def do(self, path: str, **args: Any) -> Any:
        """Run one action: its result, or the `Run` that follows it."""
        return self.client.handoff(self._outcome(path, args))

    async def _outcome(self, path: str, args: Mapping[str, Any]) -> Any:
        await self._resolve()
        return await self._link().outcome(path, dict(args))

    def status(self, invocation_id: str, cancel: bool = False) -> Any:
        """How a pending invocation went, or stop it."""
        return self.client.handoff(self._status(invocation_id, cancel))

    async def _status(self, invocation_id: str, cancel: bool) -> dict[str, Any]:
        await self._resolve()
        return await self._link().status(invocation_id, cancel)

    def program(self, source: str, confirm: bool | None = None) -> Any:
        """Run a tilolang program as this run's task."""
        return self.client.handoff(self._program(source, confirm))

    async def _program(self, source: str, confirm: bool | None) -> Any:
        await self._resolve()
        return await self._link().program(source, confirm)

    def watch(self, path: str | None = None, wait_ms: int | None = None) -> Any:
        """Each change, run event and presence change, the cursor kept."""
        return self.client.handoff(self._watch(path, wait_ms))

    async def _watch(self, path: str | None, wait_ms: int | None) -> AsyncIterator[Event]:
        await self._resolve()
        async for event in self._link().watch(path, wait_ms):
            yield event

    def run(self, title: str) -> RunOpening:
        """Group the calls inside into one run: one undo step per app."""
        return RunOpening(self, title)

    async def _call(self, tool: str, arguments: Mapping[str, Any]) -> dict[str, Any]:
        await self._resolve()
        return await self._link().call(tool, arguments)

    # -- files and keys ---------------------------------------------------

    async def _upload(self, data: bytes) -> str:
        workspace = await self._id()
        transport = self.client.transport
        session = transport.session(workspace)
        if session.id is None:
            await session.open()
        response = await transport.request(
            "PUT", f"/w/{workspace}/content", data=data, session=session.id
        )
        body: Mapping[str, Any] = {}
        try:
            parsed = response.json()
            if isinstance(parsed, dict):
                body = parsed
        except ValueError:
            body = {}
        if response.status_code >= 400 or not isinstance(body.get("content"), str):
            error = body.get("error")
            if isinstance(error, Mapping):
                raise Refused(
                    str(error.get("code") or "upload_failed"),
                    str(error.get("message") or ""),
                    [str(step) for step in error.get("next_steps") or []],
                )
            raise ProtocolError(
                f"the door answered the upload with {response.status_code}", status=response.status_code
            )
        return str(body["content"])

    async def _download(self, content: str) -> bytes:
        received = bytearray()
        while True:
            answer = await self._call(
                "tilo_get", {"content": content, "offset": len(received)}
            )
            piece = answer.get("bytes")
            size = answer.get("size")
            if not isinstance(piece, str) or not isinstance(size, int):
                raise ProtocolError(f"tilo_get answered no bytes for {content}")
            received += base64.b64decode(piece)
            if len(received) >= size:
                return bytes(received)
            if not piece:
                raise ProtocolError(
                    f"tilo_get stopped at {len(received)} of {size} bytes of {content}"
                )

    async def _mint(self, name: str | None, expires_in: int | None) -> MintedKey:
        workspace = await self._id()
        return await self.client._mint(workspace, name, expires_in)

    def mint_mcp(self, name: str | None = None, expires_in: int | None = None) -> Any:
        """A key for an agent to act in this workspace; `ws.keys.mint` is the same."""
        return self.keys.mint(name, expires_in)

    def __repr__(self) -> str:
        return f"Workspace({self._info.name if self._info else self.ref!r})"


__all__ = [
    "Workspace",
    "Files",
    "WorkspaceKeys",
    "Members",
    "RunOpening",
    "ProgramAnswer",
    "Event",
]
