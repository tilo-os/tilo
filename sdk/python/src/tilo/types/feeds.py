"""Types for Feeds (`so.tilo.feeds`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Folder` is the app's `tl.so.tilo.feeds.Folder` at `feeds/<resource>`, read with
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
    "FeedFormat",
    "Folder",
    "Feed",
    "Article",
    "Reader",
    "Refresh",
    "Library",
    "Ask",
    "AskFeed",
    "Job",
    "Taken",
    "Pulled",
    "TakeAsk",
    "Outline",
    "Outlines",
    "OpmlAsk",
    "OpmlWrite",
    "OpmlFeed",
    "OpmlText",
    "Feeds",
    "RenameFolderArgs",
    "RenameFeedArgs",
    "MoveFeedArgs",
    "EditBlockArgs",
    "InsertBlockArgs",
    "DeleteBlockArgs",
    "ShowArticleArgs",
    "SetReadArgs",
    "StarArgs",
    "SetTagsArgs",
    "ShowPlaceArgs",
    "ShowTagArgs",
    "SetScaleArgs",
    "LargerTextArgs",
    "SmallerTextArgs",
    "SetFaceArgs",
    "SetPaperArgs",
    "UnsubscribeArgs",
    "NewFolderArgs",
    "DeleteFolderArgs",
    "MarkReadArgs",
    "StopRefreshArgs",
    "PollRefreshArgs",
    "ImportOpmlArgs",
]

FeedFormat = Literal["rss", "atom", "json", "gemini"]
"""The format a subscription is read in."""

RenameFolderArgs = TypedDict("RenameFolderArgs", {
    "name": "str",
})
"""The arguments of `rename_folder`."""

RenameFeedArgs = TypedDict("RenameFeedArgs", {
    "name": "str",
})
"""The arguments of `rename_feed`."""

MoveFeedArgs = TypedDict("MoveFeedArgs", {
    "folder": NotRequired["Folder | None"],
})
"""The arguments of `move_feed`."""

EditBlockArgs = TypedDict("EditBlockArgs", {
    "block": "str",
    "text": "str",
})
"""The arguments of `edit-block`."""

InsertBlockArgs = TypedDict("InsertBlockArgs", {
    "block": "str",
    "text": "str",
})
"""The arguments of `insert-block`."""

DeleteBlockArgs = TypedDict("DeleteBlockArgs", {
    "block": "str",
})
"""The arguments of `delete-block`."""

ShowArticleArgs = TypedDict("ShowArticleArgs", {
})
"""The arguments of `show_article`."""

SetReadArgs = TypedDict("SetReadArgs", {
    "read": NotRequired["bool | None"],
})
"""The arguments of `set_read`."""

StarArgs = TypedDict("StarArgs", {
    "starred": NotRequired["bool | None"],
})
"""The arguments of `star`."""

SetTagsArgs = TypedDict("SetTagsArgs", {
    "tags": "str",
})
"""The arguments of `set_tags`."""

ShowPlaceArgs = TypedDict("ShowPlaceArgs", {
    "place": "str",
})
"""The arguments of `show_place`."""

ShowTagArgs = TypedDict("ShowTagArgs", {
    "tag": "str",
})
"""The arguments of `show_tag`."""

SetScaleArgs = TypedDict("SetScaleArgs", {
    "scale": "float",
})
"""The arguments of `set_scale`."""

LargerTextArgs = TypedDict("LargerTextArgs", {
})
"""The arguments of `larger_text`."""

SmallerTextArgs = TypedDict("SmallerTextArgs", {
})
"""The arguments of `smaller_text`."""

SetFaceArgs = TypedDict("SetFaceArgs", {
    "face": "str",
})
"""The arguments of `set_face`."""

SetPaperArgs = TypedDict("SetPaperArgs", {
    "paper": "str",
})
"""The arguments of `set_paper`."""

UnsubscribeArgs = TypedDict("UnsubscribeArgs", {
    "feed": "Feed",
})
"""The arguments of `unsubscribe`."""

NewFolderArgs = TypedDict("NewFolderArgs", {
    "name": "str",
})
"""The arguments of `new_folder`."""

DeleteFolderArgs = TypedDict("DeleteFolderArgs", {
    "folder": "Folder",
})
"""The arguments of `delete_folder`."""

MarkReadArgs = TypedDict("MarkReadArgs", {
    "place": NotRequired["str | None"],
})
"""The arguments of `mark_read`."""

StopRefreshArgs = TypedDict("StopRefreshArgs", {
})
"""The arguments of `stop_refresh`."""

PollRefreshArgs = TypedDict("PollRefreshArgs", {
})
"""The arguments of `poll_refresh`."""

ImportOpmlArgs = TypedDict("ImportOpmlArgs", {
    "file": "Any",
})
"""The arguments of `import_opml`."""

class Folder(Resource):
    """Folder — `tl.so.tilo.feeds.Folder`."""
    __surface__ = "so.tilo.feeds@b65962ae1efce5f4"
    ygksbjy5: str  # name

    def rename_folder(self, *, name: str) -> None:
        """Renames a folder."""
        return self.action("rename_folder", name=name)

    def rename_folder_many(self, items: list[RenameFolderArgs]) -> list[dict[str, Any]]:
        """Runs `rename_folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename_folder", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"rename_folder", "rename_folder_many"})


class Feed(Resource):
    """Feed — `tl.so.tilo.feeds.Feed`."""
    __surface__ = "so.tilo.feeds@b65962ae1efce5f4"
    ruq4ynjw: str  # name
    jdxu5vhb: str  # url
    kitws4yh: str  # site
    ecrfpxxe: Folder | None  # folder
    lb5mig4n: FeedFormat  # format
    @property
    def _2wtntp4k(self) -> str:
        """pin (`2wtntp4k` on the wire)."""
        return self.fields["2wtntp4k"]

    @_2wtntp4k.setter
    def _2wtntp4k(self, value: str) -> None:
        self.edit("2wtntp4k", value=value)
    py34q4xk: str  # trouble

    def rename_feed(self, *, name: str) -> None:
        """Renames a subscription, whatever its feed calls itself."""
        return self.action("rename_feed", name=name)

    def rename_feed_many(self, items: list[RenameFeedArgs]) -> list[dict[str, Any]]:
        """Runs `rename_feed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename_feed", items)

    def move_feed(self,
        *,
        folder: Folder | None = None,
    ) -> None:
        """Moves a subscription into a folder, or out of every folder."""
        args: dict[str, Any] = {}
        if folder is not None:
            args["folder"] = folder
        return self.action("move_feed", **args)

    def move_feed_many(self, items: list[MoveFeedArgs]) -> list[dict[str, Any]]:
        """Runs `move_feed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_feed", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"rename_feed", "rename_feed_many", "move_feed", "move_feed_many"})


class Article(Resource):
    """Article — `tl.so.tilo.feeds.Article`."""
    __surface__ = "so.tilo.feeds@b65962ae1efce5f4"
    chbpw5pg: Feed  # feed
    swmvsojn: str  # title
    gvntfpn6: str  # author
    p5lxkp7w: str  # url
    s2wxy2go: str | None  # published
    ok5mmsqb: str  # summary
    @property
    def _2yi7idis(self) -> str:
        """body (`2yi7idis` on the wire)."""
        return self.fields["2yi7idis"]

    @_2yi7idis.setter
    def _2yi7idis(self, value: str) -> None:
        self.edit("2yi7idis", value=value)
    y7lqgqyr: bool  # read
    hqmrnoz5: bool  # starred
    @property
    def _4nrqtndr(self) -> list[str]:
        """tags (`4nrqtndr` on the wire)."""
        return self.fields["4nrqtndr"]

    @_4nrqtndr.setter
    def _4nrqtndr(self, value: list[str]) -> None:
        self.edit("4nrqtndr", value=value)

    def edit_block(self,
        *,
        block: str,
        text: str,
    ) -> None:
        """Set one paragraph of a rich-text field to what a Markdown text says."""
        return self.action("edit-block", block=block, text=text)

    def edit_block_many(self, items: list[EditBlockArgs]) -> list[dict[str, Any]]:
        """Runs `edit-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("edit-block", items)

    def insert_block(self,
        *,
        block: str,
        text: str,
    ) -> None:
        """Put a paragraph before the one the address names."""
        return self.action("insert-block", block=block, text=text)

    def insert_block_many(self, items: list[InsertBlockArgs]) -> list[dict[str, Any]]:
        """Runs `insert-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("insert-block", items)

    def delete_block(self, *, block: str) -> None:
        """Remove the paragraph the address names, and the paragraphs it holds."""
        return self.action("delete-block", block=block)

    def delete_block_many(self, items: list[DeleteBlockArgs]) -> list[dict[str, Any]]:
        """Runs `delete-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-block", items)

    def show_article(self) -> None:
        """Shows the place an article's feed lists it in, so a search hit or an agent's `open` lands its row in the list beside it."""
        return self.action("show_article")

    def show_article_many(self, items: list[ShowArticleArgs]) -> list[dict[str, Any]]:
        """Runs `show_article` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show_article", items)

    def set_read(self,
        *,
        read: bool | None = None,
    ) -> None:
        """Marks an article read, or unread."""
        args: dict[str, Any] = {}
        if read is not None:
            args["read"] = read
        return self.action("set_read", **args)

    def set_read_many(self, items: list[SetReadArgs]) -> list[dict[str, Any]]:
        """Runs `set_read` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_read", items)

    def star(self,
        *,
        starred: bool | None = None,
    ) -> None:
        """Stars an article, or takes the star back."""
        args: dict[str, Any] = {}
        if starred is not None:
            args["starred"] = starred
        return self.action("star", **args)

    def star_many(self, items: list[StarArgs]) -> list[dict[str, Any]]:
        """Runs `star` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("star", items)

    def set_tags(self, *, tags: str) -> None:
        """Gives an article the tags a person typed, comma-separated: each trimmed, lower case, without `#`, at most 24 of them."""
        return self.action("set_tags", tags=tags)

    def set_tags_many(self, items: list[SetTagsArgs]) -> list[dict[str, Any]]:
        """Runs `set_tags` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_tags", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"edit_block", "edit_block_many", "insert_block", "insert_block_many", "delete_block", "delete_block_many", "show_article", "show_article_many", "set_read", "set_read_many", "star", "star_many", "set_tags", "set_tags_many"})


class Reader(Resource):
    """Reader — `tl.so.tilo.feeds.Reader`."""
    __surface__ = "so.tilo.feeds@b65962ae1efce5f4"
    tqbu72hj: str  # place
    lx6zg57h: float  # scale
    um6nclsf: str  # face
    to5unuga: str  # paper

    def show_place(self, *, place: str) -> None:
        """Reads from `place` in the sidebar. The place is the reader's own, so it follows them to their other devices and no one else's reading moves with it."""
        return self.action("show_place", place=place)

    def show_place_many(self, items: list[ShowPlaceArgs]) -> list[dict[str, Any]]:
        """Runs `show_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show_place", items)

    def show_tag(self, *, tag: str) -> None:
        """Shows the place a tag's articles stand in."""
        return self.action("show_tag", tag=tag)

    def show_tag_many(self, items: list[ShowTagArgs]) -> list[dict[str, Any]]:
        """Runs `show_tag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show_tag", items)

    def set_scale(self, *, scale: float) -> None:
        """Sets how large the article's text is set: 0.8 to 2 times the reading base."""
        return self.action("set_scale", scale=scale)

    def set_scale_many(self, items: list[SetScaleArgs]) -> list[dict[str, Any]]:
        """Runs `set_scale` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_scale", items)

    def larger_text(self) -> None:
        """A step larger in the reading itself."""
        return self.action("larger_text")

    def larger_text_many(self, items: list[LargerTextArgs]) -> list[dict[str, Any]]:
        """Runs `larger_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("larger_text", items)

    def smaller_text(self) -> None:
        """A step smaller."""
        return self.action("smaller_text")

    def smaller_text_many(self, items: list[SmallerTextArgs]) -> list[dict[str, Any]]:
        """Runs `smaller_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("smaller_text", items)

    def set_face(self, *, face: str) -> None:
        """The face the article is set in: `serif`, `sans` or `mono`."""
        return self.action("set_face", face=face)

    def set_face_many(self, items: list[SetFaceArgs]) -> list[dict[str, Any]]:
        """Runs `set_face` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_face", items)

    def set_paper(self, *, paper: str) -> None:
        """The paper the article is printed on: `system` follows the theme, or a fixed page for long-form reading."""
        return self.action("set_paper", paper=paper)

    def set_paper_many(self, items: list[SetPaperArgs]) -> list[dict[str, Any]]:
        """Runs `set_paper` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_paper", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"show_place", "show_place_many", "show_tag", "show_tag_many", "set_scale", "set_scale_many", "larger_text", "larger_text_many", "smaller_text", "smaller_text_many", "set_face", "set_face_many", "set_paper", "set_paper_many"})


