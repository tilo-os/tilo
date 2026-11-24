"""The values the SDK answers: workspaces, keys, listings, hits and events."""

from __future__ import annotations

from collections.abc import AsyncIterator, Awaitable, Callable, Iterator, Mapping, Sequence
from dataclasses import dataclass, field
from enum import StrEnum
from typing import Any

from ._errors import ProtocolError


class EventWhat(StrEnum):
    """What a change did to the resource it changed.

    `CREATED` made it, `EDITED` changed it, and `REMOVED` took it away. The
    store reads it from the recorded change; no caller works it out from what
    it saw before.
    """

    CREATED = "created"
    EDITED = "edited"
    REMOVED = "removed"


def _text(value: Any) -> str | None:
    return value if isinstance(value, str) else None


def _int(value: Any) -> int | None:
    return value if isinstance(value, int) else None


@dataclass(frozen=True)
class WorkspaceInfo:
    """One of the person's workspaces, as the account lists them.

    `host` is the host that keeps it, when one said. `role` is the part they
    take in it: `owner` for one of their own, else the role they were invited
    with (`viewer`, `commenter`, `editor`, `administrator`). It is `""` when
    the API did not say. `labels` are what the person labelled it with when
    they made it (`{"customer_id": "cus_8121"}`), read by their own keys alone.
    `unattended` is whether the person let the keys made for it alone run
    commands in its Linux machine without asking them each time.
    """

    id: str
    name: str
    host: str
    role: str = ""
    labels: Mapping[str, str] = field(default_factory=dict)
    unattended: bool = False

    @classmethod
    def of(cls, raw: Mapping[str, Any]) -> WorkspaceInfo:
        labels = raw.get("labels")
        return cls(
            id=str(raw.get("id") or ""),
            name=str(raw.get("name") or ""),
            host=str(raw.get("host") or ""),
            role=str(raw.get("role") or ""),
            labels={
                str(name): value
                for name, value in (labels.items() if isinstance(labels, Mapping) else ())
                if isinstance(value, str)
            },
            unattended=raw.get("unattended") is True,
        )


@dataclass(frozen=True)
class WorkspaceExport:
    """Everything a workspace holds, as `workspace.export()` reads it.

    `resources` are every resource of every app, each as a read answers it;
    `unread` names what did not read, each with why.
    """

    workspace: WorkspaceInfo
    exported_at_ms: int
    resources: Sequence[Mapping[str, Any]]
    unread: Sequence[Mapping[str, Any]]


@dataclass(frozen=True)
class WorkspaceList:
    """The workspaces a key reaches, wherever they are hosted."""

    workspaces: Sequence[WorkspaceInfo] = ()

    @classmethod
    def of(cls, raw: Mapping[str, Any]) -> WorkspaceList:
        return cls(
            workspaces=[
                WorkspaceInfo.of(item)
                for item in raw.get("workspaces") or []
                if isinstance(item, Mapping)
            ]
        )

    def named(self, id_or_name: str) -> WorkspaceInfo | None:
        """The workspace an id or a name names, names case-insensitively."""
        for workspace in self.workspaces:
            if workspace.id == id_or_name:
                return workspace
        folded = id_or_name.casefold()
        for workspace in self.workspaces:
            if workspace.name.casefold() == folded:
                return workspace
        return None

    def __iter__(self) -> Iterator[WorkspaceInfo]:
        return iter(self.workspaces)

    def __len__(self) -> int:
        return len(self.workspaces)


@dataclass(frozen=True)
class Key:
    """A key as the account lists it: a grant, its scope, its expiry (`None`
    for a key that lasts until revoked), when it was last used and which
    device of the person approved it. `token` is the secret itself, which only
    the answer that made the key gives: a listed key carries none."""

    grant: str
    name: str = ""
    scope: str = ""
    kind: str = ""
    created_at_ms: int | None = None
    expires_at_ms: int | None = None
    last_used_at_ms: int | None = None
    created_by: str | None = None
    browser_pages: bool = False
    token: str = ""

    @classmethod
    def of(cls, raw: Mapping[str, Any]) -> Key:
        return cls(
            grant=str(raw.get("grant") or ""),
            name=str(raw.get("name") or ""),
            scope=str(raw.get("scope") or ""),
            kind=str(raw.get("kind") or ""),
            created_at_ms=_int(raw.get("created_at_ms")),
            expires_at_ms=_int(raw.get("expires_at_ms")),
            last_used_at_ms=_int(raw.get("last_used_at_ms")),
            created_by=(
                str(raw["created_by"]) if raw.get("created_by") is not None else None
            ),
            browser_pages=bool(raw.get("browser_pages")),
            # A key being approved answers `key`; a listed one answers no secret.
            token=str(raw.get("token") or raw.get("key") or ""),
        )

    @classmethod
    def granted(cls, grant: Mapping[str, Any]) -> Key:
        """A key as the workspace that admitted it keeps its grant: `scope` the
        workspace it reaches alone, `account` for every one of the person's."""
        return cls(
            grant=str(grant.get("id") or ""),
            name=str(grant.get("name") or ""),
            scope=str(grant.get("scope") or "account"),
            kind="minted" if grant.get("parent") else "key",
            created_at_ms=_int(grant.get("created_ms")),
            expires_at_ms=_int(grant.get("expires_ms")),
        )


