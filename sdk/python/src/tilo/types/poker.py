"""Types for Poker (`so.tilo.poker`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Table` is the app's `so.tilo.poker.table` at `poker/<resource>`, read with
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
    "Award",
    "Hand",
    "InHand",
    "Move",
    "Player",
    "Street",
    "Table",
    "SitArgs",
    "LeaveArgs",
    "DealArgs",
    "FoldArgs",
    "CallArgs",
    "RaiseArgs",
    "AllInArgs",
    "ShowArgs",
    "SkipArgs",
    "RebuyArgs",
]

class Award(TypedDict):
    player: str
    amount: int
    category: str

class Hand(TypedDict):
    number: int
    street: Street
    players: list[InHand]
    button: int
    deck: list[int]
    board: list[int]
    to_act: int | None
    current_bet: int
    min_raise: int
    since_ms: int
    awards: list[Award]

class InHand(TypedDict):
    player: str
    seat: int
    stack: int
    bet: int
    committed: int
    cards: list[int]
    folded: bool
    acted: bool
    shown: bool
    last: Move

Move = Literal["None", "SmallBlind", "BigBlind", "Check", "Call", "Bet", "Raise", "AllIn", "Fold"]

class Player(TypedDict):
    seat: int
    chips: int
    joined_ms: int

Street = Literal["Idle", "Preflop", "Flop", "Turn", "River", "Done"]

SitArgs = TypedDict("SitArgs", {
    "seat": "int",
})
"""The arguments of `sit`."""

LeaveArgs = TypedDict("LeaveArgs", {
})
"""The arguments of `leave`."""

DealArgs = TypedDict("DealArgs", {
})
"""The arguments of `deal`."""

FoldArgs = TypedDict("FoldArgs", {
})
"""The arguments of `fold`."""

CallArgs = TypedDict("CallArgs", {
})
"""The arguments of `call`."""

RaiseArgs = TypedDict("RaiseArgs", {
    "to": "float",
})
"""The arguments of `raise`."""

AllInArgs = TypedDict("AllInArgs", {
})
"""The arguments of `all-in`."""

ShowArgs = TypedDict("ShowArgs", {
})
"""The arguments of `show`."""

SkipArgs = TypedDict("SkipArgs", {
})
"""The arguments of `skip`."""

RebuyArgs = TypedDict("RebuyArgs", {
})
"""The arguments of `rebuy`."""

class Table(Resource):
    """Poker table — `so.tilo.poker.table`."""
    __surface__ = "so.tilo.poker@846e8ee1be6c2b3b"
    players: dict[str, Player]
    hand: Hand
    button_seat: int

    def sit(self, *, seat: int) -> None:
        """Runs the app's action."""
        return self.action("sit", seat=seat)

    def sit_many(self, items: list[SitArgs]) -> list[dict[str, Any]]:
        """Runs `sit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("sit", items)

    def leave(self) -> None:
        """Runs the app's action."""
        return self.action("leave")

    def leave_many(self, items: list[LeaveArgs]) -> list[dict[str, Any]]:
        """Runs `leave` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("leave", items)

    def deal(self) -> None:
        """Runs the app's action."""
        return self.action("deal")

    def deal_many(self, items: list[DealArgs]) -> list[dict[str, Any]]:
        """Runs `deal` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("deal", items)

    def fold(self) -> None:
        """Runs the app's action."""
        return self.action("fold")

    def fold_many(self, items: list[FoldArgs]) -> list[dict[str, Any]]:
        """Runs `fold` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("fold", items)

    def call(self) -> None:
        """Runs the app's action."""
        return self.action("call")

    def call_many(self, items: list[CallArgs]) -> list[dict[str, Any]]:
        """Runs `call` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("call", items)

    def raise_(self, *, to: float) -> None:
        """Runs the app's action."""
        return self.action("raise", to=to)

    def raise_many(self, items: list[RaiseArgs]) -> list[dict[str, Any]]:
        """Runs `raise` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("raise", items)

    def all_in(self) -> None:
        """Runs the app's action."""
        return self.action("all-in")

    def all_in_many(self, items: list[AllInArgs]) -> list[dict[str, Any]]:
        """Runs `all-in` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("all-in", items)

    def show(self) -> None:
        """Runs the app's action."""
        return self.action("show")

    def show_many(self, items: list[ShowArgs]) -> list[dict[str, Any]]:
        """Runs `show` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show", items)

    def skip(self) -> None:
        """Runs the app's action."""
        return self.action("skip")

    def skip_many(self, items: list[SkipArgs]) -> list[dict[str, Any]]:
        """Runs `skip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("skip", items)

    def rebuy(self) -> None:
        """Runs the app's action."""
        return self.action("rebuy")

    def rebuy_many(self, items: list[RebuyArgs]) -> list[dict[str, Any]]:
        """Runs `rebuy` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rebuy", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"sit", "sit_many", "leave", "leave_many", "deal", "deal_many", "fold", "fold_many", "call", "call_many", "raise_", "raise_many", "all_in", "all_in_many", "show", "show_many", "skip", "skip_many", "rebuy", "rebuy_many"})


