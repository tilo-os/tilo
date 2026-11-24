"""Types for Discussion (`so.tilo.lang.discussion`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Origin` is the app's `tl.so.tilo.lang.discussion.Origin` at `discussion/<resource>`, read with
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
    "Origin",
    "Comment",
    "Comments",
    "Discussion",
    "EditBlockArgs",
    "InsertBlockArgs",
    "DeleteBlockArgs",
    "ResolveArgs",
    "DeleteArgs",
]

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

ResolveArgs = TypedDict("ResolveArgs", {
    "resolved": NotRequired["bool | None"],
})
"""The arguments of `resolve`."""

DeleteArgs = TypedDict("DeleteArgs", {
    "c": "Comment",
})
"""The arguments of `delete`."""

class Origin(Resource):
    """Origin — `tl.so.tilo.lang.discussion.Origin`."""
    __surface__ = "so.tilo.lang.discussion@d49b0379a6c070b7"
    xxyuyumk: str  # author
    gmvkzzuz: str | None  # handle
    nfpru2aa: str  # source
    wuizz4mi: str | None  # url
    semowww2: str | None  # reference
    @property
    def _3wgbb6lz(self) -> str | None:
        """created (`3wgbb6lz` on the wire)."""
        return self.fields["3wgbb6lz"]

    @_3wgbb6lz.setter
    def _3wgbb6lz(self, value: str | None) -> None:
        self.edit("3wgbb6lz", value=value)
    @property
    def _2wr4psic(self) -> str | None:
        """updated (`2wr4psic` on the wire)."""
        return self.fields["2wr4psic"]

    @_2wr4psic.setter
    def _2wr4psic(self, value: str | None) -> None:
        self.edit("2wr4psic", value=value)


class Comment(Resource):
    """Comment — `tl.so.tilo.lang.discussion.Comment`."""
    __surface__ = "so.tilo.lang.discussion@d49b0379a6c070b7"
    @property
    def _5q4mbecw(self) -> str:
        """text (`5q4mbecw` on the wire)."""
        return self.fields["5q4mbecw"]

    @_5q4mbecw.setter
    def _5q4mbecw(self, value: str) -> None:
        self.edit("5q4mbecw", value=value)
    ng7oourf: Comment | None  # reply_to
    lz4a7tbk: bool  # resolved
    c3oavkxa: bool  # by_agent
    uiyftp3p: Origin | None  # origin
    f6337kvt: str  # step
    hqpdw4ih: str  # lookup
    tovzcpp5: bool  # incorporated

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

    # the app's `edit` action is the SDK's own: run it with `action("edit")`.
    def reply(self, *, text: str) -> Comment:
        """Answers a comment: the reply stands under the remark it answers, as a conversation's answer does, and what it is about is what the remark is about."""
        return self.action("reply", text=text)

    def resolve(self,
        *,
        resolved: bool | None = None,
    ) -> None:
        """Marks a comment as taken care of, or opens it again. Whoever may read the thread may resolve it: resolving says the words were dealt with, not that its author agrees."""
        args: dict[str, Any] = {}
        if resolved is not None:
            args["resolved"] = resolved
        return self.action("resolve", **args)

    def resolve_many(self, items: list[ResolveArgs]) -> list[dict[str, Any]]:
        """Runs `resolve` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("resolve", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"edit_block", "edit_block_many", "insert_block", "insert_block_many", "delete_block", "delete_block_many", "reply", "resolve", "resolve_many"})


class Comments(Resource):
    """Comments — `tl.so.tilo.lang.discussion.Comments`."""
    __surface__ = "so.tilo.lang.discussion@d49b0379a6c070b7"
    @property
    def _3g3duquj(self) -> list[Entry[Comment]]:
        """items (`3g3duquj` on the wire)."""
        return self.fields["3g3duquj"]

    @_3g3duquj.setter
    def _3g3duquj(self, value: list[Entry[Comment]]) -> None:
        self.edit("3g3duquj", value=value)

    def comment(self,
        *,
        text: str,
        origin: Origin | None = None,
        step: str | None = None,
        lookup: str | None = None,
        incorporated: bool | None = None,
    ) -> Comment:
        """Says something about the resource the comment is made from: the thread under a task, a note or a mail says it with `.made_from(r)`, so the comment records what it is about. An agent says something the same way, its comment its own as any person's is."""
        args: dict[str, Any] = {}
        args["text"] = text
        if origin is not None:
            args["origin"] = origin
        if step is not None:
            args["step"] = step
        if lookup is not None:
            args["lookup"] = lookup
        if incorporated is not None:
            args["incorporated"] = incorporated
        return self.action("comment", **args)

    def delete(self, *, c: Comment) -> None:
        """Deletes a comment. Only its author may. The platform takes it back rather than asking first, and the removal is a version of the comment, so who removed it stays answerable."""
        return self.action("delete", c=c)

    def delete_many(self, items: list[DeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"comment", "delete", "delete_many"})


class Discussion(Resource):
    """Discussion — `tl.so.tilo.lang.discussion`."""
    __surface__ = "so.tilo.lang.discussion@d49b0379a6c070b7"