@dataclass(frozen=True)
class WorkspaceUsage:
    """What one workspace kept and ran on its host this billing period."""

    workspace: str
    vm_seconds: int = 0
    snapshot_seconds: int = 0
    running: bool = False
    stored_bytes: int = 0

    @classmethod
    def of(cls, raw: Mapping[str, Any]) -> WorkspaceUsage:
        return cls(
            workspace=str(raw.get("workspace") or ""),
            vm_seconds=_int(raw.get("vm_seconds")) or 0,
            snapshot_seconds=_int(raw.get("snapshot_seconds")) or 0,
            running=bool(raw.get("running")),
            stored_bytes=_int(raw.get("stored_bytes")) or 0,
        )


@dataclass(frozen=True)
class Usage:
    """The person's account on the host: its plan, and per workspace what it
    kept and the VM time it ran. `raw` is the whole account answer."""

    host: str
    plan: str | None = None
    workspaces: Sequence[WorkspaceUsage] = ()
    raw: Mapping[str, Any] = field(default_factory=dict)

    @property
    def vm_seconds(self) -> int:
        """The VM seconds every workspace ran this billing period."""
        return sum(workspace.vm_seconds for workspace in self.workspaces)

    @property
    def running(self) -> list[str]:
        """The workspaces with a VM running now."""
        return [workspace.workspace for workspace in self.workspaces if workspace.running]

    @classmethod
    def of(cls, raw: Mapping[str, Any]) -> Usage:
        subscription = raw.get("subscription")
        subscription = subscription if isinstance(subscription, Mapping) else {}
        usage = subscription.get("usage")
        usage = usage if isinstance(usage, Mapping) else {}
        return cls(
            host=str(raw.get("host") or ""),
            plan=_text(subscription.get("plan")),
            workspaces=tuple(
                WorkspaceUsage.of(row)
                for row in usage.get("workspaces") or []
                if isinstance(row, Mapping)
            ),
            raw=dict(raw),
        )


@dataclass(frozen=True)
class MintedKey:
    """A key just minted: the token to use, and where it points."""

    grant: str
    token: str
    name: str = ""
    workspace: str = ""
    mcp_url: str = ""
    expires_at_ms: int | None = None

    @classmethod
    def of(cls, raw: Mapping[str, Any]) -> MintedKey:
        return cls(
            grant=str(raw.get("grant") or ""),
            token=str(raw.get("token") or ""),
            name=str(raw.get("name") or ""),
            workspace=str(raw.get("workspace") or ""),
            mcp_url=str(raw.get("mcp_url") or ""),
            expires_at_ms=_int(raw.get("expires_at_ms")),
        )

    def client(self, url: str | None = None) -> Any:
        """A client of this key, for code that hands one to an agent."""
        from ._client import AsyncTilo

        return AsyncTilo(key=self.token, url=url)


@dataclass(frozen=True)
class Invitation:
    """An invitation into a workspace: the link that brings one person in.

    Following `link` signs them in (by approving an agent)
    and lands them in the workspace as `role`. No mail is sent: with an
    `email`, `sent` is false and `next` says to hand the link over.
    """

    link: str
    role: str
    workspace: str = ""
    invite: str = ""
    expires_at_ns: int | None = None
    email: str | None = None
    sent: bool = False
    next: str | None = None

    @classmethod
    def of(cls, raw: Mapping[str, Any]) -> Invitation:
        return cls(
            link=str(raw.get("link") or ""),
            role=str(raw.get("role") or ""),
            workspace=str(raw.get("workspace") or ""),
            invite=str(raw.get("invite") or ""),
            expires_at_ns=_int(raw.get("expires_at_ns")),
            email=_text(raw.get("email")),
            sent=bool(raw.get("sent")),
            next=_text(raw.get("next")),
        )


