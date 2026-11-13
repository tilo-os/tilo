"""One workspace: what is in it, what to do there, and who is there."""

from __future__ import annotations

import builtins
import uuid
from collections.abc import AsyncIterator, Mapping
from typing import Any
from urllib.parse import quote

from ._endpoint import CURRENT, Endpoint, RunGroup, segments
from ._errors import ProtocolError
from ._models import (
    Event,
    Hit,
    Invitation,
    Listing,
    Member,
    MintedKey,
    ProgramAnswer,
    Row,
    WorkspaceInfo,
)
from ._resource import Resource

RUN_PREFIX = "sdk:"


def _bytes_of(data: Any) -> bytes:
    if isinstance(data, bytes):
        return data
    if isinstance(data, bytearray):
        return bytes(data)
    if hasattr(data, "read"):
        return data.read()
    with open(data, "rb") as file:
        return file.read()


class Files:
    """File bytes in and out of the workspace, named `tilo.content:<hash>`.

    Upload first, then an action takes the reference; an action that makes a
    file (Writer's `export-content`) answers one to download.
    """

    def __init__(self, workspace: Workspace) -> None:
        self.workspace = workspace

    def upload(self, path_or_bytes: Any) -> Any:
        """Upload a file (a path or bytes) and answer its `tilo.content:<hash>`."""
        return self.workspace.client.handoff(self.workspace._upload(_bytes_of(path_or_bytes)))

    def download(self, content: str) -> Any:
        """The bytes of `content`, a `tilo.content:<hash>` in this workspace."""
        return self.workspace.client.handoff(self.workspace._download(content))


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

    def __init__(self, workspace: Workspace) -> None:
        self.workspace = workspace

    def invite(self, role: str = "viewer", email: str | None = None) -> Any:
        """An `Invitation`: the link that brings one person in as `role`."""
        return self.workspace.client.handoff(self._invite(role, email))

    async def _invite(self, role: str, email: str | None) -> Invitation:
        body: dict[str, Any] = {"role": role}
        if email is not None:
            body["email"] = email
        endpoint = await self.workspace._endpoint()
        result = await endpoint.json("POST", endpoint.path("members", "invitations"), json=body)
        return Invitation.of(result)

    def list(self) -> Any:
        """The `Member`s of the workspace, each with their role."""
        return self.workspace.client.handoff(self._list())

    async def _list(self) -> builtins.list[Member]:
        endpoint = await self.workspace._endpoint()
        result = await endpoint.json("GET", endpoint.path("members"))
        rows = result.get("members")
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
            self.group = RunGroup(
                self.workspace._endpoint_now(), self.title, _run_id(self.title)
            )
        client = self.workspace.client
        if client.sync:
            self._previous = client.run_group
            client.run_group = self.group
        else:
            self._token = CURRENT.set(self.group)
        return self.group

    def _close(self) -> None:
        client = self.workspace.client
        if client.sync:
            client.run_group = self._previous
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
        """The workspace as the account lists it: its id, name, host and your role."""
        return self.client.handoff(self._resolve())

    def _endpoint_now(self) -> Endpoint:
        if self._info is None:
            raise ProtocolError(
                f"the workspace {self.ref!r} is not resolved yet: use an awaited call first"
            )
        return Endpoint(self.client, self._info.id)

    async def _endpoint(self) -> Endpoint:
        await self._resolve()
        return self._endpoint_now()

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
        endpoint = await self._endpoint()
        answer = await endpoint.json(
            "GET",
            endpoint.path("resources"),
            query={"path": path, "kind": kind, "limit": limit, "cursor": cursor},
        )
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

    async def _read(self, path: str) -> Mapping[str, Any]:
        endpoint = await self._endpoint()
        return await endpoint.json("GET", endpoint.path("resources", segments(path)))

    def find(self, query: str, app: str | None = None, limit: int | None = None) -> Any:
        """Search the workspace by words, best first."""
        return self.client.handoff(self._find(query, app, limit))

    async def _find(self, query: str, app: str | None, limit: int | None) -> builtins.list[Hit]:
        endpoint = await self._endpoint()
        answer = await endpoint.json(
            "GET",
            endpoint.path("search"),
            query={"q": query, "app": app, "limit": limit},
        )
        return [
            Hit.of(hit)
            for hit in answer.get("hits") or []
            if isinstance(hit, Mapping)
        ]

    # -- acting ----------------------------------------------------------

    def do(self, path: str, **args: Any) -> Any:
        """Run one action: its result, or the `Run` that follows it."""
        return self.client.handoff(self._outcome(path, args))

    async def _outcome(
        self, path: str, args: Mapping[str, Any], *, surface: str | None = None
    ) -> Any:
        endpoint = await self._endpoint()
        return await endpoint.act(path, args, surface=surface)

    def status(self, run: str, cancel: bool = False) -> Any:
        """How a pending run went, or stop it."""
        return self.client.handoff(self._status(run, cancel))

    async def _status(self, run: str, cancel: bool) -> Mapping[str, Any]:
        return await (await self._endpoint()).status(run, cancel)

    def program(self, source: str, confirm: bool | None = None) -> Any:
        """Run a tilolang program as this run's task."""
        return self.client.handoff(self._program(source, confirm))

    async def _program(self, source: str, confirm: bool | None) -> ProgramAnswer:
        return await (await self._endpoint()).program(source, confirm)

    def watch(self, path: str | None = None) -> Any:
        """Each change, run event and presence change, from now on."""
        return self.client.handoff(self._watch(path))

    async def _watch(self, path: str | None) -> AsyncIterator[Event]:
        async for event in (await self._endpoint()).watch(path):
            yield event

    def link(self, path: str, *, mode: str | None = None) -> Any:
        """The link to hand a person for what `path` names (`notes/Todo`).

        It opens the record in its app for the workspace's members and
        carries no secret: anyone else must be invited first. `mode` is
        `app` (the app alone, the default) or `desktop`.
        """
        return self.client.handoff(self._link(path, mode))

    async def _link(self, path: str, mode: str | None) -> str:
        endpoint = await self._endpoint()
        answer = await endpoint.json(
            "GET", endpoint.path("links"), query={"path": path, "mode": mode}
        )
        link = answer.get("link")
        if not isinstance(link, str) or not link:
            raise ProtocolError(f"the workspace answered no link for {path}")
        return link

    def run(self, title: str) -> RunOpening:
        """Group the calls inside into one run: one undo step per app."""
        return RunOpening(self, title)

    # -- files and keys ---------------------------------------------------

    async def _upload(self, data: bytes) -> str:
        endpoint = await self._endpoint()
        answer = await endpoint.json(
            "POST",
            endpoint.path("uploads"),
            data=data,
            headers={"content-type": "application/octet-stream"},
        )
        content = answer.get("content")
        if not isinstance(content, str) or not content:
            raise ProtocolError("the upload answered no content reference")
        return content

    async def _download(self, content: str) -> bytes:
        endpoint = await self._endpoint()
        answer = await endpoint.call(
            "GET", endpoint.path("content", quote(content, safe="")), grouped=False
        )
        return answer.body

    async def _mint(self, name: str | None, expires_in: int | None) -> MintedKey:
        return await self.client._mint(await self._id(), name, expires_in)

    def __repr__(self) -> str:
        return f"Workspace({self._info.name if self._info else self.ref!r})"


__all__ = [
    "Event",
    "Files",
    "Listing",
    "Members",
    "ProgramAnswer",
    "Row",
    "RunOpening",
    "Workspace",
    "WorkspaceKeys",
]
