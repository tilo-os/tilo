"""Types for Windows Apps (`so.tilo.windows`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `windows/<resource>`, read with
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
    "OpenWith",
    "Notice",
    "Windows",
    "TiloWindows",
]

OpenWith = Literal["wine", "proton", "vm"]
"""How a program opens: with Wine or Proton in a private capsule, or in the Windows appliance."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.windows@03167a1d76fc0991"
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


class Windows(Resource):
    """Windows — `tl.so.tilo.windows.Windows`."""
    __surface__ = "so.tilo.windows@03167a1d76fc0991"
    @property
    def _3bu5zzlt(self) -> dict[str, OpenWith]:
        """routes (`3bu5zzlt` on the wire)."""
        return self.fields["3bu5zzlt"]

    @_3bu5zzlt.setter
    def _3bu5zzlt(self, value: dict[str, OpenWith]) -> None:
        self.edit("3bu5zzlt", value=value)

    def set_route(self,
        *,
        program: str,
        route: OpenWith,
    ) -> None:
        """Choose how a program opens from now on, and carry the choice to this computer."""
        return self.action("set_route", program=program, route=route)

    def use_recommendation(self, *, program: str) -> None:
        """Let a program open the way the catalogue recommends, and carry that to this computer."""
        return self.action("use_recommendation", program=program)

    def installed(self, *, file: Any) -> None:
        """Install the installer the person chose, or the one a file manager opened here."""
        return self.action("installed", file=file)

    def opened_file(self,
        *,
        program: Any,
        file: Any,
    ) -> None:
        """Open the document the person chose in `program`."""
        return self.action("opened_file", program=program, file=file)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"set_route", "use_recommendation", "installed", "opened_file"})


class TiloWindows(Resource):
    """Windows Apps — `tl.so.tilo.windows`."""
    __surface__ = "so.tilo.windows@03167a1d76fc0991"
    def imported_iso(self, *, file: Any) -> None:
        """Set up the appliance from the Windows ISO the person chose."""
        return self.action("imported_iso", file=file)

    def recovery_desktop(self) -> None:
        """Open the appliance's recovery desktop."""
        return self.action("recovery_desktop")

    def start_windows(self) -> None:
        """Start the appliance."""
        return self.action("start_windows")

    def restart_windows(self) -> None:
        """Restart the appliance."""
        return self.action("restart_windows")

    def suspend_windows(self) -> None:
        """Suspend the appliance."""
        return self.action("suspend_windows")

    def resume_windows(self) -> None:
        """Resume the appliance."""
        return self.action("resume_windows")

    def shutdown_windows(self) -> None:
        """Shut the appliance down."""
        return self.action("shutdown_windows")

    def temporary_network(self) -> None:
        """Connect the appliance for activation, for a while."""
        return self.action("temporary_network")

    def update_network(self) -> None:
        """Connect the appliance for Windows Update, for a while."""
        return self.action("update_network")

    def revoke_network(self) -> None:
        """End the appliance's temporary connection."""
        return self.action("revoke_network")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"imported_iso", "recovery_desktop", "start_windows", "restart_windows", "suspend_windows", "resume_windows", "shutdown_windows", "temporary_network", "update_network", "revoke_network"})


