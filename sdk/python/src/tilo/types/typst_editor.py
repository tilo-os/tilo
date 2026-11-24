"""Types for Typst (`so.tilo.typst-editor`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `typst-editor/<resource>`, read with
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
    "Severity",
    "Notice",
    "Problem",
    "Page",
    "Heading",
    "Document",
    "Library",
    "CompileAsk",
    "PageAsk",
    "TextAsk",
    "OutlineAsk",
    "PageRow",
    "ProblemRow",
    "CompiledRow",
    "HeadingRow",
    "CellRow",
    "TypstEditor",
    "TypesetArgs",
    "TypesetIfStaleArgs",
    "TakeProblemsArgs",
    "TakePagesArgs",
    "ReadOutlineArgs",
    "DrawPageArgs",
    "DrawNextArgs",
    "SideBySideArgs",
    "EditorViewArgs",
    "PreviewViewArgs",
    "ProblemsArgs",
    "OutlineArgs",
    "ZoomInArgs",
    "ZoomOutArgs",
    "FitWidthArgs",
    "TurnToArgs",
    "GoToProblemArgs",
    "GoToHeadingArgs",
    "GoToArgs",
    "CloseDocumentArgs",
]

Severity = Literal["error", "warning"]
"""How much a problem the typesetter found matters."""

TypesetArgs = TypedDict("TypesetArgs", {
})
"""The arguments of `typeset`."""

TypesetIfStaleArgs = TypedDict("TypesetIfStaleArgs", {
})
"""The arguments of `typeset_if_stale`."""

TakeProblemsArgs = TypedDict("TakeProblemsArgs", {
    "rows": "list[ProblemRow]",
})
"""The arguments of `take_problems`."""

TakePagesArgs = TypedDict("TakePagesArgs", {
    "rows": "list[PageRow]",
})
"""The arguments of `take_pages`."""

ReadOutlineArgs = TypedDict("ReadOutlineArgs", {
})
"""The arguments of `read_outline`."""

DrawPageArgs = TypedDict("DrawPageArgs", {
    "page": "Page",
    "width": "int",
})
"""The arguments of `draw_page`."""

DrawNextArgs = TypedDict("DrawNextArgs", {
})
"""The arguments of `draw_next`."""

SideBySideArgs = TypedDict("SideBySideArgs", {
})
"""The arguments of `side_by_side`."""

EditorViewArgs = TypedDict("EditorViewArgs", {
})
"""The arguments of `editor_view`."""

PreviewViewArgs = TypedDict("PreviewViewArgs", {
})
"""The arguments of `preview_view`."""

ProblemsArgs = TypedDict("ProblemsArgs", {
})
"""The arguments of `problems`."""

OutlineArgs = TypedDict("OutlineArgs", {
})
"""The arguments of `outline`."""

ZoomInArgs = TypedDict("ZoomInArgs", {
})
"""The arguments of `zoom_in`."""

ZoomOutArgs = TypedDict("ZoomOutArgs", {
})
"""The arguments of `zoom_out`."""

FitWidthArgs = TypedDict("FitWidthArgs", {
    "width": "int",
})
"""The arguments of `fit_width`."""

TurnToArgs = TypedDict("TurnToArgs", {
    "page": "Page",
})
"""The arguments of `turn_to`."""

GoToProblemArgs = TypedDict("GoToProblemArgs", {
    "problem": "Problem",
})
"""The arguments of `go_to_problem`."""

GoToHeadingArgs = TypedDict("GoToHeadingArgs", {
    "heading": "Heading",
})
"""The arguments of `go_to_heading`."""

GoToArgs = TypedDict("GoToArgs", {
    "line": "int",
    "column": "int",
})
"""The arguments of `go_to`."""

CloseDocumentArgs = TypedDict("CloseDocumentArgs", {
    "d": "Document",
})
"""The arguments of `close_document`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.typst-editor@4cf9b224fe02d3e5"
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


