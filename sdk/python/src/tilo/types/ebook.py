"""Types for E-Book Reader (`so.tilo.ebook`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Library` is the app's `ebook.library` at `ebook/<resource>`, read with
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
    "StoredContent",
    "Library",
    "BooksCatalogues",
    "Files",
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

class StoredContent(TypedDict):
    hash: str
    size: int

class Library(Resource):
    """Books — `ebook.library`."""
    entries: list[LibraryEntry]
    progress: dict[str, Progress]
    settings: Settings
    default_font: str
    prefs: LibraryPrefs
    fonts: list[FontEntry]
    schemes: list[ColorScheme]
    annotations: list[Mark]

    def shelve(self,
        *,
        book: str,
        title: str,
        author: str,
        subject: str,
        shared: bool,
        content: StoredContent | None = None,
    ) -> Run | None:
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

    def shelve_found(self,
        *,
        book: str,
        title: str,
        author: str,
        subject: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("shelve-found", book=book, title=title, author=author, subject=subject)

    def remove_from_library(self, *, book: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-from-library", book=book)

    def index_book(self,
        *,
        book: str,
        index: StoredContent,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("index-book", book=book, index=index)

    def record_progress(self,
        *,
        book: str,
        chapter: int,
        block: int,
        fraction: float,
        opened: bool,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("record-progress", book=book, chapter=chapter, block=block, fraction=fraction, opened=opened)

    def add_note(self,
        *,
        passage: Passage,
        color: str,
        text: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("add-note", passage=passage, color=color, text=text)

    def add_comment(self,
        *,
        highlight: str,
        text: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("add-comment", highlight=highlight, text=text)

    def toggle_bookmark(self,
        *,
        book: str,
        chapter: int,
        block: int,
        excerpt: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("toggle-bookmark", book=book, chapter=chapter, block=block, excerpt=excerpt)

    def highlight(self,
        *,
        book: str,
        chapter: int,
        color: str,
        replacing: list[str],
        pieces: list[HiPiece],
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("highlight", book=book, chapter=chapter, color=color, replacing=replacing, pieces=pieces)

    def change_color(self,
        *,
        highlight: str,
        color: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("change-color", highlight=highlight, color=color)

    def delete_highlight(self, *, highlight: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-highlight", highlight=highlight)

    def delete_annotation(self, *, annotation: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-annotation", annotation=annotation)

    def remove_font(self, *, font: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-font", font=font)

    def delete_scheme(self, *, scheme: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-scheme", scheme=scheme)

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


class BooksCatalogues(Resource):
    """Book catalogues — `settings.books-catalogues`."""
    keep_catalogues: bool


class Files(Resource):
    """Book files — `ebook.files`."""
    devices: dict[str, Any]

    def keep(self,
        *,
        device: str,
        kept: Any,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("keep", device=device, kept=kept)


