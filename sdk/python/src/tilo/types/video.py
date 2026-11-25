"""Types for Video (`so.tilo.video`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `video/<resource>`, read with
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
    "Clip",
    "Playlist",
    "PlaylistEntry",
    "Bookmark",
    "Library",
    "Video",
    "TogglePlayingArgs",
    "DeletePlaylistArgs",
    "AddToPlaylistArgs",
    "AddToWatchLaterArgs",
    "DeleteBookmarkArgs",
    "OpenDroppedArgs",
    "PlayClipArgs",
    "RestartClipArgs",
]

TogglePlayingArgs = TypedDict("TogglePlayingArgs", {
})
"""The arguments of `toggle_playing`."""

DeletePlaylistArgs = TypedDict("DeletePlaylistArgs", {
    "pl": "Playlist",
})
"""The arguments of `delete_playlist`."""

AddToPlaylistArgs = TypedDict("AddToPlaylistArgs", {
    "pl": "Playlist",
    "c": "Clip",
})
"""The arguments of `add_to_playlist`."""

AddToWatchLaterArgs = TypedDict("AddToWatchLaterArgs", {
    "c": "Clip",
})
"""The arguments of `add_to_watch_later`."""

DeleteBookmarkArgs = TypedDict("DeleteBookmarkArgs", {
    "b": "Bookmark",
})
"""The arguments of `delete_bookmark`."""

OpenDroppedArgs = TypedDict("OpenDroppedArgs", {
    "files": "list[Any]",
})
"""The arguments of `open_dropped`."""

PlayClipArgs = TypedDict("PlayClipArgs", {
    "c": "Clip",
})
"""The arguments of `play_clip`."""

RestartClipArgs = TypedDict("RestartClipArgs", {
    "c": "Clip",
})
"""The arguments of `restart_clip`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.video@28f43ffb04ea7f57"
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


