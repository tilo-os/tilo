"""Types for Feeds (`so.tilo.feeds`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Library` is the app's `feeds.library` at `feeds/<resource>`, read with
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
    "Feed",
    "FeedKind",
    "Folder",
    "Outline",
    "Article",
    "BlockKind",
    "ContentBlock",
    "Fetched",
    "Library",
    "Articles",
    "AddSubscriptionArgs",
    "UnsubscribeArgs",
    "NewFolderArgs",
    "DeleteFolderArgs",
    "ImportSubscriptionsArgs",
    "NoteFeedArgs",
    "ReceiveArgs",
    "MarkReadArgs",
    "SetReadArgs",
    "SetStarredArgs",
    "SetTagsArgs",
    "ForgetFeedArgs",
]

class Feed(TypedDict):
    title: str
    url: str
    site_url: str
    folder: str | None
    kind: FeedKind
    certificate_pin: str | None

FeedKind = Literal["rss", "atom", "json", "gemini"]

class Folder(TypedDict):
    name: str
    collapsed: bool

class Outline(TypedDict):
    title: str
    url: str
    site_url: str
    folder: str | None

class Article(TypedDict):
    feed_id: str
    title: str
    author: str
    url: str
    published: int
    summary: str
    blocks: list[ContentBlock]
    read: bool
    starred: bool
    tags: list[str]

BlockKind = Literal["Heading1", "Heading2", "Heading3", "Paragraph", "Bullet", "Quote", "Code", "Link", "Divider"]

class ContentBlock(TypedDict):
    kind: BlockKind
    text: str
    href: str | None

class Fetched(TypedDict):
    title: str
    author: str
    url: str
    published: int
    summary: str
    blocks: list[ContentBlock]

AddSubscriptionArgs = TypedDict("AddSubscriptionArgs", {
    "url": "str",
    "folder": NotRequired["str | None"],
})
"""The arguments of `add-subscription`."""

UnsubscribeArgs = TypedDict("UnsubscribeArgs", {
    "feed": "str",
})
"""The arguments of `unsubscribe`."""

NewFolderArgs = TypedDict("NewFolderArgs", {
    "name": "str",
})
"""The arguments of `new-folder`."""

DeleteFolderArgs = TypedDict("DeleteFolderArgs", {
    "folder": "str",
})
"""The arguments of `delete-folder`."""

ImportSubscriptionsArgs = TypedDict("ImportSubscriptionsArgs", {
    "outlines": "list[Outline]",
})
"""The arguments of `import-subscriptions`."""

NoteFeedArgs = TypedDict("NoteFeedArgs", {
    "feed": "str",
    "title": NotRequired["str | None"],
    "certificate_pin": NotRequired["str | None"],
})
"""The arguments of `note-feed`."""

ReceiveArgs = TypedDict("ReceiveArgs", {
    "feed": "str",
    "articles": "list[Fetched]",
})
"""The arguments of `receive`."""

MarkReadArgs = TypedDict("MarkReadArgs", {
    "feed": NotRequired["str | None"],
})
"""The arguments of `mark-read`."""

SetReadArgs = TypedDict("SetReadArgs", {
    "article": "str",
    "read": "bool",
})
"""The arguments of `set-read`."""

SetStarredArgs = TypedDict("SetStarredArgs", {
    "article": "str",
    "starred": "bool",
})
"""The arguments of `set-starred`."""

SetTagsArgs = TypedDict("SetTagsArgs", {
    "article": "str",
    "tags": "list[str]",
})
"""The arguments of `set-tags`."""

ForgetFeedArgs = TypedDict("ForgetFeedArgs", {
    "feed": "str",
})
"""The arguments of `forget-feed`."""

class Library(Resource):
    """Feeds — `feeds.library`."""
    __surface__ = "so.tilo.feeds@8acf229de253169c"
    folders: list[Entry[Folder]]
    feeds: list[Entry[Feed]]

    def add_subscription(self,
        *,
        url: str,
        folder: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["url"] = url
        if folder is not None:
            args["folder"] = folder
        return self.action("add-subscription", **args)

    def add_subscription_many(self, items: list[AddSubscriptionArgs]) -> list[dict[str, Any]]:
        """Runs `add-subscription` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-subscription", items)

    def unsubscribe(self, *, feed: str) -> None:
        """Runs the app's action."""
        return self.action("unsubscribe", feed=feed)

    def unsubscribe_many(self, items: list[UnsubscribeArgs]) -> list[dict[str, Any]]:
        """Runs `unsubscribe` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unsubscribe", items)

    def new_folder(self, *, name: str) -> None:
        """Runs the app's action."""
        return self.action("new-folder", name=name)

    def new_folder_many(self, items: list[NewFolderArgs]) -> list[dict[str, Any]]:
        """Runs `new-folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("new-folder", items)

    def delete_folder(self, *, folder: str) -> None:
        """Runs the app's action."""
        return self.action("delete-folder", folder=folder)

    def delete_folder_many(self, items: list[DeleteFolderArgs]) -> list[dict[str, Any]]:
        """Runs `delete-folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-folder", items)

    def import_subscriptions(self, *, outlines: list[Outline]) -> None:
        """Runs the app's action."""
        return self.action("import-subscriptions", outlines=outlines)

    def import_subscriptions_many(self, items: list[ImportSubscriptionsArgs]) -> list[dict[str, Any]]:
        """Runs `import-subscriptions` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import-subscriptions", items)

    def note_feed(self,
        *,
        feed: str,
        title: str | None = None,
        certificate_pin: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["feed"] = feed
        if title is not None:
            args["title"] = title
        if certificate_pin is not None:
            args["certificate_pin"] = certificate_pin
        return self.action("note-feed", **args)

    def note_feed_many(self, items: list[NoteFeedArgs]) -> list[dict[str, Any]]:
        """Runs `note-feed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("note-feed", items)

    def export_opml(self) -> None:
        """Runs the app's action."""
        return self.action("export-opml")

    def reader_text_larger(self) -> None:
        """Runs the app's action."""
        return self.action("reader-text-larger")

    def reader_text_smaller(self) -> None:
        """Runs the app's action."""
        return self.action("reader-text-smaller")

    def reader_font(self, *, font: str) -> None:
        """Runs the app's action."""
        return self.action("reader-font", font=font)

    def reader_paper(self, *, paper: str) -> None:
        """Runs the app's action."""
        return self.action("reader-paper", paper=paper)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"add_subscription", "add_subscription_many", "unsubscribe", "unsubscribe_many", "new_folder", "new_folder_many", "delete_folder", "delete_folder_many", "import_subscriptions", "import_subscriptions_many", "note_feed", "note_feed_many", "export_opml", "reader_text_larger", "reader_text_smaller", "reader_font", "reader_paper"})


class Articles(Resource):
    """Articles — `feeds.articles`."""
    __surface__ = "so.tilo.feeds@8acf229de253169c"
    articles: list[Entry[Article]]
    @property
    def read_through(self) -> dict[str, int]:
        """Read only."""
        return self.fields["read_through"]

    def receive(self,
        *,
        feed: str,
        articles: list[Fetched],
    ) -> None:
        """Runs the app's action."""
        return self.action("receive", feed=feed, articles=articles)

    def receive_many(self, items: list[ReceiveArgs]) -> list[dict[str, Any]]:
        """Runs `receive` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("receive", items)

    def mark_read(self,
        *,
        feed: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if feed is not None:
            args["feed"] = feed
        return self.action("mark-read", **args)

    def mark_read_many(self, items: list[MarkReadArgs]) -> list[dict[str, Any]]:
        """Runs `mark-read` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("mark-read", items)

    def set_read(self,
        *,
        article: str,
        read: bool,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-read", article=article, read=read)

    def set_read_many(self, items: list[SetReadArgs]) -> list[dict[str, Any]]:
        """Runs `set-read` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-read", items)

    def set_starred(self,
        *,
        article: str,
        starred: bool,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-starred", article=article, starred=starred)

    def set_starred_many(self, items: list[SetStarredArgs]) -> list[dict[str, Any]]:
        """Runs `set-starred` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-starred", items)

    def set_tags(self,
        *,
        article: str,
        tags: list[str],
    ) -> None:
        """Runs the app's action."""
        return self.action("set-tags", article=article, tags=tags)

    def set_tags_many(self, items: list[SetTagsArgs]) -> list[dict[str, Any]]:
        """Runs `set-tags` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-tags", items)

    def forget_feed(self, *, feed: str) -> None:
        """Runs the app's action."""
        return self.action("forget-feed", feed=feed)

    def forget_feed_many(self, items: list[ForgetFeedArgs]) -> list[dict[str, Any]]:
        """Runs `forget-feed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget-feed", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"receive", "receive_many", "mark_read", "mark_read_many", "set_read", "set_read_many", "set_starred", "set_starred_many", "set_tags", "set_tags_many", "forget_feed", "forget_feed_many"})