class Refresh(Resource):
    """Refresh — `tl.so.tilo.feeds.Refresh`."""
    __surface__ = "so.tilo.feeds@b65962ae1efce5f4"
    crwgoxd6: str  # job
    dzn4ssl3: str  # state
    @property
    def _7d33zhis(self) -> int:
        """done (`7d33zhis` on the wire)."""
        return self.fields["7d33zhis"]

    @_7d33zhis.setter
    def _7d33zhis(self, value: int) -> None:
        self.edit("7d33zhis", value=value)
    mq3vs354: int  # total
    pvpr56lq: int  # fresh
    gbspv2g7: int  # failed
    dbqvtyzb: str  # note


class Library(Resource):
    """Library — `tl.so.tilo.feeds.Library`."""
    __surface__ = "so.tilo.feeds@b65962ae1efce5f4"
    urf3jsnz: list[Entry[Folder]]  # folders
    ba25drsv: list[Entry[Feed]]  # feeds
    mymgo65x: list[Entry[Article]]  # articles
    @property
    def _53g3dh5y(self) -> list[Entry[Reader]]:
        """readers (`53g3dh5y` on the wire)."""
        return self.fields["53g3dh5y"]

    @_53g3dh5y.setter
    def _53g3dh5y(self, value: list[Entry[Reader]]) -> None:
        self.edit("53g3dh5y", value=value)
    ptibzrny: Refresh  # refresh

    def begin_reading(self) -> Reader:
        """Gives the person a reading of their own here, if they have none yet: the place they read in and how the article's page is set."""
        return self.action("begin_reading")

    def add_subscription(self,
        *,
        url: str,
        folder: Folder | None = None,
    ) -> Feed:
        """Subscribes to a feed's address, or shows the subscription this library already has."""
        args: dict[str, Any] = {}
        args["url"] = url
        if folder is not None:
            args["folder"] = folder
        return self.action("add_subscription", **args)

    def unsubscribe(self, *, feed: Feed) -> None:
        """Unsubscribes: the subscription leaves the sidebar, and what it brought in is taken away with it. Undo brings both back."""
        return self.action("unsubscribe", feed=feed)

    def unsubscribe_many(self, items: list[UnsubscribeArgs]) -> list[dict[str, Any]]:
        """Runs `unsubscribe` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unsubscribe", items)

    def new_folder(self, *, name: str) -> None:
        """Starts a folder that keeps related subscriptions together; its subscriptions stay when it goes."""
        return self.action("new_folder", name=name)

    def new_folder_many(self, items: list[NewFolderArgs]) -> list[dict[str, Any]]:
        """Runs `new_folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("new_folder", items)

    def delete_folder(self, *, folder: Folder) -> None:
        """Deletes a folder; its subscriptions stay, at the top of the sidebar."""
        return self.action("delete_folder", folder=folder)

    def delete_folder_many(self, items: list[DeleteFolderArgs]) -> list[dict[str, Any]]:
        """Runs `delete_folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_folder", items)

    def mark_read(self,
        *,
        place: str | None = None,
    ) -> None:
        """Marks everything the place `place` lists as read."""
        args: dict[str, Any] = {}
        if place is not None:
            args["place"] = place
        return self.action("mark_read", **args)

    def mark_read_many(self, items: list[MarkReadArgs]) -> list[dict[str, Any]]:
        """Runs `mark_read` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("mark_read", items)

    # the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
    def stop_refresh(self) -> None:
        """Stops a refresh that is running."""
        return self.action("stop_refresh")

    def stop_refresh_many(self, items: list[StopRefreshArgs]) -> list[dict[str, Any]]:
        """Runs `stop_refresh` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("stop_refresh", items)

    def poll_refresh(self) -> None:
        """Takes what the device has read so far: each subscription's answer as it lands, and what the feed says of itself with it. What a refresh brought in is nobody's step of their own, so landing it says nothing and offers no undo of its own; an article the library already keeps is not added twice. Landing writes, so it stands here rather than in a call of its own: a run that calls another action is run twice, and the device's answers are not its to answer twice."""
        return self.action("poll_refresh")

    def poll_refresh_many(self, items: list[PollRefreshArgs]) -> list[dict[str, Any]]:
        """Runs `poll_refresh` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("poll_refresh", items)

    def import_opml(self, *, file: Any) -> None:
        """Subscribes to what an OPML file lists: every address the library does not have yet, in the folders the file names, made where missing."""
        return self.action("import_opml", file=file)

    def import_opml_many(self, items: list[ImportOpmlArgs]) -> list[dict[str, Any]]:
        """Runs `import_opml` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import_opml", items)

    def export_opml(self) -> Any:
        """The library's subscriptions as an OPML 2.0 file."""
        return self.action("export_opml")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"begin_reading", "add_subscription", "unsubscribe", "unsubscribe_many", "new_folder", "new_folder_many", "delete_folder", "delete_folder_many", "mark_read", "mark_read_many", "stop_refresh", "stop_refresh_many", "poll_refresh", "poll_refresh_many", "import_opml", "import_opml_many", "export_opml"})


