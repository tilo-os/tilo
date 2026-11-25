"""Types for Disks (`so.tilo.disks`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `disks/<resource>`, read with
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
    "Notice",
    "Disks",
    "TiloDisks",
    "NameDriveArgs",
]

NameDriveArgs = TypedDict("NameDriveArgs", {
    "serial": "str",
    "name": "str",
})
"""The arguments of `name_drive`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.disks@5d78ccc3c7eb6c6b"
    @property
    def notice_to(self) -> str:
        """to (`notice-to` on the wire)."""
        return self.fields["notice-to"]

    @notice_to.setter
    def notice_to(self, value: str) -> None:
        self.edit("notice-to", value=value)
    @property
    def notice_title(self) -> str:
        """title (`notice-title` on the wire)."""
        return self.fields["notice-title"]

    @notice_title.setter
    def notice_title(self, value: str) -> None:
        self.edit("notice-title", value=value)
    @property
    def notice_body(self) -> str:
        """body (`notice-body` on the wire)."""
        return self.fields["notice-body"]

    @notice_body.setter
    def notice_body(self, value: str) -> None:
        self.edit("notice-body", value=value)
    @property
    def notice_opens(self) -> Any:
        """opens (`notice-opens` on the wire)."""
        return self.fields["notice-opens"]

    @notice_opens.setter
    def notice_opens(self, value: Any) -> None:
        self.edit("notice-opens", value=value)
    @property
    def notice_seen(self) -> bool:
        """seen (`notice-seen` on the wire)."""
        return self.fields["notice-seen"]

    @notice_seen.setter
    def notice_seen(self, value: bool) -> None:
        self.edit("notice-seen", value=value)
    @property
    def notice_device(self) -> str:
        """device (`notice-device` on the wire)."""
        return self.fields["notice-device"]

    @notice_device.setter
    def notice_device(self, value: str) -> None:
        self.edit("notice-device", value=value)
    @property
    def notice_actions(self) -> list[str]:
        """actions (`notice-actions` on the wire)."""
        return self.fields["notice-actions"]

    @notice_actions.setter
    def notice_actions(self, value: list[str]) -> None:
        self.edit("notice-actions", value=value)


class Disks(Resource):
    """Disks — `tl.so.tilo.disks.Disks`."""
    __surface__ = "so.tilo.disks@5d78ccc3c7eb6c6b"
    @property
    def _6mf2skfm(self) -> dict[str, str]:
        """names (`6mf2skfm` on the wire)."""
        return self.fields["6mf2skfm"]

    @_6mf2skfm.setter
    def _6mf2skfm(self, value: dict[str, str]) -> None:
        self.edit("6mf2skfm", value=value)

    def name_drive(self,
        *,
        serial: str,
        name: str,
    ) -> None:
        """A drive's name in the words of the person reading it, by its serial."""
        return self.action("name_drive", serial=serial, name=name)

    def name_drive_many(self, items: list[NameDriveArgs]) -> list[dict[str, Any]]:
        """Runs `name_drive` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("name_drive", items)

    def open_in_files(self, *, path: str) -> None:
        """Opens a mounted folder in Files."""
        return self.action("open_in_files", path=path)

    def open_usage(self) -> None:
        """Opens Disk Usage, where a folder's use is measured."""
        return self.action("open_usage")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"name_drive", "name_drive_many", "open_in_files", "open_usage"})


class TiloDisks(Resource):
    """Disks — `tl.so.tilo.disks`."""
    __surface__ = "so.tilo.disks@5d78ccc3c7eb6c6b"
    def mount_first(self, *, drive: Any) -> None:
        """Mounts the drive's first partition, which is what a drive without a filesystem of its own holds."""
        return self.action("mount_first", drive=drive)

    def format_device(self,
        *,
        device_name: str,
        filesystem: str,
    ) -> None:
        """Erases the device the confirmation named, and writes the chosen filesystem."""
        return self.action("format_device", device_name=device_name, filesystem=filesystem)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"mount_first", "format_device"})


