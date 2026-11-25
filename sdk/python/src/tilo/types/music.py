"""Types for Music (`so.tilo.music`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `music/<resource>`, read with
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
    "Track",
    "Album",
    "Artist",
    "Playlist",
    "SpotifyDevice",
    "SpotifyQueueItem",
    "Section",
    "Suggestion",
    "Mixtape",
    "Taste",
    "Library",
    "Music",
    "RemoveOfflineArgs",
    "PlayAlbumArgs",
    "PlayMixtapeArgs",
    "PickCoverArgs",
    "SetCoverArgs",
    "FollowStreamArgs",
    "AddToQueueArgs",
    "PlayNextArgs",
    "HideRecArgs",
    "RestoreRecArgs",
    "UpdateTasteArgs",
    "DismissArtistArgs",
    "PreferArtistArgs",
    "QueueMovedArgs",
]

RemoveOfflineArgs = TypedDict("RemoveOfflineArgs", {
})
"""The arguments of `remove_offline`."""

PlayAlbumArgs = TypedDict("PlayAlbumArgs", {
})
"""The arguments of `play_album`."""

PlayMixtapeArgs = TypedDict("PlayMixtapeArgs", {
})
"""The arguments of `play_mixtape`."""

PickCoverArgs = TypedDict("PickCoverArgs", {
    "file": "Any",
})
"""The arguments of `pick_cover`."""

SetCoverArgs = TypedDict("SetCoverArgs", {
    "file": "Any",
})
"""The arguments of `set_cover`."""

FollowStreamArgs = TypedDict("FollowStreamArgs", {
    "p": "Any",
})
"""The arguments of `follow_stream`."""

AddToQueueArgs = TypedDict("AddToQueueArgs", {
    "t": "Track",
})
"""The arguments of `add_to_queue`."""

PlayNextArgs = TypedDict("PlayNextArgs", {
    "t": "Track",
})
"""The arguments of `play_next`."""

HideRecArgs = TypedDict("HideRecArgs", {
    "t": "Track",
})
"""The arguments of `hide_rec`."""

RestoreRecArgs = TypedDict("RestoreRecArgs", {
    "t": "Track",
})
"""The arguments of `restore_rec`."""

UpdateTasteArgs = TypedDict("UpdateTasteArgs", {
    "which": "str",
    "on": "bool",
})
"""The arguments of `update_taste`."""

DismissArtistArgs = TypedDict("DismissArtistArgs", {
    "a": "Artist",
})
"""The arguments of `dismiss_artist`."""

PreferArtistArgs = TypedDict("PreferArtistArgs", {
    "a": "Artist",
})
"""The arguments of `prefer_artist`."""

QueueMovedArgs = TypedDict("QueueMovedArgs", {
    "p": "Any",
    "ids": "list[str]",
})
"""The arguments of `queue_moved`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.music@7417b198334861f3"
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


