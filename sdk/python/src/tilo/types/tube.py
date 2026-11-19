"""Types for Tube (`so.tilo.tube`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Library` is the app's `tube.library` at `tube/<resource>`, read with
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
    "Reply",
    "Library",
    "WatchHistory",
    "ReplyToCommentArgs",
    "RetractArgs",
    "RecordArgs",
    "ProgressArgs",
    "ImportArgs",
    "ForgetArgs",
    "ClearArgs",
]

class Reply(TypedDict):
    comment: str
    text: str

ReplyToCommentArgs = TypedDict("ReplyToCommentArgs", {
    "comment": "str",
    "text": "str",
})
"""The arguments of `reply-to-comment`."""

RetractArgs = TypedDict("RetractArgs", {
    "reply": "str",
})
"""The arguments of `retract`."""

RecordArgs = TypedDict("RecordArgs", {
    "video": "str",
    "at_ms": "int",
})
"""The arguments of `record`."""

ProgressArgs = TypedDict("ProgressArgs", {
    "video": "str",
    "position_ms": "int",
    "duration_ms": "int",
})
"""The arguments of `progress`."""

ImportArgs = TypedDict("ImportArgs", {
    "watched": "list[Any]",
})
"""The arguments of `import`."""

ForgetArgs = TypedDict("ForgetArgs", {
    "video": "str",
})
"""The arguments of `forget`."""

ClearArgs = TypedDict("ClearArgs", {
})
"""The arguments of `clear`."""

class Library(Resource):
    """Tube — `tube.library`."""
    __surface__ = "so.tilo.tube@a76b54e24761b7aa"
    likes: list[str]
    queue: list[str]
    subscriptions: list[str]
    comment_likes: list[str]
    replies: list[Entry[Reply]]

    def reply_to_comment(self,
        *,
        comment: str,
        text: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("reply-to-comment", comment=comment, text=text)

    def reply_to_comment_many(self, items: list[ReplyToCommentArgs]) -> list[dict[str, Any]]:
        """Runs `reply-to-comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reply-to-comment", items)

    def retract(self, *, reply: str) -> None:
        """Runs the app's action."""
        return self.action("retract", reply=reply)

    def retract_many(self, items: list[RetractArgs]) -> list[dict[str, Any]]:
        """Runs `retract` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("retract", items)

    def search(self, *, query: str) -> None:
        """Runs the app's action."""
        return self.action("search", query=query)

    def toggle_playback(self) -> None:
        """Runs the app's action."""
        return self.action("toggle-playback")

    def seek(self, *, seconds: float) -> None:
        """Runs the app's action."""
        return self.action("seek", seconds=seconds)

    def skip_segment(self) -> None:
        """Runs the app's action."""
        return self.action("skip-segment")

    def set_preference(self,
        *,
        preference: str,
        enabled: bool,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-preference", preference=preference, enabled=enabled)

    def set_preferred_quality(self, *, quality: str) -> None:
        """Runs the app's action."""
        return self.action("set-preferred-quality", quality=quality)

    def set_segment_handling(self,
        *,
        category: str,
        mode: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-segment-handling", category=category, mode=mode)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"reply_to_comment", "reply_to_comment_many", "retract", "retract_many", "search", "toggle_playback", "seek", "skip_segment", "set_preference", "set_preferred_quality", "set_segment_handling"})


class WatchHistory(Resource):
    """Watch history — `tube.watch-history`."""
    __surface__ = "so.tilo.tube@a76b54e24761b7aa"
    watched: dict[str, int]
    positions: dict[str, int]

    def record(self,
        *,
        video: str,
        at_ms: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("record", video=video, at_ms=at_ms)

    def record_many(self, items: list[RecordArgs]) -> list[dict[str, Any]]:
        """Runs `record` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("record", items)

    def progress(self,
        *,
        video: str,
        position_ms: int,
        duration_ms: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("progress", video=video, position_ms=position_ms, duration_ms=duration_ms)

    def progress_many(self, items: list[ProgressArgs]) -> list[dict[str, Any]]:
        """Runs `progress` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("progress", items)

    def import_(self, *, watched: list[Any]) -> None:
        """Runs the app's action."""
        return self.action("import", watched=watched)

    def import_many(self, items: list[ImportArgs]) -> list[dict[str, Any]]:
        """Runs `import` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import", items)

    def forget(self, *, video: str) -> None:
        """Runs the app's action."""
        return self.action("forget", video=video)

    def forget_many(self, items: list[ForgetArgs]) -> list[dict[str, Any]]:
        """Runs `forget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget", items)

    def clear(self) -> None:
        """Runs the app's action."""
        return self.action("clear")

    def clear_many(self, items: list[ClearArgs]) -> list[dict[str, Any]]:
        """Runs `clear` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"record", "record_many", "progress", "progress_many", "import_", "import_many", "forget", "forget_many", "clear", "clear_many"})


