"""The client: the account's workspaces and keys, and the transport they go to."""

from __future__ import annotations

import asyncio
import inspect
import time
import uuid
from collections.abc import Mapping
from typing import Any

from ._api import Api
from ._endpoint import Endpoint
from ._errors import AskPending, ProtocolError, Refused
from ._models import Key, MintedKey, Usage, WorkspaceInfo, WorkspaceList
from ._sync import LoopThread
from ._transport import HttpTransport, Transport, native_transport


class Workspaces:
    """The person's workspaces: list them, make one, delete one."""

    def __init__(self, client: AsyncTilo) -> None:
        self.client = client

    def list(self) -> Any:
        """Every workspace this key reaches, wherever it is hosted."""
        return self.client.handoff(self.client._workspace_list())

    def create(self, name: str) -> Any:
        """Make a workspace of the person's own, named `name`."""
        return self.client.handoff(self.client._create_workspace(name))

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
        `AskPending` with the link they approve it at; `await ask.wait()` is
        the `Key` once they do.
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

    @property
    def approval(self) -> str:
        """The last part of the link: the ask it names."""
        return self.url.rstrip("/").rsplit("/", 1)[-1]

    def wait(self) -> Any:
        """Follow the approval until the person decides, then the `Key`."""
        return self.client.handoff(self._wait())

    async def _wait(self) -> Key:
        deadline = time.monotonic() + self.expires_in
        while True:
            response = await self.client.api.call(
                "POST", "/keys/poll", json={"poll": self.poll}, mutating=False
            )
            body = response.json()
            if response.status == 200:
                return Key.of(body if isinstance(body, Mapping) else {})
            if response.status == 202:
                if time.monotonic() >= deadline:
                    raise Refused("expired", "nothing approved the key in time")
                await asyncio.sleep(self.interval)
                continue
            raise ProtocolError(
                f"the approval answered {response.status}", status=response.status
            )

    def __repr__(self) -> str:
        return f"SignIn({self.url!r})"


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
        url: str | None = None,
        *,
        http: Any = None,
        retry_deadline: float | None = None,
        transport: Transport | None = None,
    ) -> None:
        """`url` is the API server (`TILO_API_URL`, `https://api.tilo.sh`) and
        `key` the `tilo_sk_…` it takes (`TILO_API_KEY`). `retry_deadline` is
        how long, in seconds, a call keeps asking again while the host is busy
        with the person's changes (60 by default; 0 raises `RateLimited` at
        once). `transport` is where the calls go instead of HTTP, as
        [`AsyncTilo.host`] makes one."""
        self.transport = transport or HttpTransport(url, key, http=http)
        self.api = Api(self.transport, retry_deadline)
        self.catalog: WorkspaceList | None = None
        self.workspaces = Workspaces(self)
        self.keys = AccountKeys(self)
        # The run a group of the sync client's calls is a step of: it has no
        # contextvars across threads, so it names the run here.
        self.run_group: Any = None
        self.loop: Any = None

    @classmethod
    def host(cls, origin: str, key_file: Any = None) -> AsyncTilo:
        """A client of the Tilo host at `origin`, served in process.

        It is the same `/v1` a server answers over HTTP, served by the host
        itself from the Rust extension `tilo._native`: no socket, and no key
        over the wire. `key_file` names this client's key, a device of the
        person: the host makes one there the first time.
        """
        return cls(transport=native_transport(origin, key_file))

    # -- what the sync client and this one share -------------------------

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

    async def aclose(self) -> None:
        """Close what this client keeps open: the HTTP connections, say."""
        await self.transport.aclose()

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

    def workspace(self, id_or_name: str) -> Any:
        """One workspace, by its id or by its name."""
        from ._workspace import Workspace

        return Workspace(self, id_or_name)

    async def _workspace_list(self) -> WorkspaceList:
        endpoint = self._endpoint()
        answer = await endpoint.json("GET", endpoint.path("workspaces"))
        self.catalog = WorkspaceList.of(answer)
        return self.catalog

    async def _workspace_info(self, ref: str) -> WorkspaceInfo:
        try:
            listing = await self._workspace_list()
        except (Refused, ProtocolError):
            if _looks_like_id(ref):
                return WorkspaceInfo(id=ref, name="", host="")
            raise
        found = listing.named(ref)
        # A workspace the listing does not name still answers to its id, as
        # the API decides.
        if found is None and _looks_like_id(ref):
            return WorkspaceInfo(id=ref, name="", host="")
        if found is None:
            raise Refused(
                "not_found",
                f"no workspace {ref!r}",
                ["await tilo.workspaces.list() names them"],
            )
        return found

    async def _create_workspace(self, name: str) -> WorkspaceInfo:
        endpoint = self._endpoint()
        answer = await endpoint.json("POST", endpoint.path("workspaces"), json={"name": name})
        self.catalog = None
        return WorkspaceInfo.of(answer)

    async def _workspace_id(self, workspace: Any) -> str:
        """The id of a workspace named by it, by its name, or as a `Workspace`."""
        if isinstance(workspace, str):
            if _looks_like_id(workspace):
                return workspace
            return (await self._workspace_info(workspace)).id
        resolve = getattr(workspace, "_id", None)
        if resolve is not None:
            return await resolve()
        raise ProtocolError(
            f"a workspace is named by an id, a name or a Workspace, not {workspace!r}"
        )

    async def _delete_workspace(self, workspace: Any) -> Any:
        endpoint = self._endpoint()
        answer = await endpoint.json(
            "DELETE",
            endpoint.path("workspaces", await self._workspace_id(workspace)),
        )
        self.catalog = None
        return answer

    def usage(self) -> Any:
        """The person's account on the host: per workspace, what it keeps and
        the VM time it ran. A key reaching all their workspaces reads it."""
        return self.handoff(self._usage())

    async def _usage(self) -> Usage:
        endpoint = self._endpoint()
        return Usage.of(await endpoint.json("GET", endpoint.path("account")))

    # -- keys ------------------------------------------------------------

    async def _keys_list(self) -> list[Key]:
        endpoint = self._endpoint()
        answer = await endpoint.json("GET", endpoint.path("keys"))
        keys = answer.get("keys")
        return [Key.of(key) for key in keys or [] if isinstance(key, Mapping)]

    async def _mint(
        self, workspace: Any, name: str | None, expires_in: int | None
    ) -> MintedKey:
        body: dict[str, Any] = {"workspace": await self._workspace_id(workspace)}
        if name is not None:
            body["name"] = name
        if expires_in is not None:
            body["expires_in"] = expires_in
        endpoint = self._endpoint()
        return MintedKey.of(await endpoint.json("POST", endpoint.path("keys"), json=body))

    async def _revoke(self, grant: str) -> Any:
        endpoint = self._endpoint()
        return await endpoint.json("DELETE", endpoint.path("keys", grant))

    async def _key_create(
        self,
        name: str,
        workspace: Any,
        expires_in: int | None,
        browser_pages: bool = True,
    ) -> Key:
        sign_in = await self._sign_in(
            name,
            None if workspace is None else await self._workspace_id(workspace),
            expires_in,
            browser_pages,
        )
        raise AskPending(
            sign_in.url,
            sign_in.approval,
            question={"name": name},
            follow=sign_in._wait,
        )

    def sign_in(
        self,
        name: str | None = None,
        workspace: str | None = None,
        expires_in: int | None = None,
        browser_pages: bool = True,
    ) -> Any:
        """A key this machine does not have yet: show the link, wait for approval.

        It lasts until revoked unless `expires_in` (seconds) says otherwise,
        and operates web pages in Browser unless `browser_pages` is false.
        """
        return self.handoff(self._sign_in(name, workspace, expires_in, browser_pages))

    async def _sign_in(
        self,
        name: str | None,
        workspace: str | None,
        expires_in: int | None,
        browser_pages: bool = True,
    ) -> SignIn:
        body: dict[str, Any] = {}
        if name is not None:
            body["name"] = name
        if workspace is not None:
            body["workspace"] = workspace
        if expires_in is not None:
            body["expires_in"] = expires_in
        if not browser_pages:
            body["browser_pages"] = False
        answer = await self.api.json("POST", "/keys/new", json=body, mutating=False)
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
        url: str | None = None,
        *,
        http: Any = None,
        retry_deadline: float | None = None,
        transport: Transport | None = None,
    ) -> None:
        super().__init__(
            key, url, http=http, retry_deadline=retry_deadline, transport=transport
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