"""Types for Disks (`so.tilo.disks`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`System` is the app's `disks.system` at `disks/<resource>`, read with
`await ….load()`: its fields are the app's fields (read-only ones without a setter)
and its methods are its actions. A method answers what the app declares; an action
that takes longer answers a `Run`. A record is the dict the wire carries, and an
enum whose cases carry fields is an object whose tag the app's schema names. An
action whose name is one the class already has is run with `action("…")`, which
the file says where it is.
"""

from __future__ import annotations

from typing import Any, Literal, TypedDict
from tilo import Resource, Run

__all__ = [
    "System",
]

class System(Resource):
    """Storage devices — `disks.system`."""
    __surface__ = "so.tilo.disks@172626c1bedc29c8"
    names: dict[str, str]

    def name_drive(self,
        *,
        serial: str,
        name: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["serial"] = serial
        if name is not None:
            args["name"] = name
        return self.action("name-drive", **args)

    def rescan(self) -> None:
        """Runs the app's action."""
        return self.action("rescan")

    def select(self, *, device: str) -> None:
        """Runs the app's action."""
        return self.action("select", device=device)

    def show_tab(self, *, tab: str) -> None:
        """Runs the app's action."""
        return self.action("show-tab", tab=tab)


