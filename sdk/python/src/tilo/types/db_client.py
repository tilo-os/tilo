"""Types for Database Client (`so.tilo.db-client`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Workspace` is the app's `db-client.workspace` at `db-client/<resource>`, read with
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
    "Workspace",
]

class Workspace(Resource):
    """Database — `db-client.workspace`."""
    connections: Any
    saved_queries: Any
    sql: Any

    def add_connection(self,
        *,
        name: str,
        engine: str,
        location: str | None = None,
        database: str | None = None,
        user: str | None = None,
    ) -> Run | None:
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

    def remove_connection(self, *, connection: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-connection", connection=connection)

    def save_query(self,
        *,
        name: str,
        sql: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["name"] = name
        if sql is not None:
            args["sql"] = sql
        return self.action("save-query", **args)

    def remove_query(self, *, query: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-query", query=query)