class Track(Resource):
    """Track — `tl.so.tilo.music.Track`."""
    __surface__ = "so.tilo.music@7417b198334861f3"
    wapyyydf: str  # key
    zpnajesf: str  # title
    pzaz5i7w: str  # artist
    kyegi6xm: str  # artist_key
    j67d2q4o: str  # album
    ojqrhkks: str  # album_key
    @property
    def _2sm6xpgo(self) -> int:
        """seconds (`2sm6xpgo` on the wire)."""
        return self.fields["2sm6xpgo"]

    @_2sm6xpgo.setter
    def _2sm6xpgo(self, value: int) -> None:
        self.edit("2sm6xpgo", value=value)
    wy2gg3hw: int  # plays
    yaujsp7u: str  # cover
    hlfttyoj: str  # found_for
    @property
    def _47b7sivc(self) -> str:
        """stream (`47b7sivc` on the wire)."""
        return self.fields["47b7sivc"]

    @_47b7sivc.setter
    def _47b7sivc(self, value: str) -> None:
        self.edit("47b7sivc", value=value)
    fdkr4wxf: str  # offline
    @property
    def _2hvaqxk2(self) -> str:
        """lyrics (`2hvaqxk2` on the wire)."""
        return self.fields["2hvaqxk2"]

    @_2hvaqxk2.setter
    def _2hvaqxk2(self, value: str) -> None:
        self.edit("2hvaqxk2", value=value)
    @property
    def _4hjvzjcl(self) -> str:
        """lyrics_state (`4hjvzjcl` on the wire)."""
        return self.fields["4hjvzjcl"]

    @_4hjvzjcl.setter
    def _4hjvzjcl(self, value: str) -> None:
        self.edit("4hjvzjcl", value=value)
    wt3jlodv: bool  # liked

    def play_song(self) -> None:
        """Plays one song, alone: what a row's press does. The place is the shared session's own record, so the song continues on this device or another."""
        return self.action("play_song")

    def play_rec(self) -> None:
        """Plays a suggested song."""
        return self.action("play_rec")

    def like(self,
        *,
        liked: bool | None = None,
    ) -> None:
        """Likes a song, or takes the like back. Where the song is Spotify's and the person's Spotify is connected, their Spotify library keeps it too, and what Spotify says is what the song shows."""
        args: dict[str, Any] = {}
        if liked is not None:
            args["liked"] = liked
        return self.action("like", **args)

    def keep_offline(self) -> None:
        """Keeps a song on this device, for playing with no network."""
        return self.action("keep_offline")

    def remove_offline(self) -> None:
        """Takes the kept copy away; the song still plays from the catalogue."""
        return self.action("remove_offline")

    def remove_offline_many(self, items: list[RemoveOfflineArgs]) -> list[dict[str, Any]]:
        """Runs `remove_offline` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_offline", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"play_song", "play_rec", "like", "keep_offline", "remove_offline", "remove_offline_many"})


class Album(Resource):
    """Album — `tl.so.tilo.music.Album`."""
    __surface__ = "so.tilo.music@7417b198334861f3"
    xlmick3d: str  # key
    ui7q6nc2: str  # title
    c36cztfw: str  # artist
    @property
    def _2px6jkdl(self) -> str:
        """artist_key (`2px6jkdl` on the wire)."""
        return self.fields["2px6jkdl"]

    @_2px6jkdl.setter
    def _2px6jkdl(self, value: str) -> None:
        self.edit("2px6jkdl", value=value)
    aecqneo6: int  # year
    aajvirg5: str  # release_kind
    lzsbpa3s: str  # cover
    hsjkx4hy: str  # found_for
    @property
    def _6ygyv2n2(self) -> bool:
        """in_library (`6ygyv2n2` on the wire)."""
        return self.fields["6ygyv2n2"]

    @_6ygyv2n2.setter
    def _6ygyv2n2(self, value: bool) -> None:
        self.edit("6ygyv2n2", value=value)

    def play_album(self) -> None:
        """Plays an album's songs from the top."""
        return self.action("play_album")

    def play_album_many(self, items: list[PlayAlbumArgs]) -> list[dict[str, Any]]:
        """Runs `play_album` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("play_album", items)

    def save_album(self,
        *,
        keep: bool | None = None,
    ) -> None:
        """Keeps an album in the library, or takes it out: at Spotify too, when it is a Spotify album and the person's Spotify is connected."""
        args: dict[str, Any] = {}
        if keep is not None:
            args["keep"] = keep
        return self.action("save_album", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"play_album", "play_album_many", "save_album"})


class Artist(Resource):
    """Artist — `tl.so.tilo.music.Artist`."""
    __surface__ = "so.tilo.music@7417b198334861f3"
    a3jwtsne: str  # key
    aht5vji7: str  # name
    p7suhaoo: int  # subscribers
    xmevyzrv: str  # avatar
    aazaiqyu: str  # found_for
    wkrm32am: bool  # followed
    nb4ehhax: list[str]  # genres

    def follow_artist(self,
        *,
        followed: bool | None = None,
    ) -> None:
        """Follows an artist, or stops: at Spotify too, when they are a Spotify artist and the person's Spotify is connected."""
        args: dict[str, Any] = {}
        if followed is not None:
            args["followed"] = followed
        return self.action("follow_artist", **args)

    def refresh_artist(self) -> None:
        """Asks the catalogue for an artist's page."""
        return self.action("refresh_artist")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"follow_artist", "refresh_artist"})


