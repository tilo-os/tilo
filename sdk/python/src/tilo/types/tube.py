"""Types for YouTube (`so.tilo.tube`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `tube/<resource>`, read with
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
    "Channel",
    "Video",
    "Playlist",
    "Comment",
    "Segment",
    "Reply",
    "Subscription",
    "QueuedVideo",
    "PlaybackPrefs",
    "WatchEntry",
    "PairedDevice",
    "GrayjayPairAsk",
    "GrayjaySyncAsk",
    "GrayjayPeer",
    "GrayjaySubscription",
    "GrayjayJob",
    "GrayjayOutcome",
    "GrayjayRead",
    "YouTube",
    "Tube",
    "SubscribeArgs",
    "ToggleSubscriptionArgs",
    "LikeArgs",
    "WatchLaterArgs",
    "ForgetArgs",
    "RetractReplyArgs",
    "PlayItemsArgs",
    "DeleteHistoryArgs",
    "SetAutoplayArgs",
    "SetQualityArgs",
    "SetSponsorblockArgs",
    "SetDearrowArgs",
    "SetDislikesArgs",
    "SetCommentsArgs",
    "SetSpeedArgs",
    "PairDeviceArgs",
    "ApproveDeviceArgs",
    "RevokeDeviceArgs",
    "SyncDevicesArgs",
    "AutoSyncArgs",
    "GrayjayEndedArgs",
    "KeepPairedArgs",
    "KeepExchangeArgs",
    "FilterSearchArgs",
    "EndSearchArgs",
    "LikeCommentArgs",
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

RetractReplyArgs = TypedDict("RetractReplyArgs", {
})
"""The arguments of `retract_reply`."""

PlayItemsArgs = TypedDict("PlayItemsArgs", {
    "videos": "list[Video]",
    "at": "str",
})
"""The arguments of `play_items`."""

DeleteHistoryArgs = TypedDict("DeleteHistoryArgs", {
})
"""The arguments of `delete_history`."""

SetAutoplayArgs = TypedDict("SetAutoplayArgs", {
    "on": "bool",
})
"""The arguments of `set_autoplay`."""

SetQualityArgs = TypedDict("SetQualityArgs", {
    "quality": "str",
})
"""The arguments of `set_quality`."""

SetSponsorblockArgs = TypedDict("SetSponsorblockArgs", {
    "on": "bool",
})
"""The arguments of `set_sponsorblock`."""

SetDearrowArgs = TypedDict("SetDearrowArgs", {
    "on": "bool",
})
"""The arguments of `set_dearrow`."""

SetDislikesArgs = TypedDict("SetDislikesArgs", {
    "on": "bool",
})
"""The arguments of `set_dislikes`."""

SetCommentsArgs = TypedDict("SetCommentsArgs", {
    "on": "bool",
})
"""The arguments of `set_comments`."""

SetSpeedArgs = TypedDict("SetSpeedArgs", {
    "speed": "float",
})
"""The arguments of `set_speed`."""

PairDeviceArgs = TypedDict("PairDeviceArgs", {
    "invite": "str",
})
"""The arguments of `pair_device`."""

ApproveDeviceArgs = TypedDict("ApproveDeviceArgs", {
    "device": "PairedDevice",
})
"""The arguments of `approve_device`."""

RevokeDeviceArgs = TypedDict("RevokeDeviceArgs", {
    "device": "PairedDevice",
})
"""The arguments of `revoke_device`."""

SyncDevicesArgs = TypedDict("SyncDevicesArgs", {
})
"""The arguments of `sync_devices`."""

AutoSyncArgs = TypedDict("AutoSyncArgs", {
})
"""The arguments of `auto_sync`."""

GrayjayEndedArgs = TypedDict("GrayjayEndedArgs", {
    "j": "Any",
})
"""The arguments of `grayjay_ended`."""

KeepPairedArgs = TypedDict("KeepPairedArgs", {
    "answer": "str",
})
"""The arguments of `keep_paired`."""

KeepExchangeArgs = TypedDict("KeepExchangeArgs", {
    "answer": "str",
})
"""The arguments of `keep_exchange`."""

FilterSearchArgs = TypedDict("FilterSearchArgs", {
    "kind": "str",
})
"""The arguments of `filter_search`."""

EndSearchArgs = TypedDict("EndSearchArgs", {
})
"""The arguments of `end_search`."""

LikeCommentArgs = TypedDict("LikeCommentArgs", {
    "c": "Comment",
    "liked": NotRequired["bool | None"],
})
"""The arguments of `like_comment`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.tube@8213a8f9a9dcf15a"
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


