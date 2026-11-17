"""Types for Activity (`so.tilo.activity`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Filter` is the app's `activity.filter` at `activity/<resource>`, read with
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
    "Filter",
    "ClearFiltersArgs",
]

ClearFiltersArgs = TypedDict("ClearFiltersArgs", {
})
"""The arguments of `clear-filters`."""

class Filter(Resource):
    """Workspace activity — `activity.filter`."""
    __surface__ = "so.tilo.activity@f7fe2955835947ba"
    period: str
    author: str
    lens: str
    query: str

    def clear_filters(self) -> None:
        """Runs the app's action."""
        return self.action("clear-filters")

    def clear_filters_many(self, items: list[ClearFiltersArgs]) -> list[dict[str, Any]]:
        """Runs `clear-filters` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear-filters", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"clear_filters", "clear_filters_many"})