class Ask(Resource):
    """Ask — `tl.so.tilo.feeds.Ask`."""
    __surface__ = "so.tilo.feeds@b65962ae1efce5f4"
    ajc3gacu: list[AskFeed]  # feeds


class AskFeed(Resource):
    """AskFeed — `tl.so.tilo.feeds.AskFeed`."""
    __surface__ = "so.tilo.feeds@b65962ae1efce5f4"
    wvgxfoj6: str  # url
    rzd7dr5x: str  # pin


class Job(Resource):
    """Job — `tl.so.tilo.feeds.Job`."""
    __surface__ = "so.tilo.feeds@b65962ae1efce5f4"
    vxpbania: str  # state
    qen3hx3f: int  # done
    fm7ik3wg: int  # total
    krtut5p6: list[str]  # ready


class Taken(Resource):
    """Taken — `tl.so.tilo.feeds.Taken`."""
    __surface__ = "so.tilo.feeds@b65962ae1efce5f4"
    karmfgtt: bool  # ok
    mkw76ot4: str  # title
    vxvxtmdp: str  # pin
    rcxpbdkb: str  # reason
    tkh3vj2o: int  # status
    @property
    def _3oef7xt5(self) -> str:
        """message (`3oef7xt5` on the wire)."""
        return self.fields["3oef7xt5"]

    @_3oef7xt5.setter
    def _3oef7xt5(self, value: str) -> None:
        self.edit("3oef7xt5", value=value)
    in2no76u: list[str]  # articles


