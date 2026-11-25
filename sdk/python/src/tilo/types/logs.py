"""Types for Logs (`so.tilo.logs`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `logs/<resource>`, read with
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
    "Logs",
    "ExportEntry",
    "ExportField",
    "TiloLogs",
    "SetSeverityArgs",
    "SetUnitArgs",
    "SetBootArgs",
    "SetSearchArgs",
    "ChooseArgs",
    "ClearFiltersArgs",
]

SetSeverityArgs = TypedDict("SetSeverityArgs", {
    "value": "str",
})
"""The arguments of `set_severity`."""

SetUnitArgs = TypedDict("SetUnitArgs", {
    "value": "str",
})
"""The arguments of `set_unit`."""

SetBootArgs = TypedDict("SetBootArgs", {
    "value": "str",
})
"""The arguments of `set_boot`."""

SetSearchArgs = TypedDict("SetSearchArgs", {
    "value": "str",
})
"""The arguments of `set_search`."""

ChooseArgs = TypedDict("ChooseArgs", {
    "key": "str",
})
"""The arguments of `choose`."""

ClearFiltersArgs = TypedDict("ClearFiltersArgs", {
})
"""The arguments of `clear_filters`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.logs@8f6a67d584534406"
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


class Logs(Resource):
    """Logs — `tl.so.tilo.logs.Logs`."""
    __surface__ = "so.tilo.logs@8f6a67d584534406"
    @property
    def _6e5izmwp(self) -> str:
        """severity (`6e5izmwp` on the wire)."""
        return self.fields["6e5izmwp"]

    @_6e5izmwp.setter
    def _6e5izmwp(self, value: str) -> None:
        self.edit("6e5izmwp", value=value)
    lq27tlwx: str  # unit
    vzywk5iy: str  # boot
    yuygrdvc: str  # search
    rumvop3c: bool  # following
    cctmzqe2: str  # selected

    def made_export(self) -> str:
        """The records an export writes, one JSON object a line."""
        return self.action("made_export")

    def set_severity(self, *, value: str) -> None:
        """Shows records of at least `value`'s severity."""
        return self.action("set_severity", value=value)

    def set_severity_many(self, items: list[SetSeverityArgs]) -> list[dict[str, Any]]:
        """Runs `set_severity` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_severity", items)

    def set_unit(self, *, value: str) -> None:
        """Shows one unit's records, or every unit's when `value` is empty."""
        return self.action("set_unit", value=value)

    def set_unit_many(self, items: list[SetUnitArgs]) -> list[dict[str, Any]]:
        """Runs `set_unit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_unit", items)

    def set_boot(self, *, value: str) -> None:
        """Shows one boot's records, or every boot's when `value` is empty."""
        return self.action("set_boot", value=value)

    def set_boot_many(self, items: list[SetBootArgs]) -> list[dict[str, Any]]:
        """Runs `set_boot` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_boot", items)

    def set_search(self, *, value: str) -> None:
        """Narrows the records to those holding `value`."""
        return self.action("set_search", value=value)

    def set_search_many(self, items: list[SetSearchArgs]) -> list[dict[str, Any]]:
        """Runs `set_search` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_search", items)

    def choose(self, *, key: str) -> None:
        """Shows one record's detail."""
        return self.action("choose", key=key)

    def choose_many(self, items: list[ChooseArgs]) -> list[dict[str, Any]]:
        """Runs `choose` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("choose", items)

    def clear_filters(self) -> None:
        """Clears every journal filter."""
        return self.action("clear_filters")

    def clear_filters_many(self, items: list[ClearFiltersArgs]) -> list[dict[str, Any]]:
        """Runs `clear_filters` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_filters", items)

    # the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
    def follow(self, *, on: bool) -> None:
        """Follows new records as they arrive, or stops following them."""
        return self.action("follow", on=on)

    def poll_tail(self) -> None:
        """Reads the records written since the last one read, while following."""
        return self.action("poll_tail")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"made_export", "set_severity", "set_severity_many", "set_unit", "set_unit_many", "set_boot", "set_boot_many", "set_search", "set_search_many", "choose", "choose_many", "clear_filters", "clear_filters_many", "follow", "poll_tail"})


class ExportEntry(Resource):
    """ExportEntry — `tl.so.tilo.logs.ExportEntry`."""
    __surface__ = "so.tilo.logs@8f6a67d584534406"
    @property
    def _37kvzek7(self) -> int:
        """realtime_us (`37kvzek7` on the wire)."""
        return self.fields["37kvzek7"]

    @_37kvzek7.setter
    def _37kvzek7(self, value: int) -> None:
        self.edit("37kvzek7", value=value)
    v3wlyikw: str  # unit
    ur4aajno: str  # identifier
    wsg4pkbu: int  # priority
    cd6taqqw: str  # message
    xkxlpyrt: int | None  # pid
    @property
    def _5wwthj2f(self) -> str:
        """hostname (`5wwthj2f` on the wire)."""
        return self.fields["5wwthj2f"]

    @_5wwthj2f.setter
    def _5wwthj2f(self, value: str) -> None:
        self.edit("5wwthj2f", value=value)
    odiofhx6: str  # boot_id
    @property
    def _3ra7d3km(self) -> str:
        """transport (`3ra7d3km` on the wire)."""
        return self.fields["3ra7d3km"]

    @_3ra7d3km.setter
    def _3ra7d3km(self, value: str) -> None:
        self.edit("3ra7d3km", value=value)
    djjluyru: list[ExportField]  # extra


class ExportField(Resource):
    """ExportField — `tl.so.tilo.logs.ExportField`."""
    __surface__ = "so.tilo.logs@8f6a67d584534406"
    mj25pae6: str  # name
    pnqueidz: str  # value


class TiloLogs(Resource):
    """Logs — `tl.so.tilo.logs`."""
    __surface__ = "so.tilo.logs@8f6a67d584534406"
    def copy_entry(self, *, entry: Any) -> None:
        """Copies one record, with its time, unit, program and message."""
        return self.action("copy_entry", entry=entry)

    def copy_message(self, *, entry: Any) -> None:
        """Copies one record's message."""
        return self.action("copy_message", entry=entry)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"copy_entry", "copy_message"})


