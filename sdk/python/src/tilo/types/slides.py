"""Types for Slides (`so.tilo.slides`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Deck` is the app's `tl.so.tilo.slides.Deck` at `slides/<resource>`, read with
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
    "Align",
    "Background",
    "ElementKind",
    "Layout",
    "Look",
    "Palette",
    "Deck",
    "Slide",
    "Element",
    "DeckFile",
    "SlideFile",
    "ElementFile",
    "Outline",
    "Colours",
    "Slides",
    "DuplicateSlideArgs",
    "DuplicateCurrentSlideArgs",
    "DeleteSlideArgs",
    "MoveSlideArgs",
    "SetTextArgs",
    "SetPaletteArgs",
    "SetLookArgs",
    "SetTitleArgs",
    "OpenPresentationArgs",
    "AdoptArgs",
    "ClearNoticeArgs",
    "ImportFileArgs",
    "ImportFilesArgs",
    "BeginPresentingArgs",
    "EndPresentingArgs",
    "PauseClockArgs",
    "ResumeClockArgs",
    "AdoptSlideArgs",
    "EditBlockArgs",
    "InsertBlockArgs",
    "DeleteBlockArgs",
    "DuplicateElementArgs",
    "DeleteElementArgs",
    "BringToFrontArgs",
    "SendToBackArgs",
    "MoveElementArgs",
    "MoveElementToArgs",
    "NudgeElementToArgs",
    "ResizeElementArgs",
    "ResizeElementToArgs",
    "SetLayoutArgs",
    "SetBackgroundArgs",
    "SetNotesArgs",
    "PasteElementArgs",
    "AddPictureArgs",
    "ElementEditBlockArgs",
    "ElementInsertBlockArgs",
    "ElementDeleteBlockArgs",
    "SetStyleArgs",
]

Align = Literal["left", "center", "right", "justify"]
"""How a text box sets its words."""

Background = Literal["surface", "accent", "deep"]
"""What a slide stands on: the deck's surface, its accent, or its deep tone."""

ElementKind = Literal["text", "rect", "ellipse", "line", "image"]
"""What an element is: words, a shape, a line or a picture."""

Layout = Literal["title", "title_content", "two_column", "section", "blank"]
"""How a slide arranges what it holds: the shape its content boxes start in."""

Look = Literal["light", "dark"]
"""A deck's own light or dark surfaces, so it looks the same on every screen and in a saved file."""

Palette = Literal["indigo", "emerald", "amber", "rose", "sky"]
"""Which hue drives the deck's accent. Every other colour comes from the deck's look."""

DuplicateSlideArgs = TypedDict("DuplicateSlideArgs", {
    "s": "Slide",
})
"""The arguments of `duplicate_slide`."""

DuplicateCurrentSlideArgs = TypedDict("DuplicateCurrentSlideArgs", {
    "showing": "str",
})
"""The arguments of `duplicate_current_slide`."""

DeleteSlideArgs = TypedDict("DeleteSlideArgs", {
    "s": "Slide",
})
"""The arguments of `delete_slide`."""

MoveSlideArgs = TypedDict("MoveSlideArgs", {
    "s": "Slide",
    "at": "int",
})
"""The arguments of `move_slide`."""

SetTextArgs = TypedDict("SetTextArgs", {
    "s": "Slide",
    "title": NotRequired["str | None"],
    "body": NotRequired["str | None"],
})
"""The arguments of `set_text`."""

SetPaletteArgs = TypedDict("SetPaletteArgs", {
    "palette": "Palette",
})
"""The arguments of `set_palette`."""

SetLookArgs = TypedDict("SetLookArgs", {
    "look": "Look",
})
"""The arguments of `set_look`."""

SetTitleArgs = TypedDict("SetTitleArgs", {
    "title": "str",
})
"""The arguments of `set_title`."""

OpenPresentationArgs = TypedDict("OpenPresentationArgs", {
    "file": "Any",
})
"""The arguments of `open_presentation`."""

AdoptArgs = TypedDict("AdoptArgs", {
    "file": "Any",
})
"""The arguments of `adopt`."""

