"""Types for Firewall (`so.tilo.firewall`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Config` is the app's `firewall.config` at `firewall/<resource>`, read with
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
    "Action",
    "Matcher",
    "Proto",
    "StoredRule",
    "StoredZone",
    "Target",
    "ZoneKind",
    "Config",
    "Applied",
    "StartArgs",
    "StopArgs",
    "EnterPanicModeArgs",
    "ExitPanicModeArgs",
    "DeleteRuleArgs",
    "ImportArgs",
    "AppliedArgs",
]

Action = Literal["allow", "deny"]

Matcher = dict[str, Any]
"""One of Service, Port: an object whose case the app tags it with."""

Proto = Literal["Tcp", "Udp"]

class StoredRule(TypedDict):
    matcher: Matcher
    action: Action
    source: str
    enabled: bool

class StoredZone(TypedDict):
    target: Target
    interfaces: list[str]
    services: list[str]
    rules: list[Entry[StoredRule]]
    masquerade: bool
    deleted_rules: list[str]

Target = Literal["default", "accept", "reject", "drop"]

ZoneKind = Literal["drop", "block", "public", "external", "dmz", "work", "home", "internal", "trusted"]

StartArgs = TypedDict("StartArgs", {
})
"""The arguments of `start`."""

StopArgs = TypedDict("StopArgs", {
})
"""The arguments of `stop`."""

EnterPanicModeArgs = TypedDict("EnterPanicModeArgs", {
})
"""The arguments of `enter-panic-mode`."""

ExitPanicModeArgs = TypedDict("ExitPanicModeArgs", {
})
"""The arguments of `exit-panic-mode`."""

DeleteRuleArgs = TypedDict("DeleteRuleArgs", {
    "zone": "ZoneKind",
    "rule": "str",
})
"""The arguments of `delete-rule`."""

ImportArgs = TypedDict("ImportArgs", {
    "config": "Any",
})
"""The arguments of `import`."""

AppliedArgs = TypedDict("AppliedArgs", {
    "device": "str",
    "config": "str",
})
"""The arguments of `applied`."""

class Config(Resource):
    """Firewall — `firewall.config`."""
    __surface__ = "so.tilo.firewall@d4e5e1b79ae77004"
    @property
    def enabled(self) -> bool:
        """Read only."""
        return self.fields["enabled"]
    @property
    def panic_mode(self) -> bool:
        """Read only."""
        return self.fields["panic_mode"]
    default_zone: ZoneKind
    zones: dict[str, StoredZone]

    def start(self) -> None:
        """Runs the app's action."""
        return self.action("start")

    def start_many(self, items: list[StartArgs]) -> list[dict[str, Any]]:
        """Runs `start` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("start", items)

    def stop(self) -> None:
        """Runs the app's action."""
        return self.action("stop")

    def stop_many(self, items: list[StopArgs]) -> list[dict[str, Any]]:
        """Runs `stop` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("stop", items)

    def enter_panic_mode(self) -> None:
        """Runs the app's action."""
        return self.action("enter-panic-mode")

    def enter_panic_mode_many(self, items: list[EnterPanicModeArgs]) -> list[dict[str, Any]]:
        """Runs `enter-panic-mode` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("enter-panic-mode", items)

    def exit_panic_mode(self) -> None:
        """Runs the app's action."""
        return self.action("exit-panic-mode")

    def exit_panic_mode_many(self, items: list[ExitPanicModeArgs]) -> list[dict[str, Any]]:
        """Runs `exit-panic-mode` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("exit-panic-mode", items)

    def delete_rule(self,
        *,
        zone: ZoneKind,
        rule: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("delete-rule", zone=zone, rule=rule)

    def delete_rule_many(self, items: list[DeleteRuleArgs]) -> list[dict[str, Any]]:
        """Runs `delete-rule` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-rule", items)

    def import_(self, *, config: Any) -> None:
        """Runs the app's action."""
        return self.action("import", config=config)

    def import_many(self, items: list[ImportArgs]) -> list[dict[str, Any]]:
        """Runs `import` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import", items)

    def read_blocked(self) -> None:
        """Runs the app's action."""
        return self.action("read-blocked")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"start", "start_many", "stop", "stop_many", "enter_panic_mode", "enter_panic_mode_many", "exit_panic_mode", "exit_panic_mode_many", "delete_rule", "delete_rule_many", "import_", "import_many", "read_blocked"})


class Applied(Resource):
    """Applied configuration — `firewall.applied`."""
    __surface__ = "so.tilo.firewall@d4e5e1b79ae77004"
    devices: dict[str, str]

    def applied(self,
        *,
        device: str,
        config: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("applied", device=device, config=config)

    def applied_many(self, items: list[AppliedArgs]) -> list[dict[str, Any]]:
        """Runs `applied` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("applied", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"applied", "applied_many"})


