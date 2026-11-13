"""Types for Typst Editor (`so.tilo.typst-editor`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Workspace` is the app's `typst-editor.workspace` at `typst-editor/<resource>`, read with
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
    "TypstFile",
    "Workspace",
]

class TypstFile(TypedDict):
    name: str
    path: str | None
    source: str

class Workspace(Resource):
    """Typst workspace — `typst-editor.workspace`."""
    __surface__ = "so.tilo.typst-editor@f31407ab794a3630"
    files: list[TypstFile]

    def new_file(self,
        *,
        name: str,
        path: str | None = None,
        source: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["name"] = name
        if path is not None:
            args["path"] = path
        if source is not None:
            args["source"] = source
        return self.action("new-file", **args)

    def saved_at(self,
        *,
        file: str,
        path: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("saved-at", file=file, path=path)

    # the app's `load` action is the SDK's own: run it with `action("load")`.
    def close_file(self, *, file: str) -> Run | None:
        """Runs the app's action."""
        return self.action("close-file", file=file)


