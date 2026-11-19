"""Types for Calculator (`so.tilo.calculator`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Document` is the app's `calculator.document` at `calculator/<resource>`, read with
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
    "HistoryEntry",
    "PlotFn",
    "Document",
    "CommitHistoryArgs",
    "DeleteHistoryEntryArgs",
    "ClearHistoryArgs",
    "AddFunctionArgs",
    "AddSurfaceArgs",
    "DeleteFunctionArgs",
    "DeleteSurfaceArgs",
]

class HistoryEntry(TypedDict):
    expr: str
    result: str

class PlotFn(TypedDict):
    expr: str
    visible: bool

CommitHistoryArgs = TypedDict("CommitHistoryArgs", {
    "expr": "str",
    "result": "str",
})
"""The arguments of `commit-history`."""

DeleteHistoryEntryArgs = TypedDict("DeleteHistoryEntryArgs", {
    "entry": "str",
})
"""The arguments of `delete-history-entry`."""

ClearHistoryArgs = TypedDict("ClearHistoryArgs", {
})
"""The arguments of `clear-history`."""

AddFunctionArgs = TypedDict("AddFunctionArgs", {
    "formula": "str",
})
"""The arguments of `add-function`."""

AddSurfaceArgs = TypedDict("AddSurfaceArgs", {
    "formula": "str",
})
"""The arguments of `add-surface`."""

DeleteFunctionArgs = TypedDict("DeleteFunctionArgs", {
    "function": "str",
})
"""The arguments of `delete-function`."""

DeleteSurfaceArgs = TypedDict("DeleteSurfaceArgs", {
    "surface": "str",
})
"""The arguments of `delete-surface`."""

class Document(Resource):
    """Calculator — `calculator.document`."""
    __surface__ = "so.tilo.calculator@52956067930d21b2"
    @property
    def history(self) -> list[Entry[HistoryEntry]]:
        """Read only."""
        return self.fields["history"]
    funcs: list[Entry[PlotFn]]
    surfaces: list[Entry[PlotFn]]

    def commit_history(self,
        *,
        expr: str,
        result: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("commit-history", expr=expr, result=result)

    def commit_history_many(self, items: list[CommitHistoryArgs]) -> list[dict[str, Any]]:
        """Runs `commit-history` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("commit-history", items)

    def delete_history_entry(self, *, entry: str) -> None:
        """Runs the app's action."""
        return self.action("delete-history-entry", entry=entry)

    def delete_history_entry_many(self, items: list[DeleteHistoryEntryArgs]) -> list[dict[str, Any]]:
        """Runs `delete-history-entry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-history-entry", items)

    def clear_history(self) -> None:
        """Runs the app's action."""
        return self.action("clear-history")

    def clear_history_many(self, items: list[ClearHistoryArgs]) -> list[dict[str, Any]]:
        """Runs `clear-history` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear-history", items)

    def add_function(self, *, formula: str) -> None:
        """Runs the app's action."""
        return self.action("add-function", formula=formula)

    def add_function_many(self, items: list[AddFunctionArgs]) -> list[dict[str, Any]]:
        """Runs `add-function` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-function", items)

    def add_surface(self, *, formula: str) -> None:
        """Runs the app's action."""
        return self.action("add-surface", formula=formula)

    def add_surface_many(self, items: list[AddSurfaceArgs]) -> list[dict[str, Any]]:
        """Runs `add-surface` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-surface", items)

    def delete_function(self, *, function: str) -> None:
        """Runs the app's action."""
        return self.action("delete-function", function=function)

    def delete_function_many(self, items: list[DeleteFunctionArgs]) -> list[dict[str, Any]]:
        """Runs `delete-function` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-function", items)

    def delete_surface(self, *, surface: str) -> None:
        """Runs the app's action."""
        return self.action("delete-surface", surface=surface)

    def delete_surface_many(self, items: list[DeleteSurfaceArgs]) -> list[dict[str, Any]]:
        """Runs `delete-surface` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-surface", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"commit_history", "commit_history_many", "delete_history_entry", "delete_history_entry_many", "clear_history", "clear_history_many", "add_function", "add_function_many", "add_surface", "add_surface_many", "delete_function", "delete_function_many", "delete_surface", "delete_surface_many"})


