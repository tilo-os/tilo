"""Types for YouTube (`so.tilo.tube`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Channel` is the app's `tl.so.tilo.tube.Channel` at `tube/<resource>`, read with
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
    "Channel",
    "Video",
    "Playlist",
    "Subscription",
    "QueuedVideo",
    "PlaybackPrefs",
    "WatchEntry",
    "YouTube",
    "Tube",
    "SubscribeArgs",
    "ToggleSubscriptionArgs",
    "LikeArgs",
    "WatchLaterArgs",
    "ForgetArgs",
    "DeleteHistoryArgs",
    "SetAutoplayArgs",
    "SetSpeedArgs",
    "SetQualityArgs",
    "SetCaptionsArgs",
    "EndSearchArgs",
]

SubscribeArgs = TypedDict("SubscribeArgs", {
    "subscribed": NotRequired["bool | None"],
})
"""The arguments of `subscribe`."""

ToggleSubscriptionArgs = TypedDict("ToggleSubscriptionArgs", {
})
"""The arguments of `toggle_subscription`."""

LikeArgs = TypedDict("LikeArgs", {
    "liked": NotRequired["bool | None"],
})
"""The arguments of `like`."""

WatchLaterArgs = TypedDict("WatchLaterArgs", {
    "saved": NotRequired["bool | None"],
})
"""The arguments of `watch_later`."""

ForgetArgs = TypedDict("ForgetArgs", {
})
"""The arguments of `forget`."""

DeleteHistoryArgs = TypedDict("DeleteHistoryArgs", {
})
"""The arguments of `delete_history`."""

SetAutoplayArgs = TypedDict("SetAutoplayArgs", {
    "on": "bool",
})
"""The arguments of `set_autoplay`."""

SetSpeedArgs = TypedDict("SetSpeedArgs", {
    "speed": "float",
})
"""The arguments of `set_speed`."""

SetQualityArgs = TypedDict("SetQualityArgs", {
    "quality": "str",
})
"""The arguments of `set_quality`."""

SetCaptionsArgs = TypedDict("SetCaptionsArgs", {
    "on": "bool",
})
"""The arguments of `set_captions`."""

EndSearchArgs = TypedDict("EndSearchArgs", {
})
"""The arguments of `end_search`."""

class Channel(Resource):
    """Channel — `tl.so.tilo.tube.Channel`."""
    __surface__ = "so.tilo.tube@a86ce7b3c95bf2b1"
    @property
    def _26ctwf34(self) -> str:
        """key (`26ctwf34` on the wire)."""
        return self.fields["26ctwf34"]

    @_26ctwf34.setter
    def _26ctwf34(self, value: str) -> None:
        self.edit("26ctwf34", value=value)
    ya2u4cw6: str  # name
    woyix7h2: str  # handle
    ah5nkqv7: int  # subscribers
    @property
    def _2qxzpxur(self) -> str:
        """avatar (`2qxzpxur` on the wire)."""
        return self.fields["2qxzpxur"]

    @_2qxzpxur.setter
    def _2qxzpxur(self, value: str) -> None:
        self.edit("2qxzpxur", value=value)
    d4mzmvrr: str  # banner
    rzhtvjsv: str  # description
    lfopp7e7: bool  # verified
    @property
    def _2ap24kjm(self) -> str:
        """found_for (`2ap24kjm` on the wire)."""
        return self.fields["2ap24kjm"]

    @_2ap24kjm.setter
    def _2ap24kjm(self, value: str) -> None:
        self.edit("2ap24kjm", value=value)

    def subscribe(self,
        *,
        subscribed: bool | None = None,
    ) -> None:
        """Subscribes to the channel, or unsubscribes: the button says which it will do."""
        args: dict[str, Any] = {}
        if subscribed is not None:
            args["subscribed"] = subscribed
        return self.action("subscribe", **args)

    def subscribe_many(self, items: list[SubscribeArgs]) -> list[dict[str, Any]]:
        """Runs `subscribe` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("subscribe", items)

    def toggle_subscription(self) -> None:
        """Subscribes to the channel, or takes the subscription back."""
        return self.action("toggle_subscription")

    def toggle_subscription_many(self, items: list[ToggleSubscriptionArgs]) -> list[dict[str, Any]]:
        """Runs `toggle_subscription` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("toggle_subscription", items)

    def refresh_channel(self) -> None:
        """Asks the service for the channel's videos."""
        return self.action("refresh_channel")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"subscribe", "subscribe_many", "toggle_subscription", "toggle_subscription_many", "refresh_channel"})


