"""Types for Board (`so.tilo.lang.board`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Card` is the app's `tl.so.tilo.lang.board.Card` at `board/<resource>`, read with
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
    "Card",
    "Column",
    "Board",
    "LangBoard",
    "MoveToArgs",
    "MoveBeforeArgs",
    "DeleteCardArgs",
    "RenameColumnArgs",
]

MoveToArgs = TypedDict("MoveToArgs", {
    "col": "Column",
})
"""The arguments of `move_to`."""

MoveBeforeArgs = TypedDict("MoveBeforeArgs", {
    "card": "Card",
    "next": "Card",
})
"""The arguments of `move_before`."""

DeleteCardArgs = TypedDict("DeleteCardArgs", {
    "card": "Card",
})
"""The arguments of `delete_card`."""

RenameColumnArgs = TypedDict("RenameColumnArgs", {
    "title": "str",
})
"""The arguments of `rename_column`."""

class Card(Resource):
    """Card — `tl.so.tilo.lang.board.Card`."""
    __surface__ = "so.tilo.lang.board@e670a7b03c39eb42"
    acqkumvl: str  # title
    @property
    def _6hfpfyn7(self) -> str:
        """notes (`6hfpfyn7` on the wire)."""
        return self.fields["6hfpfyn7"]

    @_6hfpfyn7.setter
    def _6hfpfyn7(self, value: str) -> None:
        self.edit("6hfpfyn7", value=value)
    @property
    def _4oqrwuyr(self) -> Column:
        """column (`4oqrwuyr` on the wire)."""
        return self.fields["4oqrwuyr"]

    @_4oqrwuyr.setter
    def _4oqrwuyr(self, value: Column) -> None:
        self.edit("4oqrwuyr", value=value)

    def move_to(self, *, col: Column) -> None:
        """Moves a card to the foot of a column."""
        return self.action("move_to", col=col)

    def move_to_many(self, items: list[MoveToArgs]) -> list[dict[str, Any]]:
        """Runs `move_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_to", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"move_to", "move_to_many"})


class Column(Resource):
    """Column — `tl.so.tilo.lang.board.Column`."""
    __surface__ = "so.tilo.lang.board@e670a7b03c39eb42"
    @property
    def _3vw7cjrp(self) -> str:
        """title (`3vw7cjrp` on the wire)."""
        return self.fields["3vw7cjrp"]

    @_3vw7cjrp.setter
    def _3vw7cjrp(self, value: str) -> None:
        self.edit("3vw7cjrp", value=value)
    k24oebin: list[Entry[Card]]  # cards

    def new_card(self,
        *,
        title: str | None = None,
    ) -> Card:
        """Adds a card at the foot of a column, under the title typed in the entry that adds there: a card is named where it is made, not left 'Untitled'. An entry with nothing but space in it makes no card: it never runs this."""
        args: dict[str, Any] = {}
        if title is not None:
            args["title"] = title
        return self.action("new_card", **args)

    def move_before(self,
        *,
        card: Card,
        next: Card,
    ) -> None:
        """Moves a card to just above another, in that card's column."""
        return self.action("move_before", card=card, next=next)

    def move_before_many(self, items: list[MoveBeforeArgs]) -> list[dict[str, Any]]:
        """Runs `move_before` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_before", items)

    def delete_card(self, *, card: Card) -> None:
        """Deletes a card."""
        return self.action("delete_card", card=card)

    def delete_card_many(self, items: list[DeleteCardArgs]) -> list[dict[str, Any]]:
        """Runs `delete_card` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_card", items)

    def rename_column(self, *, title: str) -> None:
        """Renames a column."""
        return self.action("rename_column", title=title)

    def rename_column_many(self, items: list[RenameColumnArgs]) -> list[dict[str, Any]]:
        """Runs `rename_column` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename_column", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_card", "move_before", "move_before_many", "delete_card", "delete_card_many", "rename_column", "rename_column_many"})


class Board(Resource):
    """Board — `tl.so.tilo.lang.board.Board`."""
    __surface__ = "so.tilo.lang.board@e670a7b03c39eb42"
    hpfol33z: list[Entry[Column]]  # columns
    tuoyjxa6: list[Entry[Card]]  # cards

    def new_column(self) -> Column:
        """Adds a column at the right of the board."""
        return self.action("new_column")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_column"})


class LangBoard(Resource):
    """Board — `tl.so.tilo.lang.board`."""
    __surface__ = "so.tilo.lang.board@e670a7b03c39eb42"