ClearNoticeArgs = TypedDict("ClearNoticeArgs", {
})
"""The arguments of `clear_notice`."""

ImportFileArgs = TypedDict("ImportFileArgs", {
    "file": "Any",
})
"""The arguments of `import_file`."""

ImportFilesArgs = TypedDict("ImportFilesArgs", {
    "files": "list[Any]",
})
"""The arguments of `import_files`."""

BeginPresentingArgs = TypedDict("BeginPresentingArgs", {
})
"""The arguments of `begin_presenting`."""

EndPresentingArgs = TypedDict("EndPresentingArgs", {
})
"""The arguments of `end_presenting`."""

PauseClockArgs = TypedDict("PauseClockArgs", {
})
"""The arguments of `pause_clock`."""

ResumeClockArgs = TypedDict("ResumeClockArgs", {
})
"""The arguments of `resume_clock`."""

AdoptSlideArgs = TypedDict("AdoptSlideArgs", {
    "f": "SlideFile",
})
"""The arguments of `adopt_slide`."""

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

DuplicateElementArgs = TypedDict("DuplicateElementArgs", {
    "e": "Element",
})
"""The arguments of `duplicate_element`."""

DeleteElementArgs = TypedDict("DeleteElementArgs", {
    "e": "Element",
})
"""The arguments of `delete_element`."""

BringToFrontArgs = TypedDict("BringToFrontArgs", {
    "e": "Element",
})
"""The arguments of `bring_to_front`."""

SendToBackArgs = TypedDict("SendToBackArgs", {
    "e": "Element",
})
"""The arguments of `send_to_back`."""

MoveElementArgs = TypedDict("MoveElementArgs", {
    "e": "Element",
    "x": "int",
    "y": "int",
})
"""The arguments of `move_element`."""

MoveElementToArgs = TypedDict("MoveElementToArgs", {
    "target": "str",
    "at": "list[int]",
})
"""The arguments of `move_element_to`."""

NudgeElementToArgs = TypedDict("NudgeElementToArgs", {
    "target": "str",
    "dx": "int",
    "dy": "int",
})
"""The arguments of `nudge_element_to`."""

ResizeElementArgs = TypedDict("ResizeElementArgs", {
    "e": "Element",
    "at": "list[int]",
})
"""The arguments of `resize_element`."""

ResizeElementToArgs = TypedDict("ResizeElementToArgs", {
    "target": "str",
    "at": "list[int]",
})
"""The arguments of `resize_element_to`."""

SetLayoutArgs = TypedDict("SetLayoutArgs", {
    "layout": "Layout",
})
"""The arguments of `set_layout`."""

SetBackgroundArgs = TypedDict("SetBackgroundArgs", {
    "background": "Background",
})
"""The arguments of `set_background`."""

SetNotesArgs = TypedDict("SetNotesArgs", {
    "notes": "str",
})
"""The arguments of `set_notes`."""

PasteElementArgs = TypedDict("PasteElementArgs", {
})
"""The arguments of `paste_element`."""

AddPictureArgs = TypedDict("AddPictureArgs", {
    "file": "Any",
})
"""The arguments of `add_picture`."""

ElementEditBlockArgs = TypedDict("ElementEditBlockArgs", {
    "block": "str",
    "text": "str",
})
"""The arguments of `edit-block`."""

ElementInsertBlockArgs = TypedDict("ElementInsertBlockArgs", {
    "block": "str",
    "text": "str",
})
"""The arguments of `insert-block`."""

ElementDeleteBlockArgs = TypedDict("ElementDeleteBlockArgs", {
    "block": "str",
})
"""The arguments of `delete-block`."""

SetStyleArgs = TypedDict("SetStyleArgs", {
    "fill": NotRequired["str | None"],
    "stroke": NotRequired["str | None"],
    "stroke_width": NotRequired["float | None"],
    "font_size": NotRequired["float | None"],
    "align": NotRequired["Align | None"],
    "bold": NotRequired["bool | None"],
    "italic": NotRequired["bool | None"],
    "radius": NotRequired["float | None"],
})
"""The arguments of `set_style`."""

