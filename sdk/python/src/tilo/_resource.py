"""A resource as a typed object: its fields, its actions, and its edits.

A field reads from the resource's `fields`; a method call is the resource's action
(`read_form_fields()` runs `read-form-fields`); assigning a name is a field edit, through the
generic `field-edit` action every resource publishes. `form.form_values["surname"] = "Doe"` is
the same edit one level down.

A resource made through generated types names its app's surface in every call (`Tilo-Surface`):
a workspace whose app publishes another surface refuses the call (`surface_changed`) instead of
running it.
"""

from __future__ import annotations

import asyncio
import functools
import json
from collections.abc import AsyncIterator, Iterable, Iterator, Mapping, MutableMapping, Sequence
from typing import Any, Generic, TypedDict, TypeVar

from ._errors import Refused

# Attributes that are the SDK's own, never an action of the resource.
OWN = frozenset(
    {
        "workspace",
        "path",
        "kind",
        "surface",
        "fields",
        "children",
        "raw",
        "load",
        "refresh",
        "flush",
        "action",
        "edit",
        "remove",
        "watch",
        "has",
        "many",
    }
)

T = TypeVar("T")

# A bulk call's bounds: at most this many members, and about this much JSON, per call.
MANY_MEMBERS = 500
MANY_BYTES = 4 * 1024 * 1024


class Entry(TypedDict, Generic[T]):
    """One item of a collection field, as its read lists it: the id an action
    takes it by, and its value. `issues[0]["id"]` names the issue `rename`
    takes, and `issues[0]["value"]["title"]` is its title."""

    id: str
    value: T


def kebab(name: str) -> str:
    """`read_form_fields` or `readFormFields` as an action id: `read-form-fields`."""
    out: list[str] = []
    for index, character in enumerate(name):
        if character == "_":
            out.append("-")
            continue
        if not character.isupper():
            out.append(character)
            continue
        previous = name[index - 1] if index else ""
        following = name[index + 1] if index + 1 < len(name) else ""
        if previous and (
            previous.islower() or previous.isdigit() or previous.isupper() and following.islower()
        ):
            out.append("-")
        out.append(character.lower())
    return "".join(out)


def segments(steps: Sequence[Any]) -> list[dict[str, Any]]:
    """A field path as the field-edit action takes it.

    The first step names a field of the resource; later steps are map keys, and a mapping
    (`{"item": "c01e"}`) is passed through as an item or cell.
    """
    built: list[dict[str, Any]] = []
    for index, step in enumerate(steps):
        if isinstance(step, Mapping):
            built.append(dict(step))
        elif isinstance(step, str):
            built.append({"field" if index == 0 else "key": step})
        elif isinstance(step, int):
            built.append({"key": str(step)})
        else:
            raise TypeError(f"a field path step is a name or a mapping, not {step!r}")
    if not built:
        raise TypeError("a field edit names a field")
    return built


def made(member: int, item: int = 0) -> dict[str, dict[str, int]]:
    """A reference to an item an earlier item of the same `many` call makes.

    `member` is the item's place in the call, counted from 0, and `item` which of the ids it made
    (counted from 0, in the order it made them): usually 0, as a create makes one. The call fills
    the reference with that id as it applies, and the change keeps the reference, so every device
    resolves it the same.
    """
    return {"$made": {"member": member, "item": item}}


class FieldMap(MutableMapping[str, Any]):
    """A map field: reading gives its values, assigning sends a field edit."""

    def __init__(self, resource: Resource, steps: tuple[Any, ...], value: Mapping[str, Any]) -> None:
        self._resource = resource
        self._steps = steps
        self._value = dict(value)

    def _wrap(self, key: str, value: Any) -> Any:
        return wrap(self._resource, self._steps + (key,), value)

    def __getitem__(self, key: str) -> Any:
        return self._wrap(key, self._value[key])

    def __setitem__(self, key: str, value: Any) -> None:
        self._value[key] = value
        self._resource._send_edit("set", self._steps + (key,), value)

    def __delitem__(self, key: str) -> None:
        del self._value[key]
        self._resource._send_edit("remove-key", self._steps + (key,), None)

    def __iter__(self) -> Iterator[str]:
        return iter(self._value)

    def __len__(self) -> int:
        return len(self._value)

    def __repr__(self) -> str:
        return f"FieldMap({dict(self._value)!r})"


