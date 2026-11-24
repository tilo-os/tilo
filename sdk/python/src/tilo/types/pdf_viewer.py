"""Types for PDF Viewer (`so.tilo.pdf-viewer`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `pdf-viewer/<resource>`, read with
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
    "MarkKind",
    "Notice",
    "Document",
    "Page",
    "Picture",
    "Outline",
    "Link",
    "Field",
    "Mark",
    "Stroke",
    "Comment",
    "Reply",
    "Signature",
    "Library",
    "Runs",
    "PageRow",
    "GlyphRow",
    "ImportRow",
    "OutlineRow",
    "MarkRow",
    "MarkupRow",
    "FieldRow",
    "PageOut",
    "MarkOut",
    "ReplyOut",
    "ValueOut",
    "AdoptedOut",
    "SaveRequest",
    "MarkColour",
    "PdfViewer",
    "LayOutArgs",
    "ReadOutlineArgs",
    "ReadFieldsArgs",
    "ReadPageArgs",
    "ReadLinksArgs",
    "ShowPageArgs",
    "ReadNextArgs",
    "DrawNextArgs",
    "RotatePageArgs",
    "MovePageArgs",
    "DeletePageArgs",
    "ImportPagesArgs",
    "AddNoteArgs",
    "WriteMarkArgs",
    "MoveMarkArgs",
    "RecolourArgs",
    "DeleteMarkArgs",
    "ResolveCommentArgs",
    "EditCommentArgs",
    "DeleteCommentArgs",
    "DeleteReplyArgs",
    "MoveSignatureArgs",
    "MovePlacedSignatureArgs",
    "SignWithNameArgs",
    "DeleteSignatureArgs",
    "FillFieldArgs",
    "ChooseFieldArgs",
    "CheckFieldArgs",
    "RememberReadingArgs",
    "AdoptMarkupArgs",
    "TurnToArgs",
    "ReorderArgs",
    "PressArgs",
    "ReleaseArgs",
    "EditBlockArgs",
    "InsertBlockArgs",
    "DeleteBlockArgs",
    "ReplyEditBlockArgs",
    "ReplyInsertBlockArgs",
    "ReplyDeleteBlockArgs",
    "SignAsArgs",
    "DrawSignatureArgs",
]

MarkKind = Literal["markup", "ink", "note", "text"]
"""What a mark does on a page."""

LayOutArgs = TypedDict("LayOutArgs", {
})
"""The arguments of `lay_out`."""

ReadOutlineArgs = TypedDict("ReadOutlineArgs", {
})
"""The arguments of `read_outline`."""

ReadFieldsArgs = TypedDict("ReadFieldsArgs", {
})
"""The arguments of `read_fields`."""

ReadPageArgs = TypedDict("ReadPageArgs", {
    "p": "Page",
})
"""The arguments of `read_page`."""

ReadLinksArgs = TypedDict("ReadLinksArgs", {
    "p": "Page",
})
"""The arguments of `read_links`."""

ShowPageArgs = TypedDict("ShowPageArgs", {
    "p": "Page",
    "width": "int",
})
"""The arguments of `show_page`."""

ReadNextArgs = TypedDict("ReadNextArgs", {
})
"""The arguments of `read_next`."""

DrawNextArgs = TypedDict("DrawNextArgs", {
})
"""The arguments of `draw_next`."""

RotatePageArgs = TypedDict("RotatePageArgs", {
    "p": "Page",
})
"""The arguments of `rotate_page`."""

MovePageArgs = TypedDict("MovePageArgs", {
    "p": "Page",
    "after": NotRequired["Page | None"],
})
"""The arguments of `move_page`."""

DeletePageArgs = TypedDict("DeletePageArgs", {
    "p": "Page",
})
"""The arguments of `delete_page`."""

ImportPagesArgs = TypedDict("ImportPagesArgs", {
    "after": NotRequired["Page | None"],
    "file": "Any",
})
"""The arguments of `import_pages`."""

AddNoteArgs = TypedDict("AddNoteArgs", {
    "p": "Page",
    "words": "str",
})
"""The arguments of `add_note`."""

WriteMarkArgs = TypedDict("WriteMarkArgs", {
    "m": "Mark",
    "words": "str",
    "rects": NotRequired["list[float] | None"],
})
"""The arguments of `write_mark`."""

MoveMarkArgs = TypedDict("MoveMarkArgs", {
    "m": "Mark",
    "x": "float",
    "y": "float",
})
"""The arguments of `move_mark`."""

RecolourArgs = TypedDict("RecolourArgs", {
    "m": "Mark",
    "colour": "str",
})
"""The arguments of `recolour`."""

DeleteMarkArgs = TypedDict("DeleteMarkArgs", {
    "m": "Mark",
})
"""The arguments of `delete_mark`."""

ResolveCommentArgs = TypedDict("ResolveCommentArgs", {
    "c": "Comment",
    "resolved": "bool",
})
"""The arguments of `resolve_comment`."""

EditCommentArgs = TypedDict("EditCommentArgs", {
    "c": "Comment",
    "words": "str",
})
"""The arguments of `edit_comment`."""

DeleteCommentArgs = TypedDict("DeleteCommentArgs", {
    "c": "Comment",
})
"""The arguments of `delete_comment`."""

DeleteReplyArgs = TypedDict("DeleteReplyArgs", {
    "c": "Comment",
    "r": "Reply",
})
"""The arguments of `delete_reply`."""

MoveSignatureArgs = TypedDict("MoveSignatureArgs", {
    "s": "Signature",
    "x": "float",
    "y": "float",
})
"""The arguments of `move_signature`."""

MovePlacedSignatureArgs = TypedDict("MovePlacedSignatureArgs", {
    "p": "Page",
    "who": "str",
    "at": "list[int]",
})
"""The arguments of `move_placed_signature`."""

SignWithNameArgs = TypedDict("SignWithNameArgs", {
    "p": "Page",
    "signer": "str",
})
"""The arguments of `sign_with_name`."""

DeleteSignatureArgs = TypedDict("DeleteSignatureArgs", {
    "s": "Signature",
})
"""The arguments of `delete_signature`."""

FillFieldArgs = TypedDict("FillFieldArgs", {
    "name": "str",
    "text": "str",
})
"""The arguments of `fill_field`."""

ChooseFieldArgs = TypedDict("ChooseFieldArgs", {
    "name": "str",
    "choice": "str",
})
"""The arguments of `choose_field`."""

CheckFieldArgs = TypedDict("CheckFieldArgs", {
    "name": "str",
    "on": "bool",
    "export": NotRequired["str | None"],
})
"""The arguments of `check_field`."""

RememberReadingArgs = TypedDict("RememberReadingArgs", {
    "p": "Page",
    "fraction": "float",
    "zoom": "float",
})
"""The arguments of `remember_reading`."""

AdoptMarkupArgs = TypedDict("AdoptMarkupArgs", {
})
"""The arguments of `adopt_markup`."""

TurnToArgs = TypedDict("TurnToArgs", {
    "at": "int",
    "zoom": "float",
})
"""The arguments of `turn_to`."""

ReorderArgs = TypedDict("ReorderArgs", {
    "index": "int",
    "at": "int",
})
"""The arguments of `reorder`."""

PressArgs = TypedDict("PressArgs", {
    "p": "Page",
    "at": "list[float]",
    "tool": "str",
    "colour": "str",
})
"""The arguments of `press`."""

ReleaseArgs = TypedDict("ReleaseArgs", {
    "p": "Page",
    "tool": "str",
    "colour": "str",
    "start": NotRequired["list[float] | None"],
    "end": NotRequired["list[float] | None"],
    "stroke": "list[float]",
})
"""The arguments of `release`."""

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

ReplyEditBlockArgs = TypedDict("ReplyEditBlockArgs", {
    "block": "str",
    "text": "str",
})
"""The arguments of `edit-block`."""

ReplyInsertBlockArgs = TypedDict("ReplyInsertBlockArgs", {
    "block": "str",
    "text": "str",
})
"""The arguments of `insert-block`."""

ReplyDeleteBlockArgs = TypedDict("ReplyDeleteBlockArgs", {
    "block": "str",
})
"""The arguments of `delete-block`."""

SignAsArgs = TypedDict("SignAsArgs", {
    "name": "str",
})
"""The arguments of `sign_as`."""

DrawSignatureArgs = TypedDict("DrawSignatureArgs", {
    "strokes": "list[list[float]]",
})
"""The arguments of `draw_signature`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
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
    """Document — `tl.so.tilo.pdf-viewer.Document`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    peajfzyh: str  # source
    mxgrycmo: str  # source_name
    ebwmtj4p: str  # trouble
    @property
    def _2n6kpf64(self) -> str:
        """imported_source (`2n6kpf64` on the wire)."""
        return self.fields["2n6kpf64"]

    @_2n6kpf64.setter
    def _2n6kpf64(self, value: str) -> None:
        self.edit("2n6kpf64", value=value)
    zst2moht: str  # save_trouble
    vhujxgcb: list[Entry[Page]]  # pages
    zhtkbfeh: list[Entry[Picture]]  # pictures
    @property
    def _22zxqkpo(self) -> list[Entry[Outline]]:
        """outlines (`22zxqkpo` on the wire)."""
        return self.fields["22zxqkpo"]

    @_22zxqkpo.setter
    def _22zxqkpo(self, value: list[Entry[Outline]]) -> None:
        self.edit("22zxqkpo", value=value)
    @property
    def _7gcxsbu5(self) -> list[Entry[Link]]:
        """links (`7gcxsbu5` on the wire)."""
        return self.fields["7gcxsbu5"]

    @_7gcxsbu5.setter
    def _7gcxsbu5(self, value: list[Entry[Link]]) -> None:
        self.edit("7gcxsbu5", value=value)
    oushu5xx: list[Entry[Field]]  # fields
    ilzd6zzn: list[Entry[Mark]]  # marks
    @property
    def _7zfoarqk(self) -> list[Entry[Comment]]:
        """comments (`7zfoarqk` on the wire)."""
        return self.fields["7zfoarqk"]

    @_7zfoarqk.setter
    def _7zfoarqk(self, value: list[Entry[Comment]]) -> None:
        self.edit("7zfoarqk", value=value)
    s7stggro: list[Entry[Signature]]  # signatures
    uwnyzedo: dict[str, str]  # form_text
    zoztpthf: dict[str, str]  # form_choice
    @property
    def _2waifcc4(self) -> dict[str, bool]:
        """form_checked (`2waifcc4` on the wire)."""
        return self.fields["2waifcc4"]

    @_2waifcc4.setter
    def _2waifcc4(self, value: dict[str, bool]) -> None:
        self.edit("2waifcc4", value=value)
    kew5sddy: dict[str, str]  # form_export
    cvhgaqat: dict[str, str]  # adopted
    hflhpsk4: Page | None  # reading_page
    au5nrsiz: float  # reading_fraction
    fg6xtkn7: float  # reading_zoom

    def lay_out(self) -> None:
        """Lays a document out on its own bytes: one page for each page of the file, in their order, at the size the file draws it, its contents and its form."""
        return self.action("lay_out")

    def lay_out_many(self, items: list[LayOutArgs]) -> list[dict[str, Any]]:
        """Runs `lay_out` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("lay_out", items)

    def read_outline(self) -> None:
        """Reads the document's contents, so the review panel lists them without asking the engine each time it is shown."""
        return self.action("read_outline")

    def read_outline_many(self, items: list[ReadOutlineArgs]) -> list[dict[str, Any]]:
        """Runs `read_outline` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("read_outline", items)

    def read_fields(self) -> None:
        """Reads the document's form fields, so their widgets are drawn over the pages and filled in place."""
        return self.action("read_fields")

    def read_fields_many(self, items: list[ReadFieldsArgs]) -> list[dict[str, Any]]:
        """Runs `read_fields` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("read_fields", items)

    def read_page(self, *, p: Page) -> None:
        """Reads the page's words once, so the workspace's search finds the page by them and a person can select them, and reads the links it carries."""
        return self.action("read_page", p=p)

    def read_page_many(self, items: list[ReadPageArgs]) -> list[dict[str, Any]]:
        """Runs `read_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("read_page", items)

    def read_links(self, *, p: Page) -> None:
        """Reads the links the page carries: each area it covers and where it leads."""
        return self.action("read_links", p=p)

    def read_links_many(self, items: list[ReadLinksArgs]) -> list[dict[str, Any]]:
        """Runs `read_links` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("read_links", items)

    def show_page(self,
        *,
        p: Page,
        width: int,
    ) -> None:
        """Draws one page as a picture at `width` pixels, kept so a person looking at it again, or another member of the workspace, does not wait for the engine."""
        return self.action("show_page", p=p, width=width)

    def show_page_many(self, items: list[ShowPageArgs]) -> list[dict[str, Any]]:
        """Runs `show_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show_page", items)

    def read_next(self) -> None:
        """Reads the words of the next page nobody read."""
        return self.action("read_next")

    def read_next_many(self, items: list[ReadNextArgs]) -> list[dict[str, Any]]:
        """Runs `read_next` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("read_next", items)

    def draw_next(self) -> None:
        """Draws the next page nobody drew at the reader's zoom."""
        return self.action("draw_next")

    def draw_next_many(self, items: list[DrawNextArgs]) -> list[dict[str, Any]]:
        """Runs `draw_next` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("draw_next", items)

    def rotate_page(self, *, p: Page) -> None:
        """Turns a page a quarter clockwise."""
        return self.action("rotate_page", p=p)

    def rotate_page_many(self, items: list[RotatePageArgs]) -> list[dict[str, Any]]:
        """Runs `rotate_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rotate_page", items)

    def move_page(self,
        *,
        p: Page,
        after: Page | None = None,
    ) -> None:
        """Puts a page after another one, or first."""
        args: dict[str, Any] = {}
        args["p"] = p
        if after is not None:
            args["after"] = after
        return self.action("move_page", **args)

    def move_page_many(self, items: list[MovePageArgs]) -> list[dict[str, Any]]:
        """Runs `move_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_page", items)

    def duplicate_page(self, *, p: Page) -> Page:
        """Copies a page right after itself. The marks stay with the page they were made on."""
        return self.action("duplicate_page", p=p)

    def delete_page(self, *, p: Page) -> None:
        """Takes a page out of the document; a document keeps at least one."""
        return self.action("delete_page", p=p)

    def delete_page_many(self, items: list[DeletePageArgs]) -> list[dict[str, Any]]:
        """Runs `delete_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_page", items)

    def import_pages(self,
        *,
        file: Any,
        after: Page | None = None,
    ) -> None:
        """Takes the pages of another PDF or of a picture into the document, after a page (first when none is named)."""
        args: dict[str, Any] = {}
        if after is not None:
            args["after"] = after
        args["file"] = file
        return self.action("import_pages", **args)

    def import_pages_many(self, items: list[ImportPagesArgs]) -> list[dict[str, Any]]:
        """Runs `import_pages` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import_pages", items)

    def mark_text(self,
        *,
        p: Page,
        style: str,
        colour: str,
        rects: list[float],
    ) -> Mark:
        """Marks text on a page: a highlight, an underline, a strikethrough or a squiggle over the lines a selection covers, in the reader's colour."""
        return self.action("mark_text", p=p, style=style, colour=colour, rects=rects)

    def mark_selection(self,
        *,
        p: Page,
        style: str,
        colour: str,
        first: int,
        last: int,
    ) -> Mark:
        """Marks exactly the words a selection covers: the runs of `p` from `first` to `last`, both counted in, as a highlight, an underline, a strikethrough or a squiggle in the colour the reader holds. This is how a person marks lines of text they selected, the way the words are read; dragging a box still marks the words the box covers."""
        return self.action("mark_selection", p=p, style=style, colour=colour, first=first, last=last)

    def draw_ink(self,
        *,
        p: Page,
        colour: str,
        strokes: list[list[float]],
    ) -> Mark:
        """Draws ink on a page: one list of points a stroke, its points as x and y in turn in fractions of the page's box."""
        return self.action("draw_ink", p=p, colour=colour, strokes=strokes)

    def new_note(self,
        *,
        p: Page,
        x: float,
        y: float,
        colour: str,
    ) -> Mark:
        """Puts a sticky note on a page where the person pressed, with nothing written in it yet: what it says is written in the conversation it starts."""
        return self.action("new_note", p=p, x=x, y=y, colour=colour)

    def add_note(self,
        *,
        p: Page,
        words: str,
    ) -> None:
        """Adds a note with these words at the top of a page: for the palette, a script and an agent."""
        return self.action("add_note", p=p, words=words)

    def add_note_many(self, items: list[AddNoteArgs]) -> list[dict[str, Any]]:
        """Runs `add_note` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_note", items)

    def new_text_box(self,
        *,
        p: Page,
        x: float,
        y: float,
        width: float,
        height: float,
    ) -> Mark:
        """Puts a box of words on a page."""
        return self.action("new_text_box", p=p, x=x, y=y, width=width, height=height)

    def write_mark(self,
        *,
        m: Mark,
        words: str,
        rects: list[float] | None = None,
    ) -> None:
        """Changes what a mark says, and its box when one is named."""
        args: dict[str, Any] = {}
        args["m"] = m
        args["words"] = words
        if rects is not None:
            args["rects"] = rects
        return self.action("write_mark", **args)

    def write_mark_many(self, items: list[WriteMarkArgs]) -> list[dict[str, Any]]:
        """Runs `write_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("write_mark", items)

    def move_mark(self,
        *,
        m: Mark,
        x: float,
        y: float,
    ) -> None:
        """Puts a note where the person moved it."""
        return self.action("move_mark", m=m, x=x, y=y)

    def move_mark_many(self, items: list[MoveMarkArgs]) -> list[dict[str, Any]]:
        """Runs `move_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_mark", items)

    def recolour(self,
        *,
        m: Mark,
        colour: str,
    ) -> None:
        """Gives a mark another colour."""
        return self.action("recolour", m=m, colour=colour)

    def recolour_many(self, items: list[RecolourArgs]) -> list[dict[str, Any]]:
        """Runs `recolour` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("recolour", items)

    def delete_mark(self, *, m: Mark) -> None:
        """Takes a mark away, and what was said about it with it."""
        return self.action("delete_mark", m=m)

    def delete_mark_many(self, items: list[DeleteMarkArgs]) -> list[dict[str, Any]]:
        """Runs `delete_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_mark", items)

    def comment_on(self,
        *,
        p: Page,
        words: str,
        m: Mark | None = None,
    ) -> Comment:
        """Says something about a page, or about one of its marks."""
        args: dict[str, Any] = {}
        args["p"] = p
        if m is not None:
            args["m"] = m
        args["words"] = words
        return self.action("comment_on", **args)

    def reply(self,
        *,
        c: Comment,
        words: str,
    ) -> Reply:
        """Answers a comment."""
        return self.action("reply", c=c, words=words)

    def resolve_comment(self,
        *,
        c: Comment,
        resolved: bool,
    ) -> None:
        """Marks a conversation as resolved, or opens it again."""
        return self.action("resolve_comment", c=c, resolved=resolved)

    def resolve_comment_many(self, items: list[ResolveCommentArgs]) -> list[dict[str, Any]]:
        """Runs `resolve_comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("resolve_comment", items)

    def edit_comment(self,
        *,
        c: Comment,
        words: str,
    ) -> None:
        """Rewrites what a comment says."""
        return self.action("edit_comment", c=c, words=words)

    def edit_comment_many(self, items: list[EditCommentArgs]) -> list[dict[str, Any]]:
        """Runs `edit_comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("edit_comment", items)

    def delete_comment(self, *, c: Comment) -> None:
        """Takes a conversation away; a conversation about a sticky note takes the note with it when it is the last one about it."""
        return self.action("delete_comment", c=c)

    def delete_comment_many(self, items: list[DeleteCommentArgs]) -> list[dict[str, Any]]:
        """Runs `delete_comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_comment", items)

    def delete_reply(self,
        *,
        c: Comment,
        r: Reply,
    ) -> None:
        """Deletes one answer of a conversation."""
        return self.action("delete_reply", c=c, r=r)

    def delete_reply_many(self, items: list[DeleteReplyArgs]) -> list[dict[str, Any]]:
        """Runs `delete_reply` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_reply", items)

    def place_signature(self,
        *,
        p: Page,
        x: float,
        y: float,
        width: float | None = None,
        height: float | None = None,
    ) -> Signature:
        """Signs a page where the person pressed: the signature they drew, else their name set in the signature's box."""
        args: dict[str, Any] = {}
        args["p"] = p
        args["x"] = x
        args["y"] = y
        if width is not None:
            args["width"] = width
        if height is not None:
            args["height"] = height
        return self.action("place_signature", **args)

    def move_signature(self,
        *,
        s: Signature,
        x: float,
        y: float,
    ) -> None:
        """Puts a placed signature at another point of its page: its top-leading corner as fractions of the page's box. It is a change like any other, so everyone looking sees it move and one undo takes the move back."""
        return self.action("move_signature", s=s, x=x, y=y)

    def move_signature_many(self, items: list[MoveSignatureArgs]) -> list[dict[str, Any]]:
        """Runs `move_signature` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_signature", items)

    def move_placed_signature(self,
        *,
        p: Page,
        who: str,
        at: list[int],
    ) -> None:
        """Moves the signature `who` names, as the place over its page reports it: its new corner as whole points of the page."""
        return self.action("move_placed_signature", p=p, who=who, at=at)

    def move_placed_signature_many(self, items: list[MovePlacedSignatureArgs]) -> list[dict[str, Any]]:
        """Runs `move_placed_signature` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_placed_signature", items)

    def sign_with_name(self,
        *,
        p: Page,
        signer: str,
    ) -> None:
        """Signs a page with a typed name, in its lower right corner: for the palette, a script and an agent."""
        return self.action("sign_with_name", p=p, signer=signer)

    def sign_with_name_many(self, items: list[SignWithNameArgs]) -> list[dict[str, Any]]:
        """Runs `sign_with_name` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("sign_with_name", items)

    def delete_signature(self, *, s: Signature) -> None:
        """Takes a signature off the page."""
        return self.action("delete_signature", s=s)

    def delete_signature_many(self, items: list[DeleteSignatureArgs]) -> list[dict[str, Any]]:
        """Runs `delete_signature` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_signature", items)

    def fill_field(self,
        *,
        name: str,
        text: str,
    ) -> None:
        """Fills a form field with words."""
        return self.action("fill_field", name=name, text=text)

    def fill_field_many(self, items: list[FillFieldArgs]) -> list[dict[str, Any]]:
        """Runs `fill_field` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("fill_field", items)

    def choose_field(self,
        *,
        name: str,
        choice: str,
    ) -> None:
        """Chooses an option of a form's choice."""
        return self.action("choose_field", name=name, choice=choice)

    def choose_field_many(self, items: list[ChooseFieldArgs]) -> list[dict[str, Any]]:
        """Runs `choose_field` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("choose_field", items)

    def check_field(self,
        *,
        name: str,
        on: bool,
        export: str | None = None,
    ) -> None:
        """Turns a form's box on or off."""
        args: dict[str, Any] = {}
        args["name"] = name
        args["on"] = on
        if export is not None:
            args["export"] = export
        return self.action("check_field", **args)

    def check_field_many(self, items: list[CheckFieldArgs]) -> list[dict[str, Any]]:
        """Runs `check_field` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("check_field", items)

    def remember_reading(self,
        *,
        p: Page,
        fraction: float,
        zoom: float,
    ) -> None:
        """Remembers where the reader left off."""
        return self.action("remember_reading", p=p, fraction=fraction, zoom=zoom)

    def remember_reading_many(self, items: list[RememberReadingArgs]) -> list[dict[str, Any]]:
        """Runs `remember_reading` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remember_reading", items)

    def adopt_markup(self) -> None:
        """Takes over the mark-up the PDF's own bytes carry: its highlights, notes with their answers, free text and ink become this document's, once per source. The file's own objects are kept, so a copy saved with them shows each mark once."""
        return self.action("adopt_markup")

    def adopt_markup_many(self, items: list[AdoptMarkupArgs]) -> list[dict[str, Any]]:
        """Runs `adopt_markup` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("adopt_markup", items)

    def find_in(self, *, query: str) -> list[str]:
        """Finds the words `query` in the document's pages: each page that says them, with how often, as `Page 3: 2`. An agent reads a PDF's answer without the window."""
        return self.action("find_in", query=query)

    def copy_selected(self,
        *,
        p: Page,
        from_: int,
        to: int,
    ) -> None:
        """Copies the words a page shows between two runs, as selecting them and pressing Copy does."""
        args: dict[str, Any] = {}
        args["p"] = p
        args["from"] = from_
        args["to"] = to
        return self.action("copy_selected", **args)

    def copy_page(self, *, p: Page) -> None:
        """Copies every word a page shows."""
        return self.action("copy_page", p=p)

    def save_copy(self) -> Any:
        """Saves a copy of the PDF with the reader's mark-up in it: its pages in the reader's order, its marks, what was said about them, the form's values and the file's own mark-up kept where nobody changed it."""
        return self.action("save_copy")

    def turn_to(self,
        *,
        at: int,
        zoom: float,
    ) -> None:
        """Remembers that the reader turned to the page at `at`."""
        return self.action("turn_to", at=at, zoom=zoom)

    def turn_to_many(self, items: list[TurnToArgs]) -> list[dict[str, Any]]:
        """Runs `turn_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("turn_to", items)

    def reorder(self,
        *,
        index: int,
        at: int,
    ) -> None:
        """Puts the page at `index` where the rail dropped it: after the page at `at`."""
        return self.action("reorder", index=index, at=at)

    def reorder_many(self, items: list[ReorderArgs]) -> list[dict[str, Any]]:
        """Runs `reorder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reorder", items)

    def import_after(self,
        *,
        file: Any,
        p: Page | None = None,
    ) -> Document:
        """Imports pages after `p`: what the rail's add asks the person for."""
        args: dict[str, Any] = {}
        if p is not None:
            args["p"] = p
        args["file"] = file
        return self.action("import_after", **args)

    def press(self,
        *,
        p: Page,
        at: list[float],
        tool: str,
        colour: str,
    ) -> None:
        """What pressing on a page does, by the tool the window holds: places a note, a text box or a signature where the person pressed. A marking tool starts a selection instead, which the window keeps."""
        return self.action("press", p=p, at=at, tool=tool, colour=colour)

    def press_many(self, items: list[PressArgs]) -> list[dict[str, Any]]:
        """Runs `press` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("press", items)

    def release(self,
        *,
        p: Page,
        tool: str,
        colour: str,
        stroke: list[float],
        start: list[float] | None = None,
        end: list[float] | None = None,
    ) -> None:
        """What letting go does: ink keeps the stroke drawn, and a marking tool marks the words the box covers, or the box itself where the page's words are not read."""
        args: dict[str, Any] = {}
        args["p"] = p
        args["tool"] = tool
        args["colour"] = colour
        if start is not None:
            args["start"] = start
        if end is not None:
            args["end"] = end
        args["stroke"] = stroke
        return self.action("release", **args)

    def release_many(self, items: list[ReleaseArgs]) -> list[dict[str, Any]]:
        """Runs `release` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("release", items)

    def speak(self, *, words: str) -> Comment | None:
        """Says something about the page the window shows."""
        return self.action("speak", words=words)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"lay_out", "lay_out_many", "read_outline", "read_outline_many", "read_fields", "read_fields_many", "read_page", "read_page_many", "read_links", "read_links_many", "show_page", "show_page_many", "read_next", "read_next_many", "draw_next", "draw_next_many", "rotate_page", "rotate_page_many", "move_page", "move_page_many", "duplicate_page", "delete_page", "delete_page_many", "import_pages", "import_pages_many", "mark_text", "mark_selection", "draw_ink", "new_note", "add_note", "add_note_many", "new_text_box", "write_mark", "write_mark_many", "move_mark", "move_mark_many", "recolour", "recolour_many", "delete_mark", "delete_mark_many", "comment_on", "reply", "resolve_comment", "resolve_comment_many", "edit_comment", "edit_comment_many", "delete_comment", "delete_comment_many", "delete_reply", "delete_reply_many", "place_signature", "move_signature", "move_signature_many", "move_placed_signature", "move_placed_signature_many", "sign_with_name", "sign_with_name_many", "delete_signature", "delete_signature_many", "fill_field", "fill_field_many", "choose_field", "choose_field_many", "check_field", "check_field_many", "remember_reading", "remember_reading_many", "adopt_markup", "adopt_markup_many", "find_in", "copy_selected", "copy_page", "save_copy", "turn_to", "turn_to_many", "reorder", "reorder_many", "import_after", "press", "press_many", "release", "release_many", "speak"})


class Page(Resource):
    """Page — `tl.so.tilo.pdf-viewer.Page`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    oedigi3h: int  # source_page
    vphbhfba: str  # imported
    @property
    def _5c4rqnpi(self) -> str:
        """kind (`5c4rqnpi` on the wire)."""
        return self.fields["5c4rqnpi"]

    @_5c4rqnpi.setter
    def _5c4rqnpi(self, value: str) -> None:
        self.edit("5c4rqnpi", value=value)
    qxyy4u3k: float  # width
    bepcv7nh: float  # height
    vlm7goob: int  # rotation
    fwoikm62: str  # words
    qko77qye: list[float]  # runs
    mxsaptcd: list[str]  # run_words
    kdrgc55v: bool  # read