class Video(Resource):
    """Video — `tl.so.tilo.tube.Video`."""
    __surface__ = "so.tilo.tube@a86ce7b3c95bf2b1"
    irzy3c7m: str  # key
    fpxk5w7b: str  # title
    mhkply6c: str  # channel_key
    @property
    def _2mklwbfq(self) -> str:
        """channel_name (`2mklwbfq` on the wire)."""
        return self.fields["2mklwbfq"]

    @_2mklwbfq.setter
    def _2mklwbfq(self, value: str) -> None:
        self.edit("2mklwbfq", value=value)
    lfky4erv: str  # channel_avatar
    txtos5p6: int  # views
    @property
    def _2ruyhh5f(self) -> str:
        """published (`2ruyhh5f` on the wire)."""
        return self.fields["2ruyhh5f"]

    @_2ruyhh5f.setter
    def _2ruyhh5f(self, value: str) -> None:
        self.edit("2ruyhh5f", value=value)
    mvlf65b5: int  # duration
    pfdoae2w: str  # description
    ddfiordt: int  # likes
    l25oorez: str  # thumbnail
    pkbzjs7l: str  # stream
    jfam7tjf: bool  # live
    bifohbpv: bool  # liked
    @property
    def _4mojz3hw(self) -> bool:
        """trending (`4mojz3hw` on the wire)."""
        return self.fields["4mojz3hw"]

    @_4mojz3hw.setter
    def _4mojz3hw(self, value: bool) -> None:
        self.edit("4mojz3hw", value=value)
    @property
    def _5o5fpqgh(self) -> str:
        """found_for (`5o5fpqgh` on the wire)."""
        return self.fields["5o5fpqgh"]

    @_5o5fpqgh.setter
    def _5o5fpqgh(self, value: str) -> None:
        self.edit("5o5fpqgh", value=value)

    def like(self,
        *,
        liked: bool | None = None,
    ) -> None:
        """Likes the video, or takes the like back."""
        args: dict[str, Any] = {}
        if liked is not None:
            args["liked"] = liked
        return self.action("like", **args)

    def like_many(self, items: list[LikeArgs]) -> list[dict[str, Any]]:
        """Runs `like` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("like", items)

    def watch_later(self,
        *,
        saved: bool | None = None,
    ) -> None:
        """Puts the video on the workspace's watch queue, or takes it off. A video put on it goes after the ones already waiting, so the queue plays in the order people added to it."""
        args: dict[str, Any] = {}
        if saved is not None:
            args["saved"] = saved
        return self.action("watch_later", **args)

    def watch_later_many(self, items: list[WatchLaterArgs]) -> list[dict[str, Any]]:
        """Runs `watch_later` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("watch_later", items)

    def play(self) -> None:
        """Plays the video, whether or not it was played before."""
        return self.action("play")

    def resume(self) -> None:
        """Plays the video from where the person left off, when they did not finish it."""
        return self.action("resume")

    def progress(self, *, at: float) -> None:
        """Records how far into the video the person is, and that they watched it now: it moves to the front of the history, and asks the service for the address its bytes play from when that is not known yet. Starting a video, resuming it and moving the playhead as it plays are one action, so a video has one place it is played from."""
        return self.action("progress", at=at)

    def forget(self) -> None:
        """Takes one video out of the person's own history."""
        return self.action("forget")

    def forget_many(self, items: list[ForgetArgs]) -> list[dict[str, Any]]:
        """Runs `forget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget", items)

    def retry_playback(self) -> None:
        """Asks the service for where the video's bytes play from again, after the player could not show them: the address the client holds is the service's alone to write, and the answer it gives now is what the player opens."""
        return self.action("retry_playback")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"like", "like_many", "watch_later", "watch_later_many", "play", "resume", "progress", "forget", "forget_many", "retry_playback"})


class Playlist(Resource):
    """Playlist — `tl.so.tilo.tube.Playlist`."""
    __surface__ = "so.tilo.tube@a86ce7b3c95bf2b1"
    ow7glfos: str  # key
    ljyc7mx2: str  # title
    bzfmd6pg: str  # channel_key
    zvfpfva3: str  # channel_name
    @property
    def _6cllrl7x(self) -> int:
        """video_count (`6cllrl7x` on the wire)."""
        return self.fields["6cllrl7x"]

    @_6cllrl7x.setter
    def _6cllrl7x(self, value: int) -> None:
        self.edit("6cllrl7x", value=value)
    @property
    def _4xvzshd7(self) -> str:
        """thumbnail (`4xvzshd7` on the wire)."""
        return self.fields["4xvzshd7"]

    @_4xvzshd7.setter
    def _4xvzshd7(self, value: str) -> None:
        self.edit("4xvzshd7", value=value)
    dge65utd: list[str]  # video_keys
    p3yw6p2h: str  # found_for

    def refresh_playlist(self) -> None:
        """Asks the service for what the playlist holds, in its maker's order."""
        return self.action("refresh_playlist")

    def play_playlist(self) -> None:
        """Plays the playlist from its first video."""
        return self.action("play_playlist")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"refresh_playlist", "play_playlist"})