class Pulled(Resource):
    """Pulled — `tl.so.tilo.feeds.Pulled`."""
    __surface__ = "so.tilo.feeds@b65962ae1efce5f4"
    qablmehl: str  # title
    @property
    def _5sdtzscn(self) -> str:
        """author (`5sdtzscn` on the wire)."""
        return self.fields["5sdtzscn"]

    @_5sdtzscn.setter
    def _5sdtzscn(self, value: str) -> None:
        self.edit("5sdtzscn", value=value)
    ae5rtinu: str  # url
    gmi4oufp: int  # published
    pj5brijd: str  # summary
    t2byonw5: str  # body


class TakeAsk(Resource):
    """TakeAsk — `tl.so.tilo.feeds.TakeAsk`."""
    __surface__ = "so.tilo.feeds@b65962ae1efce5f4"
    @property
    def _5mcvbcwk(self) -> str:
        """job (`5mcvbcwk` on the wire)."""
        return self.fields["5mcvbcwk"]

    @_5mcvbcwk.setter
    def _5mcvbcwk(self, value: str) -> None:
        self.edit("5mcvbcwk", value=value)
    ixh37lm6: str  # url


class Outline(Resource):
    """Outline — `tl.so.tilo.feeds.Outline`."""
    __surface__ = "so.tilo.feeds@b65962ae1efce5f4"
    esmrzlts: str  # title
    fhfn2dm2: str  # url
    akah2ujo: str  # site
    otz67kho: str  # folder


