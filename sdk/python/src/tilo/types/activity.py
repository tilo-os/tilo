"""Types for Activity (`so.tilo.activity`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Window` is the app's `tl.so.tilo.activity.Window` at `activity/<resource>`, read with
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
    "Activity",
]

class Window(Resource):
    """Window — `tl.so.tilo.activity.Window`."""
    __surface__ = "so.tilo.activity@b9adf3436f2f77fa"
    ukfxyqw3: str  # place


class Activity(Resource):
    """Activity — `tl.so.tilo.activity`."""
    __surface__ = "so.tilo.activity@b9adf3436f2f77fa"

