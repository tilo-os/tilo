"""Types for PDF Viewer (`so.tilo.pdf-viewer`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Document` is the app's `pdf.document` at `pdf-viewer/<resource>`, read with
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
    "AdoptedAnnotation",
    "AdoptedEntry",
    "AdoptedReply",
    "AdoptedThread",
    "AnnoKind",
    "Annotation",
    "Author",
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
    "RecordReadingPositionArgs",
    "StartPagePlanArgs",
    "PinSourceArgs",
    "AdoptExistingArgs",
    "ImportPagesArgs",
    "DuplicatePageArgs",
    "DeletePageArgs",
    "MovePageArgs",
    "RotatePageArgs",
    "AddAnnotationArgs",
    "DeleteAnnotationArgs",
    "CreateNoteArgs",
    "AddNoteArgs",
    "AddCommentArgs",
    "ReplyArgs",
    "DeleteCommentArgs",
    "AddSignatureArgs",
    "AddTypedSignatureArgs",
    "DeleteSignatureArgs",
    "SetSignerArgs",
    "SetStrokesArgs",
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
    author: Author
    chars: list[int] | None
    created_ms: int
    space: GeomSpace

Author = dict[str, Any]
"""One of member, named: an object whose case the app tags it with."""

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

class Comment(TypedDict):
    anchor: PdfAnchor
    author: Author
    date: str
    text: str
    body: list[ClipBlock]
    resolved: bool
    replies: list[Entry[Reply]]

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
"""One of Bool, Int, Str: an object whose case the app tags it with."""

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
    author: Author
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
    without_text: list[int]

class Stroke(TypedDict):
    xy: list[float]

RecordReadingPositionArgs = TypedDict("RecordReadingPositionArgs", {
    "page": "str",
    "fraction": "float",
    "zoom": "float",
})
"""The arguments of `record-reading-position`."""

StartPagePlanArgs = TypedDict("StartPagePlanArgs", {
    "pages": "int",
})
"""The arguments of `start-page-plan`."""

PinSourceArgs = TypedDict("PinSourceArgs", {
    "source": "Any",
    "name": "str",
})
"""The arguments of `pin-source`."""

AdoptExistingArgs = TypedDict("AdoptExistingArgs", {
    "source": "str",
    "markup": "ExistingMarkup",
})
"""The arguments of `adopt-existing`."""

ImportPagesArgs = TypedDict("ImportPagesArgs", {
    "after": NotRequired["str | None"],
    "pages": "list[PagePlanEntry]",
})
"""The arguments of `import-pages`."""

DuplicatePageArgs = TypedDict("DuplicatePageArgs", {
    "page": "str",
    "copy": NotRequired["str | None"],
})
"""The arguments of `duplicate-page`."""

DeletePageArgs = TypedDict("DeletePageArgs", {
    "page": "str",
})
"""The arguments of `delete-page`."""

MovePageArgs = TypedDict("MovePageArgs", {
    "page": "str",
    "after": NotRequired["str | None"],
})
"""The arguments of `move-page`."""

RotatePageArgs = TypedDict("RotatePageArgs", {
    "page": "str",
})
"""The arguments of `rotate-page`."""

AddAnnotationArgs = TypedDict("AddAnnotationArgs", {
    "annotation": "NewAnnotation",
})
"""The arguments of `add-annotation`."""

DeleteAnnotationArgs = TypedDict("DeleteAnnotationArgs", {
    "annotation": "str",
})
"""The arguments of `delete-annotation`."""

CreateNoteArgs = TypedDict("CreateNoteArgs", {
    "note": "NewNote",
})
"""The arguments of `create-note`."""

AddNoteArgs = TypedDict("AddNoteArgs", {
    "text": "str",
    "page": NotRequired["int | None"],
})
"""The arguments of `add-note`."""

AddCommentArgs = TypedDict("AddCommentArgs", {
    "comment": "NewComment",
})
"""The arguments of `add-comment`."""

ReplyArgs = TypedDict("ReplyArgs", {
    "comment": "str",
    "text": "str",
    "id": NotRequired["str | None"],
    "rich": NotRequired["list[ClipBlock] | None"],
})
"""The arguments of `reply`."""

DeleteCommentArgs = TypedDict("DeleteCommentArgs", {
    "comment": "str",
})
"""The arguments of `delete-comment`."""

AddSignatureArgs = TypedDict("AddSignatureArgs", {
    "signature": "NewSignature",
})
"""The arguments of `add-signature`."""

AddTypedSignatureArgs = TypedDict("AddTypedSignatureArgs", {
    "signer": "str",
    "page": NotRequired["int | None"],
})
"""The arguments of `add-typed-signature`."""

DeleteSignatureArgs = TypedDict("DeleteSignatureArgs", {
    "signature": "str",
})
"""The arguments of `delete-signature`."""

SetSignerArgs = TypedDict("SetSignerArgs", {
    "name": "str",
})
"""The arguments of `set-signer`."""

SetStrokesArgs = TypedDict("SetStrokesArgs", {
    "strokes": "list[list[Any]]",
})
"""The arguments of `set-strokes`."""

class Document(Resource):
    """PDF — `pdf.document`."""
    __surface__ = "so.tilo.pdf-viewer@f7a099e9e6b08a4f"
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
    pages: list[Entry[PageOccurrence]]
    annotations: list[Entry[Annotation]]
    comments: list[Entry[Comment]]
    signatures: list[Entry[StoredSignature]]
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
    ) -> None:
        """Runs the app's action."""
        return self.action("record-reading-position", page=page, fraction=fraction, zoom=zoom)

    def record_reading_position_many(self, items: list[RecordReadingPositionArgs]) -> list[dict[str, Any]]:
        """Runs `record-reading-position` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("record-reading-position", items)

    def start_page_plan(self, *, pages: int) -> None:
        """Runs the app's action."""
        return self.action("start-page-plan", pages=pages)

    def start_page_plan_many(self, items: list[StartPagePlanArgs]) -> list[dict[str, Any]]:
        """Runs `start-page-plan` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("start-page-plan", items)

    def pin_source(self,
        *,
        source: Any,
        name: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("pin-source", source=source, name=name)

    def pin_source_many(self, items: list[PinSourceArgs]) -> list[dict[str, Any]]:
        """Runs `pin-source` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("pin-source", items)

    def adopt_existing(self,
        *,
        source: str,
        markup: ExistingMarkup,
    ) -> None:
        """Runs the app's action."""
        return self.action("adopt-existing", source=source, markup=markup)

    def adopt_existing_many(self, items: list[AdoptExistingArgs]) -> list[dict[str, Any]]:
        """Runs `adopt-existing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("adopt-existing", items)

    def import_pages(self,
        *,
        pages: list[PagePlanEntry],
        after: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if after is not None:
            args["after"] = after
        args["pages"] = pages
        return self.action("import-pages", **args)

    def import_pages_many(self, items: list[ImportPagesArgs]) -> list[dict[str, Any]]:
        """Runs `import-pages` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import-pages", items)

    def duplicate_page(self,
        *,
        page: str,
        copy: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["page"] = page
        if copy is not None:
            args["copy"] = copy
        return self.action("duplicate-page", **args)

    def duplicate_page_many(self, items: list[DuplicatePageArgs]) -> list[dict[str, Any]]:
        """Runs `duplicate-page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("duplicate-page", items)

    def delete_page(self, *, page: str) -> None:
        """Runs the app's action."""
        return self.action("delete-page", page=page)

    def delete_page_many(self, items: list[DeletePageArgs]) -> list[dict[str, Any]]:
        """Runs `delete-page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-page", items)

    def move_page(self,
        *,
        page: str,
        after: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["page"] = page
        if after is not None:
            args["after"] = after
        return self.action("move-page", **args)

    def move_page_many(self, items: list[MovePageArgs]) -> list[dict[str, Any]]:
        """Runs `move-page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move-page", items)

    def rotate_page(self, *, page: str) -> None:
        """Runs the app's action."""
        return self.action("rotate-page", page=page)

    def rotate_page_many(self, items: list[RotatePageArgs]) -> list[dict[str, Any]]:
        """Runs `rotate-page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rotate-page", items)

    def add_annotation(self, *, annotation: NewAnnotation) -> None:
        """Runs the app's action."""
        return self.action("add-annotation", annotation=annotation)

    def add_annotation_many(self, items: list[AddAnnotationArgs]) -> list[dict[str, Any]]:
        """Runs `add-annotation` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-annotation", items)

    def delete_annotation(self, *, annotation: str) -> None:
        """Runs the app's action."""
        return self.action("delete-annotation", annotation=annotation)

    def delete_annotation_many(self, items: list[DeleteAnnotationArgs]) -> list[dict[str, Any]]:
        """Runs `delete-annotation` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-annotation", items)

    def create_note(self, *, note: NewNote) -> None:
        """Runs the app's action."""
        return self.action("create-note", note=note)

    def create_note_many(self, items: list[CreateNoteArgs]) -> list[dict[str, Any]]:
        """Runs `create-note` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("create-note", items)

    def add_note(self,
        *,
        text: str,
        page: int | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["text"] = text
        if page is not None:
            args["page"] = page
        return self.action("add-note", **args)

    def add_note_many(self, items: list[AddNoteArgs]) -> list[dict[str, Any]]:
        """Runs `add-note` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-note", items)

    def add_comment(self, *, comment: NewComment) -> None:
        """Runs the app's action."""
        return self.action("add-comment", comment=comment)

    def add_comment_many(self, items: list[AddCommentArgs]) -> list[dict[str, Any]]:
        """Runs `add-comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-comment", items)

    def reply(self,
        *,
        comment: str,
        text: str,
        id: str | None = None,
        rich: list[ClipBlock] | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["comment"] = comment
        args["text"] = text
        if id is not None:
            args["id"] = id
        if rich is not None:
            args["rich"] = rich
        return self.action("reply", **args)

    def reply_many(self, items: list[ReplyArgs]) -> list[dict[str, Any]]:
        """Runs `reply` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reply", items)

    def delete_comment(self, *, comment: str) -> None:
        """Runs the app's action."""
        return self.action("delete-comment", comment=comment)

    def delete_comment_many(self, items: list[DeleteCommentArgs]) -> list[dict[str, Any]]:
        """Runs `delete-comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-comment", items)

    def add_signature(self, *, signature: NewSignature) -> None:
        """Runs the app's action."""
        return self.action("add-signature", signature=signature)

    def add_signature_many(self, items: list[AddSignatureArgs]) -> list[dict[str, Any]]:
        """Runs `add-signature` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-signature", items)

    def add_typed_signature(self,
        *,
        signer: str,
        page: int | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["signer"] = signer
        if page is not None:
            args["page"] = page
        return self.action("add-typed-signature", **args)

    def add_typed_signature_many(self, items: list[AddTypedSignatureArgs]) -> list[dict[str, Any]]:
        """Runs `add-typed-signature` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-typed-signature", items)

    def delete_signature(self, *, signature: str) -> None:
        """Runs the app's action."""
        return self.action("delete-signature", signature=signature)

    def delete_signature_many(self, items: list[DeleteSignatureArgs]) -> list[dict[str, Any]]:
        """Runs `delete-signature` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-signature", items)

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

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"record_reading_position", "record_reading_position_many", "start_page_plan", "start_page_plan_many", "pin_source", "pin_source_many", "adopt_existing", "adopt_existing_many", "import_pages", "import_pages_many", "duplicate_page", "duplicate_page_many", "delete_page", "delete_page_many", "move_page", "move_page_many", "rotate_page", "rotate_page_many", "add_annotation", "add_annotation_many", "delete_annotation", "delete_annotation_many", "create_note", "create_note_many", "add_note", "add_note_many", "add_comment", "add_comment_many", "reply", "reply_many", "delete_comment", "delete_comment_many", "add_signature", "add_signature_many", "add_typed_signature", "add_typed_signature_many", "delete_signature", "delete_signature_many", "read_text", "read_form_fields", "look_page"})


class SignatureProfile(Resource):
    """Signature — `pdf.signature-profile`."""
    __surface__ = "so.tilo.pdf-viewer@f7a099e9e6b08a4f"
    signer: str
    strokes: list[Stroke]

    def set_signer(self, *, name: str) -> None:
        """Runs the app's action."""
        return self.action("set-signer", name=name)

    def set_signer_many(self, items: list[SetSignerArgs]) -> list[dict[str, Any]]:
        """Runs `set-signer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-signer", items)

    def set_strokes(self, *, strokes: list[list[Any]]) -> None:
        """Runs the app's action."""
        return self.action("set-strokes", strokes=strokes)

    def set_strokes_many(self, items: list[SetStrokesArgs]) -> list[dict[str, Any]]:
        """Runs `set-strokes` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-strokes", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"set_signer", "set_signer_many", "set_strokes", "set_strokes_many"})


