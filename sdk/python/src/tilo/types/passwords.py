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
    "WorkspaceVault",
    "KeepSealedArgs",
    "ImportSealedArgs",
    "RemoveSealedArgs",
    "KeepUsesArgs",
]

KeepSealedArgs = TypedDict("KeepSealedArgs", {
    "id": "str",
    "ciphertext": "str",
})
"""The arguments of `keep-sealed`."""

ImportSealedArgs = TypedDict("ImportSealedArgs", {
    "id": "str",
    "ciphertext": "str",
})
"""The arguments of `import-sealed`."""

RemoveSealedArgs = TypedDict("RemoveSealedArgs", {
    "id": "str",
})
"""The arguments of `remove-sealed`."""

KeepUsesArgs = TypedDict("KeepUsesArgs", {
    "id": "str",
    "ciphertext": "str",
})
"""The arguments of `keep-uses`."""

class Vault(Resource):
    """Password vault — `passwords.vault`."""
    __surface__ = "so.tilo.passwords@8ac1e6ef206532c5"
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
    __actions__ = frozenset({"add", "list_logins", "cancel", "copy", "copy_identity", "copy_origins", "reveal", "filter"})


class WorkspaceVault(Resource):
    """Workspace vault — `passwords.workspace-vault`."""
    __surface__ = "so.tilo.passwords@8ac1e6ef206532c5"
    @property
    def sealed(self) -> list[Entry[str]]:
        """Read only."""
        return self.fields["sealed"]
    @property
    def imported(self) -> list[Entry[str]]:
        """Read only."""
        return self.fields["imported"]
    @property
    def removed(self) -> list[str]:
        """Read only."""
        return self.fields["removed"]
    @property
    def used(self) -> list[Entry[str]]:
        """Read only."""
        return self.fields["used"]

    def keep_sealed(self,
        *,
        id: str,
        ciphertext: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("keep-sealed", id=id, ciphertext=ciphertext)

    def keep_sealed_many(self, items: list[KeepSealedArgs]) -> list[dict[str, Any]]:
        """Runs `keep-sealed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep-sealed", items)

    def import_sealed(self,
        *,
        id: str,
        ciphertext: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("import-sealed", id=id, ciphertext=ciphertext)

    def import_sealed_many(self, items: list[ImportSealedArgs]) -> list[dict[str, Any]]:
        """Runs `import-sealed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import-sealed", items)

    def remove_sealed(self, *, id: str) -> None:
        """Runs the app's action."""
        return self.action("remove-sealed", id=id)

    def remove_sealed_many(self, items: list[RemoveSealedArgs]) -> list[dict[str, Any]]:
        """Runs `remove-sealed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-sealed", items)

    def keep_uses(self,
        *,
        id: str,
        ciphertext: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("keep-uses", id=id, ciphertext=ciphertext)

    def keep_uses_many(self, items: list[KeepUsesArgs]) -> list[dict[str, Any]]:
        """Runs `keep-uses` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep-uses", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"keep_sealed", "keep_sealed_many", "import_sealed", "import_sealed_many", "remove_sealed", "remove_sealed_many", "keep_uses", "keep_uses_many"})