class Picture(Resource):
    """Picture — `tl.so.tilo.pdf-viewer.Picture`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    e5fx4rry: Page  # page
    lps7jwgj: int  # width
    ht6kgmaq: str  # url


class Outline(Resource):
    """Outline — `tl.so.tilo.pdf-viewer.Outline`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    qfml2pla: str  # title
    @property
    def _34ukafjp(self) -> int:
        """page (`34ukafjp` on the wire)."""
        return self.fields["34ukafjp"]

    @_34ukafjp.setter
    def _34ukafjp(self, value: int) -> None:
        self.edit("34ukafjp", value=value)
    v7hjvwyk: int  # level


class Link(Resource):
    """Link — `tl.so.tilo.pdf-viewer.Link`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    xivruibf: Page  # page
    sepq4iwp: list[float]  # rect
    yfuvjgjt: str  # kind
    lzpsucsi: str  # to


class Field(Resource):
    """Field — `tl.so.tilo.pdf-viewer.Field`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    @property
    def _5ghgpzwq(self) -> str:
        """name (`5ghgpzwq` on the wire)."""
        return self.fields["5ghgpzwq"]

    @_5ghgpzwq.setter
    def _5ghgpzwq(self, value: str) -> None:
        self.edit("5ghgpzwq", value=value)
    @property
    def _4mifdrqw(self) -> str:
        """kind (`4mifdrqw` on the wire)."""
        return self.fields["4mifdrqw"]

    @_4mifdrqw.setter
    def _4mifdrqw(self, value: str) -> None:
        self.edit("4mifdrqw", value=value)
    enpzi5ta: str  # value
    f6evcr4f: bool  # checked
    jjifolfg: str  # export
    ya7ugvvf: list[str]  # options
    h2kxabyf: int  # page
    ijs3t4gj: list[float]  # rect
    eh3gs42n: bool  # read_only
    lxh5hh6y: bool  # required


