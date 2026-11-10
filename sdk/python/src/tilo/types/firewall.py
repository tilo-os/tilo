"""Types for Firewall (`so.tilo.firewall`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Config` is the app's `firewall.config` at `firewall/<resource>`, read with
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
    "Action",
    "Matcher",
    "Proto",
    "StoredRule",
    "StoredZone",
    "Target",
    "ZoneKind",
    "Config",
    "Applied",
]

Action = Literal["allow", "deny"]

Matcher = dict[str, Any]
"""One of service, port: an object whose case the app tags it with."""

Proto = Literal["tcp", "udp"]

class StoredRule(TypedDict):
    matcher: Matcher
    action: Action
    source: str
    enabled: bool

class StoredZone(TypedDict):
    target: Target
    interfaces: list[str]
    services: list[str]
    rules: list[StoredRule]
    masquerade: bool
    deleted_rules: list[str]

Target = Literal["default", "accept", "reject", "drop"]

ZoneKind = Literal["drop", "block", "public", "external", "dmz", "work", "home", "internal", "trusted"]

class Config(Resource):
    """Firewall — `firewall.config`."""
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

    def start(self) -> Run | None:
        """Runs the app's action."""
        return self.action("start")

    def stop(self) -> Run | None:
        """Runs the app's action."""
        return self.action("stop")

    def enter_panic_mode(self) -> Run | None:
        """Runs the app's action."""
        return self.action("enter-panic-mode")

    def exit_panic_mode(self) -> Run | None:
        """Runs the app's action."""
        return self.action("exit-panic-mode")

    def delete_rule(self,
        *,
        zone: ZoneKind,
        rule: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-rule", zone=zone, rule=rule)

    def import_(self, *, config: Any) -> Run | None:
        """Runs the app's action."""
        return self.action("import", config=config)

    def read_blocked(self) -> None:
        """Runs the app's action."""
        return self.action("read-blocked")


class Applied(Resource):
    """Applied configuration — `firewall.applied`."""
    devices: dict[str, str]

    def applied(self,
        *,
        device: str,
        config: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("applied", device=device, config=config)


