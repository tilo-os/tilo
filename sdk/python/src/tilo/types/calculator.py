"""Types for Calculator (`so.tilo.calculator`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Document` is the app's `calculator.document` at `calculator/<resource>`, read with
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
    "HistoryEntry",
    "PlotFn",
    "Document",
]

class HistoryEntry(TypedDict):
    expr: str
    result: str

class PlotFn(TypedDict):
    expr: str
    visible: bool

class Document(Resource):
    """Calculator — `calculator.document`."""
    __surface__ = "so.tilo.calculator@e852dcdbed36b5fa"
    @property
    def history(self) -> list[HistoryEntry]:
        """Read only."""
        return self.fields["history"]
    funcs: list[PlotFn]
    surfaces: list[PlotFn]

    def commit_history(self,
        *,
        expr: str,
        result: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("commit-history", expr=expr, result=result)

    def delete_history_entry(self, *, entry: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-history-entry", entry=entry)

    def clear_history(self) -> Run | None:
        """Runs the app's action."""
        return self.action("clear-history")

    def add_function(self, *, formula: str) -> Run | None:
        """Runs the app's action."""
        return self.action("add-function", formula=formula)

    def add_surface(self, *, formula: str) -> Run | None:
        """Runs the app's action."""
        return self.action("add-surface", formula=formula)

    def delete_function(self, *, function: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-function", function=function)

    def delete_surface(self, *, surface: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-surface", surface=surface)


