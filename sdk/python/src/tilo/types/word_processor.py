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
    "WriteCellArgs",
    "SetStyleLookArgs",
    "SetStyleInkArgs",
    "NewStyleArgs",
    "DuplicateStyleArgs",
    "DeleteStyleArgs",
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
    "row": NotRequired["int | None"],
    "table": NotRequired["int | None"],
})
"""The arguments of `add_row`."""

AddColumnArgs = TypedDict("AddColumnArgs", {
    "column": NotRequired["int | None"],
    "table": NotRequired["int | None"],
})
"""The arguments of `add_column`."""

DeleteRowArgs = TypedDict("DeleteRowArgs", {
    "row": "int",
    "table": NotRequired["int | None"],
})
"""The arguments of `delete_row`."""

DeleteColumnArgs = TypedDict("DeleteColumnArgs", {
    "column": "int",
    "table": NotRequired["int | None"],
})
"""The arguments of `delete_column`."""

DeleteTableArgs = TypedDict("DeleteTableArgs", {
    "table": NotRequired["int | None"],
})
"""The arguments of `delete_table`."""

WriteCellArgs = TypedDict("WriteCellArgs", {
    "row": "int",
    "column": "int",
    "text": "str",
    "table": NotRequired["int | None"],
})
"""The arguments of `write_cell`."""

SetStyleLookArgs = TypedDict("SetStyleLookArgs", {
    "style": "str",
    "font": NotRequired["str | None"],
    "size": NotRequired["float | None"],
    "bold": NotRequired["bool | None"],
    "italic": NotRequired["bool | None"],
    "color": NotRequired["str | None"],
    "align": NotRequired["str | None"],
    "before": NotRequired["float | None"],
    "after": NotRequired["float | None"],
    "indent": NotRequired["float | None"],
})
"""The arguments of `set_style_look`."""

SetStyleInkArgs = TypedDict("SetStyleInkArgs", {
    "style": "str",
    "swatch": "int",
})
"""The arguments of `set_style_ink`."""

NewStyleArgs = TypedDict("NewStyleArgs", {
    "name": "str",
    "from": NotRequired["str | None"],
})
"""The arguments of `new_style`."""

DuplicateStyleArgs = TypedDict("DuplicateStyleArgs", {
    "style": "str",
})
"""The arguments of `duplicate_style`."""