class Playlist(Resource):
    """Playlist — `tl.so.tilo.music.Playlist`."""
    __surface__ = "so.tilo.music@7417b198334861f3"
    zliedtfz: str  # key
    rtkxqj2p: str  # title
    htibtwee: str  # subtitle
    @property
    def _4rfi4yqu(self) -> int:
        """track_count (`4rfi4yqu` on the wire)."""
        return self.fields["4rfi4yqu"]

    @_4rfi4yqu.setter
    def _4rfi4yqu(self, value: int) -> None:
        self.edit("4rfi4yqu", value=value)
    qdbwyy7o: str  # cover
    ac7ep6rb: str  # found_for
    qu3zxatt: str  # description

    def open_playlist(self) -> None:
        """Asks the catalogue for a playlist's songs."""
        return self.action("open_playlist")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"open_playlist"})


class SpotifyDevice(Resource):
    """SpotifyDevice — `tl.so.tilo.music.SpotifyDevice`."""
    __surface__ = "so.tilo.music@7417b198334861f3"
    @property
    def _4ruem7bk(self) -> str:
        """key (`4ruem7bk` on the wire)."""
        return self.fields["4ruem7bk"]

    @_4ruem7bk.setter
    def _4ruem7bk(self, value: str) -> None:
        self.edit("4ruem7bk", value=value)
    fysa7usi: str  # name
    ajgkhxtc: str  # kind
    njju5rcv: bool  # active
    daqeango: int  # volume


class SpotifyQueueItem(Resource):
    """SpotifyQueueItem — `tl.so.tilo.music.SpotifyQueueItem`."""
    __surface__ = "so.tilo.music@7417b198334861f3"
    r4rop6xj: str  # key
    teom6gyk: str  # title
    b4nh57k6: str  # artist
    cjgrr3sx: str  # album
    edgjqah3: str  # cover
    gbooyryg: int  # seconds


class Section(Resource):
    """Section — `tl.so.tilo.music.Section`."""
    __surface__ = "so.tilo.music@7417b198334861f3"
    jsrmddga: str  # key
    @property
    def _2kqqdved(self) -> str:
        """title (`2kqqdved` on the wire)."""
        return self.fields["2kqqdved"]

    @_2kqqdved.setter
    def _2kqqdved(self, value: str) -> None:
        self.edit("2kqqdved", value=value)
    lp6qsyhj: int  # order


class Suggestion(Resource):
    """Suggestion — `tl.so.tilo.music.Suggestion`."""
    __surface__ = "so.tilo.music@7417b198334861f3"
    qh34h2ch: str  # key
    xh6birha: str  # section
    a5bkjkyq: str  # kind
    gqxd4uhg: str  # target
    @property
    def _5a3o3zds(self) -> str:
        """title (`5a3o3zds` on the wire)."""
        return self.fields["5a3o3zds"]

    @_5a3o3zds.setter
    def _5a3o3zds(self, value: str) -> None:
        self.edit("5a3o3zds", value=value)
    oeqw4xpb: str  # subtitle
    @property
    def _5gpg6lqq(self) -> str:
        """cover (`5gpg6lqq` on the wire)."""
        return self.fields["5gpg6lqq"]

    @_5gpg6lqq.setter
    def _5gpg6lqq(self, value: str) -> None:
        self.edit("5gpg6lqq", value=value)


