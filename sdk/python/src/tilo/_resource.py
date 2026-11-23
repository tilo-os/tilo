"""A resource as a typed object: its fields, its actions, and its edits.

Attribute reads come from the resource's read; a method call is the
resource's action (`read_form_fields()` runs `read-form-fields`); assigning a
name is a field edit, through the generic `field-edit` action every resource
publishes. `form.form_values["surname"] = "Doe"` is the same edit one level
down.

A generated class names the app surface it was written for (`__surface__`);
an action or edit run through one says so, and a workspace whose app
publishes another surface refuses it as `SurfaceChanged`.
"""

from __future__ import annotations

import asyncio
import functools
import json
from collections.abc import AsyncIterator, Iterable, Iterator, Mapping, MutableMapping, Sequence
from typing import Any, Generic, TypeVar, TypedDict

from ._endpoint import SURFACE_HEADER, segments as resource_path
from ._errors import ProtocolError
from ._models import Event

# Attributes that are the SDK's own, never an action of the resource.
OWN = frozenset(
    {
        "workspace",
        "path",
        "kind",
        "fields",
        "children",
        "cursor",
        "raw",
        "load",
        "refresh",
        "flush",
        "do",
        "edit",
        "remove",
        "watch",
        "action",
        "has",
        "many",
    }
)

T = TypeVar("T")


class Entry(TypedDict, Generic[T]):
    """One item of a collection field, as its read lists it: the id an action
    takes it by, and its value. `issues[0]["id"]` names the issue `rename`
    takes, and `issues[0]["value"]["title"]` is its title."""

    id: str
    value: T


# A bulk call's bounds: at most this many members, and about this much JSON, per call.
MANY_MEMBERS = 500
MANY_BYTES = 4 * 1024 * 1024


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
        if previous and (previous.islower() or previous.isdigit() or previous.isupper() and following.islower()):
            out.append("-")
        out.append(character.lower())
    return "".join(out)


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
    """A field's value as this proxy hands it out: maps stay editable."""
    if isinstance(value, Mapping):
        return FieldMap(resource, steps, value)
    if isinstance(value, list):
        return [wrap(resource, steps + (index,), item) for index, item in enumerate(value)]
    return value


