"""Types for Windows Apps (`so.tilo.windows`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Manager` is the app's `windows.manager` at `windows/<resource>`, read with
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
    "Route",
    "Manager",
]

Route = Literal["wine", "proton", "vm"]

class Manager(Resource):
    """Windows manager — `windows.manager`."""
    __surface__ = "so.tilo.windows@88e1e8cbfc0827d1"
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


