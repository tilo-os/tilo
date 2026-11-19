"""Types for Tilo Installer (`so.tilo.imager`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Sources` is the app's `imager.sources` at `imager/<resource>`, read with
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
    "Filesystem",
    "InstallProfile",
    "PreparedSetupExpectation",
    "RecentSource",
    "Source",
    "SourceIdentity",
    "SourceKind",
    "UserAccount",
    "Sources",
    "SelectSourceArgs",
    "ClearSourceArgs",
]

Filesystem = Literal["btrfs", "ext4", "xfs"]

class InstallProfile(TypedDict):
    purpose: str
    hostname: str
    user: UserAccount
    locale: str
    keyboard: str
    timezone: str
    filesystem: Filesystem
    wifi_ssid: str
    device_label: str
    enroll_local_identity: bool
    prepared_setup: PreparedSetupExpectation | None

class PreparedSetupExpectation(TypedDict):
    schema: int
    sealed_bytes: int
    ciphertext_sha256: str

class RecentSource(TypedDict):
    id: str
    rank: int
    source: Source

class Source(TypedDict):
    kind: SourceKind
    path: str
    name: str
    size: int
    label: str
    installable: bool
    identity: SourceIdentity | None
    expected_sha256: str | None

class SourceIdentity(TypedDict):
    volume: int
    file: int

SourceKind = Literal["IsoFile", "TiloImage"]

class UserAccount(TypedDict):
    full_name: str
    username: str

SelectSourceArgs = TypedDict("SelectSourceArgs", {
    "source": "Source",
})
"""The arguments of `select-source`."""

ClearSourceArgs = TypedDict("ClearSourceArgs", {
})
"""The arguments of `clear-source`."""

class Sources(Resource):
    """Image sources — `imager.sources`."""
    __surface__ = "so.tilo.imager@4a37f2987b3e5ed3"
    @property
    def source(self) -> Source | None:
        """Read only."""
        return self.fields["source"]
    @property
    def recent(self) -> list[Entry[RecentSource]]:
        """Read only."""
        return self.fields["recent"]
    profile: InstallProfile

    def select_source(self, *, source: Source) -> None:
        """Runs the app's action."""
        return self.action("select-source", source=source)

    def select_source_many(self, items: list[SelectSourceArgs]) -> list[dict[str, Any]]:
        """Runs `select-source` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("select-source", items)

    def clear_source(self) -> None:
        """Runs the app's action."""
        return self.action("clear-source")

    def clear_source_many(self, items: list[ClearSourceArgs]) -> list[dict[str, Any]]:
        """Runs `clear-source` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear-source", items)

    def status(self) -> None:
        """Runs the app's action."""
        return self.action("status")

    def export_config(self) -> None:
        """Runs the app's action."""
        return self.action("export-config")

    def rescan(self) -> None:
        """Runs the app's action."""
        return self.action("rescan")

    def choose_drive(self, *, drive: str) -> None:
        """Runs the app's action."""
        return self.action("choose-drive", drive=drive)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"select_source", "select_source_many", "clear_source", "clear_source_many", "status", "export_config", "rescan", "choose_drive"})


