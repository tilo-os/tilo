"""A resource as a typed object: its fields, its actions, and its edits.

Attribute reads come from `tilo_read`'s `fields`; a method call is the
resource's action (`read_form_fields()` runs `read-form-fields`); assigning a
name is a field edit, through the generic `field-edit` action every resource
publishes. `form.form_values["surname"] = "Doe"` is the same edit one level
down.
"""

from __future__ import annotations

import asyncio
import functools
from typing import Any, AsyncIterator, Iterator, Mapping, MutableMapping, Sequence

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
    }
)


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
        """The whole answer the last read got, as the door sent it."""
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
        answer = await self._workspace._call("tilo_read", {"path": self.path})
        object.__setattr__(self, "_answer", answer)
        children = answer.get("children")
        object.__setattr__(
            self, "_children", tuple(children) if isinstance(children, list) else ()
        )
        return self

    def refresh(self) -> Any:
        """Read it again, keeping no field of the earlier read."""
        return self.load()

    # -- fields ----------------------------------------------------------

    def has(self, name: str) -> bool:
        """Whether the read snapshot holds a field `name`."""
        fields = (self._answer or {}).get("fields")
        return isinstance(fields, Mapping) and name in fields

    def __getattr__(self, name: str) -> Any:
        if name.startswith("_"):
            raise AttributeError(name)
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
        """Run one of the resource's actions by name."""
        return self._workspace.client.handoff(
            self._workspace._outcome(f"{self.path}#{kebab(name)}", args)
        )

    def do(self, name: str, **args: Any) -> Any:
        """`action`, by its name in the app's own words."""
        return self.action(name, **args)

    def edit(self, *steps: Any, op: str = "set", value: Any = None) -> Any:
        """A field edit: the steps name the field, its map keys and its items."""
        return self._workspace.client.handoff(
            self._workspace._outcome(
                f"{self.path}#field-edit",
                {"edit": {"path": segments(steps), "op": op, "value": value}},
            )
        )

    def remove(self, *steps: Any) -> Any:
        """Take a map entry away: `remove("form_values", "surname")`."""
        return self.edit(*steps, op="remove-key")

    def _send_edit(self, op: str, steps: tuple[Any, ...], value: Any) -> None:
        coroutine = self._workspace._outcome(
            f"{self.path}#field-edit",
            {"edit": {"path": segments(steps), "op": op, "value": value}},
        )
        transport = self._workspace.client.transport
        if transport.sync:
            transport.handoff(coroutine)
            return
        self._edits.append(asyncio.ensure_future(coroutine))

    def flush(self) -> Any:
        """Await the edits that are on their way, raising the first refusal."""
        if self._workspace.client.transport.sync:
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

    def watch(self, wait_ms: int | None = None) -> AsyncIterator[Event]:
        """Each change to this resource, as it happens."""
        return self._workspace.watch(self.path, wait_ms=wait_ms)

    def __repr__(self) -> str:
        return f"Resource({self.path!r})"


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
