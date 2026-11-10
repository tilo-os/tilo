"""Types for Remote Desktop (`so.tilo.remote`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`ConnectionList` is the app's `remote.connection-list` at `remote/<resource>`, read with
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
    "ConnectionList",
]

class ConnectionList(Resource):
    """Remote connections — `remote.connection-list`."""
    connections: Any

    def add_connection(self,
        *,
        name: str,
        host: str,
        kind: Any,
        port: int | None = None,
        user: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["name"] = name
        args["host"] = host
        args["kind"] = kind
        if port is not None:
            args["port"] = port
        if user is not None:
            args["user"] = user
        return self.action("add-connection", **args)

    def delete_connection(self, *, connection: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-connection", connection=connection)

    def mark_used(self,
        *,
        connection: str,
        at_ms: int,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("mark-used", connection=connection, at_ms=at_ms)


