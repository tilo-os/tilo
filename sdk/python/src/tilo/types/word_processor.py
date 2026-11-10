"""Types for Word Processor (`so.tilo.word-processor`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Document` is the app's `word-processor.document` at `word-processor/<resource>`, read with
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
    "ClipAttr",
    "ClipBlock",
    "Comment",
    "MarkSpan",
    "MarkValue",
    "Reply",
    "ReadDocument",
    "Document",
]

ClipAttr = dict[str, Any]
"""One of bool, int, str: an object whose case the app tags it with."""

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
    author: str
    date: str
    text: str
    body: list[ClipBlock]
    resolved: bool
    replies: list[Reply]

class MarkSpan(TypedDict):
    start: int
    end: int
    name: str
    value: MarkValue

MarkValue = dict[str, Any]
"""One of bool, int, str: an object whose case the app tags it with."""

class Reply(TypedDict):
    author: str
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

class Document(Resource):
    """Document — `word-processor.document`."""
    body: str
    comments: list[Comment]

    def append_paragraph(self, *, text: str) -> Any | Run:
        """Runs the app's action."""
        return self.action("append-paragraph", text=text)

    def replace_body(self, *, markdown: str) -> Any | Run:
        """Runs the app's action."""
        return self.action("replace-body", markdown=markdown)

    def replace_text(self,
        *,
        find: str,
        replace: str,
        all: bool | None = None,
    ) -> Any | Run:
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
    ) -> Any | Run:
        """Runs the app's action."""
        return self.action("insert-table", after=after, rows=rows, columns=columns)

    def add_row(self, *, table: str) -> Any | Run:
        """Runs the app's action."""
        return self.action("add-row", table=table)

    def add_column(self, *, table: str) -> Any | Run:
        """Runs the app's action."""
        return self.action("add-column", table=table)

    def delete_row(self,
        *,
        table: str,
        row: int,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-row", table=table, row=row)

    def delete_column(self,
        *,
        table: str,
        column: int,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-column", table=table, column=column)

    def delete_table(self, *, table: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-table", table=table)

    def delete_image(self, *, image: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-image", image=image)

    def accept_change(self, *, change: str) -> Run | None:
        """Runs the app's action."""
        return self.action("accept-change", change=change)

    def reject_change(self, *, change: str) -> Run | None:
        """Runs the app's action."""
        return self.action("reject-change", change=change)

    def accept_all_changes(self) -> Run | None:
        """Runs the app's action."""
        return self.action("accept-all-changes")

    def reject_all_changes(self) -> Run | None:
        """Runs the app's action."""
        return self.action("reject-all-changes")

    def add_comment(self,
        *,
        block: str,
        start: int,
        end: int,
        text: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("add-comment", block=block, start=start, end=end, text=text)

    def reply_to_comment(self,
        *,
        comment: str,
        text: str,
        body: list[ClipBlock] | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["comment"] = comment
        args["text"] = text
        if body is not None:
            args["body"] = body
        return self.action("reply-to-comment", **args)

    def resolve_comment(self, *, comment: str) -> Run | None:
        """Runs the app's action."""
        return self.action("resolve-comment", comment=comment)

    def adopt_comments(self) -> Run | None:
        """Runs the app's action."""
        return self.action("adopt-comments")

    def set_page_margins(self,
        *,
        top: float,
        right: float,
        bottom: float,
        left: float,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-page-margins", top=top, right=right, bottom=bottom, left=left)

    def generate_toc(self) -> Run | None:
        """Runs the app's action."""
        return self.action("generate-toc")

    def note_file_written(self, *, digest: str) -> Run | None:
        """Runs the app's action."""
        return self.action("note-file-written", digest=digest)

    def share_image(self) -> None:
        """Runs the app's action."""
        return self.action("share-image")

    def read_file(self,
        *,
        file: Any,
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