@dataclass(frozen=True)
class Member:
    """A person in a workspace, with their role there."""

    person: str
    role: str
    name: str | None = None
    administrator: bool = False

    @classmethod
    def of(cls, raw: Mapping[str, Any]) -> Member:
        return cls(
            person=str(raw.get("person") or ""),
            role=str(raw.get("role") or ""),
            name=_text(raw.get("name")),
            administrator=bool(raw.get("administrator")),
        )


@dataclass(frozen=True)
class Row:
    """One row of a listing: a resource, or an action of the resource listed."""

    path: str
    name: str = ""
    id: str | None = None
    """The resource's own id, which names it whatever else is titled the same."""
    key: str | None = None
    """The app's stable key for it, the name its actions answer it by."""
    label: str = ""
    kind: str = ""
    has_children: bool = False
    state: str | None = None
    actions: Sequence[Mapping[str, Any]] = ()
    raw: Mapping[str, Any] = field(default_factory=dict)

    @classmethod
    def of(cls, raw: Mapping[str, Any], kinds: Mapping[str, Any] | None = None) -> Row:
        """The row `raw`, its actions as the listing describes them: a row
        names the ones it offers by id, and the listing's `kinds` describes
        each kind's once."""
        kind = str(raw.get("kind") or "")
        described = [
            action
            for action in (kinds or {}).get(kind) or ()
            if isinstance(action, Mapping)
        ]
        actions: list[Mapping[str, Any]] = []
        for entry in raw.get("actions") or ():
            if isinstance(entry, str):
                actions.extend(action for action in described if action.get("id") == entry)
            elif isinstance(entry, Mapping):
                actions.append(entry)
        return cls(
            path=str(raw.get("path") or ""),
            name=str(raw.get("name") or ""),
            id=_text(raw.get("id")),
            key=_text(raw.get("key")),
            label=str(raw.get("label") or ""),
            kind=kind,
            has_children=bool(raw.get("has_children")),
            state=_text(raw.get("state")),
            actions=tuple(actions),
            raw=raw,
        )


@dataclass
class Listing:
    """What a listing answered: a resource's rows and actions, and a cursor."""

    path: str = ""
    kind: str = ""
    rows: Sequence[Row] = ()
    actions: Sequence[Any] = ()
    next_cursor: str | None = None
    _client: Any = None
    _more: Callable[[str], Awaitable[Listing]] | None = None

    @classmethod
    def of(
        cls,
        raw: Mapping[str, Any],
        client: Any = None,
        more: Callable[[str], Awaitable[Listing]] | None = None,
    ) -> Listing:
        return cls(
            path=str(raw.get("path") or ""),
            kind=str(raw.get("kind") or ""),
            rows=[
                Row.of(row, raw.get("kinds") if isinstance(raw.get("kinds"), Mapping) else None)
                for row in raw.get("rows") or []
                if isinstance(row, Mapping)
            ],
            actions=tuple(raw.get("actions") or ()),
            next_cursor=_text(raw.get("next_cursor")),
            _client=client,
            _more=more,
        )

    def more(self) -> Any:
        """The next page, or `None` when the listing is whole."""
        if self._more is None or self.next_cursor is None:
            return None
        return self._client.handoff(self._more(self.next_cursor))

    async def __aiter__(self) -> AsyncIterator[Row]:
        page: Listing | None = self
        while page is not None:
            for row in page.rows:
                yield row
            page = await page.more() if page.next_cursor else None

    def __iter__(self) -> Iterator[Row]:
        if not getattr(self._client, "sync", False):
            raise ProtocolError("the async client walks a listing with `async for`")
        page: Listing | None = self
        while page is not None:
            yield from page.rows
            page = page.more()


