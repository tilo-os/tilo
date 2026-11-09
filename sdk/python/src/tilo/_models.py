"""The values the SDK answers: workspaces, keys, listings, hits and events."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, AsyncIterator, Awaitable, Callable, Iterator, Mapping, Sequence


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
    """One of the person's workspaces, as the account lists them."""

    id: str
    name: str
    host: str

    @classmethod
    def of(cls, raw: Mapping[str, Any]) -> WorkspaceInfo:
        return cls(
            id=str(raw.get("id") or ""),
            name=str(raw.get("name") or ""),
            host=str(raw.get("host") or ""),
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
    """A key as the account lists it: a grant, its scope and its expiry."""

    grant: str
    name: str = ""
    scope: str = ""
    kind: str = ""
    created_at_ms: int | None = None
    expires_at_ms: int | None = None
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
            token=str(raw.get("token") or raw.get("key") or ""),
        )

    def client(self, door: str | None = None) -> Any:
        """A client of this key, for code that hands one to an agent."""
        from ._client import AsyncTilo

        return AsyncTilo(key=self.token, door=door)


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
        return _text(self.raw.get("path"))

    @property
    def change(self) -> str | None:
        return _text(self.raw.get("change"))

    @property
    def sentence(self) -> str | None:
        return _text(self.raw.get("sentence"))

    @property
    def by(self) -> str | None:
        return _text(self.raw.get("by"))

    @property
    def run(self) -> str | None:
        return _text(self.raw.get("run"))

    @property
    def run_id(self) -> str | None:
        return _text(self.raw.get("run_id"))

    @property
    def phase(self) -> str | None:
        return _text(self.raw.get("phase"))

    @property
    def invocation_id(self) -> str | None:
        return _text(self.raw.get("invocation_id"))

    @property
    def undone(self) -> bool:
        return bool(self.raw.get("undone"))

    @property
    def time_ms(self) -> int | None:
        return _int(self.raw.get("time_ms"))

    @property
    def here(self) -> Sequence[Person]:
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
