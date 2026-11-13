"""Types for System Monitor (`so.tilo.monitor`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Processes` is the app's `monitor.processes` at `monitor/<resource>`, read with
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
    "SortKey",
    "Processes",
]

SortKey = Literal["pid", "name", "user", "cpu", "mem", "threads"]

class Processes(Resource):
    """Processes — `monitor.processes`."""
    __surface__ = "so.tilo.monitor@ede2747c2e3967d9"
    sort: SortKey
    ascending: bool
    flat: bool

    # the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
    def show_tab(self, *, tab: str) -> None:
        """Runs the app's action."""
        return self.action("show-tab", tab=tab)

    def filter(self,
        *,
        query: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if query is not None:
            args["query"] = query
        return self.action("filter", **args)

    def inspect(self, *, process: str) -> None:
        """Runs the app's action."""
        return self.action("inspect", process=process)

    def copy_pid(self, *, process: str) -> None:
        """Runs the app's action."""
        return self.action("copy-pid", process=process)