class Channel(Resource):
    """Channel — `tl.so.tilo.tube.Channel`."""
    __surface__ = "so.tilo.tube@8213a8f9a9dcf15a"
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
    __surface__ = "so.tilo.tube@8213a8f9a9dcf15a"
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
    lhbq7qff: int  # dislikes
    l25oorez: str  # thumbnail
    vvxj4w64: str  # dearrow_title
    imamvrnr: str  # dearrow_thumbnail
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
        """Plays the video, whether or not it was played before: the session takes it as its one item, so the player, the live card and the media keys all act on it, and it continues where the person left it."""
        return self.action("play")

    def resume(self) -> None:
        """Plays the video from where the person left off, when they did not finish it."""
        return self.action("resume")

    def play_at_quality(self, *, quality: str) -> None:
        """Plays the video at `quality`, from where it stands: the player's own quality menu, which asks the service for a stream of that picture. The session keeps the item and the place it stood at, so the picture changes without the playhead moving; the address YouTube answers with is the one the player opens next, and `progress` writes it into the session's item."""
        return self.action("play_at_quality", quality=quality)

    def progress(self, *, at: float) -> None:
        """Records how far into the video the person is, and that they watched it now: it moves to the front of the history, and asks the service for the address its bytes play from when that is not known yet. Starting a video, resuming it and moving the playhead as it plays are one action, so a video has one place it is played from."""
        return self.action("progress", at=at)

    def skip_segment(self, *, at: float) -> None:
        """Skips the SponsorBlock segment the video plays into, at `at` (a share of its length), when the person skips segments: the playhead moves to the segment's end, so a sponsor read or an outro is never watched. The player reports where it got to as it plays, so this is what makes a marked segment skip on its own."""
        return self.action("skip_segment", at=at)

    def forget(self) -> None:
        """Takes one video out of the person's own history."""
        return self.action("forget")

    def forget_many(self, items: list[ForgetArgs]) -> list[dict[str, Any]]:
        """Runs `forget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget", items)

    def retry_playback(self, *, at: float) -> None:
        """Asks the service for where the video's bytes play from again, after the player could not show them: the address the client holds is the service's alone to write, and the answer it gives now is what the player opens. The session moves to where the stream stopped, so the fresh address continues there."""
        return self.action("retry_playback", at=at)

    def refresh_comments(self) -> None:
        """Asks the service for the video's comments again, for what people said since the page opened."""
        return self.action("refresh_comments")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"like", "like_many", "watch_later", "watch_later_many", "play", "resume", "play_at_quality", "progress", "skip_segment", "forget", "forget_many", "retry_playback", "refresh_comments"})


class Playlist(Resource):
    """Playlist — `tl.so.tilo.tube.Playlist`."""
    __surface__ = "so.tilo.tube@8213a8f9a9dcf15a"
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
        """Plays the playlist from its first video, and the rest in its maker's order."""
        return self.action("play_playlist")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"refresh_playlist", "play_playlist"})