class Problem(Resource):
    """Problem — `tl.so.tilo.typst-editor.Problem`."""
    __surface__ = "so.tilo.typst-editor@4cf9b224fe02d3e5"
    yt5nyujj: Severity  # severity
    i3zcji5u: str  # message
    od6pdrjz: int  # line
    xrgk5gr2: int  # column
    f7mcrdxf: int  # end_line
    @property
    def _7j5cjuet(self) -> int:
        """end_column (`7j5cjuet` on the wire)."""
        return self.fields["7j5cjuet"]

    @_7j5cjuet.setter
    def _7j5cjuet(self, value: int) -> None:
        self.edit("7j5cjuet", value=value)
    u6wlx35y: list[str]  # hints


class Page(Resource):
    """Page — `tl.so.tilo.typst-editor.Page`."""
    __surface__ = "so.tilo.typst-editor@4cf9b224fe02d3e5"
    cgrnobye: float  # width
    r4wxummm: float  # height
    @property
    def _6r2kgenm(self) -> str:
        """look (`6r2kgenm` on the wire)."""
        return self.fields["6r2kgenm"]

    @_6r2kgenm.setter
    def _6r2kgenm(self, value: str) -> None:
        self.edit("6r2kgenm", value=value)
    @property
    def _7ry6v2yt(self) -> str:
        """picture (`7ry6v2yt` on the wire)."""
        return self.fields["7ry6v2yt"]

    @_7ry6v2yt.setter
    def _7ry6v2yt(self, value: str) -> None:
        self.edit("7ry6v2yt", value=value)
    ycgenfy7: int  # picture_width
    wfjgjbj4: str  # thumb
    yusfx3a6: int  # thumb_width


class Heading(Resource):
    """Heading — `tl.so.tilo.typst-editor.Heading`."""
    __surface__ = "so.tilo.typst-editor@4cf9b224fe02d3e5"
    ougql7d2: str  # title
    ucauvawo: int  # line
    t2356zsr: int  # level


