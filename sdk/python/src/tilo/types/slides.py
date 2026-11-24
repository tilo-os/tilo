"""Types for Slides (`so.tilo.slides`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Deck` is the app's `slides.deck` at `slides/<resource>`, read with
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
    "OutlinedSlide",
    "PaletteKind",
    "Slide",
    "SlideData",
    "TextAlign",
    "Deck",
    "InsertSlideArgs",
    "DuplicateSlideArgs",
    "DeleteSlideArgs",
    "InsertElementArgs",
    "DeleteElementArgs",
    "BringToFrontArgs",
    "SendToBackArgs",
    "ReplaceDeckArgs",
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

class OutlinedSlide(TypedDict):
    title: str | None
    body: str | None
    layout: Layout | None
    notes: str | None

PaletteKind = Literal["indigo", "emerald", "amber", "rose", "sky"]

class Slide(TypedDict):
    layout: Layout
    background: Background
    notes: str
    elements: list[Entry[Element]]

class SlideData(TypedDict):
    id: str
    layout: Layout
    background: Background
    notes: str
    elements: list[ElementData]

TextAlign = Literal["left", "center", "right", "justify"]

InsertSlideArgs = TypedDict("InsertSlideArgs", {
    "at": "int",
    "slide": "SlideData",
})
"""The arguments of `insert-slide`."""

DuplicateSlideArgs = TypedDict("DuplicateSlideArgs", {
    "slide": "str",
})
"""The arguments of `duplicate-slide`."""

DeleteSlideArgs = TypedDict("DeleteSlideArgs", {
    "slide": "str",
})
"""The arguments of `delete-slide`."""

InsertElementArgs = TypedDict("InsertElementArgs", {
    "slide": "str",
    "element": "ElementData",
})
"""The arguments of `insert-element`."""

DeleteElementArgs = TypedDict("DeleteElementArgs", {
    "slide": "str",
    "element": "str",
})
"""The arguments of `delete-element`."""

BringToFrontArgs = TypedDict("BringToFrontArgs", {
    "slide": "str",
    "element": "str",
})
"""The arguments of `bring-to-front`."""

SendToBackArgs = TypedDict("SendToBackArgs", {
    "slide": "str",
    "element": "str",
})
"""The arguments of `send-to-back`."""

ReplaceDeckArgs = TypedDict("ReplaceDeckArgs", {
    "title": "str",
    "palette": "PaletteKind",
    "look": NotRequired["DeckLook | None"],
    "slides": "list[SlideData]",
})
"""The arguments of `replace-deck`."""

class Deck(Resource):
    """Deck — `slides.deck`."""
    __surface__ = "so.tilo.slides@6a033983332a1bd4"
    title: str
    palette: PaletteKind
    look: DeckLook
    slides: list[Entry[Slide]]

    def add_slide(self,
        *,
        title: str | None = None,
        body: str | None = None,
        layout: Layout | None = None,
        after: str | None = None,
    ) -> Any:
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

    def add_slides(self,
        *,
        slides: list[OutlinedSlide],
        after: str | None = None,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["slides"] = slides
        if after is not None:
            args["after"] = after
        return self.action("add-slides", **args)

    def insert_slide(self,
        *,
        at: int,
        slide: SlideData,
    ) -> None:
        """Runs the app's action."""
        return self.action("insert-slide", at=at, slide=slide)

    def insert_slide_many(self, items: list[InsertSlideArgs]) -> list[dict[str, Any]]:
        """Runs `insert-slide` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("insert-slide", items)

    def duplicate_slide(self, *, slide: str) -> None:
        """Runs the app's action."""
        return self.action("duplicate-slide", slide=slide)

    def duplicate_slide_many(self, items: list[DuplicateSlideArgs]) -> list[dict[str, Any]]:
        """Runs `duplicate-slide` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("duplicate-slide", items)

    def delete_slide(self, *, slide: str) -> None:
        """Runs the app's action."""
        return self.action("delete-slide", slide=slide)

    def delete_slide_many(self, items: list[DeleteSlideArgs]) -> list[dict[str, Any]]:
        """Runs `delete-slide` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-slide", items)

    def move_slide(self,
        *,
        slide: str,
        position: int,
    ) -> Any:
        """Runs the app's action."""
        return self.action("move-slide", slide=slide, position=position)

    def set_text(self,
        *,
        slide: str,
        title: str | None = None,
        body: str | None = None,
    ) -> Any:
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
    ) -> Any:
        """Runs the app's action."""
        return self.action("add-element", slide=slide, element=element)

    def insert_element(self,
        *,
        slide: str,
        element: ElementData,
    ) -> None:
        """Runs the app's action."""
        return self.action("insert-element", slide=slide, element=element)

    def insert_element_many(self, items: list[InsertElementArgs]) -> list[dict[str, Any]]:
        """Runs `insert-element` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("insert-element", items)

    def delete_element(self,
        *,
        slide: str,
        element: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("delete-element", slide=slide, element=element)

    def delete_element_many(self, items: list[DeleteElementArgs]) -> list[dict[str, Any]]:
        """Runs `delete-element` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-element", items)

    def bring_to_front(self,
        *,
        slide: str,
        element: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("bring-to-front", slide=slide, element=element)

    def bring_to_front_many(self, items: list[BringToFrontArgs]) -> list[dict[str, Any]]:
        """Runs `bring-to-front` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("bring-to-front", items)

    def send_to_back(self,
        *,
        slide: str,
        element: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("send-to-back", slide=slide, element=element)

    def send_to_back_many(self, items: list[SendToBackArgs]) -> list[dict[str, Any]]:
        """Runs `send-to-back` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("send-to-back", items)

    def replace_deck(self,
        *,
        title: str,
        palette: PaletteKind,
        slides: list[SlideData],
        look: DeckLook | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["title"] = title
        args["palette"] = palette
        if look is not None:
            args["look"] = look
        args["slides"] = slides
        return self.action("replace-deck", **args)

    def replace_deck_many(self, items: list[ReplaceDeckArgs]) -> list[dict[str, Any]]:
        """Runs `replace-deck` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("replace-deck", items)

    def read_deck(self) -> None:
        """Runs the app's action."""
        return self.action("read-deck")

    def export_pptx_content(self) -> None:
        """Runs the app's action."""
        return self.action("export-pptx-content")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"add_slide", "add_slides", "insert_slide", "insert_slide_many", "duplicate_slide", "duplicate_slide_many", "delete_slide", "delete_slide_many", "move_slide", "set_text", "add_element", "insert_element", "insert_element_many", "delete_element", "delete_element_many", "bring_to_front", "bring_to_front_many", "send_to_back", "send_to_back_many", "replace_deck", "replace_deck_many", "read_deck", "export_pptx_content"})


