"""One workspace: what is in it, what to do there, and who is there."""

from __future__ import annotations

import builtins
import dataclasses
import time
import uuid
from collections.abc import AsyncIterator, Mapping, Sequence
from typing import Any

from ._endpoint import CURRENT, Endpoint, InvocationGroup, result_of
from ._errors import AskPending, ProtocolError, Refused
from ._models import (
    Event,
    Hit,
    Invitation,
    Listing,
    Member,
    MintedKey,
    ProgramAnswer,
    Row,
    InvocationList,
    WorkspaceExport,
    WorkspaceInfo,
)
from ._resource import Resource

INVOCATION_PREFIX = "sdk:"


def _bytes_of(data: Any) -> bytes:
    if isinstance(data, bytes):
        return data
    if isinstance(data, bytearray):
        return bytes(data)
    if hasattr(data, "read"):
        return data.read()
    with open(data, "rb") as file:
        return file.read()


def _reference_of(value: Any) -> str | None:
    """The `vault.item:…` reference an ask answered, or `None` when the person declined."""
    if isinstance(value, Mapping) and isinstance(value.get("result"), Mapping):
        value = value["result"]
    reference = value.get("reference") if isinstance(value, Mapping) else None
    return reference if isinstance(reference, str) else None


async def _settled(answer: Any) -> Any:
    """An action's answer, waiting for a run still under way."""
    from ._invocation import Invocation

    if isinstance(answer, Invocation):
        return await answer._result(None)
    return answer


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


class Vault:
    """The workspace's vault: secrets it keeps, used without being read.

    No secret leaves the vault: `web_call` sends one it keeps as the key of a
    request, and `ask` has the person type a new one into a field the system
    writes, on their own device.
    """

    def __init__(self, workspace: Workspace) -> None:
        self.workspace = workspace

    def web_call(
        self,
        url: str,
        key: str,
        *,
        method: str | None = None,
        headers: Mapping[str, str] | None = None,
        body: str | None = None,
        reason: str | None = None,
    ) -> Any:
        """Call a service with a key the vault keeps, by its `vault.item:…` reference.

        The vault adds the key where its item says the service takes it, and
        only to the service the item belongs to. Answers `{status, headers,
        body}` with the key struck out. The person may be asked to allow the
        call: it raises `AskPending`, and `ask.wait()` answers it once they do.
        """
        return self.workspace.client.handoff(
            self.workspace._vault_web_call(url, key, method, headers, body, reason)
        )

    def ask(
        self,
        kind: str,
        belongs_to: Sequence[str],
        *,
        username: str | None = None,
        sent_in: str | None = None,
    ) -> Any:
        """Ask the person for a secret of `kind` that belongs to `belongs_to`.

        `belongs_to` names each site or server (`https://example.com`,
        `imap://mail.example.com:993`) the secret is ever handed to, and
        `sent_in` says where a key rides in a request (`basic`,
        `header:<name>`, `query:<name>`) when not as a bearer token. Answers
        the `vault.item:…` reference it is kept under, or `None` when they
        declined.
        """
        return self.workspace.client.handoff(
            self.workspace._vault_ask(kind, belongs_to, username, sent_in)
        )


class Members:
    """The people in this workspace, and bringing someone else in.

    Inviting takes a key you created reaching all your workspaces, into one
    you administer, and invites as an editor at most, or as an administrator
    where you may make administrators.
    """

    def __init__(self, workspace: Workspace) -> None:
        self.workspace = workspace

    def invite(self, role: str = "viewer", email: str | None = None) -> Any:
        """An `Invitation`: the link that brings one person in as `role`.

        `role` is `viewer`, `commenter`, `editor` or `administrator`;
        `administrator` gives them an editor's part and the administrator
        grants, where you may make administrators.
        """
        return self.workspace.client.handoff(self._invite(role, email))

    async def _invite(self, role: str, email: str | None) -> Invitation:
        args: dict[str, Any] = {"role": role}
        if email is not None:
            args["invitee_name"] = email
        endpoint = await self.workspace._endpoint()
        result = await endpoint.act("workspace/invitations#invite", args)
        invitation = Invitation.of(result if isinstance(result, Mapping) else {})
        if email is None:
            return invitation
        # Tilo sends no mail to an address it names: the link is handed over.
        return dataclasses.replace(
            invitation,
            email=email,
            next=invitation.next
            or f"Tilo sends no email: give the person this link to send to {email}",
        )

    def list(self) -> Any:
        """The `Member`s of the workspace, each with their role."""
        return self.workspace.client.handoff(self._list())

    async def _list(self) -> builtins.list[Member]:
        endpoint = await self.workspace._endpoint()
        rows = await endpoint.act("workspace/members#list", {})
        if not isinstance(rows, builtins.list):
            return []
        return [Member.of(row) for row in rows if isinstance(row, Mapping)]


