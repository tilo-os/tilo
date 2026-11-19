"""Types for Disks (`so.tilo.disks`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`System` is the app's `disks.system` at `disks/<resource>`, read with
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
    "System",
    "NameDriveArgs",
]

NameDriveArgs = TypedDict("NameDriveArgs", {
    "serial": "str",
    "name": NotRequired["str | None"],
})
"""The arguments of `name-drive`."""

class System(Resource):
    """Storage devices — `disks.system`."""
    __surface__ = "so.tilo.disks@16aeb729799dc193"
    names: dict[str, str]

    def name_drive(self,
        *,
        serial: str,
        name: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["serial"] = serial
        if name is not None:
            args["name"] = name
        return self.action("name-drive", **args)

    def name_drive_many(self, items: list[NameDriveArgs]) -> list[dict[str, Any]]:
        """Runs `name-drive` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("name-drive", items)

    def rescan(self) -> None:
        """Runs the app's action."""
        return self.action("rescan")

    def select(self, *, device: str) -> None:
        """Runs the app's action."""
        return self.action("select", device=device)

    def show_tab(self, *, tab: str) -> None:
        """Runs the app's action."""
        return self.action("show-tab", tab=tab)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"name_drive", "name_drive_many", "rescan", "select", "show_tab"})