class Subscription(Resource):
    """Subscription — `tl.so.tilo.tube.Subscription`."""
    __surface__ = "so.tilo.tube@a86ce7b3c95bf2b1"
    slhmwv67: str  # key
    ynf67l6e: str  # name


class QueuedVideo(Resource):
    """QueuedVideo — `tl.so.tilo.tube.QueuedVideo`."""
    __surface__ = "so.tilo.tube@a86ce7b3c95bf2b1"
    o3xolmld: str  # key


class PlaybackPrefs(Resource):
    """PlaybackPrefs — `tl.so.tilo.tube.PlaybackPrefs`."""
    __surface__ = "so.tilo.tube@a86ce7b3c95bf2b1"
    @property
    def _5twttwwn(self) -> str:
        """member (`5twttwwn` on the wire)."""
        return self.fields["5twttwwn"]

    @_5twttwwn.setter
    def _5twttwwn(self, value: str) -> None:
        self.edit("5twttwwn", value=value)
    vze2yftu: bool  # autoplay
    @property
    def _5bpxm33f(self) -> float:
        """speed (`5bpxm33f` on the wire)."""
        return self.fields["5bpxm33f"]

    @_5bpxm33f.setter
    def _5bpxm33f(self, value: float) -> None:
        self.edit("5bpxm33f", value=value)
    lesyml4z: str  # quality
    u3fgpv6e: bool  # captions


class WatchEntry(Resource):
    """WatchEntry — `tl.so.tilo.tube.WatchEntry`."""
    __surface__ = "so.tilo.tube@a86ce7b3c95bf2b1"
    noppuxpb: str  # key
    stuii7l7: str  # title
    j37y2bbw: str  # channel_name
    @property
    def _7mtvj3xd(self) -> str | None:
        """member (`7mtvj3xd` on the wire)."""
        return self.fields["7mtvj3xd"]

    @_7mtvj3xd.setter
    def _7mtvj3xd(self, value: str | None) -> None:
        self.edit("7mtvj3xd", value=value)
    dmm6f63t: str  # watched
    omff5ciz: float  # progress