class InvocationOpening:
    """`ws.invocation(title)`: `async with` groups, `with` does too for the sync client."""

    def __init__(self, workspace: Workspace, title: str) -> None:
        self.workspace = workspace
        self.title = title
        self.group: InvocationGroup | None = None
        self._token: Any = None
        self._previous: Any = None

    def _begin(self) -> InvocationGroup:
        if self.group is None:
            self.group = InvocationGroup(
                self.workspace._endpoint_now(), self.title, _invocation_id(self.title)
            )
        client = self.workspace.client
        if client.sync:
            self._previous = client.invocation_group
            client.invocation_group = self.group
        else:
            self._token = CURRENT.set(self.group)
        return self.group

    def _close(self) -> None:
        client = self.workspace.client
        if client.sync:
            client.invocation_group = self._previous
        elif self._token is not None:
            CURRENT.reset(self._token)
            self._token = None

    async def __aenter__(self) -> InvocationGroup:
        await self.workspace._resolve()
        return self._begin()

    async def __aexit__(self, *_: Any) -> None:
        self._close()

    def __enter__(self) -> InvocationGroup:
        self.workspace.client.handoff(self.workspace._resolve())
        return self._begin()

    def __exit__(self, *_: Any) -> None:
        self._close()


def _invocation_id(title: str) -> str:
    slug = "-".join(part for part in "".join(
        character.lower() if character.isalnum() else " " for character in title
    ).split() if part)[:40]
    return f"{INVOCATION_PREFIX}{slug or 'invocation'}:{uuid.uuid4().hex[:8]}"


