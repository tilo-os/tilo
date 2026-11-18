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
    "RecentUse",
    "Folder",
    "Tree",
    "Recent",
    "AddBookmarkArgs",
    "RemoveBookmarkArgs",
    "NewFolderArgs",
    "NewFileArgs",
    "MoveArgs",
    "WriteArgs",
    "TrashArgs",
    "RestoreArgs",
    "NoteArgs",
    "MovedArgs",
    "ForgetArgs",
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

RecentUse = Literal["opened", "changed"]

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
    "parent": "str",
    "name": "str",
    "document": NotRequired["str | None"],
})
"""The arguments of `new-folder`."""

NewFileArgs = TypedDict("NewFileArgs", {
    "parent": "str",
    "name": "str",
    "document": NotRequired["str | None"],
})
"""The arguments of `new-file`."""

MoveArgs = TypedDict("MoveArgs", {
    "node": "str",
    "parent": "str",
    "name": "str",
})
"""The arguments of `move`."""

WriteArgs = TypedDict("WriteArgs", {
    "node": "str",
    "content": "str",
    "size": "int",
})
"""The arguments of `write`."""

TrashArgs = TypedDict("TrashArgs", {
    "node": "str",
})
"""The arguments of `trash`."""

RestoreArgs = TypedDict("RestoreArgs", {
    "node": "str",
})
"""The arguments of `restore`."""

NoteArgs = TypedDict("NoteArgs", {
    "device": "str",
    "path": "str",
    "last_use": "RecentUse",
    "at_ms": "int",
})
"""The arguments of `note`."""

MovedArgs = TypedDict("MovedArgs", {
    "device": "str",
    "from": "str",
    "to": "str",
})
"""The arguments of `moved`."""

ForgetArgs = TypedDict("ForgetArgs", {
    "device": "str",
    "path": "str",
})
"""The arguments of `forget`."""

class Folder(Resource):
    """Folder — `files.folder`."""
    __surface__ = "so.tilo.file-explorer@d93c18922e3b20c1"
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

    def import_mounted(self,
        *,
        device: str,
        paths: list[str],
        folder: str,
    ) -> Any:
        """Runs the app's action."""
        return self.action("import-mounted", device=device, paths=paths, folder=folder)

    def cloud_import(self,
        *,
        device: str,
        job_id: str,
        account: str,
        entries: list[Any],
        destination: Any,
    ) -> Any:
        """Runs the app's action."""
        return self.action("cloud-import", device=device, job_id=job_id, account=account, entries=entries, destination=destination)

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

    def cloud_progress(self) -> None:
        """Runs the app's action."""
        return self.action("cloud-progress")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"add_bookmark", "add_bookmark_many", "remove_bookmark", "remove_bookmark_many", "new_folder", "rename", "trash", "move", "copy_into", "import_mounted", "cloud_import", "write_file", "compress", "extract", "record_clipboard_transfer", "record_trash_restore", "revert", "hand_over", "read_text", "copy_selection", "clipboard_status", "open_terminal", "cloud_progress"})


class Tree(Resource):
    """Files — `files.tree`."""
    __surface__ = "so.tilo.file-explorer@d93c18922e3b20c1"
    def new_folder(self,
        *,
        parent: str,
        name: str,
        document: str | None = None,
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
        parent: str,
        name: str,
        document: str | None = None,
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
        node: str,
        parent: str,
        name: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("move", node=node, parent=parent, name=name)

    def move_many(self, items: list[MoveArgs]) -> list[dict[str, Any]]:
        """Runs `move` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move", items)

    def write(self,
        *,
        node: str,
        content: str,
        size: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("write", node=node, content=content, size=size)

    def write_many(self, items: list[WriteArgs]) -> list[dict[str, Any]]:
        """Runs `write` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("write", items)

    def trash(self, *, node: str) -> None:
        """Runs the app's action."""
        return self.action("trash", node=node)

    def trash_many(self, items: list[TrashArgs]) -> list[dict[str, Any]]:
        """Runs `trash` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("trash", items)

    def restore(self, *, node: str) -> None:
        """Runs the app's action."""
        return self.action("restore", node=node)

    def restore_many(self, items: list[RestoreArgs]) -> list[dict[str, Any]]:
        """Runs `restore` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("restore", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_folder", "new_folder_many", "new_file", "new_file_many", "move", "move_many", "write", "write_many", "trash", "trash_many", "restore", "restore_many"})


class Recent(Resource):
    """Recent files — `files.recent`."""
    __surface__ = "so.tilo.file-explorer@d93c18922e3b20c1"
    devices: dict[str, Any]

    def note(self,
        *,
        device: str,
        path: str,
        last_use: RecentUse,
        at_ms: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("note", device=device, path=path, last_use=last_use, at_ms=at_ms)

    def note_many(self, items: list[NoteArgs]) -> list[dict[str, Any]]:
        """Runs `note` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("note", items)

    def moved(self,
        *,
        device: str,
        from_: str,
        to: str,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["device"] = device
        args["from"] = from_
        args["to"] = to
        return self.action("moved", **args)

    def moved_many(self, items: list[MovedArgs]) -> list[dict[str, Any]]:
        """Runs `moved` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("moved", items)

    def forget(self,
        *,
        device: str,
        path: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("forget", device=device, path=path)

    def forget_many(self, items: list[ForgetArgs]) -> list[dict[str, Any]]:
        """Runs `forget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"note", "note_many", "moved", "moved_many", "forget", "forget_many"})


