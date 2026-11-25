"""Types for Counter (`so.tilo.lang.counter`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `counter/<resource>`, read with
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
    "Counter",
    "LangCounter",
    "IncrementArgs",
    "DecrementArgs",
]

IncrementArgs = TypedDict("IncrementArgs", {
})
"""The arguments of `increment`."""

DecrementArgs = TypedDict("DecrementArgs", {
})
"""The arguments of `decrement`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.lang.counter@ce1645e68d884a2d"
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


class Counter(Resource):
    """Counter — `tl.so.tilo.lang.counter.Counter`."""
    __surface__ = "so.tilo.lang.counter@ce1645e68d884a2d"
    @property
    def _3wkdueca(self) -> int:
        """value (`3wkdueca` on the wire)."""
        return self.fields["3wkdueca"]

    @_3wkdueca.setter
    def _3wkdueca(self, value: int) -> None:
        self.edit("3wkdueca", value=value)

    def increment(self) -> None:
        """Add one to the counter."""
        return self.action("increment")

    def increment_many(self, items: list[IncrementArgs]) -> list[dict[str, Any]]:
        """Runs `increment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("increment", items)

    def decrement(self) -> None:
        """Take one away from the counter."""
        return self.action("decrement")

    def decrement_many(self, items: list[DecrementArgs]) -> list[dict[str, Any]]:
        """Runs `decrement` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("decrement", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"increment", "increment_many", "decrement", "decrement_many"})


class LangCounter(Resource):
    """Counter — `tl.so.tilo.lang.counter`."""
    __surface__ = "so.tilo.lang.counter@ce1645e68d884a2d"

