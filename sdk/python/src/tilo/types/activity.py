"""Types for Activity (`so.tilo.activity`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Filter` is the app's `activity.filter` at `activity/<resource>`, read with
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
    "Filter",
]

class Filter(Resource):
    """Workspace activity — `activity.filter`."""
    __surface__ = "so.tilo.activity@e0b48ca6887742be"
    period: str
    author: str
    lens: str
    query: str

    def clear_filters(self) -> Run | None:
        """Runs the app's action."""
        return self.action("clear-filters")