class Document(Resource):
    """Document — `tl.so.tilo.typst-editor.Document`."""
    __surface__ = "so.tilo.typst-editor@4cf9b224fe02d3e5"
    ojyy5fog: str  # name
    wigkyjqk: str  # source
    zlwvg46c: str  # typeset
    s3mabp2s: bool  # typesets
    d7qirnkw: int  # took_ms
    hk3xko3g: str  # trouble
    rrdheos7: str  # written
    bxbra2li: list[Entry[Problem]]  # problems
    jsru5ywo: list[Entry[Page]]  # pages
    cltpfy4y: list[Entry[Heading]]  # headings
    xn4pkllr: str  # pdf
    e3bwnsci: str  # pdf_name
    akp7jws5: int  # shown_width
    sfn7naaa: str  # showing
    g6gxruuq: str  # reveal
    xad4v4m3: int  # reveals
    fijcb57x: Page | None  # reading_page
    tbdqtt5g: bool  # stale

    def typeset(self) -> None:
        """Typesets the document, as the window and the background do."""
        return self.action("typeset")

    def typeset_many(self, items: list[TypesetArgs]) -> list[dict[str, Any]]:
        """Runs `typeset` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("typeset", items)

    def typeset_if_stale(self) -> None:
        """Typesets the document when its source moved past the last typeset."""
        return self.action("typeset_if_stale")

    def typeset_if_stale_many(self, items: list[TypesetIfStaleArgs]) -> list[dict[str, Any]]:
        """Runs `typeset_if_stale` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("typeset_if_stale", items)

    def take_problems(self, *, rows: list[ProblemRow]) -> None:
        """Lays the problems a typeset reported over the ones the document held, blocking ones first."""
        return self.action("take_problems", rows=rows)

    def take_problems_many(self, items: list[TakeProblemsArgs]) -> list[dict[str, Any]]:
        """Runs `take_problems` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("take_problems", items)

    def take_pages(self, *, rows: list[PageRow]) -> None:
        """Lays the typeset pages over the ones the document held: a page that draws what it drew keeps its picture, and one that draws something else is drawn again."""
        return self.action("take_pages", rows=rows)

    def take_pages_many(self, items: list[TakePagesArgs]) -> list[dict[str, Any]]:
        """Runs `take_pages` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("take_pages", items)

    def read_outline(self) -> None:
        """Reads the document's headings from its source's syntax."""
        return self.action("read_outline")

    def read_outline_many(self, items: list[ReadOutlineArgs]) -> list[dict[str, Any]]:
        """Runs `read_outline` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("read_outline", items)

    def draw_page(self,
        *,
        page: Page,
        width: int,
    ) -> None:
        """Draws one page at `width` pixels wide, and its thumbnail."""
        return self.action("draw_page", page=page, width=width)

    def draw_page_many(self, items: list[DrawPageArgs]) -> list[dict[str, Any]]:
        """Runs `draw_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("draw_page", items)

    def draw_next(self) -> None:
        """Draws the next page the window's width asks for, one a second: a long document fills in page by page, without anyone waiting for it."""
        return self.action("draw_next")

    def draw_next_many(self, items: list[DrawNextArgs]) -> list[dict[str, Any]]:
        """Runs `draw_next` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("draw_next", items)

    def side_by_side(self) -> None:
        """Shows the source beside the page."""
        return self.action("side_by_side")

    def side_by_side_many(self, items: list[SideBySideArgs]) -> list[dict[str, Any]]:
        """Runs `side_by_side` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("side_by_side", items)

    def editor_view(self) -> None:
        """Shows the source alone."""
        return self.action("editor_view")

    def editor_view_many(self, items: list[EditorViewArgs]) -> list[dict[str, Any]]:
        """Runs `editor_view` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("editor_view", items)

    def preview_view(self) -> None:
        """Shows the page alone."""
        return self.action("preview_view")

    def preview_view_many(self, items: list[PreviewViewArgs]) -> list[dict[str, Any]]:
        """Runs `preview_view` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("preview_view", items)

    def problems(self) -> None:
        """Shows what the typesetter found, or goes back to the page."""
        return self.action("problems")

    def problems_many(self, items: list[ProblemsArgs]) -> list[dict[str, Any]]:
        """Runs `problems` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("problems", items)

    def outline(self) -> None:
        """Shows the document's headings, or goes back to the page."""
        return self.action("outline")

    def outline_many(self, items: list[OutlineArgs]) -> list[dict[str, Any]]:
        """Runs `outline` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("outline", items)

    def zoom_in(self) -> None:
        """Draws the pages a step wider."""
        return self.action("zoom_in")

    def zoom_in_many(self, items: list[ZoomInArgs]) -> list[dict[str, Any]]:
        """Runs `zoom_in` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("zoom_in", items)

    def zoom_out(self) -> None:
        """Draws the pages a step narrower."""
        return self.action("zoom_out")

    def zoom_out_many(self, items: list[ZoomOutArgs]) -> list[dict[str, Any]]:
        """Runs `zoom_out` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("zoom_out", items)

    def fit_width(self, *, width: int) -> None:
        """Draws the pages as wide as the room they are shown in."""
        return self.action("fit_width", width=width)

    def fit_width_many(self, items: list[FitWidthArgs]) -> list[dict[str, Any]]:
        """Runs `fit_width` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("fit_width", items)

    def turn_to(self, *, page: Page) -> None:
        """Turns to a page, so the person comes back to it."""
        return self.action("turn_to", page=page)

    def turn_to_many(self, items: list[TurnToArgs]) -> list[dict[str, Any]]:
        """Runs `turn_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("turn_to", items)

    def go_to_problem(self, *, problem: Problem) -> None:
        """Goes to the place a problem names."""
        return self.action("go_to_problem", problem=problem)

    def go_to_problem_many(self, items: list[GoToProblemArgs]) -> list[dict[str, Any]]:
        """Runs `go_to_problem` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("go_to_problem", items)

    def go_to_heading(self, *, heading: Heading) -> None:
        """Goes to the line a heading stands on."""
        return self.action("go_to_heading", heading=heading)

    def go_to_heading_many(self, items: list[GoToHeadingArgs]) -> list[dict[str, Any]]:
        """Runs `go_to_heading` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("go_to_heading", items)

    def go_to(self,
        *,
        line: int,
        column: int,
    ) -> None:
        """Puts the caret at `line`:`column` and shows it, counting each ask so the same place shows again."""
        return self.action("go_to", line=line, column=column)

    def go_to_many(self, items: list[GoToArgs]) -> list[dict[str, Any]]:
        """Runs `go_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("go_to", items)

    def save_file(self) -> Any:
        """The document as the file it is saved as, marked as written."""
        return self.action("save_file")

    def export_pdf(self) -> Any:
        """The PDF the last typeset wrote, as the file a person saves."""
        return self.action("export_pdf")

    def copy_selection(self,
        *,
        page_at: int,
        from_: list[float] | None = None,
        to: list[float] | None = None,
    ) -> None:
        """Copies the words a person selected on a page."""
        args: dict[str, Any] = {}
        args["page_at"] = page_at
        if from_ is not None:
            args["from"] = from_
        if to is not None:
            args["to"] = to
        return self.action("copy_selection", **args)

    def copy_page(self, *, page_at: int) -> None:
        """Copies all of a page's words."""
        return self.action("copy_page", page_at=page_at)

    def page_text(self, *, page_at: int) -> str:
        """All of a page's words, as the typesetter read them."""
        return self.action("page_text", page_at=page_at)

    def selection_text(self,
        *,
        page_at: int,
        start: list[float],
        end: list[float],
    ) -> str:
        """The words of a page inside the box from `start` to `end`."""
        return self.action("selection_text", page_at=page_at, start=start, end=end)

    def page_words(self, *, page_at: int) -> list[CellRow]:
        """The placed glyphs of one page, as the typesetter read them."""
        return self.action("page_words", page_at=page_at)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"typeset", "typeset_many", "typeset_if_stale", "typeset_if_stale_many", "take_problems", "take_problems_many", "take_pages", "take_pages_many", "read_outline", "read_outline_many", "draw_page", "draw_page_many", "draw_next", "draw_next_many", "side_by_side", "side_by_side_many", "editor_view", "editor_view_many", "preview_view", "preview_view_many", "problems", "problems_many", "outline", "outline_many", "zoom_in", "zoom_in_many", "zoom_out", "zoom_out_many", "fit_width", "fit_width_many", "turn_to", "turn_to_many", "go_to_problem", "go_to_problem_many", "go_to_heading", "go_to_heading_many", "go_to", "go_to_many", "save_file", "export_pdf", "copy_selection", "copy_page", "page_text", "selection_text", "page_words"})


