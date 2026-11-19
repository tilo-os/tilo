"""Types for Word Processor (`so.tilo.word-processor`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Document` is the app's `word-processor.document` at `word-processor/<resource>`, read with
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
    "Author",
    "ClipAttr",
    "ClipBlock",
    "Comment",
    "MarkSpan",
    "MarkValue",
    "Reply",
    "ReadDocument",
    "Document",
    "DeleteRowArgs",
    "DeleteColumnArgs",
    "DeleteTableArgs",
    "DeleteImageArgs",
    "AcceptChangeArgs",
    "RejectChangeArgs",
    "AcceptAllChangesArgs",
    "RejectAllChangesArgs",
    "AddCommentArgs",
    "ReplyToCommentArgs",
    "ResolveCommentArgs",
    "AdoptCommentsArgs",
    "SetPageMarginsArgs",
    "GenerateTocArgs",
    "NoteFileWrittenArgs",
]

Author = dict[str, Any]
"""One of member, named: an object whose case the app tags it with."""

ClipAttr = dict[str, Any]
"""One of Bool, Int, Str: an object whose case the app tags it with."""

class ClipBlock(TypedDict):
    kind: str
    text: str
    marks: list[MarkSpan]
    level: int | None
    ordered: bool | None
    align: str | None
    attrs: dict[str, ClipAttr]

class Comment(TypedDict):
    anchor: Any
    author: Author
    date: str
    text: str
    body: list[ClipBlock]
    resolved: bool
    replies: list[Entry[Reply]]

class MarkSpan(TypedDict):
    start: int
    end: int
    name: str
    value: MarkValue

MarkValue = dict[str, Any]
"""One of Bool, Int, Str: an object whose case the app tags it with."""

class Reply(TypedDict):
    author: Author
    date: str
    text: str
    body: list[ClipBlock]

ReadDocument = TypedDict("ReadDocument", {
    "file": "str",
    "format": "str",
    "text": "str | None",
    "from": "int | None",
    "next": "int | None",
    "total": "int | None",
    "picture": "str | None",
    "media_type": "str | None",
    "bytes": "int | None",
    "width": "int | None",
    "height": "int | None",
})

DeleteRowArgs = TypedDict("DeleteRowArgs", {
    "table": "str",
    "row": "int",
})
"""The arguments of `delete-row`."""

DeleteColumnArgs = TypedDict("DeleteColumnArgs", {
    "table": "str",
    "column": "int",
})
"""The arguments of `delete-column`."""

DeleteTableArgs = TypedDict("DeleteTableArgs", {
    "table": "str",
})
"""The arguments of `delete-table`."""

DeleteImageArgs = TypedDict("DeleteImageArgs", {
    "image": "str",
})
"""The arguments of `delete-image`."""

AcceptChangeArgs = TypedDict("AcceptChangeArgs", {
    "change": "str",
})
"""The arguments of `accept-change`."""

RejectChangeArgs = TypedDict("RejectChangeArgs", {
    "change": "str",
})
"""The arguments of `reject-change`."""

AcceptAllChangesArgs = TypedDict("AcceptAllChangesArgs", {
})
"""The arguments of `accept-all-changes`."""

RejectAllChangesArgs = TypedDict("RejectAllChangesArgs", {
})
"""The arguments of `reject-all-changes`."""

AddCommentArgs = TypedDict("AddCommentArgs", {
    "block": "str",
    "start": "int",
    "end": "int",
    "text": "str",
})
"""The arguments of `add-comment`."""

ReplyToCommentArgs = TypedDict("ReplyToCommentArgs", {
    "comment": "str",
    "text": "str",
    "body": NotRequired["list[ClipBlock] | None"],
})
"""The arguments of `reply-to-comment`."""

ResolveCommentArgs = TypedDict("ResolveCommentArgs", {
    "comment": "str",
})
"""The arguments of `resolve-comment`."""

AdoptCommentsArgs = TypedDict("AdoptCommentsArgs", {
})
"""The arguments of `adopt-comments`."""

SetPageMarginsArgs = TypedDict("SetPageMarginsArgs", {
    "top": "float",
    "right": "float",
    "bottom": "float",
    "left": "float",
})
"""The arguments of `set-page-margins`."""

GenerateTocArgs = TypedDict("GenerateTocArgs", {
})
"""The arguments of `generate-toc`."""

NoteFileWrittenArgs = TypedDict("NoteFileWrittenArgs", {
    "digest": "str",
})
"""The arguments of `note-file-written`."""

class Document(Resource):
    """Document — `word-processor.document`."""
    __surface__ = "so.tilo.word-processor@aef8d21eae3890e2"
    body: str
    comments: list[Entry[Comment]]

    def append_paragraph(self, *, text: str) -> Any:
        """Runs the app's action."""
        return self.action("append-paragraph", text=text)

    def replace_body(self, *, markdown: str) -> Any:
        """Runs the app's action."""
        return self.action("replace-body", markdown=markdown)

    def replace_text(self,
        *,
        find: str,
        replace: str,
        all: bool | None = None,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["find"] = find
        args["replace"] = replace
        if all is not None:
            args["all"] = all
        return self.action("replace-text", **args)

    def insert_table(self,
        *,
        after: str,
        rows: int,
        columns: int,
    ) -> Any:
        """Runs the app's action."""
        return self.action("insert-table", after=after, rows=rows, columns=columns)

    def add_row(self, *, table: str) -> Any:
        """Runs the app's action."""
        return self.action("add-row", table=table)

    def add_column(self, *, table: str) -> Any:
        """Runs the app's action."""
        return self.action("add-column", table=table)

    def delete_row(self,
        *,
        table: str,
        row: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("delete-row", table=table, row=row)

    def delete_row_many(self, items: list[DeleteRowArgs]) -> list[dict[str, Any]]:
        """Runs `delete-row` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-row", items)

    def delete_column(self,
        *,
        table: str,
        column: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("delete-column", table=table, column=column)

    def delete_column_many(self, items: list[DeleteColumnArgs]) -> list[dict[str, Any]]:
        """Runs `delete-column` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-column", items)

    def delete_table(self, *, table: str) -> None:
        """Runs the app's action."""
        return self.action("delete-table", table=table)

    def delete_table_many(self, items: list[DeleteTableArgs]) -> list[dict[str, Any]]:
        """Runs `delete-table` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-table", items)

    def delete_image(self, *, image: str) -> None:
        """Runs the app's action."""
        return self.action("delete-image", image=image)

    def delete_image_many(self, items: list[DeleteImageArgs]) -> list[dict[str, Any]]:
        """Runs `delete-image` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-image", items)

    def accept_change(self, *, change: str) -> None:
        """Runs the app's action."""
        return self.action("accept-change", change=change)

    def accept_change_many(self, items: list[AcceptChangeArgs]) -> list[dict[str, Any]]:
        """Runs `accept-change` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("accept-change", items)

    def reject_change(self, *, change: str) -> None:
        """Runs the app's action."""
        return self.action("reject-change", change=change)

    def reject_change_many(self, items: list[RejectChangeArgs]) -> list[dict[str, Any]]:
        """Runs `reject-change` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reject-change", items)

    def accept_all_changes(self) -> None:
        """Runs the app's action."""
        return self.action("accept-all-changes")

    def accept_all_changes_many(self, items: list[AcceptAllChangesArgs]) -> list[dict[str, Any]]:
        """Runs `accept-all-changes` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("accept-all-changes", items)

    def reject_all_changes(self) -> None:
        """Runs the app's action."""
        return self.action("reject-all-changes")

    def reject_all_changes_many(self, items: list[RejectAllChangesArgs]) -> list[dict[str, Any]]:
        """Runs `reject-all-changes` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reject-all-changes", items)

    def add_comment(self,
        *,
        block: str,
        start: int,
        end: int,
        text: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("add-comment", block=block, start=start, end=end, text=text)

    def add_comment_many(self, items: list[AddCommentArgs]) -> list[dict[str, Any]]:
        """Runs `add-comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-comment", items)

    def reply_to_comment(self,
        *,
        comment: str,
        text: str,
        body: list[ClipBlock] | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["comment"] = comment
        args["text"] = text
        if body is not None:
            args["body"] = body
        return self.action("reply-to-comment", **args)

    def reply_to_comment_many(self, items: list[ReplyToCommentArgs]) -> list[dict[str, Any]]:
        """Runs `reply-to-comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reply-to-comment", items)

    def resolve_comment(self, *, comment: str) -> None:
        """Runs the app's action."""
        return self.action("resolve-comment", comment=comment)

    def resolve_comment_many(self, items: list[ResolveCommentArgs]) -> list[dict[str, Any]]:
        """Runs `resolve-comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("resolve-comment", items)

    def adopt_comments(self) -> None:
        """Runs the app's action."""
        return self.action("adopt-comments")

    def adopt_comments_many(self, items: list[AdoptCommentsArgs]) -> list[dict[str, Any]]:
        """Runs `adopt-comments` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("adopt-comments", items)

    def set_page_margins(self,
        *,
        top: float,
        right: float,
        bottom: float,
        left: float,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-page-margins", top=top, right=right, bottom=bottom, left=left)

    def set_page_margins_many(self, items: list[SetPageMarginsArgs]) -> list[dict[str, Any]]:
        """Runs `set-page-margins` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-page-margins", items)

    def generate_toc(self) -> None:
        """Runs the app's action."""
        return self.action("generate-toc")

    def generate_toc_many(self, items: list[GenerateTocArgs]) -> list[dict[str, Any]]:
        """Runs `generate-toc` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("generate-toc", items)

    def note_file_written(self, *, digest: str) -> None:
        """Runs the app's action."""
        return self.action("note-file-written", digest=digest)

    def note_file_written_many(self, items: list[NoteFileWrittenArgs]) -> list[dict[str, Any]]:
        """Runs `note-file-written` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("note-file-written", items)

    def share_image(self) -> None:
        """Runs the app's action."""
        return self.action("share-image")

    def read_file(self,
        *,
        file: str,
        from_: int | None = None,
    ) -> ReadDocument:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["file"] = file
        if from_ is not None:
            args["from"] = from_
        return self.action("read-file", **args)

    def read_content(self,
        *,
        content: str,
        name: str,
        from_: int | None = None,
    ) -> ReadDocument:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["content"] = content
        args["name"] = name
        if from_ is not None:
            args["from"] = from_
        return self.action("read-content", **args)

    def export_content(self,
        *,
        format: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if format is not None:
            args["format"] = format
        return self.action("export-content", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"append_paragraph", "replace_body", "replace_text", "insert_table", "add_row", "add_column", "delete_row", "delete_row_many", "delete_column", "delete_column_many", "delete_table", "delete_table_many", "delete_image", "delete_image_many", "accept_change", "accept_change_many", "reject_change", "reject_change_many", "accept_all_changes", "accept_all_changes_many", "reject_all_changes", "reject_all_changes_many", "add_comment", "add_comment_many", "reply_to_comment", "reply_to_comment_many", "resolve_comment", "resolve_comment_many", "adopt_comments", "adopt_comments_many", "set_page_margins", "set_page_margins_many", "generate_toc", "generate_toc_many", "note_file_written", "note_file_written_many", "share_image", "read_file", "read_content", "export_content"})