class Deck(Resource):
    """Deck — `tl.so.tilo.slides.Deck`."""
    __surface__ = "so.tilo.slides@cd42c8463e98a698"
    pwdtrcuk: str  # title
    ep3yewja: Palette  # palette
    @property
    def _3xehcyrf(self) -> Look:
        """look (`3xehcyrf` on the wire)."""
        return self.fields["3xehcyrf"]

    @_3xehcyrf.setter
    def _3xehcyrf(self, value: Look) -> None:
        self.edit("3xehcyrf", value=value)
    nhumnzoj: list[Entry[Slide]]  # slides
    weqf5aou: str | None  # presenting_since
    u6doynxu: int  # clock_held
    xwjxu7wj: bool  # clock_paused
    sy2pbelg: str  # notice

    def add_slide(self,
        *,
        title: str | None = None,
        body: str | None = None,
        layout: Layout | None = None,
        after: Slide | None = None,
    ) -> Slide:
        """Adds a slide after `after`, or at the end, from the template its layout names."""
        args: dict[str, Any] = {}
        if title is not None:
            args["title"] = title
        if body is not None:
            args["body"] = body
        if layout is not None:
            args["layout"] = layout
        if after is not None:
            args["after"] = after
        return self.action("add_slide", **args)

    def add_authored_slide(self,
        *,
        title: str,
        body: str,
        fallback: Layout,
        layout: Layout | None = None,
        notes: str | None = None,
        after: Slide | None = None,
    ) -> Slide:
        """Adds a slide from the template its layout names, and answers it."""
        args: dict[str, Any] = {}
        args["title"] = title
        args["body"] = body
        if layout is not None:
            args["layout"] = layout
        if notes is not None:
            args["notes"] = notes
        args["fallback"] = fallback
        if after is not None:
            args["after"] = after
        return self.action("add_authored_slide", **args)

    def add_slides(self,
        *,
        outline: str,
        after: Slide | None = None,
    ) -> Any:
        """Adds several slides in one step, in the order given: a deck written from an outline, as the JSON a person or an agent hands over."""
        args: dict[str, Any] = {}
        args["outline"] = outline
        if after is not None:
            args["after"] = after
        return self.action("add_slides", **args)

    def duplicate_slide(self, *, s: Slide) -> None:
        """Copies a slide and everything on it, right after it."""
        return self.action("duplicate_slide", s=s)

    def duplicate_slide_many(self, items: list[DuplicateSlideArgs]) -> list[dict[str, Any]]:
        """Runs `duplicate_slide` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("duplicate_slide", items)

    def duplicate_current_slide(self, *, showing: str) -> None:
        """Copies the slide the window shows, right after it."""
        return self.action("duplicate_current_slide", showing=showing)

    def duplicate_current_slide_many(self, items: list[DuplicateCurrentSlideArgs]) -> list[dict[str, Any]]:
        """Runs `duplicate_current_slide` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("duplicate_current_slide", items)

    def delete_slide(self, *, s: Slide) -> None:
        """Deletes a slide. A deck keeps at least one."""
        return self.action("delete_slide", s=s)

    def delete_slide_many(self, items: list[DeleteSlideArgs]) -> list[dict[str, Any]]:
        """Runs `delete_slide` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_slide", items)

    def move_slide(self,
        *,
        s: Slide,
        at: int,
    ) -> None:
        """Moves a slide to `at`, the place among the slides a drag reports."""
        return self.action("move_slide", s=s, at=at)

    def move_slide_many(self, items: list[MoveSlideArgs]) -> list[dict[str, Any]]:
        """Runs `move_slide` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_slide", items)

    def set_text(self,
        *,
        s: Slide,
        title: str | None = None,
        body: str | None = None,
    ) -> None:
        """Writes a slide's title and body: the first two text boxes, as the templates make them."""
        args: dict[str, Any] = {}
        args["s"] = s
        if title is not None:
            args["title"] = title
        if body is not None:
            args["body"] = body
        return self.action("set_text", **args)

    def set_text_many(self, items: list[SetTextArgs]) -> list[dict[str, Any]]:
        """Runs `set_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_text", items)

    def set_palette(self, *, palette: Palette) -> None:
        """Sets the deck's accent hue."""
        return self.action("set_palette", palette=palette)

    def set_palette_many(self, items: list[SetPaletteArgs]) -> list[dict[str, Any]]:
        """Runs `set_palette` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_palette", items)

    def set_look(self, *, look: Look) -> None:
        """Sets the deck's light or dark surfaces."""
        return self.action("set_look", look=look)

    def set_look_many(self, items: list[SetLookArgs]) -> list[dict[str, Any]]:
        """Runs `set_look` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_look", items)

    def set_title(self, *, title: str) -> None:
        """Names the deck."""
        return self.action("set_title", title=title)

    def set_title_many(self, items: list[SetTitleArgs]) -> list[dict[str, Any]]:
        """Runs `set_title` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_title", items)

    def read_deck(self) -> str:
        """A deck as text: its slides in order, each title, body and other elements with its id, and its speaker notes. An agent reads a deck without the window."""
        return self.action("read_deck")

    def open_presentation(self, *, file: Any) -> None:
        """Replaces the deck with the one a presentation file holds. The file is opened by the app that opens its kind."""
        return self.action("open_presentation", file=file)

    def open_presentation_many(self, items: list[OpenPresentationArgs]) -> list[dict[str, Any]]:
        """Runs `open_presentation` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open_presentation", items)

    def export_odp(self) -> Any:
        """The deck as an OpenDocument presentation."""
        return self.action("export_odp")

    def export_pptx(self) -> Any:
        """The deck as a PowerPoint presentation."""
        return self.action("export_pptx")

    def adopt(self, *, file: Any) -> None:
        """Reads a deck file into this deck: its slides replace what the deck holds."""
        return self.action("adopt", file=file)

    def adopt_many(self, items: list[AdoptArgs]) -> list[dict[str, Any]]:
        """Runs `adopt` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("adopt", items)

    def clear_notice(self) -> None:
        """Closes the message a deck shows about a file it could not open."""
        return self.action("clear_notice")

    def clear_notice_many(self, items: list[ClearNoticeArgs]) -> list[dict[str, Any]]:
        """Runs `clear_notice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_notice", items)

    def import_file(self, *, file: Any) -> None:
        """The deck a file holds, kept as this deck: its slides replace what it held."""
        return self.action("import_file", file=file)

    def import_file_many(self, items: list[ImportFileArgs]) -> list[dict[str, Any]]:
        """Runs `import_file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import_file", items)

    def import_files(self, *, files: list[Any]) -> None:
        """Opens the first presentation of a set dropped on the window."""
        return self.action("import_files", files=files)

    def import_files_many(self, items: list[ImportFilesArgs]) -> list[dict[str, Any]]:
        """Runs `import_files` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import_files", items)

    def begin_presenting(self) -> None:
        """Starts the presentation clock: the speaker's elapsed time runs from now."""
        return self.action("begin_presenting")

    def begin_presenting_many(self, items: list[BeginPresentingArgs]) -> list[dict[str, Any]]:
        """Runs `begin_presenting` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("begin_presenting", items)

    def end_presenting(self) -> None:
        """Stops the presentation clock."""
        return self.action("end_presenting")

    def end_presenting_many(self, items: list[EndPresentingArgs]) -> list[dict[str, Any]]:
        """Runs `end_presenting` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("end_presenting", items)

    def pause_clock(self) -> None:
        """Holds the clock where it stands, until the speaker starts it again."""
        return self.action("pause_clock")

    def pause_clock_many(self, items: list[PauseClockArgs]) -> list[dict[str, Any]]:
        """Runs `pause_clock` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("pause_clock", items)

    def resume_clock(self) -> None:
        """Runs the clock on from where it was held."""
        return self.action("resume_clock")

    def resume_clock_many(self, items: list[ResumeClockArgs]) -> list[dict[str, Any]]:
        """Runs `resume_clock` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("resume_clock", items)

    def adopt_slide(self, *, f: SlideFile) -> None:
        """Keeps one slide a deck file brings."""
        return self.action("adopt_slide", f=f)

    def adopt_slide_many(self, items: list[AdoptSlideArgs]) -> list[dict[str, Any]]:
        """Runs `adopt_slide` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("adopt_slide", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"add_slide", "add_authored_slide", "add_slides", "duplicate_slide", "duplicate_slide_many", "duplicate_current_slide", "duplicate_current_slide_many", "delete_slide", "delete_slide_many", "move_slide", "move_slide_many", "set_text", "set_text_many", "set_palette", "set_palette_many", "set_look", "set_look_many", "set_title", "set_title_many", "read_deck", "open_presentation", "open_presentation_many", "export_odp", "export_pptx", "adopt", "adopt_many", "clear_notice", "clear_notice_many", "import_file", "import_file_many", "import_files", "import_files_many", "begin_presenting", "begin_presenting_many", "end_presenting", "end_presenting_many", "pause_clock", "pause_clock_many", "resume_clock", "resume_clock_many", "adopt_slide", "adopt_slide_many"})


class Slide(Resource):
    """Slide — `tl.so.tilo.slides.Slide`."""
    __surface__ = "so.tilo.slides@cd42c8463e98a698"
    w3sjwjk2: Layout  # layout
    x3bud3c2: Background  # background
    qowtxual: str  # notes
    lbme2z4w: list[Entry[Element]]  # elements

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

    def duplicate_element(self, *, e: Element) -> None:
        """Copies an element in place on its slide, a little down and across from it."""
        return self.action("duplicate_element", e=e)

    def duplicate_element_many(self, items: list[DuplicateElementArgs]) -> list[dict[str, Any]]:
        """Runs `duplicate_element` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("duplicate_element", items)

    def add_element(self,
        *,
        kind: ElementKind,
        x: int | None = None,
        y: int | None = None,
        w: int | None = None,
        h: int | None = None,
        text: str | None = None,
        src: str | None = None,
        fill: str | None = None,
        text_color: str | None = None,
        stroke: str | None = None,
        stroke_width: float | None = None,
        font_size: float | None = None,
        align: Align | None = None,
        bold: bool | None = None,
        italic: bool | None = None,
        radius: float | None = None,
    ) -> Element:
        """Adds an element to a slide: what it is, where it stands, and any style it takes."""
        args: dict[str, Any] = {}
        args["kind"] = kind
        if x is not None:
            args["x"] = x
        if y is not None:
            args["y"] = y
        if w is not None:
            args["w"] = w
        if h is not None:
            args["h"] = h
        if text is not None:
            args["text"] = text
        if src is not None:
            args["src"] = src
        if fill is not None:
            args["fill"] = fill
        if text_color is not None:
            args["text_color"] = text_color
        if stroke is not None:
            args["stroke"] = stroke
        if stroke_width is not None:
            args["stroke_width"] = stroke_width
        if font_size is not None:
            args["font_size"] = font_size
        if align is not None:
            args["align"] = align
        if bold is not None:
            args["bold"] = bold
        if italic is not None:
            args["italic"] = italic
        if radius is not None:
            args["radius"] = radius
        return self.action("add_element", **args)

    def insert_element(self, *, element: str) -> Any:
        """Inserts an element given whole, as the JSON another deck copied: a paste from anywhere."""
        return self.action("insert_element", element=element)

    def delete_element(self, *, e: Element) -> None:
        """Deletes an element."""
        return self.action("delete_element", e=e)

    def delete_element_many(self, items: list[DeleteElementArgs]) -> list[dict[str, Any]]:
        """Runs `delete_element` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_element", items)

    def bring_to_front(self, *, e: Element) -> None:
        """Brings an element to the front."""
        return self.action("bring_to_front", e=e)

    def bring_to_front_many(self, items: list[BringToFrontArgs]) -> list[dict[str, Any]]:
        """Runs `bring_to_front` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("bring_to_front", items)

    def send_to_back(self, *, e: Element) -> None:
        """Sends an element to the back."""
        return self.action("send_to_back", e=e)

    def send_to_back_many(self, items: list[SendToBackArgs]) -> list[dict[str, Any]]:
        """Runs `send_to_back` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("send_to_back", items)

    def move_element(self,
        *,
        e: Element,
        x: int,
        y: int,
    ) -> None:
        """Moves an element to a place on the slide, as a drag or the arrow keys place it."""
        return self.action("move_element", e=e, x=x, y=y)

    def move_element_many(self, items: list[MoveElementArgs]) -> list[dict[str, Any]]:
        """Runs `move_element` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_element", items)

    def move_element_to(self,
        *,
        target: str,
        at: list[int],
    ) -> None:
        """Moves the element `target` names to `at`, as a drag on the stage reports it."""
        return self.action("move_element_to", target=target, at=at)

    def move_element_to_many(self, items: list[MoveElementToArgs]) -> list[dict[str, Any]]:
        """Runs `move_element_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_element_to", items)

    def nudge_element_to(self,
        *,
        target: str,
        dx: int,
        dy: int,
    ) -> None:
        """Nudges the element `target` names by `dx` and `dy` slide units."""
        return self.action("nudge_element_to", target=target, dx=dx, dy=dy)

    def nudge_element_to_many(self, items: list[NudgeElementToArgs]) -> list[dict[str, Any]]:
        """Runs `nudge_element_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("nudge_element_to", items)

    def resize_element(self,
        *,
        e: Element,
        at: list[int],
    ) -> None:
        """Sizes an element, as dragging its edge does."""
        return self.action("resize_element", e=e, at=at)

    def resize_element_many(self, items: list[ResizeElementArgs]) -> list[dict[str, Any]]:
        """Runs `resize_element` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("resize_element", items)

    def resize_element_to(self,
        *,
        target: str,
        at: list[int],
    ) -> None:
        """Sizes the element `target` names, as a drag on its edge reports it."""
        return self.action("resize_element_to", target=target, at=at)

    def resize_element_to_many(self, items: list[ResizeElementToArgs]) -> list[dict[str, Any]]:
        """Runs `resize_element_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("resize_element_to", items)

    def set_layout(self, *, layout: Layout) -> None:
        """Sets a slide's layout."""
        return self.action("set_layout", layout=layout)

    def set_layout_many(self, items: list[SetLayoutArgs]) -> list[dict[str, Any]]:
        """Runs `set_layout` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_layout", items)

    def set_background(self, *, background: Background) -> None:
        """Sets what a slide stands on."""
        return self.action("set_background", background=background)

    def set_background_many(self, items: list[SetBackgroundArgs]) -> list[dict[str, Any]]:
        """Runs `set_background` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_background", items)

    def set_notes(self, *, notes: str) -> None:
        """Writes what the speaker says over a slide."""
        return self.action("set_notes", notes=notes)

    def set_notes_many(self, items: list[SetNotesArgs]) -> list[dict[str, Any]]:
        """Runs `set_notes` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_notes", items)

    def cut_element(self, *, e: Element) -> None:
        """Cuts the chosen element: its JSON to the clipboard as another deck pastes it, and the element off the slide."""
        return self.action("cut_element", e=e)

    def paste_element(self) -> None:
        """Pastes an element from the clipboard onto a slide, nudged clear of the one it came from."""
        return self.action("paste_element")

    def paste_element_many(self, items: list[PasteElementArgs]) -> list[dict[str, Any]]:
        """Runs `paste_element` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("paste_element", items)

    def add_picture(self, *, file: Any) -> None:
        """Puts a chosen picture on the slide."""
        return self.action("add_picture", file=file)

    def add_picture_many(self, items: list[AddPictureArgs]) -> list[dict[str, Any]]:
        """Runs `add_picture` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_picture", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"edit_block", "edit_block_many", "insert_block", "insert_block_many", "delete_block", "delete_block_many", "duplicate_element", "duplicate_element_many", "add_element", "insert_element", "delete_element", "delete_element_many", "bring_to_front", "bring_to_front_many", "send_to_back", "send_to_back_many", "move_element", "move_element_many", "move_element_to", "move_element_to_many", "nudge_element_to", "nudge_element_to_many", "resize_element", "resize_element_many", "resize_element_to", "resize_element_to_many", "set_layout", "set_layout_many", "set_background", "set_background_many", "set_notes", "set_notes_many", "cut_element", "paste_element", "paste_element_many", "add_picture", "add_picture_many"})


class Element(Resource):
    """Element — `tl.so.tilo.slides.Element`."""
    __surface__ = "so.tilo.slides@cd42c8463e98a698"
    @property
    def _74of62le(self) -> ElementKind:
        """kind (`74of62le` on the wire)."""
        return self.fields["74of62le"]

    @_74of62le.setter
    def _74of62le(self, value: ElementKind) -> None:
        self.edit("74of62le", value=value)
    b47bgsdf: int  # x
    bh6ok4v5: int  # y
    @property
    def _53kv3kr2(self) -> int:
        """w (`53kv3kr2` on the wire)."""
        return self.fields["53kv3kr2"]

    @_53kv3kr2.setter
    def _53kv3kr2(self, value: int) -> None:
        self.edit("53kv3kr2", value=value)
    b65jrhk6: int  # h
    @property
    def _64oupu64(self) -> str:
        """body (`64oupu64` on the wire)."""
        return self.fields["64oupu64"]

    @_64oupu64.setter
    def _64oupu64(self, value: str) -> None:
        self.edit("64oupu64", value=value)
    qysmcg6z: str  # src
    @property
    def _3u7ptmma(self) -> str:
        """fill (`3u7ptmma` on the wire)."""
        return self.fields["3u7ptmma"]

    @_3u7ptmma.setter
    def _3u7ptmma(self, value: str) -> None:
        self.edit("3u7ptmma", value=value)
    dsypcewy: str  # stroke
    lsbzlmm7: float  # stroke_width
    i6lk64ca: float  # font_size
    kk6sectq: Align  # align
    @property
    def _4g7ap5xd(self) -> bool:
        """bold (`4g7ap5xd` on the wire)."""
        return self.fields["4g7ap5xd"]

    @_4g7ap5xd.setter
    def _4g7ap5xd(self, value: bool) -> None:
        self.edit("4g7ap5xd", value=value)
    haaxnqxs: bool  # italic
    pii7xmb6: float  # radius

    def edit_block(self,
        *,
        block: str,
        text: str,
    ) -> None:
        """Set one paragraph of a rich-text field to what a Markdown text says."""
        return self.action("edit-block", block=block, text=text)

    def edit_block_many(self, items: list[ElementEditBlockArgs]) -> list[dict[str, Any]]:
        """Runs `edit-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("edit-block", items)

    def insert_block(self,
        *,
        block: str,
        text: str,
    ) -> None:
        """Put a paragraph before the one the address names."""
        return self.action("insert-block", block=block, text=text)

    def insert_block_many(self, items: list[ElementInsertBlockArgs]) -> list[dict[str, Any]]:
        """Runs `insert-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("insert-block", items)

    def delete_block(self, *, block: str) -> None:
        """Remove the paragraph the address names, and the paragraphs it holds."""
        return self.action("delete-block", block=block)

    def delete_block_many(self, items: list[ElementDeleteBlockArgs]) -> list[dict[str, Any]]:
        """Runs `delete-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-block", items)

    def set_style(self,
        *,
        fill: str | None = None,
        stroke: str | None = None,
        stroke_width: float | None = None,
        font_size: float | None = None,
        align: Align | None = None,
        bold: bool | None = None,
        italic: bool | None = None,
        radius: float | None = None,
    ) -> None:
        """Sets what an element is painted with, as the inspector's swatches do."""
        args: dict[str, Any] = {}
        if fill is not None:
            args["fill"] = fill
        if stroke is not None:
            args["stroke"] = stroke
        if stroke_width is not None:
            args["stroke_width"] = stroke_width
        if font_size is not None:
            args["font_size"] = font_size
        if align is not None:
            args["align"] = align
        if bold is not None:
            args["bold"] = bold
        if italic is not None:
            args["italic"] = italic
        if radius is not None:
            args["radius"] = radius
        return self.action("set_style", **args)

    def set_style_many(self, items: list[SetStyleArgs]) -> list[dict[str, Any]]:
        """Runs `set_style` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_style", items)

    def copy_element(self) -> None:
        """Copies the chosen element to the clipboard as the JSON another deck pastes."""
        return self.action("copy_element")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"edit_block", "edit_block_many", "insert_block", "insert_block_many", "delete_block", "delete_block_many", "set_style", "set_style_many", "copy_element"})


class DeckFile(Resource):
    """DeckFile — `tl.so.tilo.slides.DeckFile`."""
    __surface__ = "so.tilo.slides@cd42c8463e98a698"
    mnmdemqn: str  # title
    iayxmtjm: str  # palette
    qzspviev: str  # look
    ajyrzs4l: list[SlideFile]  # slides


class SlideFile(Resource):
    """SlideFile — `tl.so.tilo.slides.SlideFile`."""
    __surface__ = "so.tilo.slides@cd42c8463e98a698"
    dqtkr5ot: str  # id
    sqsminna: str  # layout
    tmaage4k: str  # background
    e73d2qv7: str  # notes
    ggcmbhfn: list[ElementFile]  # elements


class ElementFile(Resource):
    """ElementFile — `tl.so.tilo.slides.ElementFile`."""
    __surface__ = "so.tilo.slides@cd42c8463e98a698"
    kpm4jhpo: str  # id
    n34lg7tm: str  # kind
    hevssfo2: float  # x
    u55ztccf: float  # y
    qqtdorgx: float  # w
    sdpmue2g: float  # h
    yd6wa63n: str  # text
    @property
    def _3jfhhhfc(self) -> str:
        """src (`3jfhhhfc` on the wire)."""
        return self.fields["3jfhhhfc"]

    @_3jfhhhfc.setter
    def _3jfhhhfc(self, value: str) -> None:
        self.edit("3jfhhhfc", value=value)
    a45as5qt: str  # fill
    lqeh5xoc: str  # stroke
    rrx5fbi7: float  # stroke_width
    cbc3umvz: float  # font_size
    vriuesyu: str  # align
    xv57dbhp: bool  # bold
    zu3zrkv4: bool  # italic
    g4nvvgmj: float  # radius


class Outline(Resource):
    """Outline — `tl.so.tilo.slides.Outline`."""
    __surface__ = "so.tilo.slides@cd42c8463e98a698"
    fmwlgt6m: str | None  # title
    @property
    def _3iepzseq(self) -> str | None:
        """body (`3iepzseq` on the wire)."""
        return self.fields["3iepzseq"]

    @_3iepzseq.setter
    def _3iepzseq(self, value: str | None) -> None:
        self.edit("3iepzseq", value=value)
    tarratvg: Layout | None  # layout
    uv2g5qve: str | None  # notes


class Colours(Resource):
    """Colours — `tl.so.tilo.slides.Colours`."""
    __surface__ = "so.tilo.slides@cd42c8463e98a698"
    i6vzkphp: str  # accent
    qp7qmrqr: str  # accent_soft
    e2yv6zxc: str  # accent_deep
    wvovch6g: str  # surface
    @property
    def _3pyvsswg(self) -> str:
        """surface_dim (`3pyvsswg` on the wire)."""
        return self.fields["3pyvsswg"]

    @_3pyvsswg.setter
    def _3pyvsswg(self, value: str) -> None:
        self.edit("3pyvsswg", value=value)
    mmxtpccn: str  # deep
    ch6doguq: str  # text
    fgu23gq2: str  # text_dim
    ojgvaj3b: str  # on_accent
    @property
    def _23vs3mco(self) -> str:
        """success (`23vs3mco` on the wire)."""
        return self.fields["23vs3mco"]

    @_23vs3mco.setter
    def _23vs3mco(self, value: str) -> None:
        self.edit("23vs3mco", value=value)
    @property
    def _4ph7cxqz(self) -> str:
        """warning (`4ph7cxqz` on the wire)."""
        return self.fields["4ph7cxqz"]

    @_4ph7cxqz.setter
    def _4ph7cxqz(self, value: str) -> None:
        self.edit("4ph7cxqz", value=value)
    g56ubagd: str  # danger
    ne7cvyra: str  # info


class Slides(Resource):
    """Slides — `tl.so.tilo.slides`."""
    __surface__ = "so.tilo.slides@cd42c8463e98a698"
    def read_presentation(self, *, file: Any) -> Any:
        """Reads the deck a presentation file holds."""
        return self.action("read_presentation", file=file)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"read_presentation"})