class Clip(Resource):
    """Clip — `tl.so.tilo.video.Clip`."""
    __surface__ = "so.tilo.video@28f43ffb04ea7f57"
    sit6wbwo: str  # title
    vnxskfep: str  # source

    def toggle_playing(self) -> None:
        """Plays or pauses the session when it is showing `c`: what the P key and the window's player control run, without needing the root passed in."""
        return self.action("toggle_playing")

    def toggle_playing_many(self, items: list[TogglePlayingArgs]) -> list[dict[str, Any]]:
        """Runs `toggle_playing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("toggle_playing", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"toggle_playing", "toggle_playing_many"})


class Playlist(Resource):
    """Playlist — `tl.so.tilo.video.Playlist`."""
    __surface__ = "so.tilo.video@28f43ffb04ea7f57"
    hktiqra6: str  # name


class PlaylistEntry(Resource):
    """PlaylistEntry — `tl.so.tilo.video.PlaylistEntry`."""
    __surface__ = "so.tilo.video@28f43ffb04ea7f57"
    pmcgj5bo: Playlist  # playlist
    gofxf7oh: Clip  # clip


class Bookmark(Resource):
    """Bookmark — `tl.so.tilo.video.Bookmark`."""
    __surface__ = "so.tilo.video@28f43ffb04ea7f57"
    h4e7iq7t: Clip  # clip
    cwamirwi: float  # at
    n323noym: str  # text


class Library(Resource):
    """Library — `tl.so.tilo.video.Library`."""
    __surface__ = "so.tilo.video@28f43ffb04ea7f57"
    cfgqumjg: list[Entry[Clip]]  # clips
    @property
    def _7sp4sk7x(self) -> list[Entry[Playlist]]:
        """playlists (`7sp4sk7x` on the wire)."""
        return self.fields["7sp4sk7x"]

    @_7sp4sk7x.setter
    def _7sp4sk7x(self, value: list[Entry[Playlist]]) -> None:
        self.edit("7sp4sk7x", value=value)
    ve6kofwh: list[Entry[PlaylistEntry]]  # entries
    srzszayp: list[Entry[Clip]]  # watch_later
    @property
    def _265jfqy5(self) -> list[Entry[Bookmark]]:
        """bookmarks (`265jfqy5` on the wire)."""
        return self.fields["265jfqy5"]

    @_265jfqy5.setter
    def _265jfqy5(self, value: list[Entry[Bookmark]]) -> None:
        self.edit("265jfqy5", value=value)
    jcuq2e4s: Any | None  # playing
    eajmuh6i: list[Entry[Any]]  # sessions

    def open_video(self, *, file: Any) -> Clip:
        """Opens a video file: the file a person picked, or one Files handed over, and plays it where it was left. The clip it already made opens instead of a second copy, so its place in history stays one."""
        return self.action("open_video", file=file)

    def create_playlist(self, *, name: str) -> Playlist:
        """Starts an empty playlist."""
        return self.action("create_playlist", name=name)

    def delete_playlist(self, *, pl: Playlist) -> None:
        """Deletes a playlist and the entries that name it."""
        return self.action("delete_playlist", pl=pl)

    def delete_playlist_many(self, items: list[DeletePlaylistArgs]) -> list[dict[str, Any]]:
        """Runs `delete_playlist` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_playlist", items)

    def add_to_playlist(self,
        *,
        pl: Playlist,
        c: Clip,
    ) -> None:
        """Adds `c` to `pl`, once."""
        return self.action("add_to_playlist", pl=pl, c=c)

    def add_to_playlist_many(self, items: list[AddToPlaylistArgs]) -> list[dict[str, Any]]:
        """Runs `add_to_playlist` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_to_playlist", items)

    def add_to_watch_later(self, *, c: Clip) -> None:
        """Keeps `c` to watch later, once."""
        return self.action("add_to_watch_later", c=c)

    def add_to_watch_later_many(self, items: list[AddToWatchLaterArgs]) -> list[dict[str, Any]]:
        """Runs `add_to_watch_later` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_to_watch_later", items)

    def add_bookmark(self,
        *,
        c: Clip,
        at: float,
        text: str | None = None,
    ) -> Bookmark:
        """Bookmarks the moment `at` of `c`, with `text` when the person wrote something."""
        args: dict[str, Any] = {}
        args["c"] = c
        args["at"] = at
        if text is not None:
            args["text"] = text
        return self.action("add_bookmark", **args)

    def delete_bookmark(self, *, b: Bookmark) -> None:
        """Deletes a bookmark or note."""
        return self.action("delete_bookmark", b=b)

    def delete_bookmark_many(self, items: list[DeleteBookmarkArgs]) -> list[dict[str, Any]]:
        """Runs `delete_bookmark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_bookmark", items)

    def open_dropped(self, *, files: list[Any]) -> None:
        """Opens the videos a person dropped on the window: each file becomes its clip, and the first plays in the shared session."""
        return self.action("open_dropped", files=files)

    def open_dropped_many(self, items: list[OpenDroppedArgs]) -> list[dict[str, Any]]:
        """Runs `open_dropped` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open_dropped", items)

    def session(self) -> Any:
        """The session this app plays through: the one it has, or one made now. It is kept both in `playing`, which a view reads, and in `sessions`, so the record is a resource of its own."""
        return self.action("session")

    def play_clip(self, *, c: Clip) -> None:
        """Plays `c` in the shared session, from where it was left (the beginning the first time): what the window's Play button, the palette and an agent's `play` run. The place is the shared session's own record, so the video continues on this device or another."""
        return self.action("play_clip", c=c)

    def play_clip_many(self, items: list[PlayClipArgs]) -> list[dict[str, Any]]:
        """Runs `play_clip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("play_clip", items)

    def restart_clip(self, *, c: Clip) -> None:
        """Plays `c` from its beginning, as a person who watched it before chooses."""
        return self.action("restart_clip", c=c)

    def restart_clip_many(self, items: list[RestartClipArgs]) -> list[dict[str, Any]]:
        """Runs `restart_clip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("restart_clip", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"open_video", "create_playlist", "delete_playlist", "delete_playlist_many", "add_to_playlist", "add_to_playlist_many", "add_to_watch_later", "add_to_watch_later_many", "add_bookmark", "delete_bookmark", "delete_bookmark_many", "open_dropped", "open_dropped_many", "session", "play_clip", "play_clip_many", "restart_clip", "restart_clip_many"})


class Video(Resource):
    """Video — `tl.so.tilo.video`."""
    __surface__ = "so.tilo.video@28f43ffb04ea7f57"

