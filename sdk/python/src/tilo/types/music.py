"""Types for Music (`so.tilo.music`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Library` is the app's `music.library` at `music/<resource>`, read with
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
    "Album",
    "Artist",
    "CatalogueTrack",
    "Playlist",
    "Preferences",
    "Track",
    "Library",
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
    tracks: list[str]
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

class Library(Resource):
    """Music — `music.library`."""
    __surface__ = "so.tilo.music@d92862e59d2efe32"
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
    playlists: list[Playlist]
    liked: list[str]
    saved_albums: list[str]
    followed_artists: list[str]

    def create_playlist(self,
        *,
        name: str,
        track: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["name"] = name
        if track is not None:
            args["track"] = track
        return self.action("create-playlist", **args)

    def delete_playlist(self, *, playlist: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-playlist", playlist=playlist)

    def import_track(self, *, song: CatalogueTrack) -> Run | None:
        """Runs the app's action."""
        return self.action("import-track", song=song)

    def spotify_account_status(self) -> None:
        """Runs the app's action."""
        return self.action("spotify-account-status")

    def search(self, *, query: str) -> None:
        """Runs the app's action."""
        return self.action("search", query=query)


