"""Types for Today (`so.tilo.lang.today`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Today` is the app's `tl.so.tilo.lang.today.Today` at `today/<resource>`, read with
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
    "Today",
    "LangToday",
]

class Today(Resource):
    """Today — `tl.so.tilo.lang.today.Today`."""
    __surface__ = "so.tilo.lang.today@b4e754135acc9ac5"

class LangToday(Resource):
    """Today — `tl.so.tilo.lang.today`."""
    __surface__ = "so.tilo.lang.today@b4e754135acc9ac5"
    def morning_cleanup(self,
        *,
        list: Any,
        nb: Any,
    ) -> int:
        """Clears the finished tasks, when there are any, and files a note saying how many were done. Both changes are the Tasks and Notes apps' own, asked at their action APIs: what they did comes back as their resources' next readings."""
        return self.action("morning_cleanup", list=list, nb=nb)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"morning_cleanup"})