def wrap(resource: Resource, steps: tuple[Any, ...], value: Any) -> Any:
    """A field's value as this object hands it out: maps stay editable."""
    if isinstance(value, Mapping):
        return FieldMap(resource, steps, value)
    if isinstance(value, list):
        return [wrap(resource, steps + (index,), item) for index, item in enumerate(value)]
    return value


class Resource:
    """One resource: a read snapshot, its actions and its field edits.

    `await ws.resource(path).load()` reads it, then `res.title` is the title and
    `res.title = "…"` edits it; `await res.flush()` waits for the edits on their way.
    """

    def __init__(self, workspace: Any, path: str, surface: str | None = None) -> None:
        object.__setattr__(self, "_workspace", workspace)
        object.__setattr__(self, "path", path)
        object.__setattr__(self, "_surface", surface)
        object.__setattr__(self, "_answer", None)
        object.__setattr__(self, "_edits", [])

    # -- the snapshot ----------------------------------------------------

    @property
    def kind(self) -> str:
        """The resource's kind, as its app names it."""
        return str((self._answer or {}).get("kind") or "")

    @property
    def surface(self) -> str | None:
        """The app's surface the types were made from, `<app>@<digest>`, when they were."""
        sent = getattr(type(self), "__surface__", None)
        return sent if isinstance(sent, str) else self._surface

    @property
    def raw(self) -> Mapping[str, Any]:
        """The whole answer the last read got, as the API sent it."""
        return self._answer or {}

    @property
    def fields(self) -> Mapping[str, Any]:
        """The read fields, each wrapped so assigning to one edits it."""
        fields = (self._answer or {}).get("fields")
        if not isinstance(fields, Mapping):
            return {}
        return {key: wrap(self, (key,), value) for key, value in fields.items()}

    @property
    def children(self) -> Sequence[Mapping[str, Any]]:
        """The child resources the last read listed."""
        children = (self._answer or {}).get("children")
        return tuple(children) if isinstance(children, list) else ()

    async def load(self) -> Resource:
        """Read the resource, after the edits on their way."""
        await self.flush()
        answer = await self._workspace.read_resource(self.path, headers=self._headers())
        object.__setattr__(self, "_answer", answer)
        return self

    async def refresh(self) -> Resource:
        """Read it again, keeping no field of the earlier read."""
        return await self.load()

    def _headers(self) -> dict[str, str]:
        surface = self.surface
        return {} if surface is None else {"tilo-surface": surface}

    # -- fields ----------------------------------------------------------

    def has(self, name: str) -> bool:
        """Whether the read snapshot holds a field `name`."""
        fields = (self._answer or {}).get("fields")
        return isinstance(fields, Mapping) and name in fields

    def __getattr__(self, name: str) -> Any:
        if name.startswith("_"):
            raise AttributeError(name)
        # An action needs no read: the types that name it are the surface.
        if name in getattr(type(self), "__actions__", ()):
            return functools.partial(self.action, name)
        answer = object.__getattribute__(self, "_answer")
        if answer is None:
            raise AttributeError(
                f"{self.path!r} is not read yet: `await ws.resource(path).load()` first"
            )
        fields = answer.get("fields")
        if isinstance(fields, Mapping) and name in fields:
            return wrap(self, (name,), fields[name])
        return functools.partial(self.action, name)

    def __setattr__(self, name: str, value: Any) -> None:
        if name.startswith("_") or name in OWN:
            object.__setattr__(self, name, value)
            return
        self._send_edit("set", (name,), value)

    def __dir__(self) -> list[str]:
        fields = (self._answer or {}).get("fields")
        names = set(OWN)
        if isinstance(fields, Mapping):
            names |= set(fields)
        return sorted(names)

    # -- actions and edits ----------------------------------------------

    async def action(self, name: str, **args: Any) -> Any:
        """Run one of the resource's actions by name: what it produced, once it finished."""
        from ._client import run

        answer = await run(
            self._workspace,
            {"path": self.path, "action": kebab(name)},
            args,
            self._headers(),
        )
        return answer.get("result") if isinstance(answer, Mapping) else None

    async def many(self, action: str, items: Sequence[Mapping[str, Any]]) -> list[dict[str, Any]]:
        """Run `action` for each of `items`, a few calls at most.

        Each call applies up to 500 items (and about 4 MiB of JSON) as one all-or-nothing step,
        so if any one of them refuses none of that call applies. Every call is the same
        invocation group: History lists it once and one undo takes it back. An empty list sends
        nothing. Answers the list of each call's answer.

        An item names one an earlier item of the same call makes with `made(place)`, e.g.
        `{"title": "Child", "parent": made(0)}` for the second item of a parent-then-child list:
        the server fills in the id the first item got as the call applies.
        """
        from ._client import current_group, run

        members = _members(action, items)
        if not members:
            return []

        async def send() -> list[dict[str, Any]]:
            answers: list[dict[str, Any]] = []
            for chunk in _chunks(members):
                answer = await run(
                    self._workspace,
                    {"path": self.path, "action": "group"},
                    {"actions": chunk},
                    self._headers(),
                )
                answers.append(dict(answer))
            return answers

        group = current_group()
        if group is not None and group.workspace is self._workspace:
            return await send()
        async with self._workspace.invocation(f"{action} ({len(members)})"):
            return await send()

    def _edit_call(self, op: str, steps: tuple[Any, ...], value: Any) -> tuple[str, dict[str, Any]]:
        """The action and arguments an edit is: the field's own setter where each field is
        written by its setter (`__setters__`), taking its whole value or an edit below it; the
        field edit elsewhere."""
        if getattr(type(self), "__setters__", False) and steps and isinstance(steps[0], str):
            setter = f"set-{steps[0].replace('_', '-')}"
            if len(steps) == 1 and op == "set":
                return setter, {"value": value}
            return setter, {"edit": {"path": segments(steps)[1:], "op": op, "value": value}}
        return "field-edit", {"edit": {"path": segments(steps), "op": op, "value": value}}

    async def edit(self, *steps: Any, op: str = "set", value: Any = None) -> Any:
        """A field edit: the steps name the field, its map keys and its items."""
        action, arguments = self._edit_call(op, steps, value)
        return await self.action(action, **arguments)

    async def remove(self, *steps: Any) -> Any:
        """Take a map entry away: `await remove("form_values", "surname")`."""
        return await self.edit(*steps, op="remove-key")

    def _send_edit(self, op: str, steps: tuple[Any, ...], value: Any) -> None:
        action, arguments = self._edit_call(op, steps, value)
        self._edits.append(asyncio.ensure_future(self.action(action, **arguments)))

    async def flush(self) -> None:
        """Await the edits that are on their way, raising the first refusal."""
        while self._edits:
            pending, self._edits = self._edits, []
            answers = await asyncio.gather(*pending, return_exceptions=True)
            for answer in answers:
                if isinstance(answer, BaseException):
                    raise answer

    # -- watching --------------------------------------------------------

    def watch(self) -> AsyncIterator[dict[str, Any]]:
        """Each change to this resource, as it happens."""
        return self._workspace.events(path=self.path)

    def __repr__(self) -> str:
        return f"Resource({self.path!r})"


