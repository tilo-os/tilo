"""The client: the account's workspaces and keys, and the door it all goes to."""

from __future__ import annotations

import asyncio
import time
import uuid
from typing import Any, Mapping

from ._errors import AskPending, ProtocolError, Refused
from ._link import Link
from ._models import Key, MintedKey, Usage, WorkspaceInfo, WorkspaceList
from ._sync import LoopThread
from ._wire import Transport

ACCOUNT = "account"


class Workspaces:
    """The person's workspaces: list them, make one, delete one."""

    def __init__(self, client: AsyncTilo) -> None:
        self.client = client

    def list(self) -> Any:
        """Every workspace this key reaches, and the hosts to create one on."""
        return self.client.handoff(self.client._workspace_list())

    def create(self, name: str, host: str | None = None) -> Any:
        """Create a workspace by name, on `host` (the default host without one)."""
        return self.client.handoff(self.client._create_workspace(name, host))

    def delete(self, workspace: Any) -> Any:
        """Delete a workspace: refused today, the host cannot yet."""
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
        self, name: str, workspace: Any = None, expires_in: int | None = None
    ) -> Any:
        """Ask the person for a new key.

        A key reaching all their workspaces lasts until revoked unless
        `expires_in` (seconds) says when it expires; one reaching a single
        workspace lasts at most 89 days. Raises `AskPending` with the link
        they approve it at; `await ask.wait()` answers the `Key` once they do.
        """
        return self.client.handoff(
            self.client._key_create(name, workspace, expires_in)
        )

    def mint(
        self,
        workspace: Any,
        name: str | None = None,
        expires_in: int | None = None,
    ) -> Any:
        """A key for one workspace, for an agent there; narrower than your own."""
        return self.client.handoff(self.client._mint(workspace, name, expires_in))

    def revoke(self, grant: str) -> Any:
        """Revoke a key and every key minted from it."""
        return self.client.handoff(self.client._revoke(grant))


class SignIn:
    """A key being approved: the link to show, and the wait for it."""

    def __init__(
        self,
        client: AsyncTilo,
        url: str,
        poll: str,
        interval: float,
        expires_in: float,
    ) -> None:
        self.client = client
        self.url = url
        self.poll = poll
        self.interval = interval
        self.expires_in = expires_in

    def wait(self) -> Any:
        """Follow the approval until the person decides, then the `Key`."""
        return self.client.handoff(self._wait())

    async def _wait(self) -> Key:
        deadline = time.monotonic() + self.expires_in
        while True:
            response = await self.client.transport.request(
                "POST", "/keys/poll", body={"poll": self.poll}
            )
            body = _json_of(response)
            if response.status_code == 200:
                return Key.of(body)
            if response.status_code == 202:
                if time.monotonic() >= deadline:
                    raise Refused("expired", "nothing approved the key in time")
                await asyncio.sleep(self.interval)
                continue
            error = body.get("error")
            if isinstance(error, Mapping):
                raise Refused(
                    str(error.get("code") or "failed"),
                    str(error.get("message") or ""),
                    [str(step) for step in error.get("next_steps") or []],
                )
            raise ProtocolError(
                f"the door answered the poll with {response.status_code}",
                status=response.status_code,
            )

    def __repr__(self) -> str:
        return f"SignIn({self.url!r})"


def _json_of(response: Any) -> Mapping[str, Any]:
    try:
        body = response.json()
    except ValueError:
        return {}
    return body if isinstance(body, Mapping) else {}


def _looks_like_id(ref: str) -> bool:
    try:
        uuid.UUID(ref)
    except ValueError:
        return False
    return True


