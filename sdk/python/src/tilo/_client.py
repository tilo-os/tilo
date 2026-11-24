"""The client: the account's workspaces and keys, over the carrier its calls go by."""

from __future__ import annotations

import inspect
import uuid
from collections.abc import Mapping
from typing import Any

from ._carrier import Carrier, DaemonCarrier, KeyCarrier, key_from
from ._endpoint import Endpoint
from ._errors import AskPending, ProtocolError, Refused
from ._invocation import Invocation
from ._models import Key, MintedKey, Usage, WorkspaceInfo, WorkspaceList
from ._sync import LoopThread

#: The keys a workspace admitted, each a record of its own: listing it names the grants this
#: key may read, and each is read as every record is. No action lists them.
KEYS_RECORD = "record:tilo.grants"


class Workspaces:
    """The person's workspaces: list them, make one, delete one."""

    def __init__(self, client: AsyncTilo) -> None:
        self.client = client

    def list(self, *, labels: Mapping[str, str] | None = None) -> Any:
        """Every workspace this key reaches, wherever it is hosted; with
        `labels`, only those labelled with every one of them."""
        if labels:
            return self.client.handoff(self.client._labelled_workspaces(labels))
        return self.client.handoff(self.client._workspace_list())

    def create(
        self,
        name: str,
        *,
        labels: Mapping[str, str] | None = None,
        idempotency_key: str | None = None,
    ) -> Any:
        """Make a workspace of the person's own, named `name`.

        `labels` (`{"customer_id": "cus_8121"}`: at most 16, read by the
        person's keys alone) are for your programs to find it by. The same
        `idempotency_key` (your customer's id, say) answers the workspace the
        first create with it made, so a retried create never makes two.
        """
        return self.client.handoff(
            self.client._create_workspace(name, labels or {}, idempotency_key)
        )

    def branch(
        self,
        source: Any,
        name: str,
        *,
        labels: Mapping[str, str] | None = None,
        idempotency_key: str | None = None,
    ) -> Any:
        """Branch `source`, a workspace of the person's own, into a new one
        named `name`.

        The branch starts with everything `source` holds, its records with
        their history and its files, and goes on apart from it: nothing
        merges back. Its chat, its members and each person's own places stay
        behind; the person is the branch's only member. `source` is an id, a
        name, a `Workspace` or what `create` answered. `labels` and
        `idempotency_key` are as `create` takes them; the source's labels do
        not come along.
        """
        return self.client.handoff(
            self.client._branch_workspace(source, name, labels or {}, idempotency_key)
        )

    def find(self, labels: Mapping[str, str]) -> Any:
        """The one workspace labelled with every one of `labels`, or `None`;
        `Refused("conflict")` when more than one is."""
        return self.client.handoff(self.client._find_workspace(labels))

    def delete(self, workspace: Any) -> Any:
        """Delete a workspace of the person's own, and everything in it: final.

        Its records, files and history go from the host and stop counting
        against the person's storage; anyone in it is told it was deleted,
        and every key minted for it is revoked. Answers `{"deleted": id,
        "revoked_keys": [grant, ...]}`. Only a key reaching all the person's
        workspaces deletes; a workspace they were invited into is `not_found`,
        as is one already deleted. The call is the confirmation: nothing asks
        again.
        """
        return self.client.handoff(self.client._delete_workspace(workspace))

    def named(self, id_or_name: str) -> WorkspaceInfo | None:
        """The workspace an id or name names, from the last listing."""
        listing = self.client.catalog
        return None if listing is None else listing.named(id_or_name)


