"""Types for Snippets (`so.tilo.snippets`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Snippet` is the app's `tl.so.tilo.snippets.Snippet` at `snippets/<resource>`, read with
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
    "FillValue",
    "Fill",
    "Library",
    "Snippets",
    "StarArgs",
    "SetTagsArgs",
    "UntagArgs",
    "CancelFillArgs",
    "ClearFillArgs",
    "DeleteArgs",
    "RenameFolderArgs",
    "RenameTagArgs",
    "RemoveTagArgs",
]

Language = Literal["text", "shell", "rust", "sql", "git", "python", "javascript", "json", "yaml", "markdown", "html", "css", "toml", "dockerfile"]
"""The kind of text a snippet is written in: its badge in the list and the colours its body is shown with."""

StarArgs = TypedDict("StarArgs", {
    "starred": NotRequired["bool | None"],
})
"""The arguments of `star`."""

SetTagsArgs = TypedDict("SetTagsArgs", {
    "tags": "str",
})
"""The arguments of `set_tags`."""

UntagArgs = TypedDict("UntagArgs", {
    "tag": "str",
})
"""The arguments of `untag`."""

CancelFillArgs = TypedDict("CancelFillArgs", {
})
"""The arguments of `cancel_fill`."""

ClearFillArgs = TypedDict("ClearFillArgs", {
})
"""The arguments of `clear_fill`."""

DeleteArgs = TypedDict("DeleteArgs", {
    "snippet": "Snippet",
})
"""The arguments of `delete`."""

RenameFolderArgs = TypedDict("RenameFolderArgs", {
    "folder": "str",
    "to": "str",
})
"""The arguments of `rename_folder`."""

RenameTagArgs = TypedDict("RenameTagArgs", {
    "tag": "str",
    "to": "str",
})
"""The arguments of `rename_tag`."""

RemoveTagArgs = TypedDict("RemoveTagArgs", {
    "tag": "str",
})
"""The arguments of `remove_tag`."""

class Snippet(Resource):
    """Snippet — `tl.so.tilo.snippets.Snippet`."""
    __surface__ = "so.tilo.snippets@275605095d14df3c"
    f7zljrnp: str  # title
    @property
    def _5k3zya5m(self) -> str:
        """body (`5k3zya5m` on the wire)."""
        return self.fields["5k3zya5m"]

    @_5k3zya5m.setter
    def _5k3zya5m(self, value: str) -> None:
        self.edit("5k3zya5m", value=value)
    wziqkkiz: str  # description
    rorcz6y2: bool  # favorite
    b5ooyg4x: str  # folder
    @property
    def _26cc5hue(self) -> Language:
        """language (`26cc5hue` on the wire)."""
        return self.fields["26cc5hue"]

    @_26cc5hue.setter
    def _26cc5hue(self, value: Language) -> None:
        self.edit("26cc5hue", value=value)
    te2jwa3o: list[str]  # tags
    kkfkhxfb: int  # uses

    def star(self,
        *,
        starred: bool | None = None,
    ) -> None:
        """Stars a snippet, or takes the star back."""
        args: dict[str, Any] = {}
        if starred is not None:
            args["starred"] = starred
        return self.action("star", **args)

    def star_many(self, items: list[StarArgs]) -> list[dict[str, Any]]:
        """Runs `star` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("star", items)

    def set_tags(self, *, tags: str) -> None:
        """Gives a snippet the tags a person typed, comma-separated: each trimmed, lower case, without `#`, at most 24 of them."""
        return self.action("set_tags", tags=tags)

    def set_tags_many(self, items: list[SetTagsArgs]) -> list[dict[str, Any]]:
        """Runs `set_tags` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_tags", items)

    def untag(self, *, tag: str) -> None:
        """Takes one tag off one snippet."""
        return self.action("untag", tag=tag)

    def untag_many(self, items: list[UntagArgs]) -> list[dict[str, Any]]:
        """Runs `untag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("untag", items)

    def copy(self) -> None:
        """Copies a snippet's body to the clipboard, and counts the use. A snippet with variables is filled in first (`begin_fill`), so what is copied is what the person meant."""
        return self.action("copy")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"star", "star_many", "set_tags", "set_tags_many", "untag", "untag_many", "copy"})


class FillValue(Resource):
    """FillValue — `tl.so.tilo.snippets.FillValue`."""
    __surface__ = "so.tilo.snippets@275605095d14df3c"
    u5ym5ttn: str  # name
    h5vss6x5: str  # value


class Fill(Resource):
    """Fill — `tl.so.tilo.snippets.Fill`."""
    __surface__ = "so.tilo.snippets@275605095d14df3c"
    qxvejddr: Snippet | None  # snippet
    e6uc6jtt: list[Entry[FillValue]]  # values

    def cancel_fill(self) -> None:
        """Puts the filling away without copying."""
        return self.action("cancel_fill")

    def cancel_fill_many(self, items: list[CancelFillArgs]) -> list[dict[str, Any]]:
        """Runs `cancel_fill` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("cancel_fill", items)

    def clear_fill(self) -> None:
        """Empties a filling, so the sheet closes."""
        return self.action("clear_fill")

    def clear_fill_many(self, items: list[ClearFillArgs]) -> list[dict[str, Any]]:
        """Runs `clear_fill` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_fill", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"cancel_fill", "cancel_fill_many", "clear_fill", "clear_fill_many"})


class Library(Resource):
    """Library — `tl.so.tilo.snippets.Library`."""
    __surface__ = "so.tilo.snippets@275605095d14df3c"
    g5k73yqh: list[Entry[Snippet]]  # snippets
    avqtjv3w: list[Entry[Fill]]  # fills

    def new_snippet(self,
        *,
        title: str,
        folder: str | None = None,
    ) -> Snippet:
        """Makes a snippet, filed under `folder` when it names one."""
        args: dict[str, Any] = {}
        args["title"] = title
        if folder is not None:
            args["folder"] = folder
        return self.action("new_snippet", **args)

    def duplicate(self,
        *,
        snippet: Snippet,
        title: str | None = None,
    ) -> Snippet:
        """Makes a second copy of a snippet, right after it; its word count starts again from nothing."""
        args: dict[str, Any] = {}
        args["snippet"] = snippet
        if title is not None:
            args["title"] = title
        return self.action("duplicate", **args)

    def delete(self, *, snippet: Snippet) -> None:
        """Deletes a snippet from the library. Undo brings it back."""
        return self.action("delete", snippet=snippet)

    def delete_many(self, items: list[DeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete", items)

    def rename_folder(self,
        *,
        folder: str,
        to: str,
    ) -> None:
        """Renames a folder everywhere it is used; an empty new name files its snippets under nothing."""
        return self.action("rename_folder", folder=folder, to=to)

    def rename_folder_many(self, items: list[RenameFolderArgs]) -> list[dict[str, Any]]:
        """Runs `rename_folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename_folder", items)

    def rename_tag(self,
        *,
        tag: str,
        to: str,
    ) -> None:
        """Renames a tag everywhere it is used."""
        return self.action("rename_tag", tag=tag, to=to)

    def rename_tag_many(self, items: list[RenameTagArgs]) -> list[dict[str, Any]]:
        """Runs `rename_tag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename_tag", items)

    def remove_tag(self, *, tag: str) -> None:
        """Takes a tag off every snippet that carries it."""
        return self.action("remove_tag", tag=tag)

    def remove_tag_many(self, items: list[RemoveTagArgs]) -> list[dict[str, Any]]:
        """Runs `remove_tag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_tag", items)

    def begin_fill(self, *, snippet: Snippet) -> Fill:
        """Starts filling in a snippet's variables: one field for each variable its body holds, emptied."""
        return self.action("begin_fill", snippet=snippet)

    def copy_filled(self, *, fill: Fill) -> None:
        """Copies the filled body, counts the use, and puts the filling away."""
        return self.action("copy_filled", fill=fill)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_snippet", "duplicate", "delete", "delete_many", "rename_folder", "rename_folder_many", "rename_tag", "rename_tag_many", "remove_tag", "remove_tag_many", "begin_fill", "copy_filled"})


class Snippets(Resource):
    """Snippets — `tl.so.tilo.snippets`."""
    __surface__ = "so.tilo.snippets@275605095d14df3c"
    def copy_text(self, *, text: str) -> None:
        """Puts `text` on the clipboard: as a secret, which histories do not keep and which leaves the clipboard after a while, when it looks like it carries a credential, so an API key does not stay there for whatever is pasted next."""
        return self.action("copy_text", text=text)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"copy_text"})


