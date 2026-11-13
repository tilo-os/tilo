"""Types for Authenticator (`so.tilo.totp`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Vault` is the app's `totp.vault` at `totp/<resource>`, read with
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
    "Algorithm",
    "StoredAccount",
    "Vault",
]

Algorithm = Literal["sha1", "sha256", "sha512"]

class StoredAccount(TypedDict):
    issuer: str
    label: str
    algorithm: Algorithm
    digits: int
    period: int
    color: int

class Vault(Resource):
    """Authenticator — `totp.vault`."""
    __surface__ = "so.tilo.totp@7557a43b95820399"
    accounts: list[StoredAccount]

    def move_up(self, *, account: str) -> Run | None:
        """Runs the app's action."""
        return self.action("move-up", account=account)

    def move_down(self, *, account: str) -> Run | None:
        """Runs the app's action."""
        return self.action("move-down", account=account)

    def delete(self, *, account: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete", account=account)

    def add_account(self) -> None:
        """Runs the app's action."""
        return self.action("add-account")

    def open_account(self, *, account: str) -> None:
        """Runs the app's action."""
        return self.action("open-account", account=account)

    def copy_code(self, *, account: str) -> None:
        """Runs the app's action."""
        return self.action("copy-code", account=account)

    def set_up_local(self, *, account: str) -> None:
        """Runs the app's action."""
        return self.action("set-up-local", account=account)


