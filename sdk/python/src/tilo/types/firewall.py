"""Types for Firewall (`so.tilo.firewall`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Rule` is the app's `tl.so.tilo.firewall.Rule` at `firewall/<resource>`, read with
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
    "Rule",
    "Zone",
    "Firewall",
    "ApplyConfig",
    "ApplyZone",
    "ApplyRule",
    "ServiceDef",
    "TiloFirewall",
    "ImportDeviceArgs",
]

ImportDeviceArgs = TypedDict("ImportDeviceArgs", {
})
"""The arguments of `import_device`."""

class Rule(Resource):
    """Rule — `tl.so.tilo.firewall.Rule`."""
    __surface__ = "so.tilo.firewall@e025fff899ca62d8"
    eeqhnuyu: str  # title
    @property
    def _3jndsc7i(self) -> str:
        """service (`3jndsc7i` on the wire)."""
        return self.fields["3jndsc7i"]

    @_3jndsc7i.setter
    def _3jndsc7i(self, value: str) -> None:
        self.edit("3jndsc7i", value=value)
    dlj2kbdr: str  # port
    nconpw3q: str  # proto
    @property
    def _2ov2rg7l(self) -> str:
        """verdict (`2ov2rg7l` on the wire)."""
        return self.fields["2ov2rg7l"]

    @_2ov2rg7l.setter
    def _2ov2rg7l(self, value: str) -> None:
        self.edit("2ov2rg7l", value=value)
    adadcwgr: str  # source
    td35s7rd: bool  # enabled


class Zone(Resource):
    """Zone — `tl.so.tilo.firewall.Zone`."""
    __surface__ = "so.tilo.firewall@e025fff899ca62d8"
    kzbzenxe: str  # kind
    xthxweeb: str  # target
    @property
    def _6oz67tou(self) -> list[str]:
        """interfaces (`6oz67tou` on the wire)."""
        return self.fields["6oz67tou"]

    @_6oz67tou.setter
    def _6oz67tou(self, value: list[str]) -> None:
        self.edit("6oz67tou", value=value)
    fcur4ope: list[str]  # services
    vyfun5ze: list[Entry[Rule]]  # rules
    bqtwzcsu: bool  # masquerade


class Firewall(Resource):
    """Firewall — `tl.so.tilo.firewall.Firewall`."""
    __surface__ = "so.tilo.firewall@e025fff899ca62d8"
    fzjbpclp: bool  # enabled
    lsn43itt: bool  # panic_mode
    qzperbb7: str  # default_zone
    fjnbtnf2: list[Entry[Zone]]  # zones

    def import_device(self) -> None:
        """Brings in what this device's firewalld holds: the zones, their services and rules, and the switches. A device whose firewalld does not answer keeps the configuration it has."""
        return self.action("import_device")

    def import_device_many(self, items: list[ImportDeviceArgs]) -> list[dict[str, Any]]:
        """Runs `import_device` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import_device", items)

    def start(self) -> None:
        """Starts firewalld."""
        return self.action("start")

    def stop(self) -> None:
        """Stops firewalld. Undo starts it again but cannot take back what reached the machine while it was open, so this is asked first."""
        return self.action("stop")

    def enter_panic(self) -> None:
        """Enters panic mode: all traffic in and out is dropped. Undo lifts it but cannot restore the connections it cut, so this is asked first."""
        return self.action("enter_panic")

    def exit_panic(self) -> None:
        """Leaves panic mode."""
        return self.action("exit_panic")

    def make_active(self, *, zone: str) -> None:
        """Makes a zone the active one."""
        return self.action("make_active", zone=zone)

    def toggle_rule(self,
        *,
        zone: str,
        rule: str,
        on: bool,
    ) -> None:
        """Turns a rule on or off in a zone."""
        return self.action("toggle_rule", zone=zone, rule=rule, on=on)

    def allow_service(self,
        *,
        zone: str,
        service: str,
        on: bool,
    ) -> None:
        """Allows a service in a zone, or stops allowing it."""
        return self.action("allow_service", zone=zone, service=service, on=on)

    def add_rule(self,
        *,
        zone: str,
        editing: str,
        as_service: bool,
        service: str,
        port: str,
        proto: str,
        verdict: str,
        source: str,
    ) -> None:
        """Adds a rule to a zone, or saves the one being edited. A rule that names no valid port or source is refused, and the form says why."""
        return self.action("add_rule", zone=zone, editing=editing, as_service=as_service, service=service, port=port, proto=proto, verdict=verdict, source=source)

    def delete_rule(self,
        *,
        zone: str,
        rule: str,
    ) -> None:
        """Deletes a rule from a zone."""
        return self.action("delete_rule", zone=zone, rule=rule)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"import_device", "import_device_many", "start", "stop", "enter_panic", "exit_panic", "make_active", "toggle_rule", "allow_service", "add_rule", "delete_rule"})


class ApplyConfig(Resource):
    """ApplyConfig — `tl.so.tilo.firewall.ApplyConfig`."""
    __surface__ = "so.tilo.firewall@e025fff899ca62d8"
    z4rwsmkt: bool  # enabled
    hfx4peji: bool  # panic_mode
    wpzvhcny: str  # default_zone
    fa3zy4tg: list[ApplyZone]  # zones


class ApplyZone(Resource):
    """ApplyZone — `tl.so.tilo.firewall.ApplyZone`."""
    __surface__ = "so.tilo.firewall@e025fff899ca62d8"
    ki3bym3t: str  # kind
    npflzozj: str  # target
    yll5vuns: list[str]  # interfaces
    kasq6ajf: list[str]  # services
    rsyraw6p: list[ApplyRule]  # rules
    mi6vac6j: bool  # masquerade


class ApplyRule(Resource):
    """ApplyRule — `tl.so.tilo.firewall.ApplyRule`."""
    __surface__ = "so.tilo.firewall@e025fff899ca62d8"
    iddy2jy5: str  # service
    rukhcwu4: str  # port
    @property
    def _46qdyqd7(self) -> str:
        """proto (`46qdyqd7` on the wire)."""
        return self.fields["46qdyqd7"]

    @_46qdyqd7.setter
    def _46qdyqd7(self, value: str) -> None:
        self.edit("46qdyqd7", value=value)
    b7tu4bhi: str  # verdict
    v2uf2efw: str  # source
    pqajeuq5: bool  # enabled


class ServiceDef(Resource):
    """ServiceDef — `tl.so.tilo.firewall.ServiceDef`."""
    __surface__ = "so.tilo.firewall@e025fff899ca62d8"
    splkq5cx: str  # key
    um6wchuo: str  # name
    @property
    def _3lp2tu4x(self) -> str:
        """ports (`3lp2tu4x` on the wire)."""
        return self.fields["3lp2tu4x"]

    @_3lp2tu4x.setter
    def _3lp2tu4x(self, value: str) -> None:
        self.edit("3lp2tu4x", value=value)
    lr6vcvpw: str  # proto
    dguxp5s2: str  # description


class TiloFirewall(Resource):
    """Firewall — `tl.so.tilo.firewall`."""
    __surface__ = "so.tilo.firewall@e025fff899ca62d8"
    def load_blocked(self) -> None:
        """Reads what the kernel dropped in the last hour."""
        return self.action("load_blocked")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"load_blocked"})


