"""Types for Disks (`so.tilo.disks`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Disks` is the app's `tl.so.tilo.disks.Disks` at `disks/<resource>`, read with
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
    "Disks",
    "TiloDisks",
    "NameDriveArgs",
]

NameDriveArgs = TypedDict("NameDriveArgs", {
    "serial": "str",
    "name": "str",
})
"""The arguments of `name_drive`."""

class Disks(Resource):
    """Disks — `tl.so.tilo.disks.Disks`."""
    __surface__ = "so.tilo.disks@4735aa45d7142f43"
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

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"name_drive", "name_drive_many"})


class TiloDisks(Resource):
    """Disks — `tl.so.tilo.disks`."""
    __surface__ = "so.tilo.disks@4735aa45d7142f43"
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


