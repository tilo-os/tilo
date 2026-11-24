"""Types for Writer (`so.tilo.word-processor`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `word-processor/<resource>`, read with
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
    "Document",
    "WordProcessor",
    "EditBlockArgs",
    "InsertBlockArgs",
    "DeleteBlockArgs",
    "DismissNoticeArgs",
    "AppendParagraphArgs",
    "ReplaceBodyArgs",
    "ReplaceTextArgs",
    "InsertTableArgs",
    "AddRowArgs",
    "AddColumnArgs",
    "DeleteRowArgs",
    "DeleteColumnArgs",
    "DeleteTableArgs",
    "SetPageMarginsArgs",
    "SetPaperArgs",
    "SetPageWordsArgs",
    "GenerateTocArgs",
    "SetHyperlinkArgs",
    "InsertPageBreakArgs",
    "PasteArgs",
    "ReplaceSelectionArgs",
    "PlaceImageArgs",
    "DeleteImageArgs",
    "ReactArgs",
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

DismissNoticeArgs = TypedDict("DismissNoticeArgs", {
})
"""The arguments of `dismiss_notice`."""

AppendParagraphArgs = TypedDict("AppendParagraphArgs", {
    "text": "str",
})
"""The arguments of `append_paragraph`."""

ReplaceBodyArgs = TypedDict("ReplaceBodyArgs", {
    "markdown": "str",
})
"""The arguments of `replace_body`."""

ReplaceTextArgs = TypedDict("ReplaceTextArgs", {
    "find": "str",
    "replace": "str",
    "all": NotRequired["bool | None"],
})
"""The arguments of `replace_text`."""

InsertTableArgs = TypedDict("InsertTableArgs", {
    "rows": NotRequired["int | None"],
    "columns": NotRequired["int | None"],
})
"""The arguments of `insert_table`."""

AddRowArgs = TypedDict("AddRowArgs", {
})
"""The arguments of `add_row`."""

AddColumnArgs = TypedDict("AddColumnArgs", {
})
"""The arguments of `add_column`."""

DeleteRowArgs = TypedDict("DeleteRowArgs", {
    "row": "int",
})
"""The arguments of `delete_row`."""

DeleteColumnArgs = TypedDict("DeleteColumnArgs", {
    "column": "int",
})
"""The arguments of `delete_column`."""

DeleteTableArgs = TypedDict("DeleteTableArgs", {
})
"""The arguments of `delete_table`."""

SetPageMarginsArgs = TypedDict("SetPageMarginsArgs", {
    "top": "float",
    "right": "float",
    "bottom": "float",
    "left": "float",
})
"""The arguments of `set_page_margins`."""

SetPaperArgs = TypedDict("SetPaperArgs", {
    "paper": "str",
})
"""The arguments of `set_paper`."""

SetPageWordsArgs = TypedDict("SetPageWordsArgs", {
    "where": "str",
    "words": "str",
})
"""The arguments of `set_page_words`."""

GenerateTocArgs = TypedDict("GenerateTocArgs", {
})
"""The arguments of `generate_toc`."""

SetHyperlinkArgs = TypedDict("SetHyperlinkArgs", {
    "target": "str",
    "text": NotRequired["str | None"],
})
"""The arguments of `set_hyperlink`."""

InsertPageBreakArgs = TypedDict("InsertPageBreakArgs", {
})
"""The arguments of `insert_page_break`."""

PasteArgs = TypedDict("PasteArgs", {
    "text": NotRequired["str | None"],
    "markdown": NotRequired["str | None"],
})
"""The arguments of `paste`."""

ReplaceSelectionArgs = TypedDict("ReplaceSelectionArgs", {
    "text": "str",
})
"""The arguments of `replace_selection`."""

PlaceImageArgs = TypedDict("PlaceImageArgs", {
    "image": "Any",
})
"""The arguments of `place_image`."""

DeleteImageArgs = TypedDict("DeleteImageArgs", {
    "image": "str",
})
"""The arguments of `delete_image`."""

ReactArgs = TypedDict("ReactArgs", {
    "emoji": "str",
})
"""The arguments of `react`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.word-processor@78785339f6d9571e"
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


