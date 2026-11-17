"""Types for Notes (`so.tilo.notes`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Notebook` is the app's `notes.notebook` at `notes/<resource>`, read with
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
from tilo import Resource, Run

__all__ = [
    "MarkdownFile",
    "Origin",
    "Page",
    "Tag",
    "Notebook",
    "RenameArgs",
    "DeleteArgs",
    "MoveNoteArgs",
    "MoveUpArgs",
    "MoveDownArgs",
    "IndentArgs",
    "OutdentArgs",
    "DuplicateArgs",
    "NewTagArgs",
    "TagArgs",
    "DeleteTagArgs",
    "KeepImageArgs",
    "PlaceImageArgs",
    "ImportMarkdownArgs",
    "ImportMarkdownFilesArgs",
    "WriteNoteArgs",
]

class MarkdownFile(TypedDict):
    path: str
    title: str | None
    markdown: str
    origin: Origin | None

class Origin(TypedDict):
    author: str
    handle: str | None
    source: str
    url: str | None
    reference: str | None
    created_ms: int | None
    updated_ms: int | None

class Page(TypedDict):
    title: str
    parent: str | None
    body: str
    tags: list[str]
    creator: str | None
    creator_name: str
    origin: Origin | None
    path: str | None
    reactions: Any

class Tag(TypedDict):
    name: str

RenameArgs = TypedDict("RenameArgs", {
    "page": "str",
    "title": "str",
})
"""The arguments of `rename`."""

DeleteArgs = TypedDict("DeleteArgs", {
    "page": "str",
})
"""The arguments of `delete`."""

MoveNoteArgs = TypedDict("MoveNoteArgs", {
    "page": "str",
    "parent": NotRequired["str | None"],
    "before": NotRequired["str | None"],
})
"""The arguments of `move-note`."""

MoveUpArgs = TypedDict("MoveUpArgs", {
    "page": "str",
})
"""The arguments of `move-up`."""

MoveDownArgs = TypedDict("MoveDownArgs", {
    "page": "str",
})
"""The arguments of `move-down`."""

IndentArgs = TypedDict("IndentArgs", {
    "page": "str",
})
"""The arguments of `indent`."""

OutdentArgs = TypedDict("OutdentArgs", {
    "page": "str",
})
"""The arguments of `outdent`."""

DuplicateArgs = TypedDict("DuplicateArgs", {
    "page": "str",
})
"""The arguments of `duplicate`."""

NewTagArgs = TypedDict("NewTagArgs", {
    "name": "str",
})
"""The arguments of `new-tag`."""

TagArgs = TypedDict("TagArgs", {
    "page": "str",
    "name": "str",
})
"""The arguments of `tag`."""

DeleteTagArgs = TypedDict("DeleteTagArgs", {
    "tag": "str",
})
"""The arguments of `delete-tag`."""

KeepImageArgs = TypedDict("KeepImageArgs", {
    "bytes": "Any",
})
"""The arguments of `keep-image`."""

PlaceImageArgs = TypedDict("PlaceImageArgs", {
    "page": "str",
    "bytes": "Any",
    "alt": NotRequired["str | None"],
})
"""The arguments of `place-image`."""

ImportMarkdownArgs = TypedDict("ImportMarkdownArgs", {
    "title": "str",
    "markdown": "str",
    "images": "list[Any]",
    "note": NotRequired["str | None"],
    "path": NotRequired["str | None"],
    "origin": NotRequired["Origin | None"],
})
"""The arguments of `import-markdown`."""

ImportMarkdownFilesArgs = TypedDict("ImportMarkdownFilesArgs", {
    "files": "list[MarkdownFile]",
})
"""The arguments of `import-markdown-files`."""

WriteNoteArgs = TypedDict("WriteNoteArgs", {
    "page": "str",
    "markdown": "str",
})
"""The arguments of `write-note`."""

class Notebook(Resource):
    """Notebook — `notes.notebook`."""
    __surface__ = "so.tilo.notes@d00ab9647430874b"
    pages: list[Page]
    tags: list[Tag]
    @property
    def images(self) -> dict[str, Any]:
        """Read only."""
        return self.fields["images"]

    def new_note(self,
        *,
        title: str,
        parent: str | None = None,
    ) -> Any | Run:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["title"] = title
        if parent is not None:
            args["parent"] = parent
        return self.action("new-note", **args)

    def new_child(self,
        *,
        page: str,
        title: str,
    ) -> Any | Run:
        """Runs the app's action."""
        return self.action("new-child", page=page, title=title)

    def rename(self,
        *,
        page: str,
        title: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("rename", page=page, title=title)

    def rename_many(self, items: list[RenameArgs]) -> list[dict[str, Any]]:
        """Runs `rename` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("rename", items)

    def delete(self, *, page: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete", page=page)

    def delete_many(self, items: list[DeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("delete", items)

    def move_note(self,
        *,
        page: str,
        parent: str | None = None,
        before: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["page"] = page
        if parent is not None:
            args["parent"] = parent
        if before is not None:
            args["before"] = before
        return self.action("move-note", **args)

    def move_note_many(self, items: list[MoveNoteArgs]) -> list[dict[str, Any]]:
        """Runs `move-note` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("move-note", items)

    def move_up(self, *, page: str) -> Run | None:
        """Runs the app's action."""
        return self.action("move-up", page=page)

    def move_up_many(self, items: list[MoveUpArgs]) -> list[dict[str, Any]]:
        """Runs `move-up` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("move-up", items)

    def move_down(self, *, page: str) -> Run | None:
        """Runs the app's action."""
        return self.action("move-down", page=page)

    def move_down_many(self, items: list[MoveDownArgs]) -> list[dict[str, Any]]:
        """Runs `move-down` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("move-down", items)

    def indent(self, *, page: str) -> Run | None:
        """Runs the app's action."""
        return self.action("indent", page=page)

    def indent_many(self, items: list[IndentArgs]) -> list[dict[str, Any]]:
        """Runs `indent` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("indent", items)

    def outdent(self, *, page: str) -> Run | None:
        """Runs the app's action."""
        return self.action("outdent", page=page)

    def outdent_many(self, items: list[OutdentArgs]) -> list[dict[str, Any]]:
        """Runs `outdent` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("outdent", items)

    def duplicate(self, *, page: str) -> Run | None:
        """Runs the app's action."""
        return self.action("duplicate", page=page)

    def duplicate_many(self, items: list[DuplicateArgs]) -> list[dict[str, Any]]:
        """Runs `duplicate` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("duplicate", items)

    def new_tag(self, *, name: str) -> Run | None:
        """Runs the app's action."""
        return self.action("new-tag", name=name)

    def new_tag_many(self, items: list[NewTagArgs]) -> list[dict[str, Any]]:
        """Runs `new-tag` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("new-tag", items)

    def tag(self,
        *,
        page: str,
        name: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("tag", page=page, name=name)

    def tag_many(self, items: list[TagArgs]) -> list[dict[str, Any]]:
        """Runs `tag` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("tag", items)

    def delete_tag(self, *, tag: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-tag", tag=tag)

    def delete_tag_many(self, items: list[DeleteTagArgs]) -> list[dict[str, Any]]:
        """Runs `delete-tag` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("delete-tag", items)

    def keep_image(self, *, bytes: Any) -> Run | None:
        """Runs the app's action."""
        return self.action("keep-image", bytes=bytes)

    def keep_image_many(self, items: list[KeepImageArgs]) -> list[dict[str, Any]]:
        """Runs `keep-image` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("keep-image", items)

    def place_image(self,
        *,
        page: str,
        bytes: Any,
        alt: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["page"] = page
        args["bytes"] = bytes
        if alt is not None:
            args["alt"] = alt
        return self.action("place-image", **args)

    def place_image_many(self, items: list[PlaceImageArgs]) -> list[dict[str, Any]]:
        """Runs `place-image` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("place-image", items)

    def create_page(self,
        *,
        title: str,
        markdown: str | None = None,
        origin: Origin | None = None,
    ) -> Any | Run:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["title"] = title
        if markdown is not None:
            args["markdown"] = markdown
        if origin is not None:
            args["origin"] = origin
        return self.action("create-page", **args)

    def import_markdown(self,
        *,
        title: str,
        markdown: str,
        images: list[Any],
        note: str | None = None,
        path: str | None = None,
        origin: Origin | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["title"] = title
        args["markdown"] = markdown
        args["images"] = images
        if note is not None:
            args["note"] = note
        if path is not None:
            args["path"] = path
        if origin is not None:
            args["origin"] = origin
        return self.action("import-markdown", **args)

    def import_markdown_many(self, items: list[ImportMarkdownArgs]) -> list[dict[str, Any]]:
        """Runs `import-markdown` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("import-markdown", items)

    def import_markdown_files(self, *, files: list[MarkdownFile]) -> Run | None:
        """Runs the app's action."""
        return self.action("import-markdown-files", files=files)

    def import_markdown_files_many(self, items: list[ImportMarkdownFilesArgs]) -> list[dict[str, Any]]:
        """Runs `import-markdown-files` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("import-markdown-files", items)

    def append_to_note(self,
        *,
        markdown: str,
        title: str | None = None,
    ) -> Any | Run:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if title is not None:
            args["title"] = title
        args["markdown"] = markdown
        return self.action("append-to-note", **args)

    def write_note(self,
        *,
        page: str,
        markdown: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("write-note", page=page, markdown=markdown)

    def write_note_many(self, items: list[WriteNoteArgs]) -> list[dict[str, Any]]:
        """Runs `write-note` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("write-note", items)

    def export_markdown_content(self,
        *,
        page: str | None = None,
        title: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if page is not None:
            args["page"] = page
        if title is not None:
            args["title"] = title
        return self.action("export-markdown-content", **args)


