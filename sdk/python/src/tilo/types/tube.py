"""Types for Tube (`so.tilo.tube`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Library` is the app's `tube.library` at `tube/<resource>`, read with
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
    "Reply",
    "Library",
    "WatchHistory",
]

class Reply(TypedDict):
    comment: str
    text: str

class Library(Resource):
    """Tube — `tube.library`."""
    __surface__ = "so.tilo.tube@352bce080225ba43"
    likes: list[str]
    queue: list[str]
    subscriptions: list[str]
    comment_likes: list[str]
    replies: list[Reply]

    def reply_to_comment(self,
        *,
        comment: str,
        text: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("reply-to-comment", comment=comment, text=text)

    def retract(self, *, reply: str) -> Run | None:
        """Runs the app's action."""
        return self.action("retract", reply=reply)

    def open_video(self, *, video: str) -> None:
        """Runs the app's action."""
        return self.action("open-video", video=video)

    def open_creator(self, *, creator: str) -> None:
        """Runs the app's action."""
        return self.action("open-creator", creator=creator)

    def open_playlist(self, *, playlist: str) -> None:
        """Runs the app's action."""
        return self.action("open-playlist", playlist=playlist)

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

    def share(self, *, video: str) -> None:
        """Runs the app's action."""
        return self.action("share", video=video)

    def mark_watched(self, *, video: str) -> None:
        """Runs the app's action."""
        return self.action("mark-watched", video=video)

    def forget_watched(self, *, video: str) -> None:
        """Runs the app's action."""
        return self.action("forget-watched", video=video)

    def clear_history(self) -> None:
        """Runs the app's action."""
        return self.action("clear-history")

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


class WatchHistory(Resource):
    """Watch history — `tube.watch-history`."""
    __surface__ = "so.tilo.tube@352bce080225ba43"
    watched: dict[str, int]
    positions: dict[str, int]

    def record(self,
        *,
        video: str,
        at_ms: int,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("record", video=video, at_ms=at_ms)

    def progress(self,
        *,
        video: str,
        position_ms: int,
        duration_ms: int,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("progress", video=video, position_ms=position_ms, duration_ms=duration_ms)

    def import_(self, *, watched: list[Any]) -> Run | None:
        """Runs the app's action."""
        return self.action("import", watched=watched)

    def forget(self, *, video: str) -> Run | None:
        """Runs the app's action."""
        return self.action("forget", video=video)

    def clear(self) -> Run | None:
        """Runs the app's action."""
        return self.action("clear")