class Library(Resource):
    """Library — `tl.so.tilo.typst-editor.Library`."""
    __surface__ = "so.tilo.typst-editor@4cf9b224fe02d3e5"
    jdegntgs: list[Entry[Document]]  # documents

    def open_typ(self, *, file: Any) -> Document:
        """A Typst file opens in the editor: Files, the person's own file system and the system's search hand it over as the file it is, and it opens on the document those words are."""
        return self.action("open_typ", file=file)

    def new_document(self,
        *,
        name: str | None = None,
        source: str | None = None,
    ) -> Document:
        """A new, empty document."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        if source is not None:
            args["source"] = source
        return self.action("new_document", **args)

    def close_document(self, *, d: Document) -> None:
        """A document leaves the workspace."""
        return self.action("close_document", d=d)

    def close_document_many(self, items: list[CloseDocumentArgs]) -> list[dict[str, Any]]:
        """Runs `close_document` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("close_document", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"open_typ", "new_document", "close_document", "close_document_many"})


class CompileAsk(Resource):
    """CompileAsk — `tl.so.tilo.typst-editor.CompileAsk`."""
    __surface__ = "so.tilo.typst-editor@4cf9b224fe02d3e5"
    d2jzgpyx: str  # source


class PageAsk(Resource):
    """PageAsk — `tl.so.tilo.typst-editor.PageAsk`."""
    __surface__ = "so.tilo.typst-editor@4cf9b224fe02d3e5"
    a66w2zy6: str  # source
    @property
    def _6jynr6xk(self) -> int:
        """page (`6jynr6xk` on the wire)."""
        return self.fields["6jynr6xk"]

    @_6jynr6xk.setter
    def _6jynr6xk(self, value: int) -> None:
        self.edit("6jynr6xk", value=value)
    m7iybhvb: int  # width


