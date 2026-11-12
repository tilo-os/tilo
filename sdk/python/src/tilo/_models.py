"""The values the SDK answers: workspaces, keys, listings, hits and events."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, AsyncIterator, Awaitable, Callable, Iterator, Mapping, Sequence

from ._errors import ProtocolError


def _text(value: Any) -> str | None:
    return value if isinstance(value, str) else None


def _int(value: Any) -> int | None:
    return value if isinstance(value, int) else None


@dataclass(frozen=True)
class Host:
    """A host workspaces can be created on."""

    name: str
    default: bool = False

    @classmethod
    def of(cls, raw: Mapping[str, Any]) -> Host:
        return cls(name=str(raw.get("name") or ""), default=bool(raw.get("default")))


@dataclass(frozen=True)
class WorkspaceInfo:
    """One of the person's workspaces, as the account lists them.

    `role` is the part they take in it: `owner` for one of their own, else the
    role they were invited with (`viewer`, `commenter`, `editor`,
    `moderator`). It is `""` when the door did not say.
    """

    id: str
    name: str
    host: str
    role: str = ""

    @classmethod
    def of(cls, raw: Mapping[str, Any]) -> WorkspaceInfo:
        return cls(
            id=str(raw.get("id") or ""),
            name=str(raw.get("name") or ""),
            host=str(raw.get("host") or ""),
            role=str(raw.get("role") or ""),
        )


@dataclass(frozen=True)
class WorkspaceList:
    """The account's workspaces and the hosts to create them on."""

    workspaces: Sequence[WorkspaceInfo] = ()
    hosts: Sequence[Host] = ()

    @classmethod
    def of(cls, raw: Mapping[str, Any]) -> WorkspaceList:
        return cls(
            workspaces=[
                WorkspaceInfo.of(item)
                for item in raw.get("workspaces") or []
                if isinstance(item, Mapping)
            ],
            hosts=[
                Host.of(item) for item in raw.get("hosts") or [] if isinstance(item, Mapping)
            ],
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
    device of the person approved it."""

    grant: str
    name: str = ""
    scope: str = ""
    kind: str = ""
    created_at_ms: int | None = None
    expires_at_ms: int | None = None
    token: str = ""
    last_used_at_ms: int | None = None
    created_by: str | None = None

    @classmethod
    def of(cls, raw: Mapping[str, Any]) -> Key:
        return cls(
            grant=str(raw.get("grant") or ""),
            name=str(raw.get("name") or ""),
            scope=str(raw.get("scope") or ""),
            kind=str(raw.get("kind") or ""),
            created_at_ms=_int(raw.get("created_at_ms")),
            expires_at_ms=_int(raw.get("expires_at_ms")),
            token=str(raw.get("token") or raw.get("key") or ""),
            last_used_at_ms=_int(raw.get("last_used_at_ms")),
            created_by=(
                str(raw["created_by"]) if raw.get("created_by") is not None else None
            ),
        )

    def client(self, door: str | None = None) -> Any:
        """A client of this key, for code that hands one to an agent."""
        from ._client import AsyncTilo

        return AsyncTilo(key=self.token, door=door)


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
        standing = raw.get("standing")
        standing = standing if isinstance(standing, Mapping) else {}
        usage = standing.get("usage")
        usage = usage if isinstance(usage, Mapping) else {}
        return cls(
            host=str(raw.get("host") or ""),
            plan=_text(standing.get("plan")),
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

    def client(self, door: str | None = None) -> Any:
        """A client of this key, for code that hands one to an agent."""
        from ._client import AsyncTilo

        return AsyncTilo(key=self.token, door=door)


@dataclass(frozen=True)
class Invitation:
    """An invitation into a workspace: the link that brings one person in.

    Following `link` signs them in (with a passkey, or by approving an
    agent) and lands them in the workspace as `role`. The door sends no mail:
    with an `email`, `sent` is false and `next` says to hand the link over.
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
    label: str = ""
    kind: str = ""
    has_children: bool = False
    state: str | None = None
    actions: Sequence[Mapping[str, Any]] = ()
    raw: Mapping[str, Any] = field(default_factory=dict)

    @classmethod
    def of(cls, raw: Mapping[str, Any]) -> Row:
        return cls(
            path=str(raw.get("path") or ""),
            name=str(raw.get("name") or ""),
            label=str(raw.get("label") or ""),
            kind=str(raw.get("kind") or ""),
            has_children=bool(raw.get("has_children")),
            state=_text(raw.get("state")),
            actions=tuple(raw.get("actions") or ()),
            raw=raw,
        )


@dataclass
class Listing:
    """What `tilo_ls` answered: a resource's rows and actions, and a cursor."""

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
            rows=[Row.of(row) for row in raw.get("rows") or [] if isinstance(row, Mapping)],
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
    """One thing `tilo_watch` told: a change, a run starting or finishing, presence."""

    kind: str
    raw: Mapping[str, Any] = field(default_factory=dict)

    @classmethod
    def of(cls, raw: Mapping[str, Any]) -> Event:
        return cls(kind=str(raw.get("kind") or ""), raw=raw)

    @property
    def path(self) -> str | None:
        """The resource or action the event is about, when it names one."""
        return _text(self.raw.get("path"))

    @property
    def change(self) -> str | None:
        """The change's own id in the app's history."""
        return _text(self.raw.get("change"))

    @property
    def sentence(self) -> str | None:
        """What changed, in the app's own words, as its history shows it."""
        return _text(self.raw.get("sentence"))

    @property
    def by(self) -> str | None:
        """Who made the change: a person's name, or the run's title when a run did."""
        return _text(self.raw.get("by"))

    @property
    def run(self) -> str | None:
        """The agent run the change was a step of, by its title, when it was one."""
        return _text(self.raw.get("run"))

    @property
    def run_id(self) -> str | None:
        """The agent run's id: the one its client opened it under."""
        return _text(self.raw.get("run_id"))

    @property
    def phase(self) -> str | None:
        """How a run stands: `started`, `succeeded`, `failed` or `cancelled`."""
        return _text(self.raw.get("phase"))

    @property
    def invocation_id(self) -> str | None:
        """The invocation a run event follows."""
        return _text(self.raw.get("invocation_id"))

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
    """What the `tilo` tool answered: a program's output and the steps it took."""

    status: str
    output: Sequence[str] = ()
    changed: int = 0
    steps: Sequence[Mapping[str, Any]] = ()
    raw: Mapping[str, Any] = field(default_factory=dict)

    @classmethod
    def of(cls, raw: Mapping[str, Any]) -> ProgramAnswer:
        return cls(
            status=str(raw.get("status") or ""),
            output=tuple(str(line) for line in raw.get("output") or ()),
            changed=_int(raw.get("changed")) or 0,
            steps=tuple(raw.get("steps") or ()),
            raw=raw,
        )

    def __iter__(self) -> Iterator[str]:
        return iter(self.output)

    def __str__(self) -> str:
        return "\n".join(self.output)