class Mark(Resource):
    """Mark — `tl.so.tilo.pdf-viewer.Mark`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    @property
    def _4x7a7juf(self) -> Page:
        """page (`4x7a7juf` on the wire)."""
        return self.fields["4x7a7juf"]

    @_4x7a7juf.setter
    def _4x7a7juf(self, value: Page) -> None:
        self.edit("4x7a7juf", value=value)
    n23wswru: MarkKind  # kind
    ikltq7t2: str  # colour
    @property
    def _35epyc2h(self) -> str:
        """style (`35epyc2h` on the wire)."""
        return self.fields["35epyc2h"]

    @_35epyc2h.setter
    def _35epyc2h(self, value: str) -> None:
        self.edit("35epyc2h", value=value)
    qtn6mlbl: list[float]  # rects
    crim3nye: float  # x
    @property
    def _3rusqrrq(self) -> float:
        """y (`3rusqrrq` on the wire)."""
        return self.fields["3rusqrrq"]

    @_3rusqrrq.setter
    def _3rusqrrq(self, value: float) -> None:
        self.edit("3rusqrrq", value=value)
    agxxncq4: str  # words
    xt4alsjd: float  # size
    fz2kryor: list[Entry[Stroke]]  # strokes
    @property
    def _2a5limjf(self) -> str:
        """author (`2a5limjf` on the wire)."""
        return self.fields["2a5limjf"]

    @_2a5limjf.setter
    def _2a5limjf(self, value: str) -> None:
        self.edit("2a5limjf", value=value)
    fkkbarxd: str  # adopted_id


class Stroke(Resource):
    """Stroke — `tl.so.tilo.pdf-viewer.Stroke`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    @property
    def _4dzcakfh(self) -> list[float]:
        """points (`4dzcakfh` on the wire)."""
        return self.fields["4dzcakfh"]

    @_4dzcakfh.setter
    def _4dzcakfh(self, value: list[float]) -> None:
        self.edit("4dzcakfh", value=value)


