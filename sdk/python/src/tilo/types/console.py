"""Types for Console (`so.tilo.console`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`View` is the app's `console.view` at `console/<resource>`, read with
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
    "View",
    "ShowEverythingArgs",
]

ShowEverythingArgs = TypedDict("ShowEverythingArgs", {
})
"""The arguments of `show-everything`."""

class View(Resource):
    """Agent sessions shown — `console.view`."""
    __surface__ = "so.tilo.console@2d09c6bc76c75d71"
    agent: str
    # the app's `workspace` field is the SDK's own `workspace`: read it with
    # `fields["workspace"]`.

    def show_everything(self) -> None:
        """Runs the app's action."""
        return self.action("show-everything")

    def show_everything_many(self, items: list[ShowEverythingArgs]) -> list[dict[str, Any]]:
        """Runs `show-everything` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show-everything", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"show_everything", "show_everything_many"})