class AsyncTilo:
    """The account: `await tilo.workspaces.list()`, `await ws.do(...)`."""

    sync = False

    def __init__(
        self,
        key: str | None = None,
        door: str | None = None,
        *,
        http: Any = None,
        retry_deadline: float | None = None,
    ) -> None:
        """`retry_deadline` is how long, in seconds, a call keeps asking again
        while the host is busy with the person's changes (60 by default; 0
        raises `RateLimited` at once)."""
        self.transport = Transport(door, key, http, retry_deadline)
        self.catalog: WorkspaceList | None = None
        self.workspaces = Workspaces(self)
        self.keys = AccountKeys(self)

    # -- what the sync client and this one share -------------------------

    def handoff(self, value: Any) -> Any:
        return self.transport.handoff(value)

    async def aclose(self) -> None:
        await self.transport.aclose()

    def close(self) -> Any:
        if self.sync:
            self.transport.loop.run(self.transport.aclose())
            self.transport.loop.stop()
            return None
        return self.aclose()

    async def __aenter__(self) -> AsyncTilo:
        return self

    async def __aexit__(self, *_: Any) -> None:
        await self.aclose()

    # -- the account -----------------------------------------------------

    def _link(self) -> Link:
        return Link(self.transport, None)

    def workspace(self, id_or_name: str) -> Any:
        """One workspace, by its id or by its name."""
        from ._workspace import Workspace

        return Workspace(self, id_or_name)

    async def _workspace_list(self) -> WorkspaceList:
        result = await self._link().outcome(f"{ACCOUNT}/workspaces#list", {})
        self.catalog = WorkspaceList.of(result if isinstance(result, Mapping) else {})
        return self.catalog

    async def _workspace_info(self, ref: str) -> WorkspaceInfo:
        try:
            listing = await self._workspace_list()
        except (Refused, ProtocolError):
            if _looks_like_id(ref):
                return WorkspaceInfo(id=ref, name="", host="")
            raise
        found = listing.named(ref)
        # A workspace the door does not name still answers to its id, as the
        # door decides.
        if found is None and _looks_like_id(ref):
            return WorkspaceInfo(id=ref, name="", host="")
        if found is None:
            raise Refused(
                "not_found",
                f"no workspace {ref!r}",
                ["await tilo.workspaces.list() names them"],
            )
        return found

    async def _create_workspace(self, name: str, host: str | None) -> WorkspaceInfo:
        arguments: dict[str, Any] = {"name": name}
        if host is not None:
            arguments["host"] = host
        result = await self._link().outcome(f"{ACCOUNT}/workspaces#create", arguments)
        self.catalog = None
        workspace = result.get("workspace") if isinstance(result, Mapping) else None
        if not isinstance(workspace, Mapping):
            raise ProtocolError("the door created a workspace without naming it")
        return WorkspaceInfo.of(workspace)

    async def _workspace_id(self, workspace: Any) -> str:
        """The id of a workspace named by it, by its name, or as a `Workspace`."""
        if isinstance(workspace, str):
            if _looks_like_id(workspace):
                return workspace
            return (await self._workspace_info(workspace)).id
        resolve = getattr(workspace, "_id", None)
        if resolve is not None:
            return await resolve()
        raise ProtocolError(f"a workspace is named by an id, a name or a Workspace, not {workspace!r}")

    async def _delete_workspace(self, workspace: Any) -> Any:
        result = await self._link().outcome(
            f"{ACCOUNT}/workspaces#delete", {"workspace": await self._workspace_id(workspace)}
        )
        self.catalog = None
        return result

    def usage(self) -> Any:
        """The person's account on the host: per workspace, what it keeps and
        the VM time it ran. A key reaching all their workspaces reads it."""
        return self.handoff(self._usage())

    async def _usage(self) -> Usage:
        answer = await self._link().call("tilo_read", {"path": f"{ACCOUNT}/usage"})
        fields = answer.get("fields")
        return Usage.of(fields if isinstance(fields, Mapping) else {})

    # -- keys ------------------------------------------------------------

    async def _keys_list(self) -> list[Key]:
        result = await self._link().outcome(f"{ACCOUNT}/keys#list", {})
        keys = result.get("keys") if isinstance(result, Mapping) else None
        return [Key.of(key) for key in keys or [] if isinstance(key, Mapping)]

    async def _mint(
        self, workspace: Any, name: str | None, expires_in: int | None
    ) -> MintedKey:
        arguments: dict[str, Any] = {"workspace": await self._workspace_id(workspace)}
        if name is not None:
            arguments["name"] = name
        if expires_in is not None:
            arguments["expires_in"] = expires_in
        result = await self._link().outcome(f"{ACCOUNT}/keys#mint", arguments)
        if not isinstance(result, Mapping):
            raise ProtocolError("the door minted a key without naming it")
        return MintedKey.of(result)

    async def _revoke(self, grant: str) -> Any:
        return await self._link().outcome(f"{ACCOUNT}/keys#revoke", {"grant": grant})

    async def _key_create(
        self, name: str, workspace: Any, expires_in: int | None
    ) -> Key:
        arguments: dict[str, Any] = {"name": name}
        if workspace is not None:
            arguments["workspace"] = await self._workspace_id(workspace)
        if expires_in is not None:
            arguments["expires_in"] = expires_in
        try:
            await self._link().call("tilo_do", {
                "path": f"{ACCOUNT}/keys#create",
                "args": arguments,
            })
        except AskPending as ask:
            ask._then(lambda answer: Key.of(answer.get("result") or {}))
            raise
        raise ProtocolError("the door made a key without asking the person")

    def sign_in(
        self,
        name: str | None = None,
        workspace: str | None = None,
        expires_in: int | None = None,
    ) -> Any:
        """A key this machine does not have yet: show the link, wait for approval.

        It lasts until revoked unless `expires_in` (seconds) says otherwise.
        """
        return self.handoff(self._sign_in(name, workspace, expires_in))

    async def _sign_in(
        self, name: str | None, workspace: str | None, expires_in: int | None
    ) -> SignIn:
        body: dict[str, Any] = {}
        if name is not None:
            body["name"] = name
        if workspace is not None:
            body["workspace"] = workspace
        if expires_in is not None:
            body["expires_in"] = expires_in
        response = await self.transport.request("POST", "/keys/new", body=body)
        answer = _json_of(response)
        if response.status_code >= 400:
            error = answer.get("error")
            if isinstance(error, Mapping):
                raise Refused(
                    str(error.get("code") or "failed"),
                    str(error.get("message") or ""),
                    [str(step) for step in error.get("next_steps") or []],
                )
            raise ProtocolError(
                f"the door answered {response.status_code}", status=response.status_code
            )
        return SignIn(
            self,
            str(answer.get("approve_url") or ""),
            str(answer.get("poll") or ""),
            float(answer.get("interval") or 2),
            float(answer.get("expires_in") or 300),
        )


class Tilo(AsyncTilo):
    """The sync client: the same surface, no `await`, its own event loop."""

    sync = True

    def __init__(
        self,
        key: str | None = None,
        door: str | None = None,
        *,
        http: Any = None,
        retry_deadline: float | None = None,
    ) -> None:
        super().__init__(key, door, http=http, retry_deadline=retry_deadline)
        self.transport.sync = True
        self.transport.loop = LoopThread()

    def __enter__(self) -> Tilo:
        return self

    def __exit__(self, *_: Any) -> None:
        self.close()

    def close(self) -> None:
        if self.transport.loop is not None:
            self.transport.loop.run(self.transport.aclose())
            self.transport.loop.stop()
            self.transport.loop = None
