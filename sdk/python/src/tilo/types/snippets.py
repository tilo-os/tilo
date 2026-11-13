"""Types for Snippets (`so.tilo.snippets`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Library` is the app's `snippets.library` at `snippets/<resource>`, read with
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
    "Language",
    "Snippet",
    "Library",
]

Language = Literal["text", "shell", "rust", "sql", "git", "python", "javascript", "json", "yaml", "markdown", "html", "css", "toml", "dockerfile"]

class Snippet(TypedDict):
    title: str
    body: str
    description: str
    favorite: bool
    folder: str
    language: Language
    tags: list[str]
    uses: int

class Library(Resource):
    """Snippet library — `snippets.library`."""
    __surface__ = "so.tilo.snippets@530e3c69f46d7e0c"
    snippets: list[Snippet]

    def new_snippet(self,
        *,
        title: str,
        folder: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["title"] = title
        if folder is not None:
            args["folder"] = folder
        return self.action("new-snippet", **args)

    def duplicate(self,
        *,
        snippet: str,
        title: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["snippet"] = snippet
        if title is not None:
            args["title"] = title
        return self.action("duplicate", **args)

    def delete(self, *, snippet: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete", snippet=snippet)

    def record_use(self, *, snippet: str) -> Run | None:
        """Runs the app's action."""
        return self.action("record-use", snippet=snippet)

    def rename_folder(self,
        *,
        folder: str,
        to: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("rename-folder", folder=folder, to=to)

    def rename_tag(self,
        *,
        tag: str,
        to: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("rename-tag", tag=tag, to=to)

    def remove_tag(self, *, tag: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-tag", tag=tag)