DeleteStyleArgs = TypedDict("DeleteStyleArgs", {
    "style": "str",
})
"""The arguments of `delete_style`."""

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
    __surface__ = "so.tilo.word-processor@554a1c0b462fbcb3"
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
    __surface__ = "so.tilo.word-processor@554a1c0b462fbcb3"
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
        """Finds `find` in the document and replaces the first of it, or every one, with `replace`. The words are replaced where they stand, as the editor replaces them: a paragraph's marks, its formatting and the comments anchored in it stay around the words that stay."""
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

    def add_row(self,
        *,
        row: int | None = None,
        table: int | None = None,
    ) -> None:
        """Adds a blank row to a table of the document, under the row `row` counted from its head at 1, or under its last row. Only that table changes, as a row added in the editor's grid does."""
        args: dict[str, Any] = {}
        if row is not None:
            args["row"] = row
        if table is not None:
            args["table"] = table
        return self.action("add_row", **args)

    def add_row_many(self, items: list[AddRowArgs]) -> list[dict[str, Any]]:
        """Runs `add_row` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_row", items)

    def add_column(self,
        *,
        column: int | None = None,
        table: int | None = None,
    ) -> None:
        """Adds a blank column to a table of the document, right of the column `column` counted from its left at 1, or right of its last."""
        args: dict[str, Any] = {}
        if column is not None:
            args["column"] = column
        if table is not None:
            args["table"] = table
        return self.action("add_column", **args)

    def add_column_many(self, items: list[AddColumnArgs]) -> list[dict[str, Any]]:
        """Runs `add_column` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_column", items)

    def delete_row(self,
        *,
        row: int,
        table: int | None = None,
    ) -> None:
        """Deletes a row of a table of the document, counted from its head at 1."""
        args: dict[str, Any] = {}
        args["row"] = row
        if table is not None:
            args["table"] = table
        return self.action("delete_row", **args)

    def delete_row_many(self, items: list[DeleteRowArgs]) -> list[dict[str, Any]]:
        """Runs `delete_row` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_row", items)

    def delete_column(self,
        *,
        column: int,
        table: int | None = None,
    ) -> None:
        """Deletes a column of a table of the document, counted from its left at 1."""
        args: dict[str, Any] = {}
        args["column"] = column
        if table is not None:
            args["table"] = table
        return self.action("delete_column", **args)

    def delete_column_many(self, items: list[DeleteColumnArgs]) -> list[dict[str, Any]]:
        """Runs `delete_column` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_column", items)

    def delete_table(self,
        *,
        table: int | None = None,
    ) -> None:
        """Deletes a table of the document and everything it holds; the words around it stay."""
        args: dict[str, Any] = {}
        if table is not None:
            args["table"] = table
        return self.action("delete_table", **args)

    def delete_table_many(self, items: list[DeleteTableArgs]) -> list[dict[str, Any]]:
        """Runs `delete_table` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_table", items)

    def write_cell(self,
        *,
        row: int,
        column: int,
        text: str,
        table: int | None = None,
    ) -> None:
        """Writes `text` in one cell of a table of the document, at `row` counted from its head at 1 and `column` from its left at 1: that cell's words change and nothing else does, as typing in the editor's grid changes them."""
        args: dict[str, Any] = {}
        args["row"] = row
        args["column"] = column
        args["text"] = text
        if table is not None:
            args["table"] = table
        return self.action("write_cell", **args)

    def write_cell_many(self, items: list[WriteCellArgs]) -> list[dict[str, Any]]:
        """Runs `write_cell` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("write_cell", items)

    def set_style_look(self,
        *,
        style: str,
        font: str | None = None,
        size: float | None = None,
        bold: bool | None = None,
        italic: bool | None = None,
        color: str | None = None,
        align: str | None = None,
        before: float | None = None,
        after: float | None = None,
        indent: float | None = None,
    ) -> None:
        """Changes how the named style `style` looks: its face (sans, serif or mono), its size in points, whether it is bold or italic, its colour (#rrggbb), how its lines align (left, center, right, justify), and the space before and after it and its indent, in points. What is left out stays as it was. The style is the document's own, so every paragraph set in it changes at once, on every device, and the change is one a person takes back."""
        args: dict[str, Any] = {}
        args["style"] = style
        if font is not None:
            args["font"] = font
        if size is not None:
            args["size"] = size
        if bold is not None:
            args["bold"] = bold
        if italic is not None:
            args["italic"] = italic
        if color is not None:
            args["color"] = color
        if align is not None:
            args["align"] = align
        if before is not None:
            args["before"] = before
        if after is not None:
            args["after"] = after
        if indent is not None:
            args["indent"] = indent
        return self.action("set_style_look", **args)

    def set_style_look_many(self, items: list[SetStyleLookArgs]) -> list[dict[str, Any]]:
        """Runs `set_style_look` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_style_look", items)

    def set_style_ink(self,
        *,
        style: str,
        swatch: int,
    ) -> None:
        """Sets the ink of the named style `style` to one of the inks the page offers, counted from 0 in the order its colour swatches show them: the page's black and greys, then blue, green, ochre, red, violet and pink."""
        return self.action("set_style_ink", style=style, swatch=swatch)

    def set_style_ink_many(self, items: list[SetStyleInkArgs]) -> list[dict[str, Any]]:
        """Runs `set_style_ink` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_style_ink", items)

    def new_style(self,
        *,
        name: str,
        from_: str | None = None,
    ) -> None:
        """Makes a new named style called `name`, looking as the style `from` does: a person sets paragraphs in it and changes its look without touching the styles every document has."""
        args: dict[str, Any] = {}
        args["name"] = name
        if from_ is not None:
            args["from"] = from_
        return self.action("new_style", **args)

    def new_style_many(self, items: list[NewStyleArgs]) -> list[dict[str, Any]]:
        """Runs `new_style` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("new_style", items)

    def duplicate_style(self, *, style: str) -> None:
        """Makes a copy of the named style `style`, called '`style` copy', to change apart from it."""
        return self.action("duplicate_style", style=style)

    def duplicate_style_many(self, items: list[DuplicateStyleArgs]) -> list[dict[str, Any]]:
        """Runs `duplicate_style` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("duplicate_style", items)

    def delete_style(self, *, style: str) -> None:
        """Deletes a style the document made: its paragraphs go back to the body's look. The styles every document has (Body, the headings, Quote and List item) stay."""
        return self.action("delete_style", style=style)

    def delete_style_many(self, items: list[DeleteStyleArgs]) -> list[dict[str, Any]]:
        """Runs `delete_style` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_style", items)

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
    __actions__ = frozenset({"edit_block", "edit_block_many", "insert_block", "insert_block_many", "delete_block", "delete_block_many", "new_document", "dismiss_notice", "dismiss_notice_many", "append_paragraph", "append_paragraph_many", "replace_body", "replace_body_many", "read_content", "replace_text", "replace_text_many", "insert_table", "insert_table_many", "add_row", "add_row_many", "add_column", "add_column_many", "delete_row", "delete_row_many", "delete_column", "delete_column_many", "delete_table", "delete_table_many", "write_cell", "write_cell_many", "set_style_look", "set_style_look_many", "set_style_ink", "set_style_ink_many", "new_style", "new_style_many", "duplicate_style", "duplicate_style_many", "delete_style", "delete_style_many", "set_page_margins", "set_page_margins_many", "set_paper", "set_paper_many", "set_page_words", "set_page_words_many", "generate_toc", "generate_toc_many", "set_hyperlink", "set_hyperlink_many", "insert_page_break", "insert_page_break_many", "paste", "paste_many", "replace_selection", "replace_selection_many", "place_image", "place_image_many", "share_image", "delete_image", "delete_image_many", "react", "react_many", "comment_on", "open_document", "export_content", "export_text", "export_document", "export_html", "read_file"})


class WordProcessor(Resource):
    """Writer — `tl.so.tilo.word-processor`."""
    __surface__ = "so.tilo.word-processor@554a1c0b462fbcb3"

