"""Types for Video Player (`so.tilo.video-player`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Library` is the app's `video-player.library` at `video-player/<resource>`, read with
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
    "Playlist",
    "PlaylistItem",
    "SharedTransport",
    "TimestampAnnotation",
    "Library",
    "Recent",
    "CreatePlaylistArgs",
    "DeletePlaylistArgs",
    "AddToPlaylistArgs",
    "AddToWatchLaterArgs",
    "AddAnnotationArgs",
    "DeleteAnnotationArgs",
    "SetTransportArgs",
    "KeepArgs",
    "StoodArgs",
]

class Playlist(TypedDict):
    name: str

class PlaylistItem(TypedDict):
    playlist: str
    media_ref: str
    title: str

class SharedTransport(TypedDict):
    playing: bool
    position_ms: int
    rate: float
    epoch_ms: int

class TimestampAnnotation(TypedDict):
    media_ref: str
    time_ms: int
    text: str

CreatePlaylistArgs = TypedDict("CreatePlaylistArgs", {
    "name": "str",
})
"""The arguments of `create-playlist`."""

DeletePlaylistArgs = TypedDict("DeletePlaylistArgs", {
    "playlist": "str",
})
"""The arguments of `delete-playlist`."""

AddToPlaylistArgs = TypedDict("AddToPlaylistArgs", {
    "playlist": "str",
    "media_ref": "str",
    "title": "str",
})
"""The arguments of `add-to-playlist`."""

AddToWatchLaterArgs = TypedDict("AddToWatchLaterArgs", {
    "media_ref": "str",
    "title": "str",
})
"""The arguments of `add-to-watch-later`."""

AddAnnotationArgs = TypedDict("AddAnnotationArgs", {
    "media_ref": "str",
    "time_ms": "int",
    "text": NotRequired["str | None"],
})
"""The arguments of `add-annotation`."""

DeleteAnnotationArgs = TypedDict("DeleteAnnotationArgs", {
    "annotation": "str",
})
"""The arguments of `delete-annotation`."""

SetTransportArgs = TypedDict("SetTransportArgs", {
    "media_ref": "str",
    "playing": "bool",
    "position_ms": "int",
    "rate": "float",
    "epoch_ms": "int",
})
"""The arguments of `set-transport`."""

KeepArgs = TypedDict("KeepArgs", {
    "device": "str",
    "rows": "Any",
})
"""The arguments of `keep`."""

StoodArgs = TypedDict("StoodArgs", {
    "device": "str",
    "id": "str",
    "position_ms": "int",
})
"""The arguments of `stood`."""

class Library(Resource):
    """Video Player — `video-player.library`."""
    __surface__ = "so.tilo.video-player@bb6bf2b768952ba1"
    playlists: list[Entry[Playlist]]
    items: list[Entry[PlaylistItem]]
    annotations: list[Entry[TimestampAnnotation]]
    @property
    def transports(self) -> dict[str, SharedTransport]:
        """Read only."""
        return self.fields["transports"]

    def create_playlist(self, *, name: str) -> None:
        """Runs the app's action."""
        return self.action("create-playlist", name=name)

    def create_playlist_many(self, items: list[CreatePlaylistArgs]) -> list[dict[str, Any]]:
        """Runs `create-playlist` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("create-playlist", items)

    def delete_playlist(self, *, playlist: str) -> None:
        """Runs the app's action."""
        return self.action("delete-playlist", playlist=playlist)

    def delete_playlist_many(self, items: list[DeletePlaylistArgs]) -> list[dict[str, Any]]:
        """Runs `delete-playlist` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-playlist", items)

    def add_to_playlist(self,
        *,
        playlist: str,
        media_ref: str,
        title: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("add-to-playlist", playlist=playlist, media_ref=media_ref, title=title)

    def add_to_playlist_many(self, items: list[AddToPlaylistArgs]) -> list[dict[str, Any]]:
        """Runs `add-to-playlist` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-to-playlist", items)

    def add_to_watch_later(self,
        *,
        media_ref: str,
        title: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("add-to-watch-later", media_ref=media_ref, title=title)

    def add_to_watch_later_many(self, items: list[AddToWatchLaterArgs]) -> list[dict[str, Any]]:
        """Runs `add-to-watch-later` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-to-watch-later", items)

    def add_annotation(self,
        *,
        media_ref: str,
        time_ms: int,
        text: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["media_ref"] = media_ref
        args["time_ms"] = time_ms
        if text is not None:
            args["text"] = text
        return self.action("add-annotation", **args)

    def add_annotation_many(self, items: list[AddAnnotationArgs]) -> list[dict[str, Any]]:
        """Runs `add-annotation` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-annotation", items)

    def delete_annotation(self, *, annotation: str) -> None:
        """Runs the app's action."""
        return self.action("delete-annotation", annotation=annotation)

    def delete_annotation_many(self, items: list[DeleteAnnotationArgs]) -> list[dict[str, Any]]:
        """Runs `delete-annotation` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-annotation", items)

    def set_transport(self,
        *,
        media_ref: str,
        playing: bool,
        position_ms: int,
        rate: float,
        epoch_ms: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-transport", media_ref=media_ref, playing=playing, position_ms=position_ms, rate=rate, epoch_ms=epoch_ms)

    def set_transport_many(self, items: list[SetTransportArgs]) -> list[dict[str, Any]]:
        """Runs `set-transport` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-transport", items)

    def open_video(self,
        *,
        path: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if path is not None:
            args["path"] = path
        return self.action("open-video", **args)

    def clear_recent(self) -> None:
        """Runs the app's action."""
        return self.action("clear-recent")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"create_playlist", "create_playlist_many", "delete_playlist", "delete_playlist_many", "add_to_playlist", "add_to_playlist_many", "add_to_watch_later", "add_to_watch_later_many", "add_annotation", "add_annotation_many", "delete_annotation", "delete_annotation_many", "set_transport", "set_transport_many", "open_video", "clear_recent"})


class Recent(Resource):
    """Recent videos — `video-player.recent`."""
    __surface__ = "so.tilo.video-player@bb6bf2b768952ba1"
    devices: dict[str, Any]

    def keep(self,
        *,
        device: str,
        rows: Any,
    ) -> None:
        """Runs the app's action."""
        return self.action("keep", device=device, rows=rows)

    def keep_many(self, items: list[KeepArgs]) -> list[dict[str, Any]]:
        """Runs `keep` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep", items)

    def stood(self,
        *,
        device: str,
        id: str,
        position_ms: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("stood", device=device, id=id, position_ms=position_ms)

    def stood_many(self, items: list[StoodArgs]) -> list[dict[str, Any]]:
        """Runs `stood` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("stood", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"keep", "keep_many", "stood", "stood_many"})


