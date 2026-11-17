"""Types for E-Book Reader (`so.tilo.ebook`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Library` is the app's `ebook.library` at `ebook/<resource>`, read with
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
from tilo import Entry, Resource, Run

__all__ = [
    "AnnoKind",
    "ColorScheme",
    "CompositionStyle",
    "FontEntry",
    "HiPiece",
    "LibraryEntry",
    "LibraryPrefs",
    "LibrarySort",
    "Mark",
    "Passage",
    "Progress",
    "ReadMode",
    "ReadTheme",
    "Settings",
    "Library",
    "BooksCatalogues",
    "Files",
    "ShelveArgs",
    "ShelveFoundArgs",
    "RemoveFromLibraryArgs",
    "IndexBookArgs",
    "RecordProgressArgs",
    "AddNoteArgs",
    "AddCommentArgs",
    "ToggleBookmarkArgs",
    "HighlightArgs",
    "ChangeColorArgs",
    "DeleteHighlightArgs",
    "DeleteAnnotationArgs",
    "RemoveFontArgs",
    "DeleteSchemeArgs",
    "KeepArgs",
]

AnnoKind = Literal["bookmark", "highlight", "note"]

class ColorScheme(TypedDict):
    id: str
    name: str
    page: list[int]
    ink: list[int]
    accent: list[int]
    dark: bool
    enabled: bool
    builtin: bool

CompositionStyle = Literal["book", "ragged"]

class FontEntry(TypedDict):
    id: str
    kind: Any
    name: str
    size_mult: float
    shown: bool

class HiPiece(TypedDict):
    block: int
    start: int
    end: int
    excerpt: str

class LibraryEntry(TypedDict):
    id: str
    shared: bool
    title: str
    author: str
    subject: str
    added: int
    content: Any
    index: Any

class LibraryPrefs(TypedDict):
    sort: LibrarySort
    hide_read: bool

LibrarySort = Literal["title", "author", "recent", "progress"]

class Mark(TypedDict):
    id: str
    doc_id: str
    kind: AnnoKind
    chapter: int
    block: int
    start: int
    end: int
    excerpt: str
    text: str
    color: Any
    created: int
    group: str

class Passage(TypedDict):
    book: str
    chapter: int
    block: int
    start: int
    end: int
    excerpt: str

class Progress(TypedDict):
    chapter: int
    block: int
    fraction: float
    opened: int

ReadMode = Literal["paged", "continuous", "two_up"]

ReadTheme = Literal["light", "sepia", "dark", "high_contrast"]

class Settings(TypedDict):
    font_scale: float
    line_spacing: float
    font_id: str
    theme: ReadTheme
    scheme_id: str
    mode: ReadMode
    composition: CompositionStyle

ShelveArgs = TypedDict("ShelveArgs", {
    "book": "str",
    "title": "str",
    "author": "str",
    "subject": "str",
    "shared": "bool",
    "content": NotRequired["Any | None"],
})
"""The arguments of `shelve`."""

ShelveFoundArgs = TypedDict("ShelveFoundArgs", {
    "book": "str",
    "title": "str",
    "author": "str",
    "subject": "str",
})
"""The arguments of `shelve-found`."""

RemoveFromLibraryArgs = TypedDict("RemoveFromLibraryArgs", {
    "book": "str",
})
"""The arguments of `remove-from-library`."""

IndexBookArgs = TypedDict("IndexBookArgs", {
    "book": "str",
    "index": "Any",
})
"""The arguments of `index-book`."""

RecordProgressArgs = TypedDict("RecordProgressArgs", {
    "book": "str",
    "chapter": "int",
    "block": "int",
    "fraction": "float",
    "opened": "bool",
})
"""The arguments of `record-progress`."""

AddNoteArgs = TypedDict("AddNoteArgs", {
    "passage": "Passage",
    "color": "str",
    "text": "str",
})
"""The arguments of `add-note`."""

AddCommentArgs = TypedDict("AddCommentArgs", {
    "highlight": "str",
    "text": "str",
})
"""The arguments of `add-comment`."""

ToggleBookmarkArgs = TypedDict("ToggleBookmarkArgs", {
    "book": "str",
    "chapter": "int",
    "block": "int",
    "excerpt": "str",
})
"""The arguments of `toggle-bookmark`."""

HighlightArgs = TypedDict("HighlightArgs", {
    "book": "str",
    "chapter": "int",
    "color": "str",
    "replacing": "list[str]",
    "pieces": "list[HiPiece]",
})
"""The arguments of `highlight`."""

ChangeColorArgs = TypedDict("ChangeColorArgs", {
    "highlight": "str",
    "color": "str",
})
"""The arguments of `change-color`."""

DeleteHighlightArgs = TypedDict("DeleteHighlightArgs", {
    "highlight": "str",
})
"""The arguments of `delete-highlight`."""

DeleteAnnotationArgs = TypedDict("DeleteAnnotationArgs", {
    "annotation": "str",
})
"""The arguments of `delete-annotation`."""

RemoveFontArgs = TypedDict("RemoveFontArgs", {
    "font": "str",
})
"""The arguments of `remove-font`."""

DeleteSchemeArgs = TypedDict("DeleteSchemeArgs", {
    "scheme": "str",
})
"""The arguments of `delete-scheme`."""

KeepArgs = TypedDict("KeepArgs", {
    "device": "str",
    "kept": "Any",
})
"""The arguments of `keep`."""

class Library(Resource):
    """Books — `ebook.library`."""
    __surface__ = "so.tilo.ebook@ae688919837912da"
    entries: list[Entry[LibraryEntry]]
    progress: dict[str, Progress]
    settings: Settings
    default_font: str
    prefs: LibraryPrefs
    fonts: list[Entry[FontEntry]]
    schemes: list[Entry[ColorScheme]]
    annotations: list[Entry[Mark]]

    def shelve(self,
        *,
        book: str,
        title: str,
        author: str,
        subject: str,
        shared: bool,
        content: Any | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["book"] = book
        args["title"] = title
        args["author"] = author
        args["subject"] = subject
        args["shared"] = shared
        if content is not None:
            args["content"] = content
        return self.action("shelve", **args)

    def shelve_many(self, items: list[ShelveArgs]) -> list[dict[str, Any]]:
        """Runs `shelve` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("shelve", items)

    def shelve_found(self,
        *,
        book: str,
        title: str,
        author: str,
        subject: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("shelve-found", book=book, title=title, author=author, subject=subject)

    def shelve_found_many(self, items: list[ShelveFoundArgs]) -> list[dict[str, Any]]:
        """Runs `shelve-found` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("shelve-found", items)

    def remove_from_library(self, *, book: str) -> None:
        """Runs the app's action."""
        return self.action("remove-from-library", book=book)

    def remove_from_library_many(self, items: list[RemoveFromLibraryArgs]) -> list[dict[str, Any]]:
        """Runs `remove-from-library` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("remove-from-library", items)

    def index_book(self,
        *,
        book: str,
        index: Any,
    ) -> None:
        """Runs the app's action."""
        return self.action("index-book", book=book, index=index)

    def index_book_many(self, items: list[IndexBookArgs]) -> list[dict[str, Any]]:
        """Runs `index-book` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("index-book", items)

    def record_progress(self,
        *,
        book: str,
        chapter: int,
        block: int,
        fraction: float,
        opened: bool,
    ) -> None:
        """Runs the app's action."""
        return self.action("record-progress", book=book, chapter=chapter, block=block, fraction=fraction, opened=opened)

    def record_progress_many(self, items: list[RecordProgressArgs]) -> list[dict[str, Any]]:
        """Runs `record-progress` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("record-progress", items)

    def add_note(self,
        *,
        passage: Passage,
        color: str,
        text: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("add-note", passage=passage, color=color, text=text)

    def add_note_many(self, items: list[AddNoteArgs]) -> list[dict[str, Any]]:
        """Runs `add-note` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("add-note", items)

    def add_comment(self,
        *,
        highlight: str,
        text: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("add-comment", highlight=highlight, text=text)

    def add_comment_many(self, items: list[AddCommentArgs]) -> list[dict[str, Any]]:
        """Runs `add-comment` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("add-comment", items)

    def toggle_bookmark(self,
        *,
        book: str,
        chapter: int,
        block: int,
        excerpt: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("toggle-bookmark", book=book, chapter=chapter, block=block, excerpt=excerpt)

    def toggle_bookmark_many(self, items: list[ToggleBookmarkArgs]) -> list[dict[str, Any]]:
        """Runs `toggle-bookmark` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("toggle-bookmark", items)

    def highlight(self,
        *,
        book: str,
        chapter: int,
        color: str,
        replacing: list[str],
        pieces: list[HiPiece],
    ) -> None:
        """Runs the app's action."""
        return self.action("highlight", book=book, chapter=chapter, color=color, replacing=replacing, pieces=pieces)

    def highlight_many(self, items: list[HighlightArgs]) -> list[dict[str, Any]]:
        """Runs `highlight` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("highlight", items)

    def change_color(self,
        *,
        highlight: str,
        color: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("change-color", highlight=highlight, color=color)

    def change_color_many(self, items: list[ChangeColorArgs]) -> list[dict[str, Any]]:
        """Runs `change-color` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("change-color", items)

    def delete_highlight(self, *, highlight: str) -> None:
        """Runs the app's action."""
        return self.action("delete-highlight", highlight=highlight)

    def delete_highlight_many(self, items: list[DeleteHighlightArgs]) -> list[dict[str, Any]]:
        """Runs `delete-highlight` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("delete-highlight", items)

    def delete_annotation(self, *, annotation: str) -> None:
        """Runs the app's action."""
        return self.action("delete-annotation", annotation=annotation)

    def delete_annotation_many(self, items: list[DeleteAnnotationArgs]) -> list[dict[str, Any]]:
        """Runs `delete-annotation` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("delete-annotation", items)

    def remove_font(self, *, font: str) -> None:
        """Runs the app's action."""
        return self.action("remove-font", font=font)

    def remove_font_many(self, items: list[RemoveFontArgs]) -> list[dict[str, Any]]:
        """Runs `remove-font` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("remove-font", items)

    def delete_scheme(self, *, scheme: str) -> None:
        """Runs the app's action."""
        return self.action("delete-scheme", scheme=scheme)

    def delete_scheme_many(self, items: list[DeleteSchemeArgs]) -> list[dict[str, Any]]:
        """Runs `delete-scheme` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("delete-scheme", items)

    def read_chapter(self,
        *,
        book: str,
        chapter: int | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["book"] = book
        if chapter is not None:
            args["chapter"] = chapter
        return self.action("read-chapter", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"shelve", "shelve_many", "shelve_found", "shelve_found_many", "remove_from_library", "remove_from_library_many", "index_book", "index_book_many", "record_progress", "record_progress_many", "add_note", "add_note_many", "add_comment", "add_comment_many", "toggle_bookmark", "toggle_bookmark_many", "highlight", "highlight_many", "change_color", "change_color_many", "delete_highlight", "delete_highlight_many", "delete_annotation", "delete_annotation_many", "remove_font", "remove_font_many", "delete_scheme", "delete_scheme_many", "read_chapter"})


class BooksCatalogues(Resource):
    """Book catalogues — `settings.books-catalogues`."""
    __surface__ = "so.tilo.ebook@ae688919837912da"
    keep_catalogues: bool


class Files(Resource):
    """Book files — `ebook.files`."""
    __surface__ = "so.tilo.ebook@ae688919837912da"
    devices: dict[str, Any]

    def keep(self,
        *,
        device: str,
        kept: Any,
    ) -> None:
        """Runs the app's action."""
        return self.action("keep", device=device, kept=kept)

    def keep_many(self, items: list[KeepArgs]) -> list[dict[str, Any]]:
        """Runs `keep` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("keep", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"keep", "keep_many"})