class TextAsk(Resource):
    """TextAsk — `tl.so.tilo.typst-editor.TextAsk`."""
    __surface__ = "so.tilo.typst-editor@4cf9b224fe02d3e5"
    rydvb6fi: str  # source
    rqodn5jw: int  # page


class OutlineAsk(Resource):
    """OutlineAsk — `tl.so.tilo.typst-editor.OutlineAsk`."""
    __surface__ = "so.tilo.typst-editor@4cf9b224fe02d3e5"
    wevbiksf: str  # source


class PageRow(Resource):
    """PageRow — `tl.so.tilo.typst-editor.PageRow`."""
    __surface__ = "so.tilo.typst-editor@4cf9b224fe02d3e5"
    mtlxa3ps: float  # width
    ay4kqc7o: float  # height
    ehra7vlr: str  # look


class ProblemRow(Resource):
    """ProblemRow — `tl.so.tilo.typst-editor.ProblemRow`."""
    __surface__ = "so.tilo.typst-editor@4cf9b224fe02d3e5"
    @property
    def _7vzeic6n(self) -> str:
        """kind (`7vzeic6n` on the wire)."""
        return self.fields["7vzeic6n"]

    @_7vzeic6n.setter
    def _7vzeic6n(self, value: str) -> None:
        self.edit("7vzeic6n", value=value)
    @property
    def _55dyzse3(self) -> str:
        """message (`55dyzse3` on the wire)."""
        return self.fields["55dyzse3"]

    @_55dyzse3.setter
    def _55dyzse3(self, value: str) -> None:
        self.edit("55dyzse3", value=value)
    bzomsyem: int | None  # line
    vwara56h: int | None  # column
    lewaebu2: int | None  # end_line
    unj2zbun: int | None  # end_column
    wakty2b2: list[str]  # hints


class CompiledRow(Resource):
    """CompiledRow — `tl.so.tilo.typst-editor.CompiledRow`."""
    __surface__ = "so.tilo.typst-editor@4cf9b224fe02d3e5"
    ffvtdeuv: bool  # ok
    t5b6wta3: int  # elapsed_ms
    siiy7bea: str | None  # pdf_url
    htehc63o: str | None  # pdf_name
    @property
    def _63pe7jqy(self) -> int | None:
        """pdf_size (`63pe7jqy` on the wire)."""
        return self.fields["63pe7jqy"]

    @_63pe7jqy.setter
    def _63pe7jqy(self, value: int | None) -> None:
        self.edit("63pe7jqy", value=value)
    uryvrrzv: str | None  # pdf_trouble


class HeadingRow(Resource):
    """HeadingRow — `tl.so.tilo.typst-editor.HeadingRow`."""
    __surface__ = "so.tilo.typst-editor@4cf9b224fe02d3e5"
    @property
    def _3olgjoh6(self) -> int:
        """line (`3olgjoh6` on the wire)."""
        return self.fields["3olgjoh6"]

    @_3olgjoh6.setter
    def _3olgjoh6(self, value: int) -> None:
        self.edit("3olgjoh6", value=value)
    lcdznshm: int  # level
    pq3mu5rf: str  # title


class CellRow(Resource):
    """CellRow — `tl.so.tilo.typst-editor.CellRow`."""
    __surface__ = "so.tilo.typst-editor@4cf9b224fe02d3e5"
    dsvn4b2o: str  # text
    erwowgdw: float  # x0
    txaxsk54: float  # x1
    vobo3z4e: float  # top
    w6ydh2nv: float  # bottom
    @property
    def _73rf4vlf(self) -> int:
        """line (`73rf4vlf` on the wire)."""
        return self.fields["73rf4vlf"]

    @_73rf4vlf.setter
    def _73rf4vlf(self, value: int) -> None:
        self.edit("73rf4vlf", value=value)


class TypstEditor(Resource):
    """Typst — `tl.so.tilo.typst-editor`."""
    __surface__ = "so.tilo.typst-editor@4cf9b224fe02d3e5"