class AccountKeys:
    """The account's keys: this key, the keys it minted, and new ones."""

    def __init__(self, client: AsyncTilo) -> None:
        self.client = client

    def list(self) -> Any:
        """This key and the keys it minted."""
        return self.client.handoff(self.client._keys_list())

    def create(
        self,
        name: str,
        workspace: Any = None,
        expires_in: int | None = None,
        browser_pages: bool = True,
    ) -> Any:
        """Ask the person for a new key: their device approves it.

        A key reaching all their workspaces lasts until revoked unless
        `expires_in` (seconds) says when it expires; one reaching a single
        workspace lasts at most 89 days. `browser_pages` (on unless you say
        no) lets the key operate web pages in its workspace's Browser —
        navigating, filling in, selecting and clicking — without asking the
        person each time; submitting a page still asks them. Raises
        `AskPending`; `ask.wait()` is the `Key` once they approve it.
        """
        return self.client.handoff(
            self.client._key_create(name, workspace, expires_in, browser_pages)
        )

    def mint(
        self,
        workspace: Any,
        name: str | None = None,
        expires_in: int | None = None,
    ) -> Any:
        """A key for one workspace, for an agent there; narrower than your own.

        The person is not asked: the key is minted from this one, reaches
        `workspace` alone and ends no later than this key does.
        """
        return self.client.handoff(self.client._mint(workspace, name, expires_in))

    def revoke(self, grant: str) -> Any:
        """Revoke a key and every key minted from it."""
        return self.client.handoff(self.client._revoke(grant))


def _looks_like_id(ref: str) -> bool:
    try:
        uuid.UUID(ref)
    except ValueError:
        return False
    return True


def _key_of(value: Any) -> Key:
    """The `Key` an action answered, here or under the whole answer's `result`."""
    if isinstance(value, Mapping):
        result = value.get("result")
        return Key.of(result if isinstance(result, Mapping) else value)
    return Key.of({})


def _invocation_of(action: str, idempotency_key: str | None) -> str:
    """The invocation a call runs under.

    A caller's `idempotency_key` names the same invocation every time, so a
    retry of that action answers what the first run answered instead of
    running it twice.
    """
    if idempotency_key is None:
        return str(uuid.uuid4())
    return str(uuid.uuid5(uuid.NAMESPACE_URL, f"tilo-sdk:{action}:{idempotency_key}"))


