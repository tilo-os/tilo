"""Types for Files (`so.tilo.file-explorer`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Folder` is the app's `files.folder` at `file-explorer/<resource>`, read with
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
    "Bookmark",
    "FileTask",
    "Receipt",
    "RecentUse",
    "Folder",
    "Tree",
    "Recent",
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

class Folder(Resource):
    """Folder — `files.folder`."""
    __surface__ = "so.tilo.file-explorer@cb62eaabb4a7bfc3"
    places: list[Bookmark]

    def add_bookmark(self,
        *,
        place: str,
        at: int | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["place"] = place
        if at is not None:
            args["at"] = at
        return self.action("add-bookmark", **args)

    def remove_bookmark(self, *, place: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-bookmark", place=place)

    def new_folder(self,
        *,
        device: str,
        parent: str,
        name: str,
    ) -> Any | Run:
        """Runs the app's action."""
        return self.action("new-folder", device=device, parent=parent, name=name)

    def rename(self,
        *,
        device: str,
        folder: str,
        entry: str,
        name: str,
    ) -> Any | Run:
        """Runs the app's action."""
        return self.action("rename", device=device, folder=folder, entry=entry, name=name)

    def trash(self,
        *,
        device: str,
        folder: str,
        entries: list[str],
    ) -> Any | Run:
        """Runs the app's action."""
        return self.action("trash", device=device, folder=folder, entries=entries)

    def move(self,
        *,
        device: str,
        from_: str,
        entries: list[str],
        to: str,
        create: bool | None = None,
    ) -> Any | Run:
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
    ) -> Any | Run:
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
    ) -> Any | Run:
        """Runs the app's action."""
        return self.action("import-mounted", device=device, paths=paths, folder=folder)

    def cloud_import(self,
        *,
        device: str,
        job_id: str,
        account: str,
        entries: list[Any],
        destination: Any,
    ) -> Any | Run:
        """Runs the app's action."""
        return self.action("cloud-import", device=device, job_id=job_id, account=account, entries=entries, destination=destination)

    def write_file(self,
        *,
        device: str,
        folder: str,
        name: str,
        text: str,
        replace: bool | None = None,
    ) -> Any | Run:
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
    ) -> Any | Run:
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
    ) -> Any | Run:
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
    ) -> Any | Run:
        """Runs the app's action."""
        return self.action("record-clipboard-transfer", device=device, receipt=receipt)

    def record_trash_restore(self,
        *,
        device: str,
        receipt: Receipt,
    ) -> Any | Run:
        """Runs the app's action."""
        return self.action("record-trash-restore", device=device, receipt=receipt)

    def revert(self, *, op: str) -> Any | Run:
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


class Tree(Resource):
    """Files — `files.tree`."""
    __surface__ = "so.tilo.file-explorer@cb62eaabb4a7bfc3"

class Recent(Resource):
    """Recent files — `files.recent`."""
    __surface__ = "so.tilo.file-explorer@cb62eaabb4a7bfc3"
    devices: dict[str, Any]

    def note(self,
        *,
        device: str,
        path: str,
        last_use: RecentUse,
        at_ms: int,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("note", device=device, path=path, last_use=last_use, at_ms=at_ms)

    def moved(self,
        *,
        device: str,
        from_: str,
        to: str,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["device"] = device
        args["from"] = from_
        args["to"] = to
        return self.action("moved", **args)

    def forget(self,
        *,
        device: str,
        path: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("forget", device=device, path=path)


