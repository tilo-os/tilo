"""Types for On-Screen Keyboard (`so.tilo.osk`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Keyboard` is the app's `osk.keyboard` at `osk/<resource>`, read with
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
    "KeyOffset",
    "Lesson",
    "Keyboard",
    "Learned",
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

class Keyboard(Resource):
    """On-screen keyboard — `osk.keyboard`."""
    __surface__ = "so.tilo.osk@cc8a202934dfe9a2"
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


class Learned(Resource):
    """What the keyboard learned — `osk.learned`."""
    __surface__ = "so.tilo.osk@cc8a202934dfe9a2"
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

    def learn(self, *, lesson: Lesson) -> Run | None:
        """Runs the app's action."""
        return self.action("learn", lesson=lesson)

    def forget(self) -> Run | None:
        """Runs the app's action."""
        return self.action("forget")