class Document(Resource):
    """Document — `tl.so.tilo.word-processor.Document`."""
    __surface__ = "so.tilo.word-processor@78785339f6d9571e"
    @property
    def _76ypz5mr(self) -> str:
        """title (`76ypz5mr` on the wire)."""
        return self.fields["76ypz5mr"]

    @_76ypz5mr.setter
    def _76ypz5mr(self, value: str) -> None:
        self.edit("76ypz5mr", value=value)
    myqdqp23: str  # body
    @property
    def _6dmyhevh(self) -> float:
        """margin_top (`6dmyhevh` on the wire)."""
        return self.fields["6dmyhevh"]

    @_6dmyhevh.setter
    def _6dmyhevh(self, value: float) -> None:
        self.edit("6dmyhevh", value=value)
    @property
    def _5kuottk3(self) -> float:
        """margin_right (`5kuottk3` on the wire)."""
        return self.fields["5kuottk3"]

    @_5kuottk3.setter
    def _5kuottk3(self, value: float) -> None:
        self.edit("5kuottk3", value=value)
    u7wfrpxp: float  # margin_bottom
    tim24jvi: float  # margin_left
    @property
    def _5lzglfac(self) -> str:
        """paper (`5lzglfac` on the wire)."""
        return self.fields["5lzglfac"]

    @_5lzglfac.setter
    def _5lzglfac(self, value: str) -> None:
        self.edit("5lzglfac", value=value)
    yojargfl: str  # header
    @property
    def _5ih7vhqz(self) -> str:
        """footer (`5ih7vhqz` on the wire)."""
        return self.fields["5ih7vhqz"]

    @_5ih7vhqz.setter
    def _5ih7vhqz(self, value: str) -> None:
        self.edit("5ih7vhqz", value=value)
    wdffnjds: str  # started
    @property
    def _4hldj6md(self) -> dict[str, list[str]]:
        """reactions (`4hldj6md` on the wire)."""
        return self.fields["4hldj6md"]

    @_4hldj6md.setter
    def _4hldj6md(self, value: dict[str, list[str]]) -> None:
        self.edit("4hldj6md", value=value)
    mgawklxl: str  # notice
    mpu4ekps: str  # chosen
    po3yh2xl: int  # chosen_count

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

    def new_document(self,
        *,
        template: str | None = None,
    ) -> Document:
        """Starts the document over: a blank page, or the Markdown one of the templates starts with. A document that holds words is replaced all the same — the change is one the person takes back, as every change is, and the templates are named in the File menu beside New."""
        args: dict[str, Any] = {}
        if template is not None:
            args["template"] = template
        return self.action("new_document", **args)

    def dismiss_notice(self) -> None:
        """Takes away the reason the page shows over the writing: the person read it."""
        return self.action("dismiss_notice")

    def dismiss_notice_many(self, items: list[DismissNoticeArgs]) -> list[dict[str, Any]]:
        """Runs `dismiss_notice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("dismiss_notice", items)

    def append_paragraph(self, *, text: str) -> None:
        """Adds a paragraph of `text` at the end of the document, as Markdown says it."""
        return self.action("append_paragraph", text=text)

    def append_paragraph_many(self, items: list[AppendParagraphArgs]) -> list[dict[str, Any]]:
        """Runs `append_paragraph` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("append_paragraph", items)

    def replace_body(self, *, markdown: str) -> None:
        """Replaces the whole body from Markdown: headings, lists, pipe tables, emphasis and links. A picture is not written into the Markdown; it is placed with `place_image`."""
        return self.action("replace_body", markdown=markdown)

    def replace_body_many(self, items: list[ReplaceBodyArgs]) -> list[dict[str, Any]]:
        """Runs `replace_body` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("replace_body", items)

    def read_content(self) -> str:
        """What the document says as Markdown, ready for an agent or a file."""
        return self.action("read_content")

    def replace_text(self,
        *,
        find: str,
        replace: str,
        all: bool | None = None,
    ) -> None:
        """Finds `find` in the document and replaces the first of it, or every one, with `replace`. A paragraph's marks stay around the words that stay."""
        args: dict[str, Any] = {}
        args["find"] = find
        args["replace"] = replace
        if all is not None:
            args["all"] = all
        return self.action("replace_text", **args)

    def replace_text_many(self, items: list[ReplaceTextArgs]) -> list[dict[str, Any]]:
        """Runs `replace_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("replace_text", items)

    def insert_table(self,
        *,
        rows: int | None = None,
        columns: int | None = None,
    ) -> None:
        """Inserts an empty table of `rows` by `columns` where the writing ends, its head the first row."""
        args: dict[str, Any] = {}
        if rows is not None:
            args["rows"] = rows
        if columns is not None:
            args["columns"] = columns
        return self.action("insert_table", **args)

    def insert_table_many(self, items: list[InsertTableArgs]) -> list[dict[str, Any]]:
        """Runs `insert_table` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("insert_table", items)

    def add_row(self) -> None:
        """Adds a row like the last of the document's first table, under it."""
        return self.action("add_row")

    def add_row_many(self, items: list[AddRowArgs]) -> list[dict[str, Any]]:
        """Runs `add_row` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_row", items)

    def add_column(self) -> None:
        """Adds a cell to every row of the document's first table."""
        return self.action("add_column")

    def add_column_many(self, items: list[AddColumnArgs]) -> list[dict[str, Any]]:
        """Runs `add_column` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_column", items)

    def delete_row(self, *, row: int) -> None:
        """Deletes a row of the document's first table, counted from its head at 1."""
        return self.action("delete_row", row=row)

    def delete_row_many(self, items: list[DeleteRowArgs]) -> list[dict[str, Any]]:
        """Runs `delete_row` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_row", items)

    def delete_column(self, *, column: int) -> None:
        """Deletes a column of the document's first table, counted from its left at 1."""
        return self.action("delete_column", column=column)

    def delete_column_many(self, items: list[DeleteColumnArgs]) -> list[dict[str, Any]]:
        """Runs `delete_column` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_column", items)

    def delete_table(self) -> None:
        """Deletes the document's first table and everything it holds."""
        return self.action("delete_table")

    def delete_table_many(self, items: list[DeleteTableArgs]) -> list[dict[str, Any]]:
        """Runs `delete_table` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_table", items)

    def set_page_margins(self,
        *,
        top: float,
        right: float,
        bottom: float,
        left: float,
    ) -> None:
        """Sets the page's margins, in points from the page's edges."""
        return self.action("set_page_margins", top=top, right=right, bottom=bottom, left=left)

    def set_page_margins_many(self, items: list[SetPageMarginsArgs]) -> list[dict[str, Any]]:
        """Runs `set_page_margins` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_page_margins", items)

    def set_paper(self, *, paper: str) -> None:
        """Sets the paper the page is written on: A4, Letter or Legal."""
        return self.action("set_paper", paper=paper)

    def set_paper_many(self, items: list[SetPaperArgs]) -> list[dict[str, Any]]:
        """Runs `set_paper` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_paper", items)

    def set_page_words(self,
        *,
        where: str,
        words: str,
    ) -> None:
        """Sets the words every page's top or bottom margin carries: `where` is `header` or `footer`. Empty words for the header leave the document's title there."""
        return self.action("set_page_words", where=where, words=words)

    def set_page_words_many(self, items: list[SetPageWordsArgs]) -> list[dict[str, Any]]:
        """Runs `set_page_words` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_page_words", items)

    def generate_toc(self) -> None:
        """Writes a table of contents at the head of the document. It is a field, not a copy: the page draws the document's headings into it as they stand, so an entry a person writes or takes away shows there at once. A document without headings is refused: there is nothing to list."""
        return self.action("generate_toc")

    def generate_toc_many(self, items: list[GenerateTocArgs]) -> list[dict[str, Any]]:
        """Runs `generate_toc` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("generate_toc", items)

    def set_hyperlink(self,
        *,
        target: str,
        text: str | None = None,
    ) -> None:
        """Gives the document's words a link at the end of it, reading `text` or the address itself."""
        args: dict[str, Any] = {}
        args["target"] = target
        if text is not None:
            args["text"] = text
        return self.action("set_hyperlink", **args)

    def set_hyperlink_many(self, items: list[SetHyperlinkArgs]) -> list[dict[str, Any]]:
        """Runs `set_hyperlink` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_hyperlink", items)

    def insert_page_break(self) -> None:
        """Insert a page break at the end of the writing."""
        return self.action("insert_page_break")

    def insert_page_break_many(self, items: list[InsertPageBreakArgs]) -> list[dict[str, Any]]:
        """Runs `insert_page_break` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("insert_page_break", items)

    def paste(self,
        *,
        text: str | None = None,
        markdown: str | None = None,
    ) -> None:
        """Puts `markdown` at the end of the writing, as a paste does."""
        args: dict[str, Any] = {}
        if text is not None:
            args["text"] = text
        if markdown is not None:
            args["markdown"] = markdown
        return self.action("paste", **args)

    def paste_many(self, items: list[PasteArgs]) -> list[dict[str, Any]]:
        """Runs `paste` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("paste", items)

    def replace_selection(self, *, text: str) -> None:
        """Replaces the whole body with `text`, as Markdown."""
        return self.action("replace_selection", text=text)

    def replace_selection_many(self, items: list[ReplaceSelectionArgs]) -> list[dict[str, Any]]:
        """Runs `replace_selection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("replace_selection", items)

    def place_image(self, *, image: Any) -> None:
        """Writes a picture in where the person is writing: the person picks the file, and the page hands the address of its content to the editor, which puts it at the caret. The document keeps the address rather than the picture."""
        return self.action("place_image", image=image)

    def place_image_many(self, items: list[PlaceImageArgs]) -> list[dict[str, Any]]:
        """Runs `place_image` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("place_image", items)

    def share_image(self) -> Any:
        """The first picture the document shows, as content another app can take."""
        return self.action("share_image")

    def delete_image(self, *, image: str) -> None:
        """Deletes the picture whose address is `image`."""
        return self.action("delete_image", image=image)

    def delete_image_many(self, items: list[DeleteImageArgs]) -> list[dict[str, Any]]:
        """Runs `delete_image` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_image", items)

    def react(self, *, emoji: str) -> None:
        """Adds your reaction with an emoji to the document, or takes it back when you had reacted with it: a press on one of the pills in the bar, or the pick the add button opened."""
        return self.action("react", emoji=emoji)

    def react_many(self, items: list[ReactArgs]) -> list[dict[str, Any]]:
        """Runs `react` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("react", items)

    def comment_on(self,
        *,
        text: str,
        at: str | None = None,
        start: int | None = None,
        end: int | None = None,
    ) -> Any:
        """Says something about the document, in the thread beside its page. When `at` names a paragraph and `start..end` cover words in it, the remark is anchored there: the editor marks the words and stands the comment's marker in the margin beside them, so it always points at what it was made about. The comment and its anchor are one change."""
        args: dict[str, Any] = {}
        args["text"] = text
        if at is not None:
            args["at"] = at
        if start is not None:
            args["start"] = start
        if end is not None:
            args["end"] = end
        return self.action("comment_on", **args)

    def open_document(self, *, file: Any) -> Document:
        """Opens a file into the document: a Word, OpenDocument, RTF, EPUB or HTML file read by the device's codecs as the Markdown the editor shows, Markdown as it stands, and plain text as one paragraph a line. A file no codec here reads is refused with what it is. Opening a file is one change, which the person takes back as any other."""
        return self.action("open_document", file=file)

    def export_content(self) -> Any:
        """The document as a Markdown file, named by its title."""
        return self.action("export_content")

    def export_text(self) -> Any:
        """The document as a plain-text file: its words, a paragraph a line."""
        return self.action("export_text")

    def export_document(self, *, format: str) -> Any:
        """The document as a Word, OpenDocument, RTF, EPUB or PDF file, named by its title: what a person sends someone who does not write in Tilo. The bytes are the device's codecs'."""
        return self.action("export_document", format=format)

    def export_html(self) -> Any:
        """The document as one HTML page: its headings, lists and paragraphs, and its words."""
        return self.action("export_html")

    def read_file(self, *, file: Any) -> str:
        """Reads a file and answers what it says, without touching the document: what an agent reads a document through before it decides what to do with it."""
        return self.action("read_file", file=file)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"edit_block", "edit_block_many", "insert_block", "insert_block_many", "delete_block", "delete_block_many", "new_document", "dismiss_notice", "dismiss_notice_many", "append_paragraph", "append_paragraph_many", "replace_body", "replace_body_many", "read_content", "replace_text", "replace_text_many", "insert_table", "insert_table_many", "add_row", "add_row_many", "add_column", "add_column_many", "delete_row", "delete_row_many", "delete_column", "delete_column_many", "delete_table", "delete_table_many", "set_page_margins", "set_page_margins_many", "set_paper", "set_paper_many", "set_page_words", "set_page_words_many", "generate_toc", "generate_toc_many", "set_hyperlink", "set_hyperlink_many", "insert_page_break", "insert_page_break_many", "paste", "paste_many", "replace_selection", "replace_selection_many", "place_image", "place_image_many", "share_image", "delete_image", "delete_image_many", "react", "react_many", "comment_on", "open_document", "export_content", "export_text", "export_document", "export_html", "read_file"})


class WordProcessor(Resource):
    """Writer — `tl.so.tilo.word-processor`."""
    __surface__ = "so.tilo.word-processor@78785339f6d9571e"

