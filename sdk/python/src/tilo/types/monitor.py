"""Types for System Monitor (`so.tilo.monitor`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `monitor/<resource>`, read with
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
    "Notice",
    "Row",
    "Monitor",
    "TiloMonitor",
]

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.monitor@582e77422602fbca"
    @property
    def notice_to(self) -> str:
        """to (`notice-to` on the wire)."""
        return self.fields["notice-to"]

    @notice_to.setter
    def notice_to(self, value: str) -> None:
        self.edit("notice-to", value=value)
    @property
    def notice_title(self) -> str:
        """title (`notice-title` on the wire)."""
        return self.fields["notice-title"]

    @notice_title.setter
    def notice_title(self, value: str) -> None:
        self.edit("notice-title", value=value)
    @property
    def notice_body(self) -> str:
        """body (`notice-body` on the wire)."""
        return self.fields["notice-body"]

    @notice_body.setter
    def notice_body(self, value: str) -> None:
        self.edit("notice-body", value=value)
    @property
    def notice_opens(self) -> Any:
        """opens (`notice-opens` on the wire)."""
        return self.fields["notice-opens"]

    @notice_opens.setter
    def notice_opens(self, value: Any) -> None:
        self.edit("notice-opens", value=value)
    @property
    def notice_seen(self) -> bool:
        """seen (`notice-seen` on the wire)."""
        return self.fields["notice-seen"]

    @notice_seen.setter
    def notice_seen(self, value: bool) -> None:
        self.edit("notice-seen", value=value)
    @property
    def notice_device(self) -> str:
        """device (`notice-device` on the wire)."""
        return self.fields["notice-device"]

    @notice_device.setter
    def notice_device(self, value: str) -> None:
        self.edit("notice-device", value=value)
    @property
    def notice_actions(self) -> list[str]:
        """actions (`notice-actions` on the wire)."""
        return self.fields["notice-actions"]

    @notice_actions.setter
    def notice_actions(self, value: list[str]) -> None:
        self.edit("notice-actions", value=value)


class Row(Resource):
    """Row — `tl.so.tilo.monitor.Row`."""
    __surface__ = "so.tilo.monitor@582e77422602fbca"
    uoc4p4dx: Any  # process
    qwzsalow: int  # depth
    @property
    def _6waph57m(self) -> bool:
        """has_children (`6waph57m` on the wire)."""
        return self.fields["6waph57m"]

    @_6waph57m.setter
    def _6waph57m(self, value: bool) -> None:
        self.edit("6waph57m", value=value)
    @property
    def _22v7oibb(self) -> float:
        """cpu (`22v7oibb` on the wire)."""
        return self.fields["22v7oibb"]

    @_22v7oibb.setter
    def _22v7oibb(self, value: float) -> None:
        self.edit("22v7oibb", value=value)
    u6hj324t: int  # memory_bytes
    cwilirks: int  # threads


class Monitor(Resource):
    """Monitor — `tl.so.tilo.monitor.Monitor`."""
    __surface__ = "so.tilo.monitor@582e77422602fbca"
    def apply_cadence(self,
        *,
        ms: int,
        paused: bool,
    ) -> None:
        """Keeps this device's reading at the cadence the person watches it by."""
        return self.action("apply_cadence", ms=ms, paused=paused)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"apply_cadence"})


class TiloMonitor(Resource):
    """System Monitor — `tl.so.tilo.monitor`."""
    __surface__ = "so.tilo.monitor@582e77422602fbca"
    def end_app(self, *, application: str) -> None:
        """Asks every process of an app to close."""
        return self.action("end_app", application=application)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"end_app"})


