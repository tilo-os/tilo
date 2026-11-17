"""Types for Passwords (`so.tilo.passwords`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Vault` is the app's `passwords.vault` at `passwords/<resource>`, read with
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
    "Vault",
]

class Vault(Resource):
    """Password vault — `passwords.vault`."""
    __surface__ = "so.tilo.passwords@0de218d2d6575cb6"
    def deny_access(self, *, proposal: str) -> None:
        """Runs the app's action."""
        return self.action("deny-access", proposal=proposal)

    def add(self) -> None:
        """Runs the app's action."""
        return self.action("add")

    def list_logins(self) -> None:
        """Runs the app's action."""
        return self.action("list-logins")

    def cancel(self) -> None:
        """Runs the app's action."""
        return self.action("cancel")

    def copy(self, *, item: str) -> None:
        """Runs the app's action."""
        return self.action("copy", item=item)

    def copy_identity(self, *, item: str) -> None:
        """Runs the app's action."""
        return self.action("copy-identity", item=item)

    def copy_origins(self, *, item: str) -> None:
        """Runs the app's action."""
        return self.action("copy-origins", item=item)

    def reveal(self, *, item: str) -> None:
        """Runs the app's action."""
        return self.action("reveal", item=item)

    def filter(self, *, filter: str) -> None:
        """Runs the app's action."""
        return self.action("filter", filter=filter)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"deny_access", "add", "list_logins", "cancel", "copy", "copy_identity", "copy_origins", "reveal", "filter"})