@dataclass(frozen=True)
class InvocationInfo:
    """One invocation of a workspace, as its listing names it: who made it,
    how it stands, when, and what it changed.

    `id` is what `ws.status(id)` reads and its `undo` takes back, while the
    host remembers its calls; one known only from the workspace's records (the
    host restarted since) has none, and is told apart by `step`, the undo step
    its changes keep in each record's history.
    """

    status: str
    """`waiting` (for the person), `running`, `ended`, `failed`,
    `cancelled` or `undone`."""
    by: str
    """Who made it: the key's name, an agent's or a person's."""
    by_kind: str = ""
    """`agent` (every key is one), `person`, `bot` or `automation`."""
    id: str | None = None
    calls: Sequence[str] = ()
    step: str | None = None
    title: str | None = None
    failure: str | None = None
    started_ms: int = 0
    ended_ms: int | None = None
    action: str | None = None
    summary: str | None = None
    """What its newest change did, without the values it carried: the
    action by its published label."""
    changes: int = 0
    undone: int = 0
    records: Sequence[Mapping[str, Any]] = ()
    undoable: bool = False
    undo_blocked: str | None = None
    recorded: bool = False
    remembered: bool = False
    origin: Mapping[str, Any] | None = None
    """The resource the call was made from (`chat.workspace.channels:c1` with its
    kind), when it named one: whoever may write it may stop the run or run it
    again."""
    retries: str | None = None
    """The invocation this one runs again, when it is a retry."""
    spend: Mapping[str, Any] | None = None
    """What a model call it made cost: `provider`, `model`, `input_tokens`,
    `output_tokens` and `cost_micros`."""
    raw: Mapping[str, Any] = field(default_factory=dict)

    @classmethod
    def of(cls, raw: Mapping[str, Any]) -> InvocationInfo:
        by = raw.get("by") if isinstance(raw.get("by"), Mapping) else {}
        return cls(
            status=str(raw.get("status") or ""),
            by=str(by.get("name") or ""),
            by_kind=str(by.get("kind") or ""),
            id=_text(raw.get("id")),
            calls=tuple(call for call in raw.get("calls") or () if isinstance(call, str)),
            step=_text(raw.get("step")),
            title=_text(raw.get("title")),
            failure=_text(raw.get("failure")),
            started_ms=_int(raw.get("started_ms")) or 0,
            ended_ms=_int(raw.get("ended_ms")),
            action=_text(raw.get("action")),
            summary=_text(raw.get("summary")),
            changes=_int(raw.get("changes")) or 0,
            undone=_int(raw.get("undone")) or 0,
            records=tuple(
                record for record in raw.get("records") or () if isinstance(record, Mapping)
            ),
            undoable=bool(raw.get("undoable")),
            undo_blocked=_text(raw.get("undo_blocked")),
            recorded=bool(raw.get("recorded")),
            remembered=bool(raw.get("remembered")),
            origin=raw.get("origin") if isinstance(raw.get("origin"), Mapping) else None,
            retries=_text(raw.get("retries")),
            spend=raw.get("spend") if isinstance(raw.get("spend"), Mapping) else None,
            raw=raw,
        )


@dataclass
class InvocationList:
    """A page of a workspace's invocations, newest first, with the totals:
    iterate it (`for` on `Tilo`, `async for` on `AsyncTilo`) to read on to the
    oldest."""

    invocations: Sequence[InvocationInfo] = ()
    total: int = 0
    """Invocations the workspace holds, of which `invocations` is a page."""
    waiting: int = 0
    """Of those, the ones waiting for the person."""
    pending: bool = False
    """A record could not be read yet: invocations that changed only it are
    missing."""
    next_cursor: str | None = None
    text: str = ""
    """The page as a person reads it, one line per invocation."""
    _client: Any = None
    _more: Callable[[str], Awaitable[InvocationList]] | None = None

    @classmethod
    def of(
        cls,
        raw: Mapping[str, Any],
        client: Any = None,
        more: Callable[[str], Awaitable[InvocationList]] | None = None,
    ) -> InvocationList:
        return cls(
            invocations=[
                InvocationInfo.of(listed)
                for listed in raw.get("items") or []
                if isinstance(listed, Mapping)
            ],
            total=_int(raw.get("total")) or 0,
            waiting=_int(raw.get("waiting")) or 0,
            pending=bool(raw.get("pending")),
            next_cursor=_text(raw.get("next_cursor")),
            text=_text(raw.get("text")) or "",
            _client=client,
            _more=more,
        )

    def more(self) -> Any:
        """The next page, older, or `None` on the last."""
        if self._more is None or self.next_cursor is None:
            return None
        return self._client.handoff(self._more(self.next_cursor))

    async def __aiter__(self) -> AsyncIterator[InvocationInfo]:
        page: InvocationList | None = self
        while page is not None:
            for listed in page.invocations:
                yield listed
            page = await page.more() if page.next_cursor else None

    def __iter__(self) -> Iterator[InvocationInfo]:
        if not getattr(self._client, "sync", False):
            raise ProtocolError(
                "the async client walks a listing of invocations with `async for`"
            )
        page: InvocationList | None = self
        while page is not None:
            yield from page.invocations
            page = page.more()


@dataclass(frozen=True)
class Hit:
    """One search hit: a path the other calls take, and what it offers."""

    path: str
    kind: str = ""
    label: str = ""
    action: str | None = None

    @classmethod
    def of(cls, raw: Mapping[str, Any]) -> Hit:
        return cls(
            path=str(raw.get("path") or ""),
            kind=str(raw.get("kind") or ""),
            label=str(raw.get("label") or ""),
            action=_text(raw.get("action")),
        )


