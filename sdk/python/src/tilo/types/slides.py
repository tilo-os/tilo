"""Types for Slides (`so.tilo.slides`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Deck` is the app's `slides.deck` at `slides/<resource>`, read with
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
    "Background",
    "ClipAttr",
    "ClipBlock",
    "DeckLook",
    "Element",
    "ElementData",
    "ElementKind",
    "Layout",
    "MarkSpan",
    "MarkValue",
    "NewElement",
    "PaletteKind",
    "Slide",
    "SlideData",
    "TextAlign",
    "Deck",
]

Background = Literal["surface", "accent", "deep"]

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

DeckLook = Literal["light", "dark"]

class Element(TypedDict):
    kind: ElementKind
    x: float
    y: float
    w: float
    h: float
    body: str
    src: str
    fill: str
    stroke: str
    stroke_width: float
    font_size: float
    align: TextAlign
    bold: bool
    italic: bool
    radius: float

class ElementData(TypedDict):
    id: str
    kind: ElementKind
    x: float
    y: float
    w: float
    h: float
    blocks: list[ClipBlock]
    src: str
    fill: str
    stroke: str
    stroke_width: float
    font_size: float
    align: TextAlign
    bold: bool
    italic: bool
    radius: float

ElementKind = Literal["text", "rect", "ellipse", "line", "image"]

Layout = Literal["title", "title_content", "two_column", "section", "blank"]

class MarkSpan(TypedDict):
    start: int
    end: int
    name: str
    value: MarkValue

MarkValue = dict[str, Any]
"""One of Bool, Int, Str: an object whose case the app tags it with."""

class NewElement(TypedDict):
    kind: ElementKind
    x: float
    y: float
    w: float
    h: float
    text: str | None
    src: str | None
    fill: str | None
    text_color: str | None
    stroke: str | None
    stroke_width: float | None
    font_size: float | None
    align: TextAlign | None
    bold: bool | None
    italic: bool | None
    radius: float | None

PaletteKind = Literal["indigo", "emerald", "amber", "rose", "sky"]

class Slide(TypedDict):
    layout: Layout
    background: Background
    notes: str
    elements: list[Element]

class SlideData(TypedDict):
    id: str
    layout: Layout
    background: Background
    notes: str
    elements: list[ElementData]

TextAlign = Literal["left", "center", "right", "justify"]

class Deck(Resource):
    """Deck — `slides.deck`."""
    __surface__ = "so.tilo.slides@ecf6ae57e9051fc7"
    title: str
    palette: PaletteKind
    look: DeckLook
    slides: list[Slide]

    def add_slide(self,
        *,
        title: str | None = None,
        body: str | None = None,
        layout: Layout | None = None,
        after: str | None = None,
    ) -> Any | Run:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if title is not None:
            args["title"] = title
        if body is not None:
            args["body"] = body
        if layout is not None:
            args["layout"] = layout
        if after is not None:
            args["after"] = after
        return self.action("add-slide", **args)

    def insert_slide(self,
        *,
        at: int,
        slide: SlideData,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("insert-slide", at=at, slide=slide)

    def duplicate_slide(self, *, slide: str) -> Run | None:
        """Runs the app's action."""
        return self.action("duplicate-slide", slide=slide)

    def delete_slide(self, *, slide: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-slide", slide=slide)

    def move_slide(self,
        *,
        slide: str,
        position: int,
    ) -> Any | Run:
        """Runs the app's action."""
        return self.action("move-slide", slide=slide, position=position)

    def set_text(self,
        *,
        slide: str,
        title: str | None = None,
        body: str | None = None,
    ) -> Any | Run:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["slide"] = slide
        if title is not None:
            args["title"] = title
        if body is not None:
            args["body"] = body
        return self.action("set-text", **args)

    def add_element(self,
        *,
        slide: str,
        element: NewElement,
    ) -> Any | Run:
        """Runs the app's action."""
        return self.action("add-element", slide=slide, element=element)

    def insert_element(self,
        *,
        slide: str,
        element: ElementData,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("insert-element", slide=slide, element=element)

    def delete_element(self,
        *,
        slide: str,
        element: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-element", slide=slide, element=element)

    def bring_to_front(self,
        *,
        slide: str,
        element: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("bring-to-front", slide=slide, element=element)

    def send_to_back(self,
        *,
        slide: str,
        element: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("send-to-back", slide=slide, element=element)

    def replace_deck(self,
        *,
        title: str,
        palette: PaletteKind,
        slides: list[SlideData],
        look: DeckLook | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["title"] = title
        args["palette"] = palette
        if look is not None:
            args["look"] = look
        args["slides"] = slides
        return self.action("replace-deck", **args)

    def read_deck(self) -> None:
        """Runs the app's action."""
        return self.action("read-deck")

    def export_pptx_content(self) -> None:
        """Runs the app's action."""
        return self.action("export-pptx-content")