class Resource:
    """One resource: a read snapshot, its actions and its field edits.

    `await ws.resource(path).load()` reads it, then `res.title` is the title
    and `res.title = "…"` edits it. With the sync client both work without
    `await`, and an assignment has landed by the time it returns.
    """

    def __init__(self, workspace: Any, path: str) -> None:
        object.__setattr__(self, "_workspace", workspace)
        object.__setattr__(self, "path", path)
        object.__setattr__(self, "_answer", None)
        object.__setattr__(self, "_edits", [])
        object.__setattr__(self, "_children", ())

    # -- the snapshot ----------------------------------------------------

    @property
    def kind(self) -> str:
        """The resource's kind, as its app names it."""
        return str((self._answer or {}).get("kind") or "")

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
        return {
            key: wrap(self, (key,), value) for key, value in fields.items()
        }

    @property
    def children(self) -> Sequence[Mapping[str, Any]]:
        """The child resources the last read listed."""
        return self._children

    @property
    def cursor(self) -> str | None:
        """The cursor the last read stopped at, when it stopped before the end."""
        cursor = (self._answer or {}).get("cursor")
        return cursor if isinstance(cursor, str) else None

    def load(self) -> Any:
        """Read the resource; `await` it with the async client."""
        return self._workspace.client.handoff(self._load())

    async def _load(self) -> Resource:
        await self._flush()
        answer = await self._workspace._read(self.path)
        object.__setattr__(self, "_answer", answer)
        children = answer.get("children")
        object.__setattr__(
            self, "_children", tuple(children) if isinstance(children, list) else ()
        )
        return self

    def refresh(self) -> Any:
        """Read it again, keeping no field of the earlier read."""
        return self.load()

    def _surface(self) -> str | None:
        """The `<app>@<digest>` the generated class was written for, if it says."""
        sent = getattr(type(self), "__surface__", None)
        return sent if isinstance(sent, str) else None

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

    def action(self, name: str, **args: Any) -> Any:
        """Run one of the resource's actions by name.

        It answers what the action produced, waiting for a run still under
        way rather than handing one back to follow.
        """
        return self._workspace.client.handoff(self._act(name, args))

    async def _act(self, name: str, args: Mapping[str, Any]) -> Any:
        from ._invocation import Invocation

        answer = await self._workspace._outcome(
            f"{self.path}#{kebab(name)}", args, surface=self._surface()
        )
        if isinstance(answer, Invocation):
            return await answer._result(None)
        return answer

    def do(self, name: str, **args: Any) -> Any:
        """`action`, by its name in the app's own words."""
        return self.action(name, **args)

    def many(self, action: str, items: Sequence[Mapping[str, Any]]) -> Any:
        """Run `action` for each of `items`, a few calls at most.

        Each call applies up to 500 items (and about 4 MiB of JSON) as one
        all-or-nothing step, so if any one of them refuses none of that call
        applies. Every call is the same run: History lists it once and one undo
        takes it back. An empty list sends nothing. Answers the list of each
        call's answer.

        An item names one an earlier item of the same call makes with
        `made(place)`, e.g. `{"title": "Child", "parent": made(0)}` for the
        second item of a parent-then-child list: the server fills in the id the
        first item got as the call applies, so nothing has to be read between
        two calls, and both are one change.
        """
        return self._workspace.client.handoff(self._many(action, items))

    async def _many(
        self, action: str, items: Sequence[Mapping[str, Any]]
    ) -> list[dict[str, Any]]:
        members = _members(action, items)
        if not members:
            return []
        endpoint = await self._workspace._endpoint()
        if endpoint.group() is not None:
            return await self._grouped(endpoint, members)
        # The chunks are one invocation group, as `async with ws.invocation(...)`
        # would make them one.
        from ._workspace import InvocationOpening

        async with InvocationOpening(self._workspace, f"{action} ({len(members)})"):
            return await self._grouped(endpoint, members)

    async def _grouped(self, endpoint: Any, members: list[dict[str, Any]]) -> list[dict[str, Any]]:
        surface = self._surface()
        headers = {SURFACE_HEADER: surface} if surface is not None else None
        answers: list[dict[str, Any]] = []
        for chunk in _chunks(members):
            answer = await endpoint.json(
                "POST",
                endpoint.path("resources", resource_path(self.path), "actions", "group"),
                json={"actions": chunk},
                headers=headers,
            )
            # Names the run on the workspace's record; a person's ask is raised here.
            endpoint.follow(answer, f"{self.path}#group")
            if answer.get("status") == "pending":
                invocation = answer.get("invocation")
                if isinstance(invocation, str) and invocation:
                    answer = dict(await endpoint.wait(invocation))
            answers.append(dict(answer))
        return answers

    def _edit_call(self, op: str, steps: tuple[Any, ...], value: Any) -> tuple[str, dict]:
        """The action and arguments an edit is: the field's own setter where each field is
        written by its setter (`__setters__`), taking its whole value or an edit below it; the
        field edit elsewhere."""
        if getattr(type(self), "__setters__", False) and steps and isinstance(steps[0], str):
            setter = f"{self.path}#set-{steps[0].replace('_', '-')}"
            if len(steps) == 1 and op == "set":
                return setter, {"value": value}
            return setter, {"edit": {"path": segments(steps)[1:], "op": op, "value": value}}
        return f"{self.path}#field-edit", {
            "edit": {"path": segments(steps), "op": op, "value": value}
        }

    def edit(self, *steps: Any, op: str = "set", value: Any = None) -> Any:
        """A field edit: the steps name the field, its map keys and its items."""
        action, arguments = self._edit_call(op, steps, value)
        return self._workspace.client.handoff(
            self._workspace._outcome(action, arguments, surface=self._surface())
        )

    def remove(self, *steps: Any) -> Any:
        """Take a map entry away: `remove("form_values", "surname")`."""
        return self.edit(*steps, op="remove-key")

    def _send_edit(self, op: str, steps: tuple[Any, ...], value: Any) -> None:
        action, arguments = self._edit_call(op, steps, value)
        coroutine = self._workspace._outcome(action, arguments, surface=self._surface())
        client = self._workspace.client
        if client.sync:
            client.handoff(coroutine)
            return
        self._edits.append(asyncio.ensure_future(coroutine))

    def flush(self) -> Any:
        """Await the edits that are on their way, raising the first refusal."""
        if self._workspace.client.sync:
            return None
        return self._flush()

    async def _flush(self) -> None:
        while self._edits:
            pending, self._edits = self._edits, []
            answers = await asyncio.gather(*pending, return_exceptions=True)
            for answer in answers:
                if isinstance(answer, BaseException):
                    raise answer

    # -- watching --------------------------------------------------------

    def watch(self) -> AsyncIterator[Event]:
        """Each change to this resource, as it happens."""
        return self._workspace.watch(self.path)

    def __repr__(self) -> str:
        return f"Resource({self.path!r})"



def _members(
    action: str, items: Iterable[Mapping[str, Any]]
) -> list[dict[str, Any]]:
    """`items` as the group action's members: each an action's arguments."""
    members: list[dict[str, Any]] = []
    for item in items:
        if not isinstance(item, Mapping):
            raise ProtocolError(
                f"a bulk call runs an action on each object of arguments, not on {item!r}"
            )
        members.append({"action": action, "args": dict(item)})
    return members


def made(member: int, item: int = 0) -> dict[str, dict[str, int]]:
    """A reference to an item an earlier item of the same `many` call makes.

    `member` is the item's place in the call, counted from 0, and `item` which
    of the ids it made (counted from 0, in the order it made them): usually 0,
    as a create makes one. The call fills the reference with that id as it
    applies, and the change keeps the reference, so every device resolves it
    the same.
    """
    return {"$made": {"member": member, "item": item}}


def _chunks(members: list[dict[str, Any]]) -> Iterator[list[dict[str, Any]]]:
    """`members` in the calls a bulk call sends: at most `MANY_MEMBERS` and `MANY_BYTES`."""
    chunk: list[dict[str, Any]] = []
    size = 0
    for member in members:
        try:
            weight = len(json.dumps(member, separators=(",", ":")).encode())
        except (TypeError, ValueError) as error:
            raise ProtocolError(f"an action's arguments are not JSON: {error}") from None
        if chunk and (len(chunk) >= MANY_MEMBERS or size + weight > MANY_BYTES):
            yield chunk
            chunk, size = [], 0
        chunk.append(member)
        size += weight + 1
    if chunk:
        yield chunk


def segments(steps: Sequence[Any]) -> list[dict[str, Any]]:
    """A field path as the field-edit action takes it.

    The first step names a field of the resource; later steps are map keys,
    and a mapping (`{"item": "c01e"}`) is passed through as an item or cell.
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
            raise ProtocolError(f"a field path step is a name or a mapping, not {step!r}")
    if not built:
        raise ProtocolError("a field edit names a field")
    return built
