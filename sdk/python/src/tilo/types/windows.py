"""Types for Windows Apps (`so.tilo.windows`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Manager` is the app's `windows.manager` at `windows/<resource>`, read with
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
    "Route",
    "Manager",
]

Route = Literal["wine", "proton", "vm"]

class Manager(Resource):
    """Windows manager — `windows.manager`."""
    __surface__ = "so.tilo.windows@5297e22e56f8e320"
    routes: dict[str, Route]

    def open_file(self,
        *,
        app: str,
        path: Any | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["app"] = app
        if path is not None:
            args["path"] = path
        return self.action("open-file", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"open_file"})