class Outlines(Resource):
    """Outlines — `tl.so.tilo.feeds.Outlines`."""
    __surface__ = "so.tilo.feeds@b65962ae1efce5f4"
    z646rsds: list[str]  # outlines


class OpmlAsk(Resource):
    """OpmlAsk — `tl.so.tilo.feeds.OpmlAsk`."""
    __surface__ = "so.tilo.feeds@b65962ae1efce5f4"
    vg5idgn5: str  # text


class OpmlWrite(Resource):
    """OpmlWrite — `tl.so.tilo.feeds.OpmlWrite`."""
    __surface__ = "so.tilo.feeds@b65962ae1efce5f4"
    pd5lnepq: list[OpmlFeed]  # feeds


class OpmlFeed(Resource):
    """OpmlFeed — `tl.so.tilo.feeds.OpmlFeed`."""
    __surface__ = "so.tilo.feeds@b65962ae1efce5f4"
    @property
    def _5w7b7tgv(self) -> str:
        """title (`5w7b7tgv` on the wire)."""
        return self.fields["5w7b7tgv"]

    @_5w7b7tgv.setter
    def _5w7b7tgv(self, value: str) -> None:
        self.edit("5w7b7tgv", value=value)
    @property
    def _5u77j2s2(self) -> str:
        """url (`5u77j2s2` on the wire)."""
        return self.fields["5u77j2s2"]

    @_5u77j2s2.setter
    def _5u77j2s2(self, value: str) -> None:
        self.edit("5u77j2s2", value=value)
    igyb7lxx: str  # site
    gjs6qx4q: str  # format
    @property
    def _367at7x3(self) -> str:
        """folder (`367at7x3` on the wire)."""
        return self.fields["367at7x3"]

    @_367at7x3.setter
    def _367at7x3(self, value: str) -> None:
        self.edit("367at7x3", value=value)


class OpmlText(Resource):
    """OpmlText — `tl.so.tilo.feeds.OpmlText`."""
    __surface__ = "so.tilo.feeds@b65962ae1efce5f4"
    q2mosawv: str  # text


class Feeds(Resource):
    """Feeds — `tl.so.tilo.feeds`."""
    __surface__ = "so.tilo.feeds@b65962ae1efce5f4"