class Comment(Resource):
    """Comment — `tl.so.tilo.pdf-viewer.Comment`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    e5qxkctu: Mark | None  # mark
    btdnd5pa: Page  # page
    ogqe2kdm: str  # words
    hcfcbfso: str  # body
    hgps2s5i: bool  # resolved
    thfbylkn: list[Entry[Reply]]  # replies

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

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"edit_block", "edit_block_many", "insert_block", "insert_block_many", "delete_block", "delete_block_many"})


class Reply(Resource):
    """Reply — `tl.so.tilo.pdf-viewer.Reply`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    @property
    def _47zrerme(self) -> str:
        """words (`47zrerme` on the wire)."""
        return self.fields["47zrerme"]

    @_47zrerme.setter
    def _47zrerme(self, value: str) -> None:
        self.edit("47zrerme", value=value)
    exvjyq6h: str  # body

    def edit_block(self,
        *,
        block: str,
        text: str,
    ) -> None:
        """Set one paragraph of a rich-text field to what a Markdown text says."""
        return self.action("edit-block", block=block, text=text)

    def edit_block_many(self, items: list[ReplyEditBlockArgs]) -> list[dict[str, Any]]:
        """Runs `edit-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("edit-block", items)

    def insert_block(self,
        *,
        block: str,
        text: str,
    ) -> None:
        """Put a paragraph before the one the address names."""
        return self.action("insert-block", block=block, text=text)

    def insert_block_many(self, items: list[ReplyInsertBlockArgs]) -> list[dict[str, Any]]:
        """Runs `insert-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("insert-block", items)

    def delete_block(self, *, block: str) -> None:
        """Remove the paragraph the address names, and the paragraphs it holds."""
        return self.action("delete-block", block=block)

    def delete_block_many(self, items: list[ReplyDeleteBlockArgs]) -> list[dict[str, Any]]:
        """Runs `delete-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-block", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"edit_block", "edit_block_many", "insert_block", "insert_block_many", "delete_block", "delete_block_many"})


