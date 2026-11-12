"""Types for PDF Viewer (`so.tilo.pdf-viewer`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Document` is the app's `pdf.document` at `pdf-viewer/<resource>`, read with
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
    "AdoptedAnnotation",
    "AdoptedEntry",
    "AdoptedReply",
    "AdoptedThread",
    "AnnoKind",
    "Annotation",
    "ClipAttr",
    "ClipBlock",
    "Comment",
    "ExistingMarkup",
    "FormEntry",
    "GeomSpace",
    "ImportedPage",
    "MarkSpan",
    "MarkValue",
    "MarkupStyle",
    "NewAnnotation",
    "NewComment",
    "NewNote",
    "NewSignature",
    "PageOccurrence",
    "PagePlanEntry",
    "PdfAnchor",
    "PtRect",
    "Reply",
    "SignatureInk",
    "StoredSignature",
    "FormFieldKind",
    "FormFieldRow",
    "FormFields",
    "PdfPageText",
    "PdfText",
    "Stroke",
    "Document",
    "SignatureProfile",
]

class AdoptedAnnotation(TypedDict):
    id: str
    page_id: str
    mark: AnnoKind
    color: Any
    author: str
    created_ms: int

class AdoptedEntry(TypedDict):
    id: str
    page: int
    index: int
    name: str
    kind: str
    print: str
    parent: str | None

class AdoptedReply(TypedDict):
    id: str
    author: str
    text: str

class AdoptedThread(TypedDict):
    id: str
    anno: str | None
    page_id: str
    author: str
    text: str
    date: str
    replies: list[AdoptedReply]

AnnoKind = dict[str, Any]
"""One of markup, ink, note, freetext: an object whose case the app tags it with."""

class Annotation(TypedDict):
    id: str
    page_id: str
    mark: Any
    color: Any
    author: str
    chars: list[int] | None
    created_ms: int
    space: GeomSpace

ClipAttr = dict[str, Any]
"""One of bool, int, str: an object whose case the app tags it with."""

class ClipBlock(TypedDict):
    kind: str
    text: str
    marks: list[MarkSpan]
    level: int | None
    ordered: bool | None
    align: str | None
    attrs: dict[str, ClipAttr]

class Comment(TypedDict):
    anchor: PdfAnchor
    author: str
    date: str
    text: str
    body: list[ClipBlock]
    resolved: bool
    replies: list[Reply]

class ExistingMarkup(TypedDict):
    annotations: list[AdoptedAnnotation]
    comments: list[AdoptedThread]
    form_values: dict[str, FormEntry]
    adopted: list[AdoptedEntry]

FormEntry = dict[str, Any]
"""One of text, checked, choice: an object whose case the app tags it with."""

GeomSpace = Literal["page", "user"]

class ImportedPage(TypedDict):
    hash: str
    kind: str
    page: int
    width_milli: int
    height_milli: int

class MarkSpan(TypedDict):
    start: int
    end: int
    name: str
    value: MarkValue

MarkValue = dict[str, Any]
"""One of bool, int, str: an object whose case the app tags it with."""

MarkupStyle = Literal["highlight", "underline", "strikeout", "squiggly"]

class NewAnnotation(TypedDict):
    id: str
    page_id: str
    mark: AnnoKind
    color: Any
    chars: list[int] | None
    space: GeomSpace

class NewComment(TypedDict):
    id: str
    anno: str | None
    page_id: str | None
    text: str
    rich: list[ClipBlock]

class NewNote(TypedDict):
    annotation: str
    comment: str
    page_id: str
    x: float
    y: float
    color: Any
    text: str
    space: GeomSpace

class NewSignature(TypedDict):
    id: str
    page_id: str
    rect: PtRect
    signer: str
    date: str
    ink: SignatureInk
    space: GeomSpace

class PageOccurrence(TypedDict):
    source_page: int
    imported: Any | None
    rotation: int

class PagePlanEntry(TypedDict):
    id: str
    source_page: int
    imported: ImportedPage | None
    rotation: int

class PdfAnchor(TypedDict):
    page_id: str
    annotation: str

class PtRect(TypedDict):
    x: float
    y: float
    w: float
    h: float

class Reply(TypedDict):
    author: str
    date: str
    text: str
    body: list[ClipBlock]

SignatureInk = dict[str, Any]
"""One of typed, drawn: an object whose case the app tags it with."""

class StoredSignature(TypedDict):
    id: str
    page_id: str
    rect: Any
    signer: str
    date: str
    ink: Any
    space: GeomSpace

FormFieldKind = Literal["text", "checkbox", "radio", "choice"]

class FormFieldRow(TypedDict):
    name: str
    kind: FormFieldKind
    value: FormEntry
    path: Any | None
    options: list[str]
    export: str | None
    page: int | None
    read_only: bool
    required: bool

class FormFields(TypedDict):
    file: str
    fields: list[FormFieldRow]

class PdfPageText(TypedDict):
    page: int
    text: str

class PdfText(TypedDict):
    file: str
    page_count: int
    pages: list[PdfPageText]
    next: int | None

class Stroke(TypedDict):
    xy: list[float]

class Document(Resource):
    """PDF — `pdf.document`."""
    @property
    def source(self) -> Any:
        """Read only."""
        return self.fields["source"]
    @property
    def source_name(self) -> str:
        """Read only."""
        return self.fields["source_name"]
    @property
    def page_plan_seeded(self) -> bool:
        """Read only."""
        return self.fields["page_plan_seeded"]
    @property
    def imported_source(self) -> str:
        """Read only."""
        return self.fields["imported_source"]
    pages: list[PageOccurrence]
    annotations: list[Annotation]
    comments: list[Comment]
    signatures: list[StoredSignature]
    form_values: dict[str, Any]
    @property
    def adopted(self) -> dict[str, Any]:
        """Read only."""
        return self.fields["adopted"]
    reading_page: str
    reading_fraction: float
    reading_zoom: float

    def record_reading_position(self,
        *,
        page: str,
        fraction: float,
        zoom: float,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("record-reading-position", page=page, fraction=fraction, zoom=zoom)

    def start_page_plan(self, *, pages: int) -> Run | None:
        """Runs the app's action."""
        return self.action("start-page-plan", pages=pages)

    def pin_source(self,
        *,
        hash: str,
        len: int,
        name: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("pin-source", hash=hash, len=len, name=name)

    def adopt_existing(self,
        *,
        source: str,
        markup: ExistingMarkup,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("adopt-existing", source=source, markup=markup)

    def import_pages(self,
        *,
        pages: list[PagePlanEntry],
        after: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if after is not None:
            args["after"] = after
        args["pages"] = pages
        return self.action("import-pages", **args)

    def duplicate_page(self,
        *,
        page: str,
        copy: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["page"] = page
        if copy is not None:
            args["copy"] = copy
        return self.action("duplicate-page", **args)

    def delete_page(self, *, page: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-page", page=page)

    def move_page(self,
        *,
        page: str,
        after: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["page"] = page
        if after is not None:
            args["after"] = after
        return self.action("move-page", **args)

    def rotate_page(self, *, page: str) -> Run | None:
        """Runs the app's action."""
        return self.action("rotate-page", page=page)

    def add_annotation(self, *, annotation: NewAnnotation) -> Run | None:
        """Runs the app's action."""
        return self.action("add-annotation", annotation=annotation)

    def delete_annotation(self, *, annotation: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-annotation", annotation=annotation)

    def create_note(self, *, note: NewNote) -> Run | None:
        """Runs the app's action."""
        return self.action("create-note", note=note)

    def add_note(self,
        *,
        text: str,
        page: int | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["text"] = text
        if page is not None:
            args["page"] = page
        return self.action("add-note", **args)

    def add_comment(self, *, comment: NewComment) -> Run | None:
        """Runs the app's action."""
        return self.action("add-comment", comment=comment)

    def reply(self,
        *,
        comment: str,
        text: str,
        id: str | None = None,
        rich: list[ClipBlock] | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["comment"] = comment
        args["text"] = text
        if id is not None:
            args["id"] = id
        if rich is not None:
            args["rich"] = rich
        return self.action("reply", **args)

    def delete_comment(self, *, comment: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-comment", comment=comment)

    def add_signature(self, *, signature: NewSignature) -> Run | None:
        """Runs the app's action."""
        return self.action("add-signature", signature=signature)

    def add_typed_signature(self,
        *,
        signer: str | None = None,
        page: int | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if signer is not None:
            args["signer"] = signer
        if page is not None:
            args["page"] = page
        return self.action("add-typed-signature", **args)

    def delete_signature(self, *, signature: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-signature", signature=signature)

    def read_text(self,
        *,
        from_page: int | None = None,
        to_page: int | None = None,
    ) -> PdfText:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if from_page is not None:
            args["from_page"] = from_page
        if to_page is not None:
            args["to_page"] = to_page
        return self.action("read-text", **args)

    def read_form_fields(self) -> FormFields:
        """Runs the app's action."""
        return self.action("read-form-fields")

    def look_page(self,
        *,
        page: int,
        size: int | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["page"] = page
        if size is not None:
            args["size"] = size
        return self.action("look-page", **args)


class SignatureProfile(Resource):
    """Signature — `pdf.signature-profile`."""
    signer: str
    strokes: list[Stroke]

    def set_signer(self, *, name: str) -> Run | None:
        """Runs the app's action."""
        return self.action("set-signer", name=name)

    def set_strokes(self, *, strokes: list[list[Any]]) -> Run | None:
        """Runs the app's action."""
        return self.action("set-strokes", strokes=strokes)


