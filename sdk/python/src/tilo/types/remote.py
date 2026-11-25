"""Types for Remote Desktop (`so.tilo.remote`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`ConnectionList` is the app's `remote.connection-list` at `remote/<resource>`, read with
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
    "AuthMethod",
    "Connection",
    "ConnectionKind",
    "ConnectionList",
    "AddConnectionArgs",
    "DeleteConnectionArgs",
    "MarkUsedArgs",
]

AuthMethod = Literal["password", "public_key", "device_identity"]

class Connection(TypedDict):
    name: str
    host: str
    port: int
    user: str
    kind: ConnectionKind
    auth: AuthMethod
    last_used_ms: int | None
    group: str
    favorite: bool

ConnectionKind = Literal["ssh", "vnc", "rdp", "tilo"]

AddConnectionArgs = TypedDict("AddConnectionArgs", {
    "name": "str",
    "host": "str",
    "kind": "ConnectionKind",
    "port": NotRequired["int | None"],
    "user": NotRequired["str | None"],
})
"""The arguments of `add-connection`."""

DeleteConnectionArgs = TypedDict("DeleteConnectionArgs", {
    "connection": "str",
})
"""The arguments of `delete-connection`."""

MarkUsedArgs = TypedDict("MarkUsedArgs", {
    "connection": "str",
    "at_ms": "int",
})
"""The arguments of `mark-used`."""

class ConnectionList(Resource):
    """Remote connections — `remote.connection-list`."""
    __surface__ = "so.tilo.remote@ab22d8658af621c3"
    connections: list[Entry[Connection]]

    def add_connection(self,
        *,
        name: str,
        host: str,
        kind: ConnectionKind,
        port: int | None = None,
        user: str | None = None,
    ) -> None:
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

    def add_connection_many(self, items: list[AddConnectionArgs]) -> list[dict[str, Any]]:
        """Runs `add-connection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-connection", items)

    def delete_connection(self, *, connection: str) -> None:
        """Runs the app's action."""
        return self.action("delete-connection", connection=connection)

    def delete_connection_many(self, items: list[DeleteConnectionArgs]) -> list[dict[str, Any]]:
        """Runs `delete-connection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-connection", items)

    def mark_used(self,
        *,
        connection: str,
        at_ms: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("mark-used", connection=connection, at_ms=at_ms)

    def mark_used_many(self, items: list[MarkUsedArgs]) -> list[dict[str, Any]]:
        """Runs `mark-used` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("mark-used", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"add_connection", "add_connection_many", "delete_connection", "delete_connection_many", "mark_used", "mark_used_many"})


