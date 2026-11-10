"""Types for Logs (`so.tilo.logs`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Journal` is the app's `logs.journal` at `logs/<resource>`, read with
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
    "Priority",
    "Journal",
]

Priority = Literal["emerg", "alert", "crit", "err", "warning", "notice", "info", "debug"]

class Journal(Resource):
    """System journal — `logs.journal`."""
    severity: Priority
    unit: str
    boot: str
    search: str

    def clear_filters(self) -> Run | None:
        """Runs the app's action."""
        return self.action("clear-filters")

    # the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
    def set_live_tail(self,
        *,
        enabled: bool | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if enabled is not None:
            args["enabled"] = enabled
        return self.action("set-live-tail", **args)

    def select_entry(self, *, entry_id: str) -> None:
        """Runs the app's action."""
        return self.action("select-entry", entry_id=entry_id)


