"""Types for Notes (`so.tilo.notes`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Notebook` is the app's `notes.notebook` at `notes/<resource>`, read with
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
    "Page",
    "Tag",
    "Notebook",
]

class Page(TypedDict):
    title: str
    parent: str | None
    body: str
    tags: list[str]
    creator: str | None
    reactions: Any

class Tag(TypedDict):
    name: str

class Notebook(Resource):
    """Notebook — `notes.notebook`."""
    __surface__ = "so.tilo.notes@cec36ca8d20cd4bb"
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

    def delete(self, *, page: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete", page=page)

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

    def move_up(self, *, page: str) -> Run | None:
        """Runs the app's action."""
        return self.action("move-up", page=page)

    def move_down(self, *, page: str) -> Run | None:
        """Runs the app's action."""
        return self.action("move-down", page=page)

    def indent(self, *, page: str) -> Run | None:
        """Runs the app's action."""
        return self.action("indent", page=page)

    def outdent(self, *, page: str) -> Run | None:
        """Runs the app's action."""
        return self.action("outdent", page=page)

    def duplicate(self, *, page: str) -> Run | None:
        """Runs the app's action."""
        return self.action("duplicate", page=page)

    def new_tag(self, *, name: str) -> Run | None:
        """Runs the app's action."""
        return self.action("new-tag", name=name)

    def tag(self,
        *,
        page: str,
        name: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("tag", page=page, name=name)

    def delete_tag(self, *, tag: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-tag", tag=tag)

    def keep_image(self, *, bytes: Any) -> Run | None:
        """Runs the app's action."""
        return self.action("keep-image", bytes=bytes)

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

    def create_page(self,
        *,
        title: str,
        markdown: str | None = None,
    ) -> Any | Run:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["title"] = title
        if markdown is not None:
            args["markdown"] = markdown
        return self.action("create-page", **args)

    def import_markdown(self,
        *,
        title: str,
        markdown: str,
        images: list[Any],
        note: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["title"] = title
        args["markdown"] = markdown
        args["images"] = images
        if note is not None:
            args["note"] = note
        return self.action("import-markdown", **args)

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


