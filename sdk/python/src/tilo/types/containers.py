"""Types for Containers (`so.tilo.containers`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Engine` is the app's `containers.engine` at `containers/<resource>`, read with
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
    "CreateDraft",
    "Engine",
]

class CreateDraft(TypedDict):
    name: str
    image: str
    ports: str
    env: str
    volumes: str
    command: str

class Engine(Resource):
    """Containers — `containers.engine`."""
    __surface__ = "so.tilo.containers@5426b1fbca56758c"
    draft: CreateDraft

    def clear_create(self) -> Run | None:
        """Runs the app's action."""
        return self.action("clear-create")

    def create(self) -> None:
        """Runs the app's action."""
        return self.action("create")