class Mixtape(Resource):
    """Mixtape — `tl.so.tilo.music.Mixtape`."""
    __surface__ = "so.tilo.music@7417b198334861f3"
    @property
    def _5d2oxk4b(self) -> str:
        """name (`5d2oxk4b` on the wire)."""
        return self.fields["5d2oxk4b"]

    @_5d2oxk4b.setter
    def _5d2oxk4b(self, value: str) -> None:
        self.edit("5d2oxk4b", value=value)
    dombkrij: list[Entry[Any]]  # tracks
    jonjr2zv: str  # cover
    ltrfuiaz: str  # description
    xp3dtewi: str  # spotify

    def play_mixtape(self) -> None:
        """Plays a mixtape's songs in the person's order."""
        return self.action("play_mixtape")

    def play_mixtape_many(self, items: list[PlayMixtapeArgs]) -> list[dict[str, Any]]:
        """Runs `play_mixtape` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("play_mixtape", items)

    def mixtape_add(self, *, t: Track) -> None:
        """Adds a song to a mixtape, and to the Spotify playlist it is."""
        return self.action("mixtape_add", t=t)

    def mixtape_remove(self, *, t: Track) -> None:
        """Takes a song out of a mixtape, and out of the Spotify playlist it is."""
        return self.action("mixtape_remove", t=t)

    def rename_mixtape(self, *, name: str) -> None:
        """Gives a playlist of the person's another name, at Spotify too when it is a Spotify one."""
        return self.action("rename_mixtape", name=name)

    def describe_mixtape(self, *, description: str) -> None:
        """Sets what a playlist says about itself, at Spotify too when it is a Spotify one."""
        return self.action("describe_mixtape", description=description)

    def send_playlist(self) -> None:
        """Sends a playlist's name and description to Spotify, which is the copy its person reads on their other devices; a playlist that is not Spotify's says nothing."""
        return self.action("send_playlist")

    def pick_cover(self, *, file: Any) -> None:
        """Gives a mixtape the cover the person picks."""
        return self.action("pick_cover", file=file)

    def pick_cover_many(self, items: list[PickCoverArgs]) -> list[dict[str, Any]]:
        """Runs `pick_cover` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("pick_cover", items)

    def set_cover(self, *, file: Any) -> None:
        """Sets a mixtape's cover to the picture the person chose."""
        return self.action("set_cover", file=file)

    def set_cover_many(self, items: list[SetCoverArgs]) -> list[dict[str, Any]]:
        """Runs `set_cover` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_cover", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"play_mixtape", "play_mixtape_many", "mixtape_add", "mixtape_remove", "rename_mixtape", "describe_mixtape", "send_playlist", "pick_cover", "pick_cover_many", "set_cover", "set_cover_many"})


class Taste(Resource):
    """Taste — `tl.so.tilo.music.Taste`."""
    __surface__ = "so.tilo.music@7417b198334861f3"
    nhxvxvrt: bool  # followed_artists
    fygjzxca: bool  # liked_songs
    splmmoir: bool  # saved_albums
    myoyyape: bool  # similar_artists
    gfhj3dh4: list[str]  # dismissed
    oa6ryv4n: list[str]  # preferred
    nsvv7dnn: list[str]  # hidden


class Library(Resource):
    """Library — `tl.so.tilo.music.Library`."""
    __surface__ = "so.tilo.music@7417b198334861f3"
    hkr3ia6d: str  # name
    cp5uymkx: list[Entry[Track]]  # tracks
    phkx3ocv: list[Entry[Album]]  # albums
    oarbmgph: list[Entry[Artist]]  # artists
    bfjw6uca: list[Entry[Playlist]]  # playlists
    k6usb4n2: list[Entry[Suggestion]]  # suggestions
    qu6nxpq3: list[Entry[Section]]  # sections
    bfayjji6: list[Entry[Mixtape]]  # mixtapes
    u4kuzao5: Taste  # taste
    scejauac: Any | None  # playing
    tnkopk2m: list[Entry[Any]]  # sessions
    cjoykprs: list[Entry[SpotifyDevice]]  # devices
    hiupwos3: list[Entry[SpotifyQueueItem]]  # spotify_queue
    d6agrwhu: str  # spotify_state
    wzbxwvai: str  # spotify_account
    q6fvzfwi: str  # spotify_product
    bnnshdna: str  # spotify_trouble
    rwf7lfs6: str  # connect_url
    gghnswba: str  # spotify_active
    sdkihe6k: str  # spotify_now
    @property
    def _22kwzmah(self) -> bool:
        """spotify_playing (`22kwzmah` on the wire)."""
        return self.fields["22kwzmah"]

    @_22kwzmah.setter
    def _22kwzmah(self, value: bool) -> None:
        self.edit("22kwzmah", value=value)
    ekk67ewc: int  # spotify_next_tracks
    gwbm6xq4: int  # spotify_next_albums
    q6w6ykgo: int  # spotify_next_playlists
    hbxo67cl: int  # spotify_next_artists
    wout6rgv: str  # catalogue
    olobgipa: str  # search_state
    foqrtqa3: str  # search_trouble
    w3c7fplo: str  # home_state
    gmizjqwj: str  # home_trouble
    vnufl2py: str  # trouble
    jelhifn2: str  # player_trouble
    bw27kdqh: float  # player_at
    x6v7yq2x: str  # player_retried
    fkvdcomi: bool  # online

    def session(self) -> Any:
        """The session this app plays through: the one it has, or one made now. It is kept both in `playing`, which a view reads, and in `sessions`, so the record is a resource of its own."""
        return self.action("session")

    def open_audio_file(self, *, file: Any) -> Track:
        """Opens an audio file: the file a person picked, or one Files handed over, and plays it. The song it already made opens instead of a second copy, so its place in history stays one."""
        return self.action("open_audio_file", file=file)

    def follow_stream(self, *, p: Any) -> None:
        """Keeps the session's source the song's own address, so the live card and another device play the one the catalogue last answered with."""
        return self.action("follow_stream", p=p)

    def follow_stream_many(self, items: list[FollowStreamArgs]) -> list[dict[str, Any]]:
        """Runs `follow_stream` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("follow_stream", items)

    def open_link(self, *, link: str) -> Any:
        """Opens a `music://` link: the song it names plays, and the album, artist or playlist it names opens on its page. A shell hands a link here whether or not a window of Music is open, and what this returns opens in the window; a link this library holds nothing for shows the library."""
        return self.action("open_link", link=link)

    def add_to_queue(self, *, t: Track) -> None:
        """Adds a song to what follows."""
        return self.action("add_to_queue", t=t)

    def add_to_queue_many(self, items: list[AddToQueueArgs]) -> list[dict[str, Any]]:
        """Runs `add_to_queue` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_to_queue", items)

    def play_next(self, *, t: Track) -> None:
        """Queues a song right after what plays."""
        return self.action("play_next", t=t)

    def play_next_many(self, items: list[PlayNextArgs]) -> list[dict[str, Any]]:
        """Runs `play_next` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("play_next", items)

    def lyrics_now(self) -> None:
        """Asks for the words of what plays, keeping the session it asks for."""
        return self.action("lyrics_now")

    def hide_rec(self, *, t: Track) -> None:
        """Hides a song from the suggestions. History can undo it, and the taste page brings it back."""
        return self.action("hide_rec", t=t)

    def hide_rec_many(self, items: list[HideRecArgs]) -> list[dict[str, Any]]:
        """Runs `hide_rec` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("hide_rec", items)

    def restore_rec(self, *, t: Track) -> None:
        """Brings a hidden song back into the suggestions."""
        return self.action("restore_rec", t=t)

    def restore_rec_many(self, items: list[RestoreRecArgs]) -> list[dict[str, Any]]:
        """Runs `restore_rec` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("restore_rec", items)

    def search(self, *, query: str) -> None:
        """Asks the catalogue for the songs, albums, artists and playlists a query finds."""
        return self.action("search", query=query)

    # the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
    def open_suggestion(self, *, row: Suggestion) -> None:
        """Does what a suggestion on the home page names."""
        return self.action("open_suggestion", row=row)

    def lyrics_of(self, *, p: Any) -> None:
        """Asks for what plays to be sung, into the song's own record."""
        return self.action("lyrics_of", p=p)

    def note_failure(self,
        *,
        reason: str,
        at: float,
    ) -> None:
        """Says why the player stopped, and where, so the notice names the place and continues there. A stream that stopped partway is first asked for again by itself, once: a source that drops a connection or an address that expired plays on without the person doing anything."""
        return self.action("note_failure", reason=reason, at=at)

    def retry_stream(self) -> None:
        """Asks the catalogue for the song's address again and plays it on from where it stopped: a stream its source stopped serving partway through, or one whose signed address expired, continues from that place rather than beginning again."""
        return self.action("retry_stream")

    def new_mixtape(self, *, name: str) -> None:
        """Makes a playlist of the person's own: at Spotify too, when it is their catalogue and their Spotify is connected, so the playlist plays there as it does here."""
        return self.action("new_mixtape", name=name)

    def set_catalogue(self, *, which: str) -> None:
        """Chooses which catalogue the app asks."""
        return self.action("set_catalogue", which=which)

    def connect_spotify(self) -> None:
        """Connects the person's Spotify: the page they sign in on is shown in the Spotify section."""
        return self.action("connect_spotify")

    def finish_spotify(self) -> None:
        """Takes the sign-in the person gave on Spotify's page."""
        return self.action("finish_spotify")

    def disconnect_spotify(self) -> None:
        """Forgets this device's Spotify sign-in."""
        return self.action("disconnect_spotify")

    def spotify_devices(self) -> None:
        """Reads the places Spotify plays on."""
        return self.action("spotify_devices")

    def transfer_playback(self, *, d: SpotifyDevice) -> None:
        """Moves what plays to `d`, from the beginning."""
        return self.action("transfer_playback", d=d)

    def remote_play(self,
        *,
        uri: str | None = None,
    ) -> None:
        """Plays `uri` on Spotify, where it plays: empty plays what was playing."""
        args: dict[str, Any] = {}
        if uri is not None:
            args["uri"] = uri
        return self.action("remote_play", **args)

    def remote_pause(self) -> None:
        """Pauses what plays on Spotify."""
        return self.action("remote_pause")

    def remote_next(self) -> None:
        """Plays what follows on Spotify."""
        return self.action("remote_next")

    def remote_previous(self) -> None:
        """Plays what played before on Spotify."""
        return self.action("remote_previous")

    def spotify_now(self) -> None:
        """Reads what plays on Spotify now."""
        return self.action("spotify_now")

    def spotify_queue(self) -> None:
        """Reads what plays next on Spotify: its own queue, which is shown here and never edited here."""
        return self.action("spotify_queue")

    def queue_on_spotify(self, *, t: Track) -> None:
        """Puts a song after what plays on Spotify."""
        return self.action("queue_on_spotify", t=t)

    def spotify_more(self, *, kind: str) -> None:
        """Asks Spotify for one more page of what the person keeps there."""
        return self.action("spotify_more", kind=kind)

    def update_taste(self,
        *,
        which: str,
        on: bool,
    ) -> None:
        """Turns one of the recommendations' switches."""
        return self.action("update_taste", which=which, on=on)

    def update_taste_many(self, items: list[UpdateTasteArgs]) -> list[dict[str, Any]]:
        """Runs `update_taste` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("update_taste", items)

    def dismiss_artist(self, *, a: Artist) -> None:
        """Leaves an artist out of the recommendations, or brings them back."""
        return self.action("dismiss_artist", a=a)

    def dismiss_artist_many(self, items: list[DismissArtistArgs]) -> list[dict[str, Any]]:
        """Runs `dismiss_artist` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("dismiss_artist", items)

    def prefer_artist(self, *, a: Artist) -> None:
        """Asks for more of an artist."""
        return self.action("prefer_artist", a=a)

    def prefer_artist_many(self, items: list[PreferArtistArgs]) -> list[dict[str, Any]]:
        """Runs `prefer_artist` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("prefer_artist", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"session", "open_audio_file", "follow_stream", "follow_stream_many", "open_link", "add_to_queue", "add_to_queue_many", "play_next", "play_next_many", "lyrics_now", "hide_rec", "hide_rec_many", "restore_rec", "restore_rec_many", "search", "open_suggestion", "lyrics_of", "note_failure", "retry_stream", "new_mixtape", "set_catalogue", "connect_spotify", "finish_spotify", "disconnect_spotify", "spotify_devices", "transfer_playback", "remote_play", "remote_pause", "remote_next", "remote_previous", "spotify_now", "spotify_queue", "queue_on_spotify", "spotify_more", "update_taste", "update_taste_many", "dismiss_artist", "dismiss_artist_many", "prefer_artist", "prefer_artist_many"})


class Music(Resource):
    """Music — `tl.so.tilo.music`."""
    __surface__ = "so.tilo.music@7417b198334861f3"
    def queue_moved(self,
        *,
        p: Any,
        ids: list[str],
    ) -> None:
        """Moves the dragged row where it was dropped: the two ids its reorder carries."""
        return self.action("queue_moved", p=p, ids=ids)

    def queue_moved_many(self, items: list[QueueMovedArgs]) -> list[dict[str, Any]]:
        """Runs `queue_moved` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("queue_moved", items)

    def like_playing(self,
        *,
        p: Any,
        liked: bool | None = None,
    ) -> None:
        """Likes what plays."""
        args: dict[str, Any] = {}
        args["p"] = p
        if liked is not None:
            args["liked"] = liked
        return self.action("like_playing", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"queue_moved", "queue_moved_many", "like_playing"})