def _members(action: str, items: Iterable[Mapping[str, Any]]) -> list[dict[str, Any]]:
    """`items` as the group action's members: each an action's arguments."""
    members: list[dict[str, Any]] = []
    for item in items:
        if not isinstance(item, Mapping):
            raise TypeError(f"a bulk call runs an action on each object of arguments, not on {item!r}")
        members.append({"action": action, "args": dict(item)})
    return members


def _chunks(members: list[dict[str, Any]]) -> Iterator[list[dict[str, Any]]]:
    """`members` in the calls a bulk call sends: at most `MANY_MEMBERS` and `MANY_BYTES`."""
    chunk: list[dict[str, Any]] = []
    size = 0
    for member in members:
        try:
            weight = len(json.dumps(member, separators=(",", ":")).encode())
        except (TypeError, ValueError) as error:
            raise TypeError(f"an action's arguments are not JSON: {error}") from None
        if chunk and (len(chunk) >= MANY_MEMBERS or size + weight > MANY_BYTES):
            yield chunk
            chunk, size = [], 0
        chunk.append(member)
        size += weight + 1
    if chunk:
        yield chunk


def resource_proxy(workspace: Any, path: str, surface: Any = None) -> Resource:
    """A resource as a typed object: fields, actions and field edits. `surface` is the app's
    version the types were made from, and every call through it names it."""
    named = surface if isinstance(surface, str) else getattr(surface, "surface", None)
    return Resource(workspace, path, named or None)