class Signature(Resource):
    """Signature — `tl.so.tilo.pdf-viewer.Signature`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    xudq4utx: Page  # page
    uyofemys: float  # x
    mai2dzre: float  # y
    tvp2usjk: float  # width
    iriyo77n: float  # height
    ozrikra3: str  # signer
    k6r3zcgg: str  # date
    @property
    def _5pbiosiz(self) -> list[Entry[Stroke]]:
        """strokes (`5pbiosiz` on the wire)."""
        return self.fields["5pbiosiz"]

    @_5pbiosiz.setter
    def _5pbiosiz(self, value: list[Entry[Stroke]]) -> None:
        self.edit("5pbiosiz", value=value)


class Library(Resource):
    """Library — `tl.so.tilo.pdf-viewer.Library`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    bqmqo56y: list[Entry[Document]]  # documents
    u2q7brpp: str  # signer
    bny56wzk: list[Entry[Stroke]]  # strokes

    def open_pdf(self, *, file: Any) -> Document:
        """A PDF opens in the reader: Files, the person's own file system and the system's search hand it over as the file it is, and it opens on the document those bytes already are."""
        return self.action("open_pdf", file=file)

    def open_source(self,
        *,
        address: str,
        name: str | None = None,
    ) -> Document:
        """Opens the PDF whose bytes the workspace keeps at `address`: the document those bytes are if one holds them, else a new one. How an agent, a link or another app opens a PDF."""
        args: dict[str, Any] = {}
        args["address"] = address
        if name is not None:
            args["name"] = name
        return self.action("open_source", **args)

    def sign_as(self, *, name: str) -> None:
        """The name typed signatures carry from now on."""
        return self.action("sign_as", name=name)

    def sign_as_many(self, items: list[SignAsArgs]) -> list[dict[str, Any]]:
        """Runs `sign_as` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("sign_as", items)

    def draw_signature(self, *, strokes: list[list[float]]) -> None:
        """Keeps the signature a person drew, so the next document is signed with one press: the strokes drawn on the pad, x and y in turn in fractions of the signature's box."""
        return self.action("draw_signature", strokes=strokes)

    def draw_signature_many(self, items: list[DrawSignatureArgs]) -> list[dict[str, Any]]:
        """Runs `draw_signature` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("draw_signature", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"open_pdf", "open_source", "sign_as", "sign_as_many", "draw_signature", "draw_signature_many"})


class Runs(Resource):
    """Runs — `tl.so.tilo.pdf-viewer.Runs`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    fmkeszhi: list[float]  # rects
    awfhznwx: list[str]  # texts


