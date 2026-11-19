"""Types for Authenticator (`so.tilo.totp`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Vault` is the app's `totp.vault` at `totp/<resource>`, read with
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
    "Algorithm",
    "StoredAccount",
    "Vault",
    "MoveUpArgs",
    "MoveDownArgs",
    "DeleteArgs",
]

Algorithm = Literal["Sha1", "Sha256", "Sha512"]

class StoredAccount(TypedDict):
    issuer: str
    label: str
    algorithm: Algorithm
    digits: int
    period: int
    color: int

MoveUpArgs = TypedDict("MoveUpArgs", {
    "account": "str",
})
"""The arguments of `move-up`."""

MoveDownArgs = TypedDict("MoveDownArgs", {
    "account": "str",
})
"""The arguments of `move-down`."""

DeleteArgs = TypedDict("DeleteArgs", {
    "account": "str",
})
"""The arguments of `delete`."""

class Vault(Resource):
    """Authenticator — `totp.vault`."""
    __surface__ = "so.tilo.totp@1beb8a5e2e56189e"
    accounts: list[Entry[StoredAccount]]

    def move_up(self, *, account: str) -> None:
        """Runs the app's action."""
        return self.action("move-up", account=account)

    def move_up_many(self, items: list[MoveUpArgs]) -> list[dict[str, Any]]:
        """Runs `move-up` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move-up", items)

    def move_down(self, *, account: str) -> None:
        """Runs the app's action."""
        return self.action("move-down", account=account)

    def move_down_many(self, items: list[MoveDownArgs]) -> list[dict[str, Any]]:
        """Runs `move-down` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move-down", items)

    def delete(self, *, account: str) -> None:
        """Runs the app's action."""
        return self.action("delete", account=account)

    def delete_many(self, items: list[DeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete", items)

    def add_account(self) -> None:
        """Runs the app's action."""
        return self.action("add-account")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"move_up", "move_up_many", "move_down", "move_down_many", "delete", "delete_many", "add_account"})