@dataclass(frozen=True)
class Person:
    """Someone in the workspace: a person, or one of its agents."""

    who: str
    name: str = ""
    kind: str = ""

    @classmethod
    def of(cls, raw: Mapping[str, Any]) -> Person:
        return cls(
            who=str(raw.get("who") or ""),
            name=str(raw.get("name") or ""),
            kind=str(raw.get("kind") or ""),
        )


@dataclass(frozen=True)
class Event:
    """One thing the workspace's stream told: a change, a run, presence.

    `kind` is the stream's event name; `raw` its data as the API gives it. A
    change names `title` (the run it was a step of), a run names `run` (its
    invocation) and `title`, and both name `group` (the run a client opened).
    """

    kind: str
    raw: Mapping[str, Any] = field(default_factory=dict)

    @classmethod
    def of(cls, kind: str, data: Mapping[str, Any]) -> Event:
        return cls(kind=kind, raw=dict(data))

    @property
    def path(self) -> str | None:
        """The resource or action the event is about, when it names one."""
        return _text(self.raw.get("path"))

    @property
    def change(self) -> str | None:
        """The change's own id in the app's history."""
        return _text(self.raw.get("change"))

    @property
    def what(self) -> EventWhat | None:
        """What the change did to its resource; None for an event that is no change."""
        value = _text(self.raw.get("what"))
        if value is None:
            return None
        try:
            return EventWhat(value)
        except ValueError:
            return None

    @property
    def sentence(self) -> str | None:
        """What changed, in the app's own words, as its history shows it."""
        return _text(self.raw.get("sentence"))

    @property
    def by(self) -> str | None:
        """Who made the change: a person's name, or a group's title when one did."""
        return _text(self.raw.get("by"))

    @property
    def resources(self) -> list[Mapping[str, Any]]:
        """The records a change wrote, each with the `path` it is read at and,
        where the change names them, its `app`, `record` and `item`."""
        found = self.raw.get("resources")
        if not isinstance(found, list):
            return []
        return [dict(resource) for resource in found if isinstance(resource, Mapping)]

    @property
    def actor(self) -> Mapping[str, Any] | None:
        """Who made a change, structured: its `kind` (`person` or `agent`) and
        `name`, and the actor's own fields besides."""
        actor = self.raw.get("actor")
        return dict(actor) if isinstance(actor, Mapping) else None

    @property
    def title(self) -> str | None:
        """The group's title: the words the client that made it opened it with."""
        return _text(self.raw.get("title"))

    @property
    def invocation(self) -> str | None:
        """The invocation an invocation event is about."""
        return _text(self.raw.get("invocation"))

    @property
    def group(self) -> str | None:
        """The group a client opened: every call carrying its id is one step."""
        return _text(self.raw.get("group"))

    @property
    def phase(self) -> str | None:
        """How it stands: `started`, `succeeded`, `failed` or `cancelled`."""
        return _text(self.raw.get("phase"))

    @property
    def undone(self) -> bool:
        """Whether the change has been taken back."""
        return bool(self.raw.get("undone"))

    @property
    def time_ms(self) -> int | None:
        """When it happened (Unix milliseconds)."""
        return _int(self.raw.get("time_ms"))

    @property
    def here(self) -> Sequence[Person]:
        """Who is in the workspace, as presence last told it."""
        return [
            Person.of(person)
            for person in self.raw.get("here") or []
            if isinstance(person, Mapping)
        ]


@dataclass(frozen=True)
class ProgramAnswer:
    """What a tilolang program answered: its output and the steps it took."""

    status: str
    output: Sequence[str] = ()
    changed: int = 0
    steps: Sequence[Mapping[str, Any]] = ()
    # What a person reads, each resource by its reference (`n3`).
    text: str = ""
    # The program session it ran in: pass it to the next program to continue it.
    session: str = ""
    raw: Mapping[str, Any] = field(default_factory=dict)

    @classmethod
    def of(cls, raw: Mapping[str, Any]) -> ProgramAnswer:
        return cls(
            status=str(raw.get("status") or ""),
            output=tuple(str(line) for line in raw.get("output") or ()),
            changed=_int(raw.get("changed")) or 0,
            steps=tuple(raw.get("steps") or ()),
            text=str(raw.get("text") or ""),
            session=str(raw.get("session") or ""),
            raw=raw,
        )

    def __iter__(self) -> Iterator[str]:
        return iter(self.output)

    def __str__(self) -> str:
        return "\n".join(self.output)
