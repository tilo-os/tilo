"""Types for Hex Editor (`so.tilo.hex-editor`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Document` is the app's `hex-editor.document` at `hex-editor/<resource>`, read with
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
    "Document",
]

class Document(Resource):
    """Hex document — `hex-editor.document`."""
    @property
    def name(self) -> Any:
        """Read only."""
        return self.fields["name"]
    @property
    def opened(self) -> Any:
        """Read only."""
        return self.fields["opened"]
    bytes: Any

    # the app's `load` action is the SDK's own: run it with `action("load")`.
    def insert_bytes(self,
        *,
        offset: int,
        bytes: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("insert-bytes", offset=offset, bytes=bytes)

    def overwrite_bytes(self,
        *,
        offset: int,
        bytes: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("overwrite-bytes", offset=offset, bytes=bytes)

    def delete_bytes(self,
        *,
        offset: int,
        len: int,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-bytes", offset=offset, len=len)