class PageRow(Resource):
    """PageRow — `tl.so.tilo.pdf-viewer.PageRow`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    @property
    def _2xoqlfit(self) -> int:
        """page (`2xoqlfit` on the wire)."""
        return self.fields["2xoqlfit"]

    @_2xoqlfit.setter
    def _2xoqlfit(self, value: int) -> None:
        self.edit("2xoqlfit", value=value)
    hsgsnasu: float  # width
    vmsxv3ma: float  # height


class GlyphRow(Resource):
    """GlyphRow — `tl.so.tilo.pdf-viewer.GlyphRow`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    e35nwhcd: str  # text
    owthh7c4: int  # line
    at7kirp3: float  # x
    w53k2oqb: float  # y
    @property
    def _7ufljz44(self) -> float:
        """w (`7ufljz44` on the wire)."""
        return self.fields["7ufljz44"]

    @_7ufljz44.setter
    def _7ufljz44(self, value: float) -> None:
        self.edit("7ufljz44", value=value)
    showgoq3: float  # h
    @property
    def _5u27ffde(self) -> bool:
        """placed (`5u27ffde` on the wire)."""
        return self.fields["5u27ffde"]

    @_5u27ffde.setter
    def _5u27ffde(self, value: bool) -> None:
        self.edit("5u27ffde", value=value)


