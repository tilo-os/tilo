"""Types for Books (`so.tilo.ebook`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `ebook/<resource>`, read with
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
    "Book",
    "Chapter",
    "Mark",
    "Progress",
    "Scheme",
    "Font",
    "Found",
    "Watched",
    "Library",
    "Highlight",
    "Ebook",
    "RemoveBookArgs",
    "SetTitleArgs",
    "SetAuthorArgs",
    "SetSubjectArgs",
    "SetLanguageArgs",
    "SetCoverArgs",
    "ClearCoverArgs",
    "RecordAtArgs",
    "ReadChapterArgs",
    "ShowPageArgs",
    "ShowPassageArgs",
    "ShowPlaceArgs",
    "SawBlocksArgs",
    "SetFinishedArgs",
    "HighlightArgs",
    "AddNoteArgs",
    "ToggleBookmarkArgs",
    "CommentArgs",
    "ChangeColorArgs",
    "EditMarkTextArgs",
    "DeleteMarkArgs",
    "EditBlockArgs",
    "InsertBlockArgs",
    "DeleteBlockArgs",
    "MarkEditBlockArgs",
    "MarkInsertBlockArgs",
    "MarkDeleteBlockArgs",
    "DropBooksArgs",
    "WatchFolderArgs",
    "ScanFolderArgs",
    "FolderedArgs",
    "SetTextSizeArgs",
    "SetLineSpacingArgs",
    "SetFaceArgs",
    "AddFontArgs",
    "RenameFontArgs",
    "SetFontSizeArgs",
    "SetFontShownArgs",
    "RemoveFontArgs",
    "SetJustifiedArgs",
    "SetShelfSortArgs",
    "SetHideFinishedArgs",
    "CycleSchemeArgs",
    "SetSchemeArgs",
    "AddSchemeArgs",
    "DeleteSchemeArgs",
    "FindBooksArgs",
    "ClearFoundArgs",
]

RemoveBookArgs = TypedDict("RemoveBookArgs", {
})
"""The arguments of `remove_book`."""

SetTitleArgs = TypedDict("SetTitleArgs", {
    "title": "str",
})
"""The arguments of `set_title`."""

SetAuthorArgs = TypedDict("SetAuthorArgs", {
    "author": "str",
})
"""The arguments of `set_author`."""

SetSubjectArgs = TypedDict("SetSubjectArgs", {
    "subject": "str",
})
"""The arguments of `set_subject`."""

SetLanguageArgs = TypedDict("SetLanguageArgs", {
    "language": "str",
})
"""The arguments of `set_language`."""

SetCoverArgs = TypedDict("SetCoverArgs", {
    "picture": "Any",
})
"""The arguments of `set_cover`."""

ClearCoverArgs = TypedDict("ClearCoverArgs", {
})
"""The arguments of `clear_cover`."""

RecordAtArgs = TypedDict("RecordAtArgs", {
    "who": "str",
    "chapter": "int",
    "block": "int",
    "page": "int",
    "place": "float",
})
"""The arguments of `record_at`."""

ReadChapterArgs = TypedDict("ReadChapterArgs", {
    "who": "str",
    "c": "Chapter",
})
"""The arguments of `read_chapter`."""

ShowPageArgs = TypedDict("ShowPageArgs", {
    "who": "str",
    "chapter": "int",
    "block": "int",
    "page": "int",
})
"""The arguments of `show_page`."""

ShowPassageArgs = TypedDict("ShowPassageArgs", {
    "who": "str",
    "chapter": "int",
    "block": "int",
})
"""The arguments of `show_passage`."""

ShowPlaceArgs = TypedDict("ShowPlaceArgs", {
    "who": "str",
    "chapter": "int",
    "block": "int",
    "page": "int",
    "place": "float",
})
"""The arguments of `show_place`."""

SawBlocksArgs = TypedDict("SawBlocksArgs", {
    "who": "str",
    "blocks": "list[int]",
})
"""The arguments of `saw_blocks`."""

SetFinishedArgs = TypedDict("SetFinishedArgs", {
    "who": "str",
    "finished": NotRequired["bool | None"],
})
"""The arguments of `set_finished`."""

HighlightArgs = TypedDict("HighlightArgs", {
    "chapter": "int",
    "ranges": "list[int]",
    "color": "str",
})
"""The arguments of `highlight`."""

AddNoteArgs = TypedDict("AddNoteArgs", {
    "chapter": "int",
    "ranges": "list[int]",
    "color": "str",
    "text": "str",
})
"""The arguments of `add_note`."""

ToggleBookmarkArgs = TypedDict("ToggleBookmarkArgs", {
    "chapter": "int",
    "block": "int",
})
"""The arguments of `toggle_bookmark`."""

CommentArgs = TypedDict("CommentArgs", {
    "m": "Mark",
    "text": "str",
})
"""The arguments of `comment`."""

ChangeColorArgs = TypedDict("ChangeColorArgs", {
    "m": "Mark",
    "color": "str",
})
"""The arguments of `change_color`."""

EditMarkTextArgs = TypedDict("EditMarkTextArgs", {
    "m": "Mark",
    "text": "str",
})
"""The arguments of `edit_mark_text`."""

DeleteMarkArgs = TypedDict("DeleteMarkArgs", {
    "m": "Mark",
})
"""The arguments of `delete_mark`."""

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

MarkEditBlockArgs = TypedDict("MarkEditBlockArgs", {
    "block": "str",
    "text": "str",
})
"""The arguments of `edit-block`."""

MarkInsertBlockArgs = TypedDict("MarkInsertBlockArgs", {
    "block": "str",
    "text": "str",
})
"""The arguments of `insert-block`."""

MarkDeleteBlockArgs = TypedDict("MarkDeleteBlockArgs", {
    "block": "str",
})
"""The arguments of `delete-block`."""

DropBooksArgs = TypedDict("DropBooksArgs", {
    "files": "list[Any]",
})
"""The arguments of `drop_books`."""

WatchFolderArgs = TypedDict("WatchFolderArgs", {
    "path": "str",
})
"""The arguments of `watch_folder`."""

ScanFolderArgs = TypedDict("ScanFolderArgs", {
    "path": "str",
})
"""The arguments of `scan_folder`."""

FolderedArgs = TypedDict("FolderedArgs", {
})
"""The arguments of `foldered`."""

SetTextSizeArgs = TypedDict("SetTextSizeArgs", {
    "size": "float",
})
"""The arguments of `set_text_size`."""

SetLineSpacingArgs = TypedDict("SetLineSpacingArgs", {
    "spacing": "float",
})
"""The arguments of `set_line_spacing`."""

SetFaceArgs = TypedDict("SetFaceArgs", {
    "face": "str",
})
"""The arguments of `set_face`."""

AddFontArgs = TypedDict("AddFontArgs", {
    "family": "str",
})
"""The arguments of `add_font`."""

RenameFontArgs = TypedDict("RenameFontArgs", {
    "f": "Font",
    "name": "str",
})
"""The arguments of `rename_font`."""

SetFontSizeArgs = TypedDict("SetFontSizeArgs", {
    "f": "Font",
    "size": "float",
})
"""The arguments of `set_font_size`."""

SetFontShownArgs = TypedDict("SetFontShownArgs", {
    "f": "Font",
    "shown": "bool",
})
"""The arguments of `set_font_shown`."""

RemoveFontArgs = TypedDict("RemoveFontArgs", {
    "f": "Font",
})
"""The arguments of `remove_font`."""

SetJustifiedArgs = TypedDict("SetJustifiedArgs", {
    "on": "bool",
})
"""The arguments of `set_justified`."""

SetShelfSortArgs = TypedDict("SetShelfSortArgs", {
    "order": "str",
})
"""The arguments of `set_shelf_sort`."""

SetHideFinishedArgs = TypedDict("SetHideFinishedArgs", {
    "hidden": "bool",
})
"""The arguments of `set_hide_finished`."""

CycleSchemeArgs = TypedDict("CycleSchemeArgs", {
})
"""The arguments of `cycle_scheme`."""

SetSchemeArgs = TypedDict("SetSchemeArgs", {
    "name": "str",
})
"""The arguments of `set_scheme`."""

AddSchemeArgs = TypedDict("AddSchemeArgs", {
    "name": "str",
    "page": "str",
    "ink": "str",
})
"""The arguments of `add_scheme`."""

DeleteSchemeArgs = TypedDict("DeleteSchemeArgs", {
    "scheme": "Scheme",
})
"""The arguments of `delete_scheme`."""

FindBooksArgs = TypedDict("FindBooksArgs", {
    "query": "str",
})
"""The arguments of `find_books`."""

ClearFoundArgs = TypedDict("ClearFoundArgs", {
})
"""The arguments of `clear_found`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.ebook@0cedf6b7fcebc8f1"
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


