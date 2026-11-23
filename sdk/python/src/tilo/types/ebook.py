"""Types for Books (`so.tilo.ebook`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Book` is the app's `tl.so.tilo.ebook.Book` at `ebook/<resource>`, read with
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
    "Book",
    "Chapter",
    "Mark",
    "Piece",
    "Progress",
    "Scheme",
    "Found",
    "Library",
    "FoundAuthor",
    "FoundBook",
    "FoundAnswer",
    "Ebook",
    "RemoveBookArgs",
    "RecordAtArgs",
    "ReadChapterArgs",
    "ShowPageArgs",
    "ShowPassageArgs",
    "ShowPlaceArgs",
    "SawBlocksArgs",
    "HighlightArgs",
    "AddNoteArgs",
    "ToggleBookmarkArgs",
    "CommentArgs",
    "ChangeColorArgs",
    "DeleteMarkArgs",
    "DropBooksArgs",
    "SetTextSizeArgs",
    "SetLineSpacingArgs",
    "SetFaceArgs",
    "SetJustifiedArgs",
    "CycleSchemeArgs",
    "SetSchemeArgs",
    "AddSchemeArgs",
    "DeleteSchemeArgs",
    "FindBooksArgs",
    "DownloadFoundArgs",
    "ClearFoundArgs",
]

RemoveBookArgs = TypedDict("RemoveBookArgs", {
})
"""The arguments of `remove_book`."""

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

DeleteMarkArgs = TypedDict("DeleteMarkArgs", {
    "m": "Mark",
})
"""The arguments of `delete_mark`."""

DropBooksArgs = TypedDict("DropBooksArgs", {
    "files": "list[Any]",
})
"""The arguments of `drop_books`."""

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

SetJustifiedArgs = TypedDict("SetJustifiedArgs", {
    "on": "bool",
})
"""The arguments of `set_justified`."""

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

DownloadFoundArgs = TypedDict("DownloadFoundArgs", {
    "f": "Found",
})
"""The arguments of `download_found`."""

ClearFoundArgs = TypedDict("ClearFoundArgs", {
})
"""The arguments of `clear_found`."""

class Book(Resource):
    """Book — `tl.so.tilo.ebook.Book`."""
    __surface__ = "so.tilo.ebook@3b10ed27c5684dba"
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

    def delete_mark(self, *, m: Mark) -> None:
        """Deletes a mark, and the comments on it."""
        return self.action("delete_mark", m=m)

    def delete_mark_many(self, items: list[DeleteMarkArgs]) -> list[dict[str, Any]]:
        """Runs `delete_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_mark", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"remove_book", "remove_book_many", "record_at", "record_at_many", "read_chapter", "read_chapter_many", "show_page", "show_page_many", "show_passage", "show_passage_many", "show_place", "show_place_many", "saw_blocks", "saw_blocks_many", "highlight", "highlight_many", "add_note", "add_note_many", "toggle_bookmark", "toggle_bookmark_many", "comment", "comment_many", "change_color", "change_color_many", "delete_mark", "delete_mark_many"})


class Chapter(Resource):
    """Chapter — `tl.so.tilo.ebook.Chapter`."""
    __surface__ = "so.tilo.ebook@3b10ed27c5684dba"
    njsapppc: str  # title
    hc6oa77y: str  # body
    awonaulm: Book | None  # book


class Mark(Resource):
    """Mark — `tl.so.tilo.ebook.Mark`."""
    __surface__ = "so.tilo.ebook@3b10ed27c5684dba"
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
    vpgm2esg: list[Piece]  # pieces


class Piece(Resource):
    """Piece — `tl.so.tilo.ebook.Piece`."""
    __surface__ = "so.tilo.ebook@3b10ed27c5684dba"
    el2sirvc: int  # block
    wjiavs4p: int  # start
    mk52mcqy: int  # end


class Progress(Resource):
    """Progress — `tl.so.tilo.ebook.Progress`."""
    __surface__ = "so.tilo.ebook@3b10ed27c5684dba"
    fw6cx373: str  # who
    hrlsiabz: int  # chapter
    dozgglst: int  # block
    j7r2dpph: int  # page
    t4pdkbb6: float  # place
    vpcehkho: str  # at


class Scheme(Resource):
    """Scheme — `tl.so.tilo.ebook.Scheme`."""
    __surface__ = "so.tilo.ebook@3b10ed27c5684dba"
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


class Found(Resource):
    """Found — `tl.so.tilo.ebook.Found`."""
    __surface__ = "so.tilo.ebook@3b10ed27c5684dba"
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


class Library(Resource):
    """Library — `tl.so.tilo.ebook.Library`."""
    __surface__ = "so.tilo.ebook@3b10ed27c5684dba"
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
    @property
    def _7iepiv5z(self) -> str:
        """draft_scheme (`7iepiv5z` on the wire)."""
        return self.fields["7iepiv5z"]

    @_7iepiv5z.setter
    def _7iepiv5z(self, value: str) -> None:
        self.edit("7iepiv5z", value=value)
    wc5z47pj: str  # custom_page
    yiozk22e: str  # custom_ink

    def open_book(self, *, file: Any) -> Book:
        """Adds the EPUB `file` to the shelf and opens it. Its title, author and language are the book's own; each chapter becomes a resource holding its words, so the book is searchable and a reader opens the chapter they are at."""
        return self.action("open_book", file=file)

    def drop_books(self, *, files: list[Any]) -> None:
        """Adds the EPUB files a person dropped on the shelf, each as `open_book` adds it."""
        return self.action("drop_books", files=files)

    def drop_books_many(self, items: list[DropBooksArgs]) -> list[dict[str, Any]]:
        """Runs `drop_books` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("drop_books", items)

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

    def set_justified(self, *, on: bool) -> None:
        """Whether the page's paragraphs are justified."""
        return self.action("set_justified", on=on)

    def set_justified_many(self, items: list[SetJustifiedArgs]) -> list[dict[str, Any]]:
        """Runs `set_justified` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_justified", items)

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

    def download_found(self, *, f: Found) -> None:
        """Downloads the book an online search found and opens it."""
        return self.action("download_found", f=f)

    def download_found_many(self, items: list[DownloadFoundArgs]) -> list[dict[str, Any]]:
        """Runs `download_found` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("download_found", items)

    def clear_found(self) -> None:
        """Takes an online search's answers away."""
        return self.action("clear_found")

    def clear_found_many(self, items: list[ClearFoundArgs]) -> list[dict[str, Any]]:
        """Runs `clear_found` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_found", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"open_book", "drop_books", "drop_books_many", "set_text_size", "set_text_size_many", "set_line_spacing", "set_line_spacing_many", "set_face", "set_face_many", "set_justified", "set_justified_many", "cycle_scheme", "cycle_scheme_many", "set_scheme", "set_scheme_many", "add_scheme", "add_scheme_many", "delete_scheme", "delete_scheme_many", "find_books", "find_books_many", "download_found", "download_found_many", "clear_found", "clear_found_many"})


class FoundAuthor(Resource):
    """FoundAuthor — `tl.so.tilo.ebook.FoundAuthor`."""
    __surface__ = "so.tilo.ebook@3b10ed27c5684dba"
    j3ucstoe: str  # name


class FoundBook(Resource):
    """FoundBook — `tl.so.tilo.ebook.FoundBook`."""
    __surface__ = "so.tilo.ebook@3b10ed27c5684dba"
    v7nyajgy: str  # title
    a4rek4qh: list[FoundAuthor] | None  # authors
    @property
    def _7uazgom7(self) -> list[str] | None:
        """languages (`7uazgom7` on the wire)."""
        return self.fields["7uazgom7"]

    @_7uazgom7.setter
    def _7uazgom7(self, value: list[str] | None) -> None:
        self.edit("7uazgom7", value=value)
    uw6rgipi: dict[str, str] | None  # formats


class FoundAnswer(Resource):
    """FoundAnswer — `tl.so.tilo.ebook.FoundAnswer`."""
    __surface__ = "so.tilo.ebook@3b10ed27c5684dba"
    ngjj6bkr: list[FoundBook]  # results


class Ebook(Resource):
    """Books — `tl.so.tilo.ebook`."""
    __surface__ = "so.tilo.ebook@3b10ed27c5684dba"

