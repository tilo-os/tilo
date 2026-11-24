"""Types for Counter (`so.tilo.lang.counter`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Counter` is the app's `tl.so.tilo.lang.counter.Counter` at `counter/<resource>`, read with
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
    "Counter",
    "LangCounter",
    "IncrementArgs",
    "DecrementArgs",
]

IncrementArgs = TypedDict("IncrementArgs", {
})
"""The arguments of `increment`."""

DecrementArgs = TypedDict("DecrementArgs", {
})
"""The arguments of `decrement`."""

class Counter(Resource):
    """Counter — `tl.so.tilo.lang.counter.Counter`."""
    __surface__ = "so.tilo.lang.counter@776de687da4ff017"
    @property
    def _3wkdueca(self) -> int:
        """value (`3wkdueca` on the wire)."""
        return self.fields["3wkdueca"]

    @_3wkdueca.setter
    def _3wkdueca(self, value: int) -> None:
        self.edit("3wkdueca", value=value)

    def increment(self) -> None:
        """Add one to the counter."""
        return self.action("increment")

    def increment_many(self, items: list[IncrementArgs]) -> list[dict[str, Any]]:
        """Runs `increment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("increment", items)

    def decrement(self) -> None:
        """Take one away from the counter."""
        return self.action("decrement")

    def decrement_many(self, items: list[DecrementArgs]) -> list[dict[str, Any]]:
        """Runs `decrement` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("decrement", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"increment", "increment_many", "decrement", "decrement_many"})


class LangCounter(Resource):
    """Counter — `tl.so.tilo.lang.counter`."""
    __surface__ = "so.tilo.lang.counter@776de687da4ff017"