class ImportRow(Resource):
    """ImportRow — `tl.so.tilo.pdf-viewer.ImportRow`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    b35hfv2r: str  # kind
    m2v5uip7: list[PageRow]  # pages


class OutlineRow(Resource):
    """OutlineRow — `tl.so.tilo.pdf-viewer.OutlineRow`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    rl5gr6nc: str  # title
    mtayx6nb: int  # page
    ewudh3r2: int  # level


class MarkRow(Resource):
    """MarkRow — `tl.so.tilo.pdf-viewer.MarkRow`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    s3btzxsu: int  # page
    @property
    def _5x3rjw33(self) -> int:
        """index (`5x3rjw33` on the wire)."""
        return self.fields["5x3rjw33"]

    @_5x3rjw33.setter
    def _5x3rjw33(self, value: int) -> None:
        self.edit("5x3rjw33", value=value)
    l6esysss: str  # name
    i3qsks26: str  # kind
    qca4t246: str | None  # color
    @property
    def _5ddf4ghi(self) -> str:
        """contents (`5ddf4ghi` on the wire)."""
        return self.fields["5ddf4ghi"]

    @_5ddf4ghi.setter
    def _5ddf4ghi(self, value: str) -> None:
        self.edit("5ddf4ghi", value=value)
    ejgwsaae: str  # author
    @property
    def _4chyadse(self) -> str:
        """created (`4chyadse` on the wire)."""
        return self.fields["4chyadse"]

    @_4chyadse.setter
    def _4chyadse(self, value: str) -> None:
        self.edit("4chyadse", value=value)
    sw7xlda5: str  # modified
    xsmehh7m: list[float]  # rect
    v7q3ceeu: list[list[float]]  # quads
    pv225oqp: list[list[float]]  # strokes
    yrxpeq4o: float | None  # size
    xkarfjs4: int | None  # in_reply_to
    rwsq3hzc: bool  # reply
    udw6i6fp: bool  # hidden
    nuljshqh: bool  # readable


class MarkupRow(Resource):
    """MarkupRow — `tl.so.tilo.pdf-viewer.MarkupRow`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    @property
    def _6cyewmj4(self) -> list[MarkRow]:
        """marks (`6cyewmj4` on the wire)."""
        return self.fields["6cyewmj4"]

    @_6cyewmj4.setter
    def _6cyewmj4(self, value: list[MarkRow]) -> None:
        self.edit("6cyewmj4", value=value)
    dk6bxvq5: list[FieldRow]  # fields


