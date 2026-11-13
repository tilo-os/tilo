"""Types for Video Player (`so.tilo.video-player`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Library` is the app's `video-player.library` at `video-player/<resource>`, read with
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
    "Playlist",
    "PlaylistItem",
    "SharedTransport",
    "TimestampAnnotation",
    "Library",
    "Recent",
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

class Library(Resource):
    """Video Player — `video-player.library`."""
    __surface__ = "so.tilo.video-player@4f05d77ea6c7a710"
    playlists: list[Playlist]
    items: list[PlaylistItem]
    annotations: list[TimestampAnnotation]
    @property
    def transports(self) -> dict[str, SharedTransport]:
        """Read only."""
        return self.fields["transports"]

    def create_playlist(self, *, name: str) -> Run | None:
        """Runs the app's action."""
        return self.action("create-playlist", name=name)

    def delete_playlist(self, *, playlist: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-playlist", playlist=playlist)

    def add_to_playlist(self,
        *,
        playlist: str,
        media_ref: str,
        title: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("add-to-playlist", playlist=playlist, media_ref=media_ref, title=title)

    def add_to_watch_later(self,
        *,
        media_ref: str,
        title: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("add-to-watch-later", media_ref=media_ref, title=title)

    def add_annotation(self,
        *,
        media_ref: str,
        time_ms: int,
        text: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["media_ref"] = media_ref
        args["time_ms"] = time_ms
        if text is not None:
            args["text"] = text
        return self.action("add-annotation", **args)

    def delete_annotation(self, *, annotation: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-annotation", annotation=annotation)

    def set_transport(self,
        *,
        media_ref: str,
        playing: bool,
        position_ms: int,
        rate: float,
        epoch_ms: int,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-transport", media_ref=media_ref, playing=playing, position_ms=position_ms, rate=rate, epoch_ms=epoch_ms)

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


class Recent(Resource):
    """Recent videos — `video-player.recent`."""
    __surface__ = "so.tilo.video-player@4f05d77ea6c7a710"
    devices: dict[str, Any]

    def keep(self,
        *,
        device: str,
        rows: Any,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("keep", device=device, rows=rows)

    def stood(self,
        *,
        device: str,
        id: str,
        position_ms: int,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("stood", device=device, id=id, position_ms=position_ms)


