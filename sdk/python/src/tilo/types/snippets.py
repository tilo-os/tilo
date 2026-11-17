"""Types for Snippets (`so.tilo.snippets`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Library` is the app's `snippets.library` at `snippets/<resource>`, read with
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
    "Language",
    "Snippet",
    "Library",
    "NewSnippetArgs",
    "DuplicateArgs",
    "DeleteArgs",
    "RecordUseArgs",
    "RenameFolderArgs",
    "RenameTagArgs",
    "RemoveTagArgs",
]

Language = Literal["PlainText", "Shell", "Rust", "Sql", "Git", "Python", "JavaScript", "Json", "Yaml", "Markdown", "Html", "Css", "Toml", "Dockerfile"]

class Snippet(TypedDict):
    title: str
    body: str
    description: str
    favorite: bool
    folder: str
    language: Language
    tags: list[str]
    uses: int

NewSnippetArgs = TypedDict("NewSnippetArgs", {
    "title": "str",
    "folder": NotRequired["str | None"],
})
"""The arguments of `new-snippet`."""

DuplicateArgs = TypedDict("DuplicateArgs", {
    "snippet": "str",
    "title": NotRequired["str | None"],
})
"""The arguments of `duplicate`."""

DeleteArgs = TypedDict("DeleteArgs", {
    "snippet": "str",
})
"""The arguments of `delete`."""

RecordUseArgs = TypedDict("RecordUseArgs", {
    "snippet": "str",
})
"""The arguments of `record-use`."""

RenameFolderArgs = TypedDict("RenameFolderArgs", {
    "folder": "str",
    "to": "str",
})
"""The arguments of `rename-folder`."""

RenameTagArgs = TypedDict("RenameTagArgs", {
    "tag": "str",
    "to": "str",
})
"""The arguments of `rename-tag`."""

RemoveTagArgs = TypedDict("RemoveTagArgs", {
    "tag": "str",
})
"""The arguments of `remove-tag`."""

class Library(Resource):
    """Snippet library — `snippets.library`."""
    __surface__ = "so.tilo.snippets@ab6662b1374b4cde"
    snippets: list[Entry[Snippet]]

    def new_snippet(self,
        *,
        title: str,
        folder: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["title"] = title
        if folder is not None:
            args["folder"] = folder
        return self.action("new-snippet", **args)

    def new_snippet_many(self, items: list[NewSnippetArgs]) -> list[dict[str, Any]]:
        """Runs `new-snippet` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("new-snippet", items)

    def duplicate(self,
        *,
        snippet: str,
        title: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["snippet"] = snippet
        if title is not None:
            args["title"] = title
        return self.action("duplicate", **args)

    def duplicate_many(self, items: list[DuplicateArgs]) -> list[dict[str, Any]]:
        """Runs `duplicate` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("duplicate", items)

    def delete(self, *, snippet: str) -> None:
        """Runs the app's action."""
        return self.action("delete", snippet=snippet)

    def delete_many(self, items: list[DeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete", items)

    def record_use(self, *, snippet: str) -> None:
        """Runs the app's action."""
        return self.action("record-use", snippet=snippet)

    def record_use_many(self, items: list[RecordUseArgs]) -> list[dict[str, Any]]:
        """Runs `record-use` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("record-use", items)

    def rename_folder(self,
        *,
        folder: str,
        to: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("rename-folder", folder=folder, to=to)

    def rename_folder_many(self, items: list[RenameFolderArgs]) -> list[dict[str, Any]]:
        """Runs `rename-folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename-folder", items)

    def rename_tag(self,
        *,
        tag: str,
        to: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("rename-tag", tag=tag, to=to)

    def rename_tag_many(self, items: list[RenameTagArgs]) -> list[dict[str, Any]]:
        """Runs `rename-tag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename-tag", items)

    def remove_tag(self, *, tag: str) -> None:
        """Runs the app's action."""
        return self.action("remove-tag", tag=tag)

    def remove_tag_many(self, items: list[RemoveTagArgs]) -> list[dict[str, Any]]:
        """Runs `remove-tag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-tag", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_snippet", "new_snippet_many", "duplicate", "duplicate_many", "delete", "delete_many", "record_use", "record_use_many", "rename_folder", "rename_folder_many", "rename_tag", "rename_tag_many", "remove_tag", "remove_tag_many"})