class FieldRow(Resource):
    """FieldRow — `tl.so.tilo.pdf-viewer.FieldRow`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    havty2vp: str  # name
    d4cnpeq3: str  # kind
    br4tfro7: str  # value
    jyx27y2l: bool  # checked
    tfvicybt: str  # export
    @property
    def _66vdxlss(self) -> list[str]:
        """options (`66vdxlss` on the wire)."""
        return self.fields["66vdxlss"]

    @_66vdxlss.setter
    def _66vdxlss(self, value: list[str]) -> None:
        self.edit("66vdxlss", value=value)
    puoybohy: int | None  # page
    ip3ujn6w: list[float] | None  # rect
    mneasolx: bool  # read_only
    i46gksbq: bool  # required


class PageOut(Resource):
    """PageOut — `tl.so.tilo.pdf-viewer.PageOut`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    d56ntmtj: int  # source
    @property
    def _5y2litv2(self) -> int:
        """page (`5y2litv2` on the wire)."""
        return self.fields["5y2litv2"]

    @_5y2litv2.setter
    def _5y2litv2(self, value: int) -> None:
        self.edit("5y2litv2", value=value)
    hgb4qbox: int  # rotation
    oaf2yyke: bool  # primary


class MarkOut(Resource):
    """MarkOut — `tl.so.tilo.pdf-viewer.MarkOut`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    qlypcvpd: int  # page
    jfdwouoh: str  # kind
    @property
    def _4aptjhj3(self) -> str | None:
        """color (`4aptjhj3` on the wire)."""
        return self.fields["4aptjhj3"]

    @_4aptjhj3.setter
    def _4aptjhj3(self, value: str | None) -> None:
        self.edit("4aptjhj3", value=value)
    @property
    def _5vi7wvna(self) -> str:
        """author (`5vi7wvna` on the wire)."""
        return self.fields["5vi7wvna"]

    @_5vi7wvna.setter
    def _5vi7wvna(self, value: str) -> None:
        self.edit("5vi7wvna", value=value)
    wt44cnbi: str  # contents
    cv6deozk: str  # name
    uzy5hqqe: int  # created_ms
    qnpoauzu: list[list[float]]  # rects
    ilro56v5: list[list[float]]  # strokes
    slisyq2r: list[float] | None  # rect
    u3zrbin4: float | None  # size


class ReplyOut(Resource):
    """ReplyOut — `tl.so.tilo.pdf-viewer.ReplyOut`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    @property
    def _7ckypqja(self) -> int:
        """page (`7ckypqja` on the wire)."""
        return self.fields["7ckypqja"]

    @_7ckypqja.setter
    def _7ckypqja(self, value: int) -> None:
        self.edit("7ckypqja", value=value)
    ksvfd5se: float  # x
    g6nqwvtx: float  # y
    qs6kxe3w: str  # author
    vqovoaba: str  # body
    kcbfphy7: str  # name
    qdwom5mm: str  # answers


class ValueOut(Resource):
    """ValueOut — `tl.so.tilo.pdf-viewer.ValueOut`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    @property
    def _4uvljgge(self) -> str:
        """name (`4uvljgge` on the wire)."""
        return self.fields["4uvljgge"]

    @_4uvljgge.setter
    def _4uvljgge(self, value: str) -> None:
        self.edit("4uvljgge", value=value)
    tlqdrzox: str  # kind
    fp6projh: str  # text
    oquhk6ja: bool  # on
    mlfgloi5: str | None  # export


class AdoptedOut(Resource):
    """AdoptedOut — `tl.so.tilo.pdf-viewer.AdoptedOut`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    yzggzvtt: int  # page
    jxgttint: int  # index
    k3tlktwg: str  # name
    ua2gidzw: str  # kind
    jlsyhy65: bool  # unchanged


class SaveRequest(Resource):
    """SaveRequest — `tl.so.tilo.pdf-viewer.SaveRequest`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    l5cfhg2u: list[PageOut]  # pages
    egrqnnmk: list[MarkOut]  # marks
    fy7mbgs7: list[ReplyOut]  # replies
    q2jlnwmi: list[ValueOut]  # values
    igdolmln: list[AdoptedOut]  # adopted


class MarkColour(Resource):
    """MarkColour — `tl.so.tilo.pdf-viewer.MarkColour`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"
    yr5hwqqr: str  # name
    fmr3zdmh: str  # value


class PdfViewer(Resource):
    """PDF Viewer — `tl.so.tilo.pdf-viewer`."""
    __surface__ = "so.tilo.pdf-viewer@85261215fd825975"

