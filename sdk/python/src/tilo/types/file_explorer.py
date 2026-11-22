"""Types for Files (`so.tilo.file-explorer`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Folder` is the app's `files.folder` at `file-explorer/<resource>`, read with
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
    "Bookmark",
    "FileTask",
    "Receipt",
    "Folder",
    "Tree",
    "Workspace",
    "AddBookmarkArgs",
    "RemoveBookmarkArgs",
    "NewFolderArgs",
    "NewFileArgs",
    "MoveArgs",
    "WriteArgs",
    "TrashArgs",
    "RestoreArgs",
    "ListChunkArgs",
]

class Bookmark(TypedDict):
    place: str

class FileTask(TypedDict):
    device: str
    op: Any

class Receipt(TypedDict):
    operation: str
    completed: int
    cut: bool
    restore: bool
    error: str | None

AddBookmarkArgs = TypedDict("AddBookmarkArgs", {
    "place": "str",
    "at": NotRequired["int | None"],
})
"""The arguments of `add-bookmark`."""

RemoveBookmarkArgs = TypedDict("RemoveBookmarkArgs", {
    "place": "str",
})
"""The arguments of `remove-bookmark`."""

NewFolderArgs = TypedDict("NewFolderArgs", {
    "parent": "Any",
    "name": "Any",
    "document": NotRequired["Any | None"],
})
"""The arguments of `new-folder`."""

NewFileArgs = TypedDict("NewFileArgs", {
    "parent": "Any",
    "name": "Any",
    "document": NotRequired["Any | None"],
})
"""The arguments of `new-file`."""

MoveArgs = TypedDict("MoveArgs", {
    "node": "Any",
    "parent": "Any",
    "name": "Any",
})
"""The arguments of `move`."""

WriteArgs = TypedDict("WriteArgs", {
    "node": "Any",
    "content": "Any",
    "size": "Any",
})
"""The arguments of `write`."""

TrashArgs = TypedDict("TrashArgs", {
    "node": "Any",
})
"""The arguments of `trash`."""

RestoreArgs = TypedDict("RestoreArgs", {
    "node": "Any",
})
"""The arguments of `restore`."""

ListChunkArgs = TypedDict("ListChunkArgs", {
    "chunk": "Any",
})
"""The arguments of `list-chunk`."""

class Folder(Resource):
    """Folder — `files.folder`."""
    __surface__ = "so.tilo.file-explorer@3561502507e2cdf8"
    places: list[Entry[Bookmark]]

    def add_bookmark(self,
        *,
        place: str,
        at: int | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["place"] = place
        if at is not None:
            args["at"] = at
        return self.action("add-bookmark", **args)

    def add_bookmark_many(self, items: list[AddBookmarkArgs]) -> list[dict[str, Any]]:
        """Runs `add-bookmark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-bookmark", items)

    def remove_bookmark(self, *, place: str) -> None:
        """Runs the app's action."""
        return self.action("remove-bookmark", place=place)

    def remove_bookmark_many(self, items: list[RemoveBookmarkArgs]) -> list[dict[str, Any]]:
        """Runs `remove-bookmark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-bookmark", items)

    def new_folder(self,
        *,
        device: str,
        parent: str,
        name: str,
    ) -> Any:
        """Runs the app's action."""
        return self.action("new-folder", device=device, parent=parent, name=name)

    def rename(self,
        *,
        device: str,
        folder: str,
        entry: str,
        name: str,
    ) -> Any:
        """Runs the app's action."""
        return self.action("rename", device=device, folder=folder, entry=entry, name=name)

    def trash(self,
        *,
        device: str,
        folder: str,
        entries: list[str],
    ) -> Any:
        """Runs the app's action."""
        return self.action("trash", device=device, folder=folder, entries=entries)

    def move(self,
        *,
        device: str,
        from_: str,
        entries: list[str],
        to: str,
        create: bool | None = None,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["device"] = device
        args["from"] = from_
        args["entries"] = entries
        args["to"] = to
        if create is not None:
            args["create"] = create
        return self.action("move", **args)

    def copy_into(self,
        *,
        device: str,
        from_: str,
        entries: list[str],
        to: str,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["device"] = device
        args["from"] = from_
        args["entries"] = entries
        args["to"] = to
        return self.action("copy-into", **args)

    def write_file(self,
        *,
        device: str,
        folder: str,
        name: str,
        text: str,
        replace: bool | None = None,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["device"] = device
        args["folder"] = folder
        args["name"] = name
        args["text"] = text
        if replace is not None:
            args["replace"] = replace
        return self.action("write-file", **args)

    def compress(self,
        *,
        device: str,
        from_: str,
        entries: list[str],
        name: str | None = None,
        format: str | None = None,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["device"] = device
        args["from"] = from_
        args["entries"] = entries
        if name is not None:
            args["name"] = name
        if format is not None:
            args["format"] = format
        return self.action("compress", **args)

    def extract(self,
        *,
        device: str,
        archive: str,
        to: Any | None = None,
        if_exists: str | None = None,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["device"] = device
        args["archive"] = archive
        if to is not None:
            args["to"] = to
        if if_exists is not None:
            args["if_exists"] = if_exists
        return self.action("extract", **args)

    def record_clipboard_transfer(self,
        *,
        device: str,
        receipt: Receipt,
    ) -> Any:
        """Runs the app's action."""
        return self.action("record-clipboard-transfer", device=device, receipt=receipt)

    def record_trash_restore(self,
        *,
        device: str,
        receipt: Receipt,
    ) -> Any:
        """Runs the app's action."""
        return self.action("record-trash-restore", device=device, receipt=receipt)

    def revert(self, *, op: str) -> Any:
        """Runs the app's action."""
        return self.action("revert", op=op)

    # the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
    def hand_over(self, *, file: str) -> None:
        """Runs the app's action."""
        return self.action("hand-over", file=file)

    def read_text(self,
        *,
        file: str,
        from_: int | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["file"] = file
        if from_ is not None:
            args["from"] = from_
        return self.action("read-text", **args)

    def copy_selection(self) -> None:
        """Runs the app's action."""
        return self.action("copy-selection")

    def clipboard_status(self) -> None:
        """Runs the app's action."""
        return self.action("clipboard-status")

    def open_terminal(self) -> None:
        """Runs the app's action."""
        return self.action("open-terminal")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"add_bookmark", "add_bookmark_many", "remove_bookmark", "remove_bookmark_many", "new_folder", "rename", "trash", "move", "copy_into", "write_file", "compress", "extract", "record_clipboard_transfer", "record_trash_restore", "revert", "hand_over", "read_text", "copy_selection", "clipboard_status", "open_terminal"})


class Tree(Resource):
    """Files — `files.tree`."""
    __surface__ = "so.tilo.file-explorer@3561502507e2cdf8"
    def new_folder(self,
        *,
        parent: Any,
        name: Any,
        document: Any | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["parent"] = parent
        args["name"] = name
        if document is not None:
            args["document"] = document
        return self.action("new-folder", **args)

    def new_folder_many(self, items: list[NewFolderArgs]) -> list[dict[str, Any]]:
        """Runs `new-folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("new-folder", items)

    def new_file(self,
        *,
        parent: Any,
        name: Any,
        document: Any | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["parent"] = parent
        args["name"] = name
        if document is not None:
            args["document"] = document
        return self.action("new-file", **args)

    def new_file_many(self, items: list[NewFileArgs]) -> list[dict[str, Any]]:
        """Runs `new-file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("new-file", items)

    def move(self,
        *,
        node: Any,
        parent: Any,
        name: Any,
    ) -> None:
        """Runs the app's action."""
        return self.action("move", node=node, parent=parent, name=name)

    def move_many(self, items: list[MoveArgs]) -> list[dict[str, Any]]:
        """Runs `move` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move", items)

    def write(self,
        *,
        node: Any,
        content: Any,
        size: Any,
    ) -> None:
        """Runs the app's action."""
        return self.action("write", node=node, content=content, size=size)

    def write_many(self, items: list[WriteArgs]) -> list[dict[str, Any]]:
        """Runs `write` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("write", items)

    def trash(self, *, node: Any) -> None:
        """Runs the app's action."""
        return self.action("trash", node=node)

    def trash_many(self, items: list[TrashArgs]) -> list[dict[str, Any]]:
        """Runs `trash` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("trash", items)

    def restore(self, *, node: Any) -> None:
        """Runs the app's action."""
        return self.action("restore", node=node)

    def restore_many(self, items: list[RestoreArgs]) -> list[dict[str, Any]]:
        """Runs `restore` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("restore", items)

    def list_chunk(self, *, chunk: Any) -> None:
        """Runs the app's action."""
        return self.action("list-chunk", chunk=chunk)

    def list_chunk_many(self, items: list[ListChunkArgs]) -> list[dict[str, Any]]:
        """Runs `list-chunk` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("list-chunk", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_folder", "new_folder_many", "new_file", "new_file_many", "move", "move_many", "write", "write_many", "trash", "trash_many", "restore", "restore_many", "list_chunk", "list_chunk_many"})


class Workspace(Resource):
    """Files window — `files.workspace`."""
    __surface__ = "so.tilo.file-explorer@3561502507e2cdf8"
    def up(self) -> None:
        """Runs the app's action."""
        return self.action("up")

    def download(self, *, node: str) -> None:
        """Runs the app's action."""
        return self.action("download", node=node)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"up", "download"})