class Workspace:
    """One workspace, named by its id, by its name, or by neither.

    Named by neither, it is the one this computer's daemon serves. Its calls
    answer directly with the sync client, and are awaitable with the async
    one.
    """

    def __init__(self, client: Any, ref: str | None = None) -> None:
        self.client = client
        self.ref = ref
        self._info: WorkspaceInfo | None = None
        self.files = Files(self)
        self.keys = WorkspaceKeys(self)
        self.members = Members(self)
        self.vault = Vault(self)

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
        self,
        path: str | None = None,
        *,
        kind: str | None = None,
        limit: int | None = None,
        cursor: str | None = None,
    ) -> Any:
        """The apps, an app's resources, or one resource's children and actions;
        `cursor` reads on from where a page ended."""
        return self.client.handoff(self._ls(path, kind, limit, cursor))

    async def _ls(
        self,
        path: str | None,
        kind: str | None,
        limit: int | None,
        cursor: str | None = None,
    ) -> Listing:
        endpoint = await self._endpoint()
        answer = await endpoint.ls(path, kind, limit, cursor)
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

    async def _read(self, path: str, *, surface: str | None = None) -> Mapping[str, Any]:
        endpoint = await self._endpoint()
        if surface is not None:
            await endpoint.check_surface(path, surface)
        return await endpoint.read_raw(path)

    def find(self, query: str, app: str | None = None, limit: int | None = None) -> Any:
        """Search the workspace by words, best first: its `search#find` action."""
        return self.client.handoff(self._find(query, app, limit))

    async def _find(self, query: str, app: str | None, limit: int | None) -> builtins.list[Hit]:
        endpoint = await self._endpoint()
        args: dict[str, Any] = {"query": query}
        if app is not None:
            args["app"] = app
        if limit is not None:
            args["limit"] = limit
        answer = await endpoint.act("search#find", args)
        hits = answer.get("hits") if isinstance(answer, Mapping) else None
        return [Hit.of(hit) for hit in hits or [] if isinstance(hit, Mapping)]

    # -- acting ----------------------------------------------------------

    def do(self, path: str, **args: Any) -> Any:
        """Run one action: its result, or the `Invocation` that follows it."""
        return self.client.handoff(self._outcome(path, args))

    async def _outcome(
        self, path: str, args: Mapping[str, Any], *, surface: str | None = None
    ) -> Any:
        endpoint = await self._endpoint()
        return await endpoint.act(path, args, surface=surface)

    def status(self, invocation: str, cancel: bool = False) -> Any:
        """How a pending invocation went, or stop it."""
        return self.client.handoff(self._status(invocation, cancel))

    async def _status(self, invocation: str, cancel: bool) -> Mapping[str, Any]:
        return await (await self._endpoint()).status(invocation, cancel)

    def invocations(self, limit: int | None = None) -> Any:
        """The workspace's invocations in records this key may read, newest
        first, whoever made them: an `InvocationList` of `InvocationInfo`s,
        `limit` to a page (50 unless said, at most 200); iterating it reads on
        to the oldest. Of each it says only what lies in those records, and its
        totals count only those.

        Each says who made it, how it stands (`waiting` for the person,
        `running`, `ended`, `failed`, `cancelled`, `undone`), when, what it
        changed and whether it can still be taken back. Its `id` is what
        `status` reads and stops; one the host no longer remembers, after it
        restarted, is listed from the workspace's records without one.
        """
        return self.client.handoff(self._invocations(limit))

    async def _invocations(
        self, limit: int | None, cursor: str | None = None
    ) -> InvocationList:
        # The workspace's invocations are a record, read whole through the record API;
        # the page is cut from it here, by where the cursor left off.
        read = await self._read("record:tilo.invocations:")
        fields = read.get("fields") if isinstance(read, Mapping) else None
        if not isinstance(fields, Mapping):
            raise ProtocolError("the workspace answered no record of its invocations")
        items = [item for item in fields.get("items") or () if isinstance(item, Mapping)]
        offset = int(cursor) if cursor and str(cursor).isdigit() else 0
        page = max(1, min(int(limit or 50), 200))
        shown = items[offset : offset + page]
        end = offset + len(shown)
        listed = {
            "items": shown,
            "total": fields.get("total") or 0,
            "waiting": fields.get("waiting") or 0,
        }
        if end < len(items):
            listed["next_cursor"] = str(end)
        return InvocationList.of(
            listed, self.client, lambda next_cursor: self._invocations(limit, next_cursor)
        )

    def program(self, source: str, session: str | None = None) -> Any:
        """Run a tilolang program as this group's task, in the program session `session` names
        (an earlier answer's `session`), or a new one."""
        return self.client.handoff(self._program(source, session))

    async def _program(self, source: str, session: str | None) -> ProgramAnswer:
        return await (await self._endpoint()).program(source, session)

    def watch(self, path: str | None = None) -> Any:
        """Each change, invocation event and presence change, from now on."""
        return self.client.handoff(self._watch(path))

    async def _watch(self, path: str | None) -> AsyncIterator[Event]:
        async for event in (await self._endpoint()).watch(path):
            yield event

    def link(self, path: str, *, mode: str | None = None) -> Any:
        """The link to hand a person for what `path` names (`notes/Todo`).

        It is the record's `share-link` action. It opens the record in its app for the workspace's members and
        carries no secret: anyone else must be invited first. `mode` is
        `app` (the app alone, the default) or `desktop`.
        """
        return self.client.handoff(self._link(path, mode))

    async def _link(self, path: str, mode: str | None) -> str:
        endpoint = await self._endpoint()
        answer = await endpoint.act(
            f"{path}#share-link", {} if mode is None else {"mode": mode}
        )
        link = answer.get("url") if isinstance(answer, Mapping) else None
        if not isinstance(link, str) or not link:
            raise ProtocolError(f"the workspace answered no link for {path}")
        return link

    def view(
        self, path: str, *, picture: bool = False, scale: float | None = None
    ) -> Any:
        """What the app's window shows for `path`, as the person sees it.

        Every label, value and control in reading order, each control's
        action and where it stands. With `picture`, the same view as a PNG,
        named by a `tilo.content:` reference that `files.download` fetches.
        The app is started without a window when it does not run.
        """
        return self.client.handoff(self._view(path, picture, scale))

    async def _view(
        self, path: str, picture: bool, scale: float | None
    ) -> Mapping[str, Any]:
        return await (await self._endpoint()).view(path, picture, scale)

    def export(self) -> Any:
        """Everything the workspace holds, read whole: a `WorkspaceExport`.

        Its listing (with its labels) and every resource of every app, read
        by walking the listings from its apps; the ones that did not read
        are in `unread`, with why. The files the resources name download
        with `files.download`. Take one before deleting a customer's
        workspace.
        """
        return self.client.handoff(self._export())

    async def _export(self) -> WorkspaceExport:
        endpoint = await self._endpoint()
        exported_at_ms = int(time.time() * 1000)
        resources: builtins.list[Mapping[str, Any]] = []
        unread: builtins.list[Mapping[str, Any]] = []
        seen: set[str] = set()
        walk: builtins.list[str | None] = [None]
        while walk:
            path = walk.pop(0)
            cursor = None
            while True:
                try:
                    listing = await endpoint.ls(path, None, None, cursor)
                except Refused as refused:
                    unread.append({"path": path or "", "error": refused.message or refused.code})
                    break
                for row in listing.get("rows") or []:
                    if not isinstance(row, Mapping):
                        continue
                    at = row.get("path")
                    if not isinstance(at, str) or at in seen:
                        continue
                    seen.add(at)
                    if row.get("reads", True):
                        try:
                            resources.append(await self._read_whole(endpoint, at))
                        except Refused as refused:
                            unread.append({"path": at, "error": refused.message or refused.code})
                    if row.get("has_children"):
                        walk.append(at)
                following = listing.get("next_cursor")
                if not isinstance(following, str) or following == cursor:
                    break
                cursor = following
        return WorkspaceExport(
            workspace=await self._resolve(),
            exported_at_ms=exported_at_ms,
            resources=resources,
            unread=unread,
        )

    @staticmethod
    async def _read_whole(endpoint: Endpoint, path: str) -> Mapping[str, Any]:
        """`path` read with every page of what it holds."""
        whole: dict[str, Any] | None = None
        cursor = None
        while True:
            page = await endpoint.read_raw(path, cursor)
            if whole is None:
                whole = dict(page)
            else:
                whole["children"] = [*whole.get("children", []), *(page.get("children") or [])]
            following = page.get("cursor")
            if not isinstance(following, str) or following == cursor:
                whole.pop("cursor", None)
                return whole
            cursor = following

    def branch(
        self,
        name: str,
        *,
        labels: Mapping[str, str] | None = None,
        idempotency_key: str | None = None,
    ) -> Any:
        """Branch this workspace, one of your own, into a new one named
        `name`: a `WorkspaceInfo`, as `tilo.workspaces.branch` answers it.

        The branch starts with everything this one holds and goes on apart
        from it; you are its only member.
        """
        return self.client.handoff(
            self.client._branch_workspace(self, name, labels or {}, idempotency_key)
        )

    def unattended(self, on: bool = True) -> Any:
        """Let the keys made for this workspace alone run commands in its
        Linux machine (`terminal#run`) without asking the person each time,
        here and in the branches made from it; `on=False` asks again from the
        next command.

        Turning it on asks the person on their approving device, whichever
        key asks: it raises `AskPending`, and `ask.wait()` answers
        `{"workspace": id, "unattended": True}` once they allow it, or raises
        `Refused` if they do not. Turning it off asks nobody. A key reaching
        all their workspaces is still asked, and so is everything else that
        asks.
        """
        return self.client.handoff(self._unattended(on))

    async def _unattended(self, on: bool) -> Any:
        endpoint = await self._endpoint()
        answer = await endpoint.do_answer(
            "account/workspaces#unattended",
            {"workspace": endpoint.workspace, "on": on},
            grouped=False,
        )
        try:
            return endpoint.follow(answer)
        except AskPending as ask:
            raise ask._then(result_of) from None

    def invocation(self, title: str) -> InvocationOpening:
        """Group the calls inside into one invocation: one undo step per app."""
        return InvocationOpening(self, title)

    # -- files, keys and the vault ----------------------------------------

    async def _upload(self, data: bytes) -> str:
        return await (await self._endpoint()).put(data)

    async def _download(self, content: str) -> bytes:
        return await (await self._endpoint()).get(content)

    async def _mint(self, name: str | None, expires_in: int | None) -> MintedKey:
        return await self.client._mint(await self._id(), name, expires_in)

    async def _vault_web_call(
        self,
        url: str,
        key: str,
        method: str | None,
        headers: Mapping[str, str] | None,
        body: str | None,
        reason: str | None,
    ) -> Mapping[str, Any]:
        args: dict[str, Any] = {"url": url, "key": key}
        if method is not None:
            args["method"] = method
        if headers is not None:
            args["headers"] = dict(headers)
        if body is not None:
            args["body"] = body
        if reason is not None:
            args["reason"] = reason
        try:
            answer = await (await self._endpoint()).act("vault/vault#web-call", args)
        except AskPending as ask:
            raise ask._then(result_of) from None
        return await _settled(answer)

    async def _vault_ask(
        self,
        kind: str,
        belongs_to: Sequence[str],
        username: str | None,
        sent_in: str | None,
    ) -> str | None:
        args: dict[str, Any] = {"kind": kind, "belongsTo": list(belongs_to)}
        if username is not None:
            args["username"] = username
        if sent_in is not None:
            args["sentIn"] = sent_in
        try:
            answer = await (await self._endpoint()).act("vault/vault#ask", args)
        except AskPending as ask:
            raise ask._then(_reference_of) from None
        return _reference_of(await _settled(answer))

    def __repr__(self) -> str:
        return f"Workspace({self._info.name if self._info else self.ref!r})"


__all__ = [
    "Event",
    "Files",
    "Listing",
    "Members",
    "ProgramAnswer",
    "Row",
    "InvocationOpening",
    "Vault",
    "Workspace",
    "WorkspaceKeys",
]