async def rows(
    workspace: Any,
    *,
    path: str | None = None,
    kind: str | None = None,
    limit: str | None = None,
) -> AsyncIterator[Mapping[str, Any]]:
    """Every row under `path` (the apps without one), page after page: `listResources` with its
    `next_cursor` followed."""
    cursor: str | None = None
    while True:
        page = await workspace.list_resources(path=path, cursor=cursor, limit=limit, kind=kind)
        for row in page.get("rows") or []:
            yield row
        cursor = page.get("next_cursor")
        if not cursor:
            return


async def exported(workspace: Any) -> dict[str, Any]:
    """Everything in the workspace, read whole: every resource of every app, found by walking the
    listings from its apps, each read with every page of what it lists. Answers
    `{"resources": [...], "unread": [{"path", "error"}]}`. The files the resources name
    (`tilo.content:` references) download with `ws.download`. Take one before deleting a
    customer's workspace."""
    found: dict[str, Any] = {"resources": [], "unread": []}
    seen: set[str] = set()
    walk: list[str | None] = [None]
    while walk:
        path = walk.pop(0)
        try:
            listing_params = {} if path is None else {"path": path}
            cursor: str | None = None
            while True:
                page = await workspace.list_resources(**listing_params, cursor=cursor)
                for row in page.get("rows") or []:
                    at = row.get("path") if isinstance(row, Mapping) else None
                    if not isinstance(at, str) or at in seen:
                        continue
                    seen.add(at)
                    if row.get("reads") is not False:
                        try:
                            found["resources"].append(await _read_whole(workspace, at))
                        except Refused as refused:
                            found["unread"].append({"path": at, "error": str(refused)})
                    if row.get("has_children") is True:
                        walk.append(at)
                following = page.get("next_cursor")
                if not isinstance(following, str) or following == cursor:
                    break
                cursor = following
        except Refused as refused:
            found["unread"].append({"path": path or "", "error": str(refused)})
    return found


async def _read_whole(workspace: Any, path: str) -> Mapping[str, Any]:
    """`path` read with every page of what it has."""
    page = await workspace.read_resource(path)
    whole = dict(page)
    whole["children"] = list(page.get("children") or [])
    while True:
        cursor = page.get("cursor")
        if not isinstance(cursor, str) or cursor == "":
            break
        page = await workspace.read_resource(path, cursor=cursor)
        whole["children"].extend(page.get("children") or [])
        if page.get("cursor") == cursor:
            break
    whole.pop("cursor", None)
    return whole


__all__ = [
    "Entry",
    "FieldMap",
    "Resource",
    "exported",
    "kebab",
    "made",
    "resource_proxy",
    "rows",
    "segments",
]
