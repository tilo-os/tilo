"""Types for Containers (`so.tilo.containers`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Engine` is the app's `containers.engine` at `containers/<resource>`, read with
`await ….load()`: its fields are the app's fields (read-only ones without a setter)
and its methods are its actions. A method answers what the app declares; an action
that takes longer answers an `Invocation`. A record is the dict the wire carries, and an
enum whose cases carry fields is an object whose tag the app's schema names. An
action whose name is one the class already has is run with `action("…")`, which
the file says where it is. An undoable action that answers nothing also has a
`<action>_many`, which applies it to every argument object of a list in a few
all-or-nothing steps, all of them one invocation group.
"""

from __future__ import annotations

from typing import Any, Literal, NotRequired, TypedDict
from tilo import Entry, Invocation, Resource

__all__ = [
    "CreateDraft",
    "Engine",
    "ClearCreateArgs",
]

class CreateDraft(TypedDict):
    name: str
    image: str
    ports: str
    env: str
    volumes: str
    command: str

ClearCreateArgs = TypedDict("ClearCreateArgs", {
})
"""The arguments of `clear-create`."""

class Engine(Resource):
    """Containers — `containers.engine`."""
    __surface__ = "so.tilo.containers@7c82185ce1bd962b"
    draft: CreateDraft

    def clear_create(self) -> None:
        """Runs the app's action."""
        return self.action("clear-create")

    def clear_create_many(self, items: list[ClearCreateArgs]) -> list[dict[str, Any]]:
        """Runs `clear-create` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear-create", items)

    def create(self) -> None:
        """Runs the app's action."""
        return self.action("create")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"clear_create", "clear_create_many", "create"})


