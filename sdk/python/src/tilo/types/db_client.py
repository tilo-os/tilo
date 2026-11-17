"""Types for Database Client (`so.tilo.db-client`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Workspace` is the app's `db-client.workspace` at `db-client/<resource>`, read with
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
    "Engine",
    "SavedConnection",
    "SavedQuery",
    "Workspace",
    "AddConnectionArgs",
    "RemoveConnectionArgs",
    "SaveQueryArgs",
    "RemoveQueryArgs",
]

Engine = Literal["sqlite", "postgres", "mysql"]

class SavedConnection(TypedDict):
    name: str
    engine: Engine
    host: str
    port: int
    database: str
    user: str
    file_path: str

class SavedQuery(TypedDict):
    name: str
    sql: str

AddConnectionArgs = TypedDict("AddConnectionArgs", {
    "name": "str",
    "engine": "str",
    "location": NotRequired["str | None"],
    "database": NotRequired["str | None"],
    "user": NotRequired["str | None"],
})
"""The arguments of `add-connection`."""

RemoveConnectionArgs = TypedDict("RemoveConnectionArgs", {
    "connection": "str",
})
"""The arguments of `remove-connection`."""

SaveQueryArgs = TypedDict("SaveQueryArgs", {
    "name": "str",
    "sql": NotRequired["str | None"],
})
"""The arguments of `save-query`."""

RemoveQueryArgs = TypedDict("RemoveQueryArgs", {
    "query": "str",
})
"""The arguments of `remove-query`."""

class Workspace(Resource):
    """Database — `db-client.workspace`."""
    __surface__ = "so.tilo.db-client@ff2837e66ce4dd02"
    connections: list[Entry[SavedConnection]]
    saved_queries: list[Entry[SavedQuery]]
    sql: str

    def add_connection(self,
        *,
        name: str,
        engine: str,
        location: str | None = None,
        database: str | None = None,
        user: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["name"] = name
        args["engine"] = engine
        if location is not None:
            args["location"] = location
        if database is not None:
            args["database"] = database
        if user is not None:
            args["user"] = user
        return self.action("add-connection", **args)

    def add_connection_many(self, items: list[AddConnectionArgs]) -> list[dict[str, Any]]:
        """Runs `add-connection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-connection", items)

    def remove_connection(self, *, connection: str) -> None:
        """Runs the app's action."""
        return self.action("remove-connection", connection=connection)

    def remove_connection_many(self, items: list[RemoveConnectionArgs]) -> list[dict[str, Any]]:
        """Runs `remove-connection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-connection", items)

    def save_query(self,
        *,
        name: str,
        sql: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["name"] = name
        if sql is not None:
            args["sql"] = sql
        return self.action("save-query", **args)

    def save_query_many(self, items: list[SaveQueryArgs]) -> list[dict[str, Any]]:
        """Runs `save-query` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("save-query", items)

    def remove_query(self, *, query: str) -> None:
        """Runs the app's action."""
        return self.action("remove-query", query=query)

    def remove_query_many(self, items: list[RemoveQueryArgs]) -> list[dict[str, Any]]:
        """Runs `remove-query` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-query", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"add_connection", "add_connection_many", "remove_connection", "remove_connection_many", "save_query", "save_query_many", "remove_query", "remove_query_many"})


