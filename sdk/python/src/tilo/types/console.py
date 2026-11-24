"""Types for Console (`so.tilo.console`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Window` is the app's `tl.so.tilo.console.Window` at `console/<resource>`, read with
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
    "Window",
    "Console",
]

class Window(Resource):
    """Window — `tl.so.tilo.console.Window`."""
    __surface__ = "so.tilo.console@5e0e5f40d75f4d45"
    e6xn3nds: str  # place


class Console(Resource):
    """Console — `tl.so.tilo.console`."""
    __surface__ = "so.tilo.console@5e0e5f40d75f4d45"

