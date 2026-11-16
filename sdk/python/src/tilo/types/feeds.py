"""Types for Feeds (`so.tilo.feeds`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Library` is the app's `feeds.library` at `feeds/<resource>`, read with
`await ….load()`: its fields are the app's fields (read-only ones without a setter)
and its methods are its actions. A method answers what the app declares; an action
that takes longer answers a `Run`. A record is the dict the wire carries, and an
enum whose cases carry fields is an object whose tag the app's schema names. An
action whose name is one the class already has is run with `action("…")`, which
the file says where it is. An undoable action that answers nothing also has a
`<action>_many`, which applies it to every argument object of a list in a few
all-or-nothing steps, all of them one run.
"""

from __future__ import annotations

from typing import Any, Literal, NotRequired, TypedDict
from tilo import Resource, Run

__all__ = [
    "Article",
    "BlockKind",
    "ContentBlock",
    "Feed",
    "FeedKind",
    "Fetched",
    "Folder",
    "Outline",
    "Library",
    "AddSubscriptionArgs",
    "UnsubscribeArgs",
    "MarkFeedReadArgs",
    "MarkEverythingReadArgs",
    "NewFolderArgs",
    "DeleteFolderArgs",
    "ImportSubscriptionsArgs",
    "RefreshedArgs",
]

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

BlockKind = Literal["heading1", "heading2", "heading3", "paragraph", "bullet", "quote", "code", "link", "divider"]

class ContentBlock(TypedDict):
    kind: BlockKind
    text: str
    href: str | None

class Feed(TypedDict):
    title: str
    url: str
    site_url: str
    folder: str | None
    kind: FeedKind
    certificate_pin: str | None

FeedKind = Literal["rss", "atom", "json", "gemini"]

class Fetched(TypedDict):
    title: str
    author: str
    url: str
    published: int
    summary: str
    blocks: list[ContentBlock]

class Folder(TypedDict):
    name: str
    collapsed: bool

class Outline(TypedDict):
    title: str
    url: str
    site_url: str
    folder: str | None

AddSubscriptionArgs = TypedDict("AddSubscriptionArgs", {
    "url": "str",
    "folder": NotRequired["str | None"],
})
"""The arguments of `add-subscription`."""

UnsubscribeArgs = TypedDict("UnsubscribeArgs", {
    "feed": "str",
})
"""The arguments of `unsubscribe`."""

MarkFeedReadArgs = TypedDict("MarkFeedReadArgs", {
    "feed": "str",
})
"""The arguments of `mark-feed-read`."""

MarkEverythingReadArgs = TypedDict("MarkEverythingReadArgs", {
})
"""The arguments of `mark-everything-read`."""

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

RefreshedArgs = TypedDict("RefreshedArgs", {
    "articles": "list[Fetched]",
    "feed": "str",
    "title": NotRequired["str | None"],
    "certificate_pin": NotRequired["str | None"],
})
"""The arguments of `refreshed`."""

class Library(Resource):
    """Feeds — `feeds.library`."""
    __surface__ = "so.tilo.feeds@a405395fdcfa75c6"
    folders: list[Folder]
    feeds: list[Feed]
    articles: list[Article]

    def add_subscription(self,
        *,
        url: str,
        folder: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["url"] = url
        if folder is not None:
            args["folder"] = folder
        return self.action("add-subscription", **args)

    def add_subscription_many(self, items: list[AddSubscriptionArgs]) -> list[dict[str, Any]]:
        """Runs `add-subscription` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("add-subscription", items)

    def unsubscribe(self, *, feed: str) -> Run | None:
        """Runs the app's action."""
        return self.action("unsubscribe", feed=feed)

    def unsubscribe_many(self, items: list[UnsubscribeArgs]) -> list[dict[str, Any]]:
        """Runs `unsubscribe` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("unsubscribe", items)

    def mark_feed_read(self, *, feed: str) -> Run | None:
        """Runs the app's action."""
        return self.action("mark-feed-read", feed=feed)

    def mark_feed_read_many(self, items: list[MarkFeedReadArgs]) -> list[dict[str, Any]]:
        """Runs `mark-feed-read` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("mark-feed-read", items)

    def mark_everything_read(self) -> Run | None:
        """Runs the app's action."""
        return self.action("mark-everything-read")

    def mark_everything_read_many(self, items: list[MarkEverythingReadArgs]) -> list[dict[str, Any]]:
        """Runs `mark-everything-read` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("mark-everything-read", items)

    def new_folder(self, *, name: str) -> Run | None:
        """Runs the app's action."""
        return self.action("new-folder", name=name)

    def new_folder_many(self, items: list[NewFolderArgs]) -> list[dict[str, Any]]:
        """Runs `new-folder` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("new-folder", items)

    def delete_folder(self, *, folder: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-folder", folder=folder)

    def delete_folder_many(self, items: list[DeleteFolderArgs]) -> list[dict[str, Any]]:
        """Runs `delete-folder` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("delete-folder", items)

    def import_subscriptions(self, *, outlines: list[Outline]) -> Run | None:
        """Runs the app's action."""
        return self.action("import-subscriptions", outlines=outlines)

    def import_subscriptions_many(self, items: list[ImportSubscriptionsArgs]) -> list[dict[str, Any]]:
        """Runs `import-subscriptions` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("import-subscriptions", items)

    def refreshed(self,
        *,
        articles: list[Fetched],
        feed: str,
        title: str | None = None,
        certificate_pin: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["articles"] = articles
        args["feed"] = feed
        if title is not None:
            args["title"] = title
        if certificate_pin is not None:
            args["certificate_pin"] = certificate_pin
        return self.action("refreshed", **args)

    def refreshed_many(self, items: list[RefreshedArgs]) -> list[dict[str, Any]]:
        """Runs `refreshed` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("refreshed", items)

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