class AsyncTilo:
    """The account: `await tilo.workspaces.list()`, `await ws.do(...)`."""

    sync = False

    def __init__(
        self,
        key: str | None = None,
        url: str | None = None,
        *,
        socket: str | None = None,
        workspace: str | None = None,
        carrier: Carrier | None = None,
        retry_deadline: float = 60.0,
    ) -> None:
        """Where the calls go.

        `carrier` given is where they go. Else a key (`key`, or
        `TILO_API_KEY`) reaches the API server at `url` (`TILO_API_URL`,
        `https://api.tilo.sh`) over HTTPS. Else the client reaches this
        computer's daemon through the `tilo._native` extension, connecting as
        whoever runs this program: `socket` names the daemon's endpoint when
        it is not the one this computer's login keeps, and `workspace` the one
        workspace the connection acts in when the launcher said nothing.

        `retry_deadline` is how long, in seconds, a key's call keeps asking
        again while the server is briefly unavailable or the workspace is not
        ready (60 by default; 0 raises its refusal at once).
        """
        self.key = key
        self.url = url
        self.socket = socket
        self.workspace_ref = workspace
        self.carrier = carrier
        self.retry_deadline = retry_deadline
        self.catalog: WorkspaceList | None = None
        self.workspaces = Workspaces(self)
        self.keys = AccountKeys(self)
        # What each workspace's app publishes, once a call checked it: (workspace, app) → surface.
        self.surfaces: dict[tuple[str | None, str], str] = {}
        # The run a group of the sync client's calls is a step of: it has no
        # contextvars across threads, so it names the run here.
        self.invocation_group: Any = None
        self.loop: Any = None

    async def _carrier(self) -> Carrier:
        """The carrier this client's calls go by, opened the first time."""
        if self.carrier is None:
            key = key_from(self.key)
            if key is not None:
                self.carrier = KeyCarrier(
                    key, self.url, retry_deadline=self.retry_deadline
                )
            else:
                self.carrier = await DaemonCarrier.connect(self.socket, self.workspace_ref)
        return self.carrier

    # -- what the sync client and this one share -------------------------

    def handoff(self, value: Any) -> Any:
        """`value` as this client's caller takes it: awaited, or walked here."""
        if not self.sync:
            return value
        if inspect.isawaitable(value):
            try:
                return self.loop.run(value)
            except AskPending as ask:
                ask._on_sync_loop(self.loop)
                raise
        if hasattr(value, "__anext__"):
            from ._sync import SyncIterator

            return SyncIterator(value, self.loop)
        return value

    async def aclose(self) -> None:
        """Close what this client keeps open: its connections, say."""
        if self.carrier is not None:
            await self.carrier.aclose()

    def close(self) -> Any:
        """Close the client; with the sync client this stops its event loop too."""
        if self.sync:
            if self.loop is not None:
                self.loop.run(self.aclose())
                self.loop.stop()
                self.loop = None
            return None
        return self.aclose()

    async def __aenter__(self) -> AsyncTilo:
        return self

    async def __aexit__(self, *_: Any) -> None:
        await self.aclose()

    # -- the account -----------------------------------------------------

    def _endpoint(self) -> Endpoint:
        return Endpoint(self)

    def workspace(self, ref: str | None = None) -> Any:
        """One workspace, by its id or by its name.

        With no `ref`, the one workspace this computer's daemon serves: an app
        the launcher started acts in that one. A key names a workspace in
        every call, so it must give one.
        """
        from ._workspace import Workspace

        return Workspace(self, ref)

    async def _workspace_list(self) -> WorkspaceList:
        self.catalog = WorkspaceList.of(await self._account_result("workspaces", "list"))
        return self.catalog

    async def _account(
        self,
        resource: str,
        action: str,
        body: Mapping[str, Any] | None = None,
        idempotency_key: str | None = None,
    ) -> Any:
        """An action of the account (`account/workspaces#create`), as every action
        is run: its result, the `Invocation` that follows it, or its ask."""
        endpoint = self._endpoint()
        path = f"account/{resource}#{action}"
        answer = await endpoint.do_answer(
            path, body or {}, invocation_id=_invocation_of(path, idempotency_key)
        )
        return endpoint.follow(answer, path)

    async def _account_result(
        self,
        resource: str,
        action: str,
        body: Mapping[str, Any] | None = None,
        idempotency_key: str | None = None,
    ) -> Mapping[str, Any]:
        """An account action's result, waiting for a run still under way."""
        answer = await self._account(resource, action, body, idempotency_key)
        if isinstance(answer, Invocation):
            answer = await answer._result(None)
        return answer if isinstance(answer, Mapping) else {}

    async def _workspace_info(self, ref: str | None) -> WorkspaceInfo:
        carrier = await self._carrier()
        served = carrier.workspace
        if ref is None:
            if served is None:
                raise ProtocolError(
                    "name a workspace: this client reaches more than the one this "
                    "computer serves"
                )
            return WorkspaceInfo(id=served, name="", host="")
        # The workspace this computer serves, and any workspace's id, are known
        # without the account's listing, which an app may not read.
        if _looks_like_id(ref) or ref == served:
            return WorkspaceInfo(id=ref, name="", host="")
        found = (await self._workspace_list()).named(ref)
        if found is None:
            raise Refused(
                "not_found",
                f"no workspace {ref!r}",
                ["await tilo.workspaces.list() names them"],
            )
        return found

    async def _create_workspace(
        self,
        name: str,
        labels: Mapping[str, str] | None = None,
        idempotency_key: str | None = None,
    ) -> WorkspaceInfo:
        body: dict[str, Any] = {"name": name}
        if labels:
            body["labels"] = dict(labels)
        answer = await self._account_result("workspaces", "create", body, idempotency_key)
        self.catalog = None
        return WorkspaceInfo.of(answer.get("workspace") or {})

    async def _branch_workspace(
        self,
        source: Any,
        name: str,
        labels: Mapping[str, str] | None = None,
        idempotency_key: str | None = None,
    ) -> WorkspaceInfo:
        source_id = (
            source.id
            if isinstance(source, WorkspaceInfo)
            else await self._workspace_id(source)
        )
        body: dict[str, Any] = {"workspace": source_id, "name": name}
        if labels:
            body["labels"] = dict(labels)
        answer = await self._account_result("workspaces", "branch", body, idempotency_key)
        self.catalog = None
        return WorkspaceInfo.of(answer.get("workspace") or {})

    async def _labelled_workspaces(self, labels: Mapping[str, str]) -> WorkspaceList:
        return WorkspaceList.of(
            await self._account_result("workspaces", "list", {"labels": dict(labels)})
        )

    async def _find_workspace(self, labels: Mapping[str, str]) -> WorkspaceInfo | None:
        found = list((await self._labelled_workspaces(labels)).workspaces)
        if len(found) > 1:
            raise Refused(
                "conflict",
                f"{len(found)} workspaces are labelled so",
                ["tilo.workspaces.list(labels=...) names them"],
            )
        return found[0] if found else None

    async def _workspace_id(self, workspace: Any) -> str:
        """The id of a workspace named by it, by its name, or as a `Workspace`."""
        if isinstance(workspace, str):
            carrier = await self._carrier()
            if _looks_like_id(workspace) or workspace == carrier.workspace:
                return workspace
            return (await self._workspace_info(workspace)).id
        found = getattr(workspace, "id", None)
        if isinstance(found, str) and found:
            return found
        resolve = getattr(workspace, "_id", None)
        if resolve is not None:
            return await resolve()
        raise ProtocolError(
            f"a workspace is named by an id, a name or a Workspace, not {workspace!r}"
        )

    async def _delete_workspace(self, workspace: Any) -> Any:
        answer = await self._account_result(
            "workspaces", "delete", {"workspace": await self._workspace_id(workspace)}
        )
        self.catalog = None
        return answer

    def usage(self) -> Any:
        """The person's account on the host: per workspace, what it keeps and
        the VM time it ran. A key reaching all their workspaces reads it."""
        return self.handoff(self._usage())

    async def _usage(self) -> Usage:
        read = await self._endpoint().read_raw("account/usage")
        fields = read.get("fields")
        return Usage.of(fields if isinstance(fields, Mapping) else {})

    # -- keys ------------------------------------------------------------

    async def _keys_list(self) -> list[Key]:
        # Each workspace keeps the keys it admitted: its listing names the ones this key may
        # read, each followed to its grant. A key reaching every workspace is in each of them.
        keys: dict[str, Key] = {}
        seen: set[str] = set()
        for workspace in (await self._workspace_list()).workspaces:
            endpoint = Endpoint(self, workspace.id)
            listed = await endpoint.ls(KEYS_RECORD)
            for row in listed.get("rows") or []:
                path = row.get("path") if isinstance(row, Mapping) else None
                # A record is named by its grant: one read in another workspace is read already.
                if not isinstance(path, str) or path in seen:
                    continue
                seen.add(path)
                read = await endpoint.read_raw(path)
                grant = read.get("fields")
                if isinstance(grant, Mapping) and grant.get("revoked_ms") is None:
                    key = Key.granted(grant)
                    keys[key.grant] = key
        return list(keys.values())

    async def _mint(
        self, workspace: Any, name: str | None, expires_in: int | None
    ) -> MintedKey:
        body: dict[str, Any] = {"workspace": await self._workspace_id(workspace)}
        if name is not None:
            body["name"] = name
        if expires_in is not None:
            body["expires_in"] = expires_in
        return MintedKey.of(await self._account_result("keys", "mint", body))

    async def _revoke(self, grant: str) -> Any:
        return await self._account_result(f"keys/{grant}", "revoke")

    async def _key_create(
        self,
        name: str,
        workspace: Any,
        expires_in: int | None,
        browser_pages: bool = True,
    ) -> Key:
        body: dict[str, Any] = {"name": name, "browser_pages": bool(browser_pages)}
        if workspace is not None:
            body["workspace"] = await self._workspace_id(workspace)
        if expires_in is not None:
            body["expires_in"] = expires_in
        try:
            answer = await self._account("keys", "create", body)
        except AskPending as ask:
            # The person approves it on their device; the wait answers their Key.
            raise ask._then(_key_of) from None
        if isinstance(answer, Invocation):
            answer = await answer._result(None)
        return _key_of(answer)


class Tilo(AsyncTilo):
    """The sync client: the same surface, no `await`, its own event loop."""

    sync = True

    def __init__(
        self,
        key: str | None = None,
        url: str | None = None,
        *,
        socket: str | None = None,
        workspace: str | None = None,
        carrier: Carrier | None = None,
        retry_deadline: float = 60.0,
    ) -> None:
        super().__init__(
            key,
            url,
            socket=socket,
            workspace=workspace,
            carrier=carrier,
            retry_deadline=retry_deadline,
        )
        self.loop = LoopThread()

    def __enter__(self) -> Tilo:
        return self

    def __exit__(self, *_: Any) -> None:
        self.close()

    def close(self) -> None:
        """Close what this client keeps open, and stop its event loop."""
        if self.loop is not None:
            self.loop.run(self.aclose())
            self.loop.stop()
            self.loop = None
