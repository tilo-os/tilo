"""Types for Notes (`so.tilo.notes`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Notebook` is the app's `notes.notebook` at `notes/<resource>`, read with
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
    "Tag",
    "ClipAttr",
    "ClipBlock",
    "ImportReads",
    "Links",
    "MarkSpan",
    "MarkValue",
    "MarkdownFile",
    "Origin",
    "Page",
    "PageCopy",
    "Placement",
    "Notebook",
    "Pages",
    "NewTagArgs",
    "DeleteTagArgs",
    "RenameArgs",
    "DeleteArgs",
    "MoveNoteArgs",
    "MoveUpArgs",
    "MoveDownArgs",
    "IndentArgs",
    "OutdentArgs",
    "TagArgs",
    "KeepImageArgs",
    "PlaceImageArgs",
    "ImportMarkdownArgs",
    "ImportMarkdownFilesArgs",
]

class Tag(TypedDict):
    name: str

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

class ImportReads(TypedDict):
    rank: str
    links: Links
    imported: bool

class Links(TypedDict):
    titles: dict[str, str]
    paths: dict[str, str]

class MarkSpan(TypedDict):
    start: int
    end: int
    name: str
    value: MarkValue

MarkValue = dict[str, Any]
"""One of Bool, Int, Str: an object whose case the app tags it with."""

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
    rank: str
    body: str
    tags: list[str]
    creator: str | None
    creator_name: str
    origin: Origin | None
    path: str | None
    reactions: Any

class PageCopy(TypedDict):
    id: str
    title: str
    parent: str | None
    rank: str
    blocks: list[ClipBlock]
    tags: list[str]

class Placement(TypedDict):
    parent: str | None
    rank: str

NewTagArgs = TypedDict("NewTagArgs", {
    "name": "str",
})
"""The arguments of `new-tag`."""

DeleteTagArgs = TypedDict("DeleteTagArgs", {
    "tag": "str",
})
"""The arguments of `delete-tag`."""

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
    "place": NotRequired["Placement | None"],
})
"""The arguments of `move-note`."""

MoveUpArgs = TypedDict("MoveUpArgs", {
    "page": "str",
    "place": NotRequired["Placement | None"],
})
"""The arguments of `move-up`."""

MoveDownArgs = TypedDict("MoveDownArgs", {
    "page": "str",
    "place": NotRequired["Placement | None"],
})
"""The arguments of `move-down`."""

IndentArgs = TypedDict("IndentArgs", {
    "page": "str",
    "place": NotRequired["Placement | None"],
})
"""The arguments of `indent`."""

OutdentArgs = TypedDict("OutdentArgs", {
    "page": "str",
    "place": NotRequired["Placement | None"],
})
"""The arguments of `outdent`."""

TagArgs = TypedDict("TagArgs", {
    "page": "str",
    "name": "str",
    "tag": NotRequired["str | None"],
})
"""The arguments of `tag`."""

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
    "reads": NotRequired["ImportReads | None"],
})
"""The arguments of `import-markdown`."""

ImportMarkdownFilesArgs = TypedDict("ImportMarkdownFilesArgs", {
    "files": "list[MarkdownFile]",
    "ranks": NotRequired["list[str] | None"],
    "links": NotRequired["Links | None"],
})
"""The arguments of `import-markdown-files`."""

class Notebook(Resource):
    """Notebook — `notes.notebook`."""
    __surface__ = "so.tilo.notes@c9a6126ebbe7501a"
    tags: list[Entry[Tag]]

    def new_tag(self, *, name: str) -> None:
        """Runs the app's action."""
        return self.action("new-tag", name=name)

    def new_tag_many(self, items: list[NewTagArgs]) -> list[dict[str, Any]]:
        """Runs `new-tag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("new-tag", items)

    def delete_tag(self, *, tag: str) -> None:
        """Runs the app's action."""
        return self.action("delete-tag", tag=tag)

    def delete_tag_many(self, items: list[DeleteTagArgs]) -> list[dict[str, Any]]:
        """Runs `delete-tag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-tag", items)

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

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_tag", "new_tag_many", "delete_tag", "delete_tag_many", "export_markdown_content"})


class Pages(Resource):
    """Notes — `notes.pages`."""
    __surface__ = "so.tilo.notes@c9a6126ebbe7501a"
    pages: list[Entry[Page]]
    @property
    def images(self) -> dict[str, Any]:
        """Read only."""
        return self.fields["images"]

    def new_note(self,
        *,
        title: str,
        parent: str | None = None,
        note: str | None = None,
        rank: str | None = None,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["title"] = title
        if parent is not None:
            args["parent"] = parent
        if note is not None:
            args["note"] = note
        if rank is not None:
            args["rank"] = rank
        return self.action("new-note", **args)

    def new_child(self,
        *,
        page: str,
        title: str,
        rank: str | None = None,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["page"] = page
        args["title"] = title
        if rank is not None:
            args["rank"] = rank
        return self.action("new-child", **args)

    def rename(self,
        *,
        page: str,
        title: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("rename", page=page, title=title)

    def rename_many(self, items: list[RenameArgs]) -> list[dict[str, Any]]:
        """Runs `rename` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename", items)

    def delete(self, *, page: str) -> None:
        """Runs the app's action."""
        return self.action("delete", page=page)

    def delete_many(self, items: list[DeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete", items)

    def move_note(self,
        *,
        page: str,
        parent: str | None = None,
        before: str | None = None,
        place: Placement | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["page"] = page
        if parent is not None:
            args["parent"] = parent
        if before is not None:
            args["before"] = before
        if place is not None:
            args["place"] = place
        return self.action("move-note", **args)

    def move_note_many(self, items: list[MoveNoteArgs]) -> list[dict[str, Any]]:
        """Runs `move-note` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move-note", items)

    def move_up(self,
        *,
        page: str,
        place: Placement | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["page"] = page
        if place is not None:
            args["place"] = place
        return self.action("move-up", **args)

    def move_up_many(self, items: list[MoveUpArgs]) -> list[dict[str, Any]]:
        """Runs `move-up` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move-up", items)

    def move_down(self,
        *,
        page: str,
        place: Placement | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["page"] = page
        if place is not None:
            args["place"] = place
        return self.action("move-down", **args)

    def move_down_many(self, items: list[MoveDownArgs]) -> list[dict[str, Any]]:
        """Runs `move-down` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move-down", items)

    def indent(self,
        *,
        page: str,
        place: Placement | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["page"] = page
        if place is not None:
            args["place"] = place
        return self.action("indent", **args)

    def indent_many(self, items: list[IndentArgs]) -> list[dict[str, Any]]:
        """Runs `indent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("indent", items)

    def outdent(self,
        *,
        page: str,
        place: Placement | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["page"] = page
        if place is not None:
            args["place"] = place
        return self.action("outdent", **args)

    def outdent_many(self, items: list[OutdentArgs]) -> list[dict[str, Any]]:
        """Runs `outdent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("outdent", items)

    def duplicate(self,
        *,
        page: str,
        copies: list[PageCopy] | None = None,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["page"] = page
        if copies is not None:
            args["copies"] = copies
        return self.action("duplicate", **args)

    def tag(self,
        *,
        page: str,
        name: str,
        tag: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["page"] = page
        args["name"] = name
        if tag is not None:
            args["tag"] = tag
        return self.action("tag", **args)

    def tag_many(self, items: list[TagArgs]) -> list[dict[str, Any]]:
        """Runs `tag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("tag", items)

    def keep_image(self, *, bytes: Any) -> None:
        """Runs the app's action."""
        return self.action("keep-image", bytes=bytes)

    def keep_image_many(self, items: list[KeepImageArgs]) -> list[dict[str, Any]]:
        """Runs `keep-image` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep-image", items)

    def place_image(self,
        *,
        page: str,
        bytes: Any,
        alt: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["page"] = page
        args["bytes"] = bytes
        if alt is not None:
            args["alt"] = alt
        return self.action("place-image", **args)

    def place_image_many(self, items: list[PlaceImageArgs]) -> list[dict[str, Any]]:
        """Runs `place-image` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("place-image", items)

    def create_page(self,
        *,
        title: str,
        markdown: str | None = None,
        origin: Origin | None = None,
        rank: str | None = None,
        links: Links | None = None,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["title"] = title
        if markdown is not None:
            args["markdown"] = markdown
        if origin is not None:
            args["origin"] = origin
        if rank is not None:
            args["rank"] = rank
        if links is not None:
            args["links"] = links
        return self.action("create-page", **args)

    def import_markdown(self,
        *,
        title: str,
        markdown: str,
        images: list[Any],
        note: str | None = None,
        path: str | None = None,
        origin: Origin | None = None,
        reads: ImportReads | None = None,
    ) -> None:
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
        if reads is not None:
            args["reads"] = reads
        return self.action("import-markdown", **args)

    def import_markdown_many(self, items: list[ImportMarkdownArgs]) -> list[dict[str, Any]]:
        """Runs `import-markdown` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import-markdown", items)

    def import_markdown_files(self,
        *,
        files: list[MarkdownFile],
        ranks: list[str] | None = None,
        links: Links | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["files"] = files
        if ranks is not None:
            args["ranks"] = ranks
        if links is not None:
            args["links"] = links
        return self.action("import-markdown-files", **args)

    def import_markdown_files_many(self, items: list[ImportMarkdownFilesArgs]) -> list[dict[str, Any]]:
        """Runs `import-markdown-files` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import-markdown-files", items)

    def append_to_note(self,
        *,
        markdown: str,
        title: str | None = None,
        note: str | None = None,
        links: Links | None = None,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if title is not None:
            args["title"] = title
        args["markdown"] = markdown
        if note is not None:
            args["note"] = note
        if links is not None:
            args["links"] = links
        return self.action("append-to-note", **args)

    def write_note(self,
        *,
        page: str,
        markdown: str,
        links: Links | None = None,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["page"] = page
        args["markdown"] = markdown
        if links is not None:
            args["links"] = links
        return self.action("write-note", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_note", "new_child", "rename", "rename_many", "delete", "delete_many", "move_note", "move_note_many", "move_up", "move_up_many", "move_down", "move_down_many", "indent", "indent_many", "outdent", "outdent_many", "duplicate", "tag", "tag_many", "keep_image", "keep_image_many", "place_image", "place_image_many", "create_page", "import_markdown", "import_markdown_many", "import_markdown_files", "import_markdown_files_many", "append_to_note", "write_note"})