class Comment(Resource):
    """Comment — `tl.so.tilo.tube.Comment`."""
    __surface__ = "so.tilo.tube@8213a8f9a9dcf15a"
    pv4laajx: str  # key
    li4xiefh: str  # video_key
    @property
    def _3ncdj3up(self) -> str:
        """author (`3ncdj3up` on the wire)."""
        return self.fields["3ncdj3up"]

    @_3ncdj3up.setter
    def _3ncdj3up(self, value: str) -> None:
        self.edit("3ncdj3up", value=value)
    lgtlgzui: str  # avatar
    az6iwuma: str  # text
    hgdwjwgt: int  # likes
    dninsxc6: str  # published
    tmf5hovn: bool  # pinned
    gp3basiz: bool  # hearted
    ha6vpfhz: int  # replies

    def reply_to_comment(self, *, text: str) -> Reply:
        """Replies to a comment: the workspace keeps the reply itself, beside the comment it answers, since YouTube lets no app post as the person. Everyone who reads the comment reads the reply."""
        return self.action("reply_to_comment", text=text)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"reply_to_comment"})


class Segment(Resource):
    """Segment — `tl.so.tilo.tube.Segment`."""
    __surface__ = "so.tilo.tube@8213a8f9a9dcf15a"
    uakovynu: str  # key
    pvlf5o2c: str  # video_key
    qlgpeq35: str  # category
    bbnmvcim: int  # start
    hts3jgqy: int  # end
    j7wuhxg5: int  # votes