class YouTube(Resource):
    """YouTube — `tl.so.tilo.tube.YouTube`."""
    __surface__ = "so.tilo.tube@a86ce7b3c95bf2b1"
    olrung72: str  # title
    ihhzrm3f: list[Entry[Channel]]  # channels
    qnfjkti5: list[Entry[Video]]  # videos
    v5sh2bpr: list[Entry[Playlist]]  # playlists
    iputtmua: list[Entry[Subscription]]  # subscriptions
    jgjqgesf: list[Entry[QueuedVideo]]  # queue
    hv25j74h: list[Entry[WatchEntry]]  # history
    epgpppzy: list[Entry[PlaybackPrefs]]  # playback
    bqesicxq: str  # search_text
    hcmy6dp7: str  # search_state
    zjw3anaz: str  # search_trouble
    nnvco6w3: str  # feed_state
    r3ksw6bn: str  # trouble
    qv7t2mza: bool  # online

    def play_queue(self) -> None:
        """Plays the queue from its oldest video: what was put on it first plays first."""
        return self.action("play_queue")

    def delete_history(self) -> None:
        """Deletes the person's own watch history, and the entries carried from before it recorded members, which this history shows them; nobody else's."""
        return self.action("delete_history")

    def delete_history_many(self, items: list[DeleteHistoryArgs]) -> list[dict[str, Any]]:
        """Runs `delete_history` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_history", items)

    def set_autoplay(self, *, on: bool) -> None:
        """Each of these keeps one of the person's own playback preferences: the record is theirs, made the first time they set one, and every other member reads their own."""
        return self.action("set_autoplay", on=on)

    def set_autoplay_many(self, items: list[SetAutoplayArgs]) -> list[dict[str, Any]]:
        """Runs `set_autoplay` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_autoplay", items)

    def set_speed(self, *, speed: float) -> None:
        """How fast videos play for the person: 0.25 to 3, 1 for the usual speed."""
        return self.action("set_speed", speed=speed)

    def set_speed_many(self, items: list[SetSpeedArgs]) -> list[dict[str, Any]]:
        """Runs `set_speed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_speed", items)

    def set_quality(self, *, quality: str) -> None:
        """The tallest picture the person wants, as YouTube names it; `auto` takes the best that fits."""
        return self.action("set_quality", quality=quality)

    def set_quality_many(self, items: list[SetQualityArgs]) -> list[dict[str, Any]]:
        """Runs `set_quality` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_quality", items)

    def set_captions(self, *, on: bool) -> None:
        """Whether videos show their captions for the person."""
        return self.action("set_captions", on=on)

    def set_captions_many(self, items: list[SetCaptionsArgs]) -> list[dict[str, Any]]:
        """Runs `set_captions` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_captions", items)

    def search(self,
        *,
        query: str | None = None,
    ) -> None:
        """Searches YouTube: the service answers with channels, playlists and videos, written where the search view lists them. The answer arrives through the platform's network, never the app's."""
        args: dict[str, Any] = {}
        if query is not None:
            args["query"] = query
        return self.action("search", **args)

    def end_search(self) -> None:
        """Leaves the search the feed is showing for the place the person chose: the results stay where the service wrote them (found again by the query that found them), and the client shows the place chosen."""
        return self.action("end_search")

    def end_search_many(self, items: list[EndSearchArgs]) -> list[dict[str, Any]]:
        """Runs `end_search` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("end_search", items)

    def refresh_feed(self) -> None:
        """Asks the service for the newest videos of each subscribed channel: what the subscriptions feed shows."""
        return self.action("refresh_feed")

    def load_trending(self) -> None:
        """Asks the service for the videos it lists as trending."""
        return self.action("load_trending")

    def open_link(self, *, link: str) -> Any:
        """Opens a link of the scheme this client claims: the video (`tube:watch?v=<key>`), channel (`tube:channel?key=<key>`) or playlist (`tube:playlist?key=<key>`) it names, or the search it asks for (`tube:search?q=<query>`). A shell hands a link here whether or not a window of the client is open, and what this returns opens in the window. The host publishes it as the system's `open-link` on the client's root, which is what a shell hands a link to."""
        return self.action("open_link", link=link)

    def play_again(self, *, h: WatchEntry) -> None:
        """Plays the video a history entry names, from where the person left off."""
        return self.action("play_again", h=h)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"play_queue", "delete_history", "delete_history_many", "set_autoplay", "set_autoplay_many", "set_speed", "set_speed_many", "set_quality", "set_quality_many", "set_captions", "set_captions_many", "search", "end_search", "end_search_many", "refresh_feed", "load_trending", "open_link", "play_again"})


class Tube(Resource):
    """YouTube — `tl.so.tilo.tube`."""
    __surface__ = "so.tilo.tube@a86ce7b3c95bf2b1"

