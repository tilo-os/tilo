"""Types for On-Screen Keyboard (`so.tilo.osk`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Keyboard` is the app's `osk.keyboard` at `osk/<resource>`, read with
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
    "KeyOffset",
    "Lesson",
    "Keyboard",
    "Learned",
    "LearnArgs",
    "ForgetArgs",
]

class KeyOffset(TypedDict):
    dx: float
    dy: float
    samples: int

class Lesson(TypedDict):
    language: str
    transitions: list[Any]
    kept: list[str]
    touch: list[Any]

LearnArgs = TypedDict("LearnArgs", {
    "lesson": "Lesson",
})
"""The arguments of `learn`."""

ForgetArgs = TypedDict("ForgetArgs", {
})
"""The arguments of `forget`."""

class Keyboard(Resource):
    """On-screen keyboard — `osk.keyboard`."""
    __surface__ = "so.tilo.osk@05e8fcea8c336e29"
    def set_layout(self, *, value: str) -> None:
        """Runs the app's action."""
        return self.action("set-layout", value=value)

    def set_size(self, *, value: str) -> None:
        """Runs the app's action."""
        return self.action("set-size", value=value)

    def set_suggestions(self, *, value: bool) -> None:
        """Runs the app's action."""
        return self.action("set-suggestions", value=value)

    def set_autocorrect(self, *, value: bool) -> None:
        """Runs the app's action."""
        return self.action("set-autocorrect", value=value)

    def set_typing_language(self, *, value: str) -> None:
        """Runs the app's action."""
        return self.action("set-typing-language", value=value)

    def set_personalization(self, *, value: bool) -> None:
        """Runs the app's action."""
        return self.action("set-personalization", value=value)

    def set_emoji_recents(self, *, value: list[str]) -> None:
        """Runs the app's action."""
        return self.action("set-emoji-recents", value=value)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"set_layout", "set_size", "set_suggestions", "set_autocorrect", "set_typing_language", "set_personalization", "set_emoji_recents"})


class Learned(Resource):
    """What the keyboard learned — `osk.learned`."""
    __surface__ = "so.tilo.osk@05e8fcea8c336e29"
    @property
    def transitions(self) -> dict[str, dict[str, dict[str, int]]]:
        """Read only."""
        return self.fields["transitions"]
    @property
    def kept(self) -> list[str]:
        """Read only."""
        return self.fields["kept"]
    @property
    def touch(self) -> dict[str, KeyOffset]:
        """Read only."""
        return self.fields["touch"]
    @property
    def restored_previous(self) -> bool:
        """Read only."""
        return self.fields["restored_previous"]

    def learn(self, *, lesson: Lesson) -> None:
        """Runs the app's action."""
        return self.action("learn", lesson=lesson)

    def learn_many(self, items: list[LearnArgs]) -> list[dict[str, Any]]:
        """Runs `learn` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("learn", items)

    def forget(self) -> None:
        """Runs the app's action."""
        return self.action("forget")

    def forget_many(self, items: list[ForgetArgs]) -> list[dict[str, Any]]:
        """Runs `forget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"learn", "learn_many", "forget", "forget_many"})


