"""Types for Music (`so.tilo.music`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Library` is the app's `music.library` at `music/<resource>`, read with
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
    "Album",
    "Artist",
    "CatalogueTrack",
    "Playlist",
    "Preferences",
    "Track",
    "Library",
    "CreatePlaylistArgs",
    "DeletePlaylistArgs",
    "ImportTrackArgs",
]

class Album(TypedDict):
    id: str
    title: str
    artist_id: str
    year: int

class Artist(TypedDict):
    id: str
    name: str
    genre: str
    monthly_listeners: int

class CatalogueTrack(TypedDict):
    track: Track
    album: Album
    artist: Artist

class Playlist(TypedDict):
    id: str
    name: str
    description: str
    tracks: list[Entry[str]]
    cover: str

class Preferences(TypedDict):
    likes: bool
    saved_albums: bool
    followed_artists: bool
    preferred_artists: list[str]
    dismissed: list[str]

class Track(TypedDict):
    id: str
    title: str
    album_id: str
    artist_id: str
    track_no: int
    duration_s: int
    yt_id: str
    plays: int

CreatePlaylistArgs = TypedDict("CreatePlaylistArgs", {
    "name": "str",
    "track": NotRequired["str | None"],
})
"""The arguments of `create-playlist`."""

DeletePlaylistArgs = TypedDict("DeletePlaylistArgs", {
    "playlist": "str",
})
"""The arguments of `delete-playlist`."""

ImportTrackArgs = TypedDict("ImportTrackArgs", {
    "song": "CatalogueTrack",
})
"""The arguments of `import-track`."""

class Library(Resource):
    """Music — `music.library`."""
    __surface__ = "so.tilo.music@e983a0f927db8d83"
    recommendations: Preferences
    @property
    def artists(self) -> dict[str, Artist]:
        """Read only."""
        return self.fields["artists"]
    @property
    def albums(self) -> dict[str, Album]:
        """Read only."""
        return self.fields["albums"]
    @property
    def tracks(self) -> dict[str, Track]:
        """Read only."""
        return self.fields["tracks"]
    playlists: list[Entry[Playlist]]
    liked: list[str]
    saved_albums: list[str]
    followed_artists: list[str]

    def create_playlist(self,
        *,
        name: str,
        track: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["name"] = name
        if track is not None:
            args["track"] = track
        return self.action("create-playlist", **args)

    def create_playlist_many(self, items: list[CreatePlaylistArgs]) -> list[dict[str, Any]]:
        """Runs `create-playlist` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("create-playlist", items)

    def delete_playlist(self, *, playlist: str) -> None:
        """Runs the app's action."""
        return self.action("delete-playlist", playlist=playlist)

    def delete_playlist_many(self, items: list[DeletePlaylistArgs]) -> list[dict[str, Any]]:
        """Runs `delete-playlist` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-playlist", items)

    def import_track(self, *, song: CatalogueTrack) -> None:
        """Runs the app's action."""
        return self.action("import-track", song=song)

    def import_track_many(self, items: list[ImportTrackArgs]) -> list[dict[str, Any]]:
        """Runs `import-track` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import-track", items)

    def spotify_account_status(self) -> None:
        """Runs the app's action."""
        return self.action("spotify-account-status")

    def search(self, *, query: str) -> None:
        """Runs the app's action."""
        return self.action("search", query=query)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"create_playlist", "create_playlist_many", "delete_playlist", "delete_playlist_many", "import_track", "import_track_many", "spotify_account_status", "search"})