class Reply(Resource):
    """Reply — `tl.so.tilo.tube.Reply`."""
    __surface__ = "so.tilo.tube@8213a8f9a9dcf15a"
    y4coaxks: str  # comment
    kwxg57v6: str  # video
    @property
    def _6psi22ua(self) -> str:
        """text (`6psi22ua` on the wire)."""
        return self.fields["6psi22ua"]

    @_6psi22ua.setter
    def _6psi22ua(self, value: str) -> None:
        self.edit("6psi22ua", value=value)
    nzwksffp: str | None  # member
    k3jskmid: str  # said

    def retract_reply(self) -> None:
        """Takes back a reply the person wrote. A reply the native app kept is the workspace's, and any member may take it back."""
        return self.action("retract_reply")

    def retract_reply_many(self, items: list[RetractReplyArgs]) -> list[dict[str, Any]]:
        """Runs `retract_reply` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("retract_reply", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"retract_reply", "retract_reply_many"})


class Subscription(Resource):
    """Subscription — `tl.so.tilo.tube.Subscription`."""
    __surface__ = "so.tilo.tube@8213a8f9a9dcf15a"
    slhmwv67: str  # key
    ynf67l6e: str  # name


class QueuedVideo(Resource):
    """QueuedVideo — `tl.so.tilo.tube.QueuedVideo`."""
    __surface__ = "so.tilo.tube@8213a8f9a9dcf15a"
    o3xolmld: str  # key


class PlaybackPrefs(Resource):
    """PlaybackPrefs — `tl.so.tilo.tube.PlaybackPrefs`."""
    __surface__ = "so.tilo.tube@8213a8f9a9dcf15a"
    @property
    def _5twttwwn(self) -> str:
        """member (`5twttwwn` on the wire)."""
        return self.fields["5twttwwn"]

    @_5twttwwn.setter
    def _5twttwwn(self, value: str) -> None:
        self.edit("5twttwwn", value=value)
    vze2yftu: bool  # autoplay
    lesyml4z: str  # quality
    vbbcsaos: bool  # sponsorblock
    @property
    def _7cyrtrv6(self) -> bool:
        """dearrow (`7cyrtrv6` on the wire)."""
        return self.fields["7cyrtrv6"]

    @_7cyrtrv6.setter
    def _7cyrtrv6(self, value: bool) -> None:
        self.edit("7cyrtrv6", value=value)
    eyoonq3d: bool  # dislikes
    lumd7e36: bool  # comments
    t2l6coaj: float  # speed


class WatchEntry(Resource):
    """WatchEntry — `tl.so.tilo.tube.WatchEntry`."""
    __surface__ = "so.tilo.tube@8213a8f9a9dcf15a"
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
    rqajp43d: int  # left_ms


class PairedDevice(Resource):
    """PairedDevice — `tl.so.tilo.tube.PairedDevice`."""
    __surface__ = "so.tilo.tube@8213a8f9a9dcf15a"
    eqjnywdb: str  # key
    xzt7hvjd: str  # name
    r6hetdhx: list[str]  # addresses
    qshipefo: int  # port
    rb3itb6c: bool  # approved
    dtjxddej: bool  # revoked
    dtxc67xp: str  # approved_at
    @property
    def _3rbysfau(self) -> str:
        """last_synced (`3rbysfau` on the wire)."""
        return self.fields["3rbysfau"]

    @_3rbysfau.setter
    def _3rbysfau(self, value: str) -> None:
        self.edit("3rbysfau", value=value)
    ovgswiz5: str  # last_error


class GrayjayPairAsk(Resource):
    """GrayjayPairAsk — `tl.so.tilo.tube.GrayjayPairAsk`."""
    __surface__ = "so.tilo.tube@8213a8f9a9dcf15a"
    vjm7bxs2: str  # invite


class GrayjaySyncAsk(Resource):
    """GrayjaySyncAsk — `tl.so.tilo.tube.GrayjaySyncAsk`."""
    __surface__ = "so.tilo.tube@8213a8f9a9dcf15a"
    ldigr4fm: list[GrayjayPeer]  # peers
    @property
    def _2ubtnp7s(self) -> list[GrayjaySubscription]:
        """subscriptions (`2ubtnp7s` on the wire)."""
        return self.fields["2ubtnp7s"]

    @_2ubtnp7s.setter
    def _2ubtnp7s(self, value: list[GrayjaySubscription]) -> None:
        self.edit("2ubtnp7s", value=value)


class GrayjayPeer(Resource):
    """GrayjayPeer — `tl.so.tilo.tube.GrayjayPeer`."""
    __surface__ = "so.tilo.tube@8213a8f9a9dcf15a"
    @property
    def _73ossitw(self) -> str:
        """key (`73ossitw` on the wire)."""
        return self.fields["73ossitw"]

    @_73ossitw.setter
    def _73ossitw(self, value: str) -> None:
        self.edit("73ossitw", value=value)
    u7p5waf7: str  # name
    eqe3dx4x: list[str]  # addresses
    b5mgdh6x: int  # port


class GrayjaySubscription(Resource):
    """GrayjaySubscription — `tl.so.tilo.tube.GrayjaySubscription`."""
    __surface__ = "so.tilo.tube@8213a8f9a9dcf15a"
    @property
    def _5k7gvetf(self) -> str:
        """url (`5k7gvetf` on the wire)."""
        return self.fields["5k7gvetf"]

    @_5k7gvetf.setter
    def _5k7gvetf(self, value: str) -> None:
        self.edit("5k7gvetf", value=value)
    o4wrklpp: str  # name


class GrayjayJob(Resource):
    """GrayjayJob — `tl.so.tilo.tube.GrayjayJob`."""
    __surface__ = "so.tilo.tube@8213a8f9a9dcf15a"
    @property
    def _4nyda52b(self) -> str:
        """state (`4nyda52b` on the wire)."""
        return self.fields["4nyda52b"]

    @_4nyda52b.setter
    def _4nyda52b(self, value: str) -> None:
        self.edit("4nyda52b", value=value)
    c7tscegn: str  # answer
    tshqcr4u: str  # error


class GrayjayOutcome(Resource):
    """GrayjayOutcome — `tl.so.tilo.tube.GrayjayOutcome`."""
    __surface__ = "so.tilo.tube@8213a8f9a9dcf15a"
    xil726fb: str  # key
    zkmfyff4: bool  # ok
    mp7b4zg2: str  # error


class GrayjayRead(Resource):
    """GrayjayRead — `tl.so.tilo.tube.GrayjayRead`."""
    __surface__ = "so.tilo.tube@8213a8f9a9dcf15a"
    oow4nbb2: list[GrayjayOutcome]  # peers
    @property
    def _2vaamao3(self) -> list[GrayjaySubscription]:
        """subscriptions (`2vaamao3` on the wire)."""
        return self.fields["2vaamao3"]

    @_2vaamao3.setter
    def _2vaamao3(self, value: list[GrayjaySubscription]) -> None:
        self.edit("2vaamao3", value=value)
    iddsonzh: list[str]  # history


class YouTube(Resource):
    """YouTube — `tl.so.tilo.tube.YouTube`."""
    __surface__ = "so.tilo.tube@8213a8f9a9dcf15a"
    olrung72: str  # title
    ihhzrm3f: list[Entry[Channel]]  # channels
    qnfjkti5: list[Entry[Video]]  # videos
    v5sh2bpr: list[Entry[Playlist]]  # playlists
    iputtmua: list[Entry[Subscription]]  # subscriptions
    jgjqgesf: list[Entry[QueuedVideo]]  # queue
    hv25j74h: list[Entry[WatchEntry]]  # history
    puten5ar: bool  # history_cleared
    ugv4b6is: list[Entry[Comment]]  # comments
    cjlgivfs: list[Entry[Segment]]  # segments
    @property
    def _5oxdbju7(self) -> list[str]:
        """liked_comments (`5oxdbju7` on the wire)."""
        return self.fields["5oxdbju7"]

    @_5oxdbju7.setter
    def _5oxdbju7(self, value: list[str]) -> None:
        self.edit("5oxdbju7", value=value)
    iphlnbkx: list[Entry[Reply]]  # replies
    epgpppzy: list[Entry[PlaybackPrefs]]  # playback
    f5zuqdsr: Any | None  # playing
    @property
    def _5hk5vbw4(self) -> list[Entry[Any]]:
        """sessions (`5hk5vbw4` on the wire)."""
        return self.fields["5hk5vbw4"]

    @_5hk5vbw4.setter
    def _5hk5vbw4(self, value: list[Entry[Any]]) -> None:
        self.edit("5hk5vbw4", value=value)
    bqesicxq: str  # search_text
    @property
    def _2pryeqkt(self) -> str:
        """search_kind (`2pryeqkt` on the wire)."""
        return self.fields["2pryeqkt"]

    @_2pryeqkt.setter
    def _2pryeqkt(self, value: str) -> None:
        self.edit("2pryeqkt", value=value)
    egducuxn: str  # search_length
    ypapv6iy: str  # search_published
    hcmy6dp7: str  # search_state
    zjw3anaz: str  # search_trouble
    nnvco6w3: str  # feed_state
    r3ksw6bn: str  # trouble
    qv7t2mza: bool  # online
    dmc6hhdb: list[Entry[PairedDevice]]  # devices
    bnycbpqz: str  # sync_working
    @property
    def _5siq65eg(self) -> str:
        """sync_job (`5siq65eg` on the wire)."""
        return self.fields["5siq65eg"]

    @_5siq65eg.setter
    def _5siq65eg(self, value: str) -> None:
        self.edit("5siq65eg", value=value)
    drdwq4ha: str  # sync_notice
    rx6sx3au: str  # sync_trouble
    slqvuard: bool  # sync_started

    def play_queue(self) -> None:
        """Plays the queue from its oldest video: what was put on it first plays first, and the rest follow in that order, so autoplay goes on down the queue."""
        return self.action("play_queue")

    def session(self) -> Any:
        """The session this app plays through: the one it has, or one made now. It is kept both in `playing`, which a view reads, and in `sessions`, so the record is a resource of its own."""
        return self.action("session")

    def play_items(self,
        *,
        videos: list[Video],
        at: str,
    ) -> None:
        """Plays `videos` in the session, beginning with `at`'s place: what plays and what follows, so a shared next, the live card and another device move through the same list. The video at `at` continues where it was left."""
        return self.action("play_items", videos=videos, at=at)

    def play_items_many(self, items: list[PlayItemsArgs]) -> list[dict[str, Any]]:
        """Runs `play_items` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("play_items", items)

    def autoplay_next(self,
        *,
        p: Any,
        share: float,
    ) -> None:
        """When the video reaches its end and the person asked for the next one, the session moves on to what follows it: the queue's next video is watched now, so its place in the history is kept and its bytes are asked for when the service has not answered with them yet."""
        return self.action("autoplay_next", p=p, share=share)

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

    def set_quality(self, *, quality: str) -> None:
        """The tallest picture the person wants, as YouTube names it; `auto` takes the best that fits."""
        return self.action("set_quality", quality=quality)

    def set_quality_many(self, items: list[SetQualityArgs]) -> list[dict[str, Any]]:
        """Runs `set_quality` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_quality", items)

    def set_sponsorblock(self, *, on: bool) -> None:
        """Whether the player skips the segments SponsorBlock marks, as they play."""
        return self.action("set_sponsorblock", on=on)

    def set_sponsorblock_many(self, items: list[SetSponsorblockArgs]) -> list[dict[str, Any]]:
        """Runs `set_sponsorblock` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_sponsorblock", items)

    def set_dearrow(self, *, on: bool) -> None:
        """Whether a video shows the community's title and picture where DeArrow has them."""
        return self.action("set_dearrow", on=on)

    def set_dearrow_many(self, items: list[SetDearrowArgs]) -> list[dict[str, Any]]:
        """Runs `set_dearrow` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_dearrow", items)

    def set_dislikes(self, *, on: bool) -> None:
        """Whether a video shows how many people disliked it, as Return YouTube Dislike estimates it."""
        return self.action("set_dislikes", on=on)

    def set_dislikes_many(self, items: list[SetDislikesArgs]) -> list[dict[str, Any]]:
        """Runs `set_dislikes` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_dislikes", items)

    def set_comments(self, *, on: bool) -> None:
        """Whether a video's page shows its comments."""
        return self.action("set_comments", on=on)

    def set_comments_many(self, items: list[SetCommentsArgs]) -> list[dict[str, Any]]:
        """Runs `set_comments` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_comments", items)

    def set_speed(self, *, speed: float) -> None:
        """How fast the person watches: every video here starts at this speed, on every device."""
        return self.action("set_speed", speed=speed)

    def set_speed_many(self, items: list[SetSpeedArgs]) -> list[dict[str, Any]]:
        """Runs `set_speed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_speed", items)

    def pair_device(self, *, invite: str) -> None:
        """Pairs a Grayjay device from the invite its Sync screen shows. The invite carries where the device answers and the one-time code that opens the pairing; the device is recorded waiting for the person's approval, which is what lets it read and write anything here."""
        return self.action("pair_device", invite=invite)

    def pair_device_many(self, items: list[PairDeviceArgs]) -> list[dict[str, Any]]:
        """Runs `pair_device` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("pair_device", items)

    def approve_device(self, *, device: PairedDevice) -> None:
        """Approves a device that paired: Grayjay shows the same fingerprint on both ends."""
        return self.action("approve_device", device=device)

    def approve_device_many(self, items: list[ApproveDeviceArgs]) -> list[dict[str, Any]]:
        """Runs `approve_device` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("approve_device", items)

    def revoke_device(self, *, device: PairedDevice) -> None:
        """Revokes a device: its key can no longer read or write anything here."""
        return self.action("revoke_device", device=device)

    def revoke_device_many(self, items: list[RevokeDeviceArgs]) -> list[dict[str, Any]]:
        """Runs `revoke_device` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("revoke_device", items)

    def sync_devices(self) -> None:
        """Exchanges with every approved device now: reads the subscriptions and watch history Grayjay holds, and sends this client's subscriptions that way."""
        return self.action("sync_devices")

    def sync_devices_many(self, items: list[SyncDevicesArgs]) -> list[dict[str, Any]]:
        """Runs `sync_devices` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("sync_devices", items)

    def auto_sync(self) -> None:
        """The approved devices are exchanged with on the timer, as Grayjay's own app does while it is open, so what either app adds reaches the other without anyone pressing. Nothing runs until a person has synced once by hand."""
        return self.action("auto_sync")

    def auto_sync_many(self, items: list[AutoSyncArgs]) -> list[dict[str, Any]]:
        """Runs `auto_sync` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("auto_sync", items)

    def grayjay_ended(self, *, j: Any) -> None:
        """The Grayjay operation ended: a pairing keeps the device it made, an exchange what it brought, and a failure or a stop says so in the window."""
        return self.action("grayjay_ended", j=j)

    def grayjay_ended_many(self, items: list[GrayjayEndedArgs]) -> list[dict[str, Any]]:
        """Runs `grayjay_ended` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("grayjay_ended", items)

    def keep_paired(self, *, answer: str) -> None:
        """Keeps the device a pairing made, waiting for the person's approval: the fingerprint beside it is what Grayjay shows on the other end."""
        return self.action("keep_paired", answer=answer)

    def keep_paired_many(self, items: list[KeepPairedArgs]) -> list[dict[str, Any]]:
        """Runs `keep_paired` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep_paired", items)

    def keep_exchange(self, *, answer: str) -> None:
        """Keeps what an exchange came to: how each device stands, the subscriptions the other apps hold, and the videos they watched."""
        return self.action("keep_exchange", answer=answer)

    def keep_exchange_many(self, items: list[KeepExchangeArgs]) -> list[dict[str, Any]]:
        """Runs `keep_exchange` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep_exchange", items)

    def search(self,
        *,
        query: str | None = None,
    ) -> None:
        """Searches YouTube: the service answers with channels, playlists and videos, written where the search view lists them. The answer arrives through the platform's network, never the app's."""
        args: dict[str, Any] = {}
        if query is not None:
            args["query"] = query
        return self.action("search", **args)

    def filter_search(self, *, kind: str) -> None:
        """Narrows the search to the kinds the person chose: `all`, `video`, `channel` or `playlist`. What the service found is kept, and the page shows the kind chosen alone, as the old chips did; asking again is the search field's, which carries the kind with it."""
        return self.action("filter_search", kind=kind)

    def filter_search_many(self, items: list[FilterSearchArgs]) -> list[dict[str, Any]]:
        """Runs `filter_search` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("filter_search", items)

    def filter_length(self, *, length: str) -> None:
        """Narrows the search to how long its videos run: `any`, `short`, `medium` or `long`."""
        return self.action("filter_length", length=length)

    def filter_published(self, *, published: str) -> None:
        """Narrows the search to how recent its results are: `any`, `hour`, `today`, `week`, `month` or `year`."""
        return self.action("filter_published", published=published)

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

    def like_comment(self,
        *,
        c: Comment,
        liked: bool | None = None,
    ) -> None:
        """Likes a comment, or takes the like back: the person's own, kept with the workspace, since YouTube lets no app act as them."""
        args: dict[str, Any] = {}
        args["c"] = c
        if liked is not None:
            args["liked"] = liked
        return self.action("like_comment", **args)

    def like_comment_many(self, items: list[LikeCommentArgs]) -> list[dict[str, Any]]:
        """Runs `like_comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("like_comment", items)

    def play_again(self, *, h: WatchEntry) -> None:
        """Plays the video a history entry names, from where the person left off."""
        return self.action("play_again", h=h)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"play_queue", "session", "play_items", "play_items_many", "autoplay_next", "delete_history", "delete_history_many", "set_autoplay", "set_autoplay_many", "set_quality", "set_quality_many", "set_sponsorblock", "set_sponsorblock_many", "set_dearrow", "set_dearrow_many", "set_dislikes", "set_dislikes_many", "set_comments", "set_comments_many", "set_speed", "set_speed_many", "pair_device", "pair_device_many", "approve_device", "approve_device_many", "revoke_device", "revoke_device_many", "sync_devices", "sync_devices_many", "auto_sync", "auto_sync_many", "grayjay_ended", "grayjay_ended_many", "keep_paired", "keep_paired_many", "keep_exchange", "keep_exchange_many", "search", "filter_search", "filter_search_many", "filter_length", "filter_published", "end_search", "end_search_many", "refresh_feed", "load_trending", "open_link", "like_comment", "like_comment_many", "play_again"})


class Tube(Resource):
    """YouTube — `tl.so.tilo.tube`."""
    __surface__ = "so.tilo.tube@8213a8f9a9dcf15a"