class Book(Resource):
    """Book — `tl.so.tilo.ebook.Book`."""
    __surface__ = "so.tilo.ebook@0cedf6b7fcebc8f1"
    zzi6jbdk: str  # title
    hzsemqyf: str  # author
    ywdncocc: str  # subject
    nczhbvr6: str  # language
    uufjkgr3: str  # source
    t7fdn6od: str  # cover
    sthxrdo4: str  # added
    s4t4ogtx: list[Entry[Chapter]]  # chapters
    kckryjj5: list[Entry[Mark]]  # marks
    y3qlwotc: list[Entry[Progress]]  # progress

    def remove_book(self) -> None:
        """Takes a book off the shelf, its chapters and marks with it. Reading it again adds it again from its file."""
        return self.action("remove_book")

    def remove_book_many(self, items: list[RemoveBookArgs]) -> list[dict[str, Any]]:
        """Runs `remove_book` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_book", items)

    def set_title(self, *, title: str) -> None:
        """Gives `b` the title a person reads it by."""
        return self.action("set_title", title=title)

    def set_title_many(self, items: list[SetTitleArgs]) -> list[dict[str, Any]]:
        """Runs `set_title` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_title", items)

    def set_author(self, *, author: str) -> None:
        """Gives `b` its author, or leaves it without one."""
        return self.action("set_author", author=author)

    def set_author_many(self, items: list[SetAuthorArgs]) -> list[dict[str, Any]]:
        """Runs `set_author` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_author", items)

    def set_subject(self, *, subject: str) -> None:
        """Gives `b` the subject line its card and its shelf filters say."""
        return self.action("set_subject", subject=subject)

    def set_subject_many(self, items: list[SetSubjectArgs]) -> list[dict[str, Any]]:
        """Runs `set_subject` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_subject", items)

    def set_language(self, *, language: str) -> None:
        """Sets the language `b` is set in, for its hyphenation."""
        return self.action("set_language", language=language)

    def set_language_many(self, items: list[SetLanguageArgs]) -> list[dict[str, Any]]:
        """Runs `set_language` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_language", items)

    def set_cover(self, *, picture: Any) -> None:
        """Puts `picture` on `b` as its cover."""
        return self.action("set_cover", picture=picture)

    def set_cover_many(self, items: list[SetCoverArgs]) -> list[dict[str, Any]]:
        """Runs `set_cover` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_cover", items)

    def clear_cover(self) -> None:
        """Takes a cover off `b`, so its card draws the book's own art."""
        return self.action("clear_cover")

    def clear_cover_many(self, items: list[ClearCoverArgs]) -> list[dict[str, Any]]:
        """Runs `clear_cover` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_cover", items)

    def record_at(self,
        *,
        who: str,
        chapter: int,
        block: int,
        page: int,
        place: float,
    ) -> None:
        """Where the person reading is in `b`: their own record, changed in place so its history says where they read and when."""
        return self.action("record_at", who=who, chapter=chapter, block=block, page=page, place=place)

    def record_at_many(self, items: list[RecordAtArgs]) -> list[dict[str, Any]]:
        """Runs `record_at` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("record_at", items)

    def read_chapter(self,
        *,
        who: str,
        c: Chapter,
    ) -> None:
        """Opens `c` in `b`: the contents and where the person is are kept together."""
        return self.action("read_chapter", who=who, c=c)

    def read_chapter_many(self, items: list[ReadChapterArgs]) -> list[dict[str, Any]]:
        """Runs `read_chapter` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("read_chapter", items)

    def show_page(self,
        *,
        who: str,
        chapter: int,
        block: int,
        page: int,
    ) -> None:
        """Shows page `page` of the book's open chapter, counted from 0 (a spread in the two-page mode)."""
        return self.action("show_page", who=who, chapter=chapter, block=block, page=page)

    def show_page_many(self, items: list[ShowPageArgs]) -> list[dict[str, Any]]:
        """Runs `show_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show_page", items)

    def show_passage(self,
        *,
        who: str,
        chapter: int,
        block: int,
    ) -> None:
        """Shows the first page of the chapter `chapter` in `b`: a link to a passage lands on it."""
        return self.action("show_passage", who=who, chapter=chapter, block=block)

    def show_passage_many(self, items: list[ShowPassageArgs]) -> list[dict[str, Any]]:
        """Runs `show_passage` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show_passage", items)

    def show_place(self,
        *,
        who: str,
        chapter: int,
        block: int,
        page: int,
        place: float,
    ) -> None:
        """How far down the column the person scrolled."""
        return self.action("show_place", who=who, chapter=chapter, block=block, page=page, place=place)

    def show_place_many(self, items: list[ShowPlaceArgs]) -> list[dict[str, Any]]:
        """Runs `show_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show_place", items)

    def saw_blocks(self,
        *,
        who: str,
        blocks: list[int],
    ) -> None:
        """Notes the block the page begins at as the person scrolls the column, so a bookmark lands where they are."""
        return self.action("saw_blocks", who=who, blocks=blocks)

    def saw_blocks_many(self, items: list[SawBlocksArgs]) -> list[dict[str, Any]]:
        """Runs `saw_blocks` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("saw_blocks", items)

    def set_finished(self,
        *,
        who: str,
        finished: bool | None = None,
    ) -> None:
        """Marks `b` finished for the person reading it, or starts it again."""
        args: dict[str, Any] = {}
        args["who"] = who
        if finished is not None:
            args["finished"] = finished
        return self.action("set_finished", **args)

    def set_finished_many(self, items: list[SetFinishedArgs]) -> list[dict[str, Any]]:
        """Runs `set_finished` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_finished", items)

    def highlight(self,
        *,
        chapter: int,
        ranges: list[int],
        color: str,
    ) -> None:
        """Highlights the passage of `b`'s open chapter: its ranges, each `block, start, end`."""
        return self.action("highlight", chapter=chapter, ranges=ranges, color=color)

    def highlight_many(self, items: list[HighlightArgs]) -> list[dict[str, Any]]:
        """Runs `highlight` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("highlight", items)

    def add_note(self,
        *,
        chapter: int,
        ranges: list[int],
        color: str,
        text: str,
    ) -> None:
        """Writes a note on the passage of `b`'s open chapter."""
        return self.action("add_note", chapter=chapter, ranges=ranges, color=color, text=text)

    def add_note_many(self, items: list[AddNoteArgs]) -> list[dict[str, Any]]:
        """Runs `add_note` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_note", items)

    def toggle_bookmark(self,
        *,
        chapter: int,
        block: int,
    ) -> None:
        """Bookmarks where the page `block` begins in `b`'s open chapter, or takes the bookmark off again."""
        return self.action("toggle_bookmark", chapter=chapter, block=block)

    def toggle_bookmark_many(self, items: list[ToggleBookmarkArgs]) -> list[dict[str, Any]]:
        """Runs `toggle_bookmark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("toggle_bookmark", items)

    def comment(self,
        *,
        m: Mark,
        text: str,
    ) -> None:
        """Comments on the highlight `m`, in the book `b`."""
        return self.action("comment", m=m, text=text)

    def comment_many(self, items: list[CommentArgs]) -> list[dict[str, Any]]:
        """Runs `comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("comment", items)

    def change_color(self,
        *,
        m: Mark,
        color: str,
    ) -> None:
        """Recolours a highlight, and the notes on it."""
        return self.action("change_color", m=m, color=color)

    def change_color_many(self, items: list[ChangeColorArgs]) -> list[dict[str, Any]]:
        """Runs `change_color` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("change_color", items)

    def edit_mark_text(self,
        *,
        m: Mark,
        text: str,
    ) -> None:
        """Rewrites what a note or a comment says. Only its author may."""
        return self.action("edit_mark_text", m=m, text=text)

    def edit_mark_text_many(self, items: list[EditMarkTextArgs]) -> list[dict[str, Any]]:
        """Runs `edit_mark_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("edit_mark_text", items)

    def delete_mark(self, *, m: Mark) -> None:
        """Deletes a mark, and the comments on it."""
        return self.action("delete_mark", m=m)

    def delete_mark_many(self, items: list[DeleteMarkArgs]) -> list[dict[str, Any]]:
        """Runs `delete_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_mark", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"remove_book", "remove_book_many", "set_title", "set_title_many", "set_author", "set_author_many", "set_subject", "set_subject_many", "set_language", "set_language_many", "set_cover", "set_cover_many", "clear_cover", "clear_cover_many", "record_at", "record_at_many", "read_chapter", "read_chapter_many", "show_page", "show_page_many", "show_passage", "show_passage_many", "show_place", "show_place_many", "saw_blocks", "saw_blocks_many", "set_finished", "set_finished_many", "highlight", "highlight_many", "add_note", "add_note_many", "toggle_bookmark", "toggle_bookmark_many", "comment", "comment_many", "change_color", "change_color_many", "edit_mark_text", "edit_mark_text_many", "delete_mark", "delete_mark_many"})


class Chapter(Resource):
    """Chapter — `tl.so.tilo.ebook.Chapter`."""
    __surface__ = "so.tilo.ebook@0cedf6b7fcebc8f1"
    njsapppc: str  # title
    hc6oa77y: str  # body
    awonaulm: Book | None  # book

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


class Mark(Resource):
    """Mark — `tl.so.tilo.ebook.Mark`."""
    __surface__ = "so.tilo.ebook@0cedf6b7fcebc8f1"
    nsx4szpu: str  # kind
    eu36rgok: int  # chapter
    kkwkq4sx: int  # block
    c7gbrwjv: int  # start
    gwydxwhz: int  # end
    pjwr2noa: str  # excerpt
    jlprlqyv: str  # color
    q6o2tid6: str  # text
    qzbrg6g6: str  # created
    jt22d5yp: str  # on
    gjbrlgkj: Book | None  # book
    vpgm2esg: list[int]  # pieces

    def edit_block(self,
        *,
        block: str,
        text: str,
    ) -> None:
        """Set one paragraph of a rich-text field to what a Markdown text says."""
        return self.action("edit-block", block=block, text=text)

    def edit_block_many(self, items: list[MarkEditBlockArgs]) -> list[dict[str, Any]]:
        """Runs `edit-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("edit-block", items)

    def insert_block(self,
        *,
        block: str,
        text: str,
    ) -> None:
        """Put a paragraph before the one the address names."""
        return self.action("insert-block", block=block, text=text)

    def insert_block_many(self, items: list[MarkInsertBlockArgs]) -> list[dict[str, Any]]:
        """Runs `insert-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("insert-block", items)

    def delete_block(self, *, block: str) -> None:
        """Remove the paragraph the address names, and the paragraphs it holds."""
        return self.action("delete-block", block=block)

    def delete_block_many(self, items: list[MarkDeleteBlockArgs]) -> list[dict[str, Any]]:
        """Runs `delete-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-block", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"edit_block", "edit_block_many", "insert_block", "insert_block_many", "delete_block", "delete_block_many"})


class Progress(Resource):
    """Progress — `tl.so.tilo.ebook.Progress`."""
    __surface__ = "so.tilo.ebook@0cedf6b7fcebc8f1"
    fw6cx373: str  # who
    hrlsiabz: int  # chapter
    dozgglst: int  # block
    j7r2dpph: int  # page
    t4pdkbb6: float  # place
    m5vjvjxn: bool  # finished
    vpcehkho: str  # at


class Scheme(Resource):
    """Scheme — `tl.so.tilo.ebook.Scheme`."""
    __surface__ = "so.tilo.ebook@0cedf6b7fcebc8f1"
    yjaxgs5w: str  # name
    zfe44ecr: str  # page
    giw4wwwc: str  # ink
    p6cg5shx: str  # accent
    @property
    def _2xhwxzlq(self) -> bool:
        """builtin (`2xhwxzlq` on the wire)."""
        return self.fields["2xhwxzlq"]

    @_2xhwxzlq.setter
    def _2xhwxzlq(self, value: bool) -> None:
        self.edit("2xhwxzlq", value=value)


class Font(Resource):
    """Font — `tl.so.tilo.ebook.Font`."""
    __surface__ = "so.tilo.ebook@0cedf6b7fcebc8f1"
    jlawoyck: str  # family
    w2n5kzdl: str  # name
    @property
    def _6pky4ug3(self) -> float:
        """size (`6pky4ug3` on the wire)."""
        return self.fields["6pky4ug3"]

    @_6pky4ug3.setter
    def _6pky4ug3(self, value: float) -> None:
        self.edit("6pky4ug3", value=value)
    qxdff5v3: bool  # shown
    @property
    def _7bsrqnib(self) -> bool:
        """builtin (`7bsrqnib` on the wire)."""
        return self.fields["7bsrqnib"]

    @_7bsrqnib.setter
    def _7bsrqnib(self, value: bool) -> None:
        self.edit("7bsrqnib", value=value)


class Found(Resource):
    """Found — `tl.so.tilo.ebook.Found`."""
    __surface__ = "so.tilo.ebook@0cedf6b7fcebc8f1"
    cuehsj4a: str  # title
    @property
    def _5z23zsgs(self) -> str:
        """author (`5z23zsgs` on the wire)."""
        return self.fields["5z23zsgs"]

    @_5z23zsgs.setter
    def _5z23zsgs(self, value: str) -> None:
        self.edit("5z23zsgs", value=value)
    @property
    def _5kd3qulh(self) -> str:
        """url (`5kd3qulh` on the wire)."""
        return self.fields["5kd3qulh"]

    @_5kd3qulh.setter
    def _5kd3qulh(self, value: str) -> None:
        self.edit("5kd3qulh", value=value)
    rgyfsse6: str  # language
    fhfwqqv3: str  # query


class Watched(Resource):
    """Watched — `tl.so.tilo.ebook.Watched`."""
    __surface__ = "so.tilo.ebook@0cedf6b7fcebc8f1"
    zfzcnyj7: str  # path
    l4rfzd5q: str  # token
    vs5vpxpc: list[str]  # given
    dafaqxgm: str  # added


class Library(Resource):
    """Library — `tl.so.tilo.ebook.Library`."""
    __surface__ = "so.tilo.ebook@0cedf6b7fcebc8f1"
    rjqqjeka: list[Entry[Book]]  # books
    gark455v: list[Entry[Found]]  # found
    b2rhddat: list[Entry[Scheme]]  # schemes
    sndaeoib: float  # text_size
    @property
    def _3fnv4dcf(self) -> float:
        """line_spacing (`3fnv4dcf` on the wire)."""
        return self.fields["3fnv4dcf"]

    @_3fnv4dcf.setter
    def _3fnv4dcf(self, value: float) -> None:
        self.edit("3fnv4dcf", value=value)
    @property
    def _5umaklng(self) -> str:
        """face (`5umaklng` on the wire)."""
        return self.fields["5umaklng"]

    @_5umaklng.setter
    def _5umaklng(self, value: str) -> None:
        self.edit("5umaklng", value=value)
    @property
    def _4nrbbruw(self) -> bool:
        """justified (`4nrbbruw` on the wire)."""
        return self.fields["4nrbbruw"]

    @_4nrbbruw.setter
    def _4nrbbruw(self, value: bool) -> None:
        self.edit("4nrbbruw", value=value)
    q2xeecyu: str  # scheme
    obt3nk5o: list[Entry[Font]]  # fonts
    @property
    def _7iepiv5z(self) -> str:
        """draft_scheme (`7iepiv5z` on the wire)."""
        return self.fields["7iepiv5z"]

    @_7iepiv5z.setter
    def _7iepiv5z(self, value: str) -> None:
        self.edit("7iepiv5z", value=value)
    wc5z47pj: str  # custom_page
    yiozk22e: str  # custom_ink
    dww4lfs6: str  # draft_folder
    trpwrvrr: list[Entry[Watched]]  # folders
    rmk5y2dl: str  # sort_by
    iw7jv3y3: bool  # hide_finished

    def open_book(self, *, file: Any) -> Book:
        """Adds the EPUB `file` to the shelf and opens it. Its title, author and language are the book's own; each chapter becomes a resource holding its words, so the book is searchable and a reader opens the chapter they are at."""
        return self.action("open_book", file=file)

    def drop_books(self, *, files: list[Any]) -> None:
        """Adds the EPUB files a person dropped on the shelf, each as `open_book` adds it."""
        return self.action("drop_books", files=files)

    def drop_books_many(self, items: list[DropBooksArgs]) -> list[dict[str, Any]]:
        """Runs `drop_books` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("drop_books", items)

    def watch_folder(self, *, path: str) -> None:
        """Watches the folder `path` for books: each EPUB it holds, and each that arrives, becomes a book, keeping the person's place as one they opened themselves does."""
        return self.action("watch_folder", path=path)

    def watch_folder_many(self, items: list[WatchFolderArgs]) -> list[dict[str, Any]]:
        """Runs `watch_folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("watch_folder", items)

    def scan_folder(self, *, path: str) -> None:
        """Reads the folder `path`'s EPUBs, giving a book to each file the library does not hold yet."""
        return self.action("scan_folder", path=path)

    def scan_folder_many(self, items: list[ScanFolderArgs]) -> list[dict[str, Any]]:
        """Runs `scan_folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("scan_folder", items)

    def foldered(self) -> None:
        """Reads every watched folder again: what the platform runs when one of them changes."""
        return self.action("foldered")

    def foldered_many(self, items: list[FolderedArgs]) -> list[dict[str, Any]]:
        """Runs `foldered` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("foldered", items)

    def set_text_size(self, *, size: float) -> None:
        """Sets the page's text size, within what a page can be read at."""
        return self.action("set_text_size", size=size)

    def set_text_size_many(self, items: list[SetTextSizeArgs]) -> list[dict[str, Any]]:
        """Runs `set_text_size` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_text_size", items)

    def set_line_spacing(self, *, spacing: float) -> None:
        """Sets the page's line spacing."""
        return self.action("set_line_spacing", spacing=spacing)

    def set_line_spacing_many(self, items: list[SetLineSpacingArgs]) -> list[dict[str, Any]]:
        """Runs `set_line_spacing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_line_spacing", items)

    def set_face(self, *, face: str) -> None:
        """Sets the face the page is set in."""
        return self.action("set_face", face=face)

    def set_face_many(self, items: list[SetFaceArgs]) -> list[dict[str, Any]]:
        """Runs `set_face` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_face", items)

    def add_font(self, *, family: str) -> None:
        """Adds the family `family`, which this device has, to the reader's faces."""
        return self.action("add_font", family=family)

    def add_font_many(self, items: list[AddFontArgs]) -> list[dict[str, Any]]:
        """Runs `add_font` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_font", items)

    def rename_font(self,
        *,
        f: Font,
        name: str,
    ) -> None:
        """Gives the face `f` the name a person reads it by; empty calls it what its family is."""
        return self.action("rename_font", f=f, name=name)

    def rename_font_many(self, items: list[RenameFontArgs]) -> list[dict[str, Any]]:
        """Runs `rename_font` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename_font", items)

    def set_font_size(self,
        *,
        f: Font,
        size: float,
    ) -> None:
        """Sets how large the face `f` reads beside the others."""
        return self.action("set_font_size", f=f, size=size)

    def set_font_size_many(self, items: list[SetFontSizeArgs]) -> list[dict[str, Any]]:
        """Runs `set_font_size` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_font_size", items)

    def set_font_shown(self,
        *,
        f: Font,
        shown: bool,
    ) -> None:
        """Says whether the face `f` appears in the reader's picker."""
        return self.action("set_font_shown", f=f, shown=shown)

    def set_font_shown_many(self, items: list[SetFontShownArgs]) -> list[dict[str, Any]]:
        """Runs `set_font_shown` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_font_shown", items)

    def remove_font(self, *, f: Font) -> None:
        """Takes a face of the person's own out of the reader's picker."""
        return self.action("remove_font", f=f)

    def remove_font_many(self, items: list[RemoveFontArgs]) -> list[dict[str, Any]]:
        """Runs `remove_font` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_font", items)

    def set_justified(self, *, on: bool) -> None:
        """Whether the page's paragraphs are justified."""
        return self.action("set_justified", on=on)

    def set_justified_many(self, items: list[SetJustifiedArgs]) -> list[dict[str, Any]]:
        """Runs `set_justified` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_justified", items)

    def set_shelf_sort(self, *, order: str) -> None:
        """Orders the shelf by title, by author, or by the books added last."""
        return self.action("set_shelf_sort", order=order)

    def set_shelf_sort_many(self, items: list[SetShelfSortArgs]) -> list[dict[str, Any]]:
        """Runs `set_shelf_sort` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_shelf_sort", items)

    def set_hide_finished(self, *, hidden: bool) -> None:
        """Leaves the books a person finished off the shelf, or shows them again."""
        return self.action("set_hide_finished", hidden=hidden)

    def set_hide_finished_many(self, items: list[SetHideFinishedArgs]) -> list[dict[str, Any]]:
        """Runs `set_hide_finished` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_hide_finished", items)

    def cycle_scheme(self) -> None:
        """Steps to the next palette the reader ships with: its bar's quick switch."""
        return self.action("cycle_scheme")

    def cycle_scheme_many(self, items: list[CycleSchemeArgs]) -> list[dict[str, Any]]:
        """Runs `cycle_scheme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("cycle_scheme", items)

    def set_scheme(self, *, name: str) -> None:
        """The palette the page is set in, by name."""
        return self.action("set_scheme", name=name)

    def set_scheme_many(self, items: list[SetSchemeArgs]) -> list[dict[str, Any]]:
        """Runs `set_scheme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_scheme", items)

    def add_scheme(self,
        *,
        name: str,
        page: str,
        ink: str,
    ) -> None:
        """A palette of the person's own, from a paper and an ink colour."""
        return self.action("add_scheme", name=name, page=page, ink=ink)

    def add_scheme_many(self, items: list[AddSchemeArgs]) -> list[dict[str, Any]]:
        """Runs `add_scheme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_scheme", items)

    def delete_scheme(self, *, scheme: Scheme) -> None:
        """Deletes a palette of the person's own. The four the reader ships with stay."""
        return self.action("delete_scheme", scheme=scheme)

    def delete_scheme_many(self, items: list[DeleteSchemeArgs]) -> list[dict[str, Any]]:
        """Runs `delete_scheme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_scheme", items)

    def find_books(self, *, query: str) -> None:
        """Searches Project Gutenberg for books matching `query`, taking the answers an earlier search left behind. The books stand under the shelf until a person downloads one."""
        return self.action("find_books", query=query)

    def find_books_many(self, items: list[FindBooksArgs]) -> list[dict[str, Any]]:
        """Runs `find_books` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("find_books", items)

    def download_found(self, *, f: Found) -> Book | None:
        """Downloads the book an online search found and opens it: the window opens the book the action returns, as it opens what any primary action makes. An answer that did not come leaves the book found, so the person can ask again."""
        return self.action("download_found", f=f)

    def clear_found(self) -> None:
        """Takes an online search's answers away."""
        return self.action("clear_found")

    def clear_found_many(self, items: list[ClearFoundArgs]) -> list[dict[str, Any]]:
        """Runs `clear_found` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_found", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"open_book", "drop_books", "drop_books_many", "watch_folder", "watch_folder_many", "scan_folder", "scan_folder_many", "foldered", "foldered_many", "set_text_size", "set_text_size_many", "set_line_spacing", "set_line_spacing_many", "set_face", "set_face_many", "add_font", "add_font_many", "rename_font", "rename_font_many", "set_font_size", "set_font_size_many", "set_font_shown", "set_font_shown_many", "remove_font", "remove_font_many", "set_justified", "set_justified_many", "set_shelf_sort", "set_shelf_sort_many", "set_hide_finished", "set_hide_finished_many", "cycle_scheme", "cycle_scheme_many", "set_scheme", "set_scheme_many", "add_scheme", "add_scheme_many", "delete_scheme", "delete_scheme_many", "find_books", "find_books_many", "download_found", "clear_found", "clear_found_many"})


class Highlight(Resource):
    """Highlight — `tl.so.tilo.ebook.Highlight`."""
    __surface__ = "so.tilo.ebook@0cedf6b7fcebc8f1"
    @property
    def _2mud3bws(self) -> str:
        """name (`2mud3bws` on the wire)."""
        return self.fields["2mud3bws"]

    @_2mud3bws.setter
    def _2mud3bws(self, value: str) -> None:
        self.edit("2mud3bws", value=value)
    ztqdbzwg: str  # value


class Ebook(Resource):
    """Books — `tl.so.tilo.ebook`."""
    __surface__ = "so.tilo.ebook@0cedf6b7fcebc8f1"
    def copy_passage(self, *, text: str) -> None:
        """Copies a passage, so it can be pasted anywhere: what a selection's Copy does."""
        return self.action("copy_passage", text=text)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"copy_passage"})


