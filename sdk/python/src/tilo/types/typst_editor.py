"""Types for Typst Editor (`so.tilo.typst-editor`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Workspace` is the app's `typst-editor.workspace` at `typst-editor/<resource>`, read with
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
    "TypstFile",
    "Workspace",
    "NewFileArgs",
    "SavedAtArgs",
    "CloseFileArgs",
]

class TypstFile(TypedDict):
    name: str
    path: str | None
    source: str

NewFileArgs = TypedDict("NewFileArgs", {
    "name": "str",
    "path": NotRequired["str | None"],
    "source": NotRequired["str | None"],
})
"""The arguments of `new-file`."""

SavedAtArgs = TypedDict("SavedAtArgs", {
    "file": "str",
    "path": "str",
})
"""The arguments of `saved-at`."""

CloseFileArgs = TypedDict("CloseFileArgs", {
    "file": "str",
})
"""The arguments of `close-file`."""

class Workspace(Resource):
    """Typst workspace — `typst-editor.workspace`."""
    __surface__ = "so.tilo.typst-editor@f7f82fa8063cb876"
    files: list[Entry[TypstFile]]

    def new_file(self,
        *,
        name: str,
        path: str | None = None,
        source: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["name"] = name
        if path is not None:
            args["path"] = path
        if source is not None:
            args["source"] = source
        return self.action("new-file", **args)

    def new_file_many(self, items: list[NewFileArgs]) -> list[dict[str, Any]]:
        """Runs `new-file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("new-file", items)

    def saved_at(self,
        *,
        file: str,
        path: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("saved-at", file=file, path=path)

    def saved_at_many(self, items: list[SavedAtArgs]) -> list[dict[str, Any]]:
        """Runs `saved-at` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("saved-at", items)

    # the app's `load` action is the SDK's own: run it with `action("load")`.
    def close_file(self, *, file: str) -> None:
        """Runs the app's action."""
        return self.action("close-file", file=file)

    def close_file_many(self, items: list[CloseFileArgs]) -> list[dict[str, Any]]:
        """Runs `close-file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("close-file", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_file", "new_file_many", "saved_at", "saved_at_many", "close_file", "close_file_many"})


