"""Types for System Monitor (`so.tilo.monitor`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Row` is the app's `tl.so.tilo.monitor.Row` at `monitor/<resource>`, read with
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
    "Row",
    "Monitor",
    "TiloMonitor",
]

class Row(Resource):
    """Row — `tl.so.tilo.monitor.Row`."""
    __surface__ = "so.tilo.monitor@9c68f8bfdaae9287"
    uoc4p4dx: Any  # process
    qwzsalow: int  # depth
    @property
    def _6waph57m(self) -> bool:
        """has_children (`6waph57m` on the wire)."""
        return self.fields["6waph57m"]

    @_6waph57m.setter
    def _6waph57m(self, value: bool) -> None:
        self.edit("6waph57m", value=value)


class Monitor(Resource):
    """Monitor — `tl.so.tilo.monitor.Monitor`."""
    __surface__ = "so.tilo.monitor@9c68f8bfdaae9287"

class TiloMonitor(Resource):
    """System Monitor — `tl.so.tilo.monitor`."""
    __surface__ = "so.tilo.monitor@9c68f8bfdaae9287"
    def end_app(self, *, application: str) -> None:
        """Asks every process of an app to close."""
        return self.action("end_app", application=application)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"end_app"})


