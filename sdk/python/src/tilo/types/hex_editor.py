"""Types for Hex Editor (`so.tilo.hex-editor`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Document` is the app's `hex-editor.document` at `hex-editor/<resource>`, read with
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
    "Document",
    "InsertBytesArgs",
    "OverwriteBytesArgs",
    "DeleteBytesArgs",
]

InsertBytesArgs = TypedDict("InsertBytesArgs", {
    "offset": "int",
    "bytes": "str",
})
"""The arguments of `insert-bytes`."""

OverwriteBytesArgs = TypedDict("OverwriteBytesArgs", {
    "offset": "int",
    "bytes": "str",
})
"""The arguments of `overwrite-bytes`."""

DeleteBytesArgs = TypedDict("DeleteBytesArgs", {
    "offset": "int",
    "len": "int",
})
"""The arguments of `delete-bytes`."""

class Document(Resource):
    """Hex document — `hex-editor.document`."""
    __surface__ = "so.tilo.hex-editor@95e151cf25dbc81a"
    @property
    def name(self) -> str | None:
        """Read only."""
        return self.fields["name"]
    @property
    def opened(self) -> int:
        """Read only."""
        return self.fields["opened"]
    bytes: Any

    # the app's `load` action is the SDK's own: run it with `action("load")`.
    def insert_bytes(self,
        *,
        offset: int,
        bytes: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("insert-bytes", offset=offset, bytes=bytes)

    def insert_bytes_many(self, items: list[InsertBytesArgs]) -> list[dict[str, Any]]:
        """Runs `insert-bytes` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("insert-bytes", items)

    def overwrite_bytes(self,
        *,
        offset: int,
        bytes: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("overwrite-bytes", offset=offset, bytes=bytes)

    def overwrite_bytes_many(self, items: list[OverwriteBytesArgs]) -> list[dict[str, Any]]:
        """Runs `overwrite-bytes` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("overwrite-bytes", items)

    def delete_bytes(self,
        *,
        offset: int,
        len: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("delete-bytes", offset=offset, len=len)

    def delete_bytes_many(self, items: list[DeleteBytesArgs]) -> list[dict[str, Any]]:
        """Runs `delete-bytes` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-bytes", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"insert_bytes", "insert_bytes_many", "overwrite_bytes", "overwrite_bytes_many", "delete_bytes", "delete_bytes_many"})


