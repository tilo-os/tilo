"""Types for Poker (`so.tilo.poker`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `poker/<resource>`, read with
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
    "Move",
    "Category",
    "Street",
    "Notice",
    "InHand",
    "Award",
    "Hand",
    "Player",
    "Table",
    "Poker",
    "SitArgs",
    "LeaveArgs",
    "DealArgs",
    "AdvanceArgs",
    "CloseBetsArgs",
    "SettleHandArgs",
    "FoldArgs",
    "CallArgs",
    "RaiseArgs",
    "AllInArgs",
    "ShowArgs",
    "SkipArgs",
    "RebuyArgs",
]

Move = Literal["none", "small_blind", "big_blind", "check", "call", "bet", "raise", "all_in", "fold"]
"""What a player did last in the hand."""

Category = Literal["high_card", "pair", "two_pair", "trips", "straight", "flush", "full_house", "quads", "straight_flush"]
"""What a five-card hand is, weakest first: the order its worth compares in."""

Street = Literal["idle", "preflop", "flop", "turn", "river", "done"]
"""Where a hand has got to."""

SitArgs = TypedDict("SitArgs", {
    "seat": NotRequired["int | None"],
})
"""The arguments of `sit`."""

LeaveArgs = TypedDict("LeaveArgs", {
})
"""The arguments of `leave`."""

DealArgs = TypedDict("DealArgs", {
    "at": NotRequired["str | None"],
})
"""The arguments of `deal`."""

AdvanceArgs = TypedDict("AdvanceArgs", {
})
"""The arguments of `advance`."""

CloseBetsArgs = TypedDict("CloseBetsArgs", {
})
"""The arguments of `close_bets`."""

SettleHandArgs = TypedDict("SettleHandArgs", {
})
"""The arguments of `settle_hand`."""

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
"""The arguments of `all_in`."""

ShowArgs = TypedDict("ShowArgs", {
})
"""The arguments of `show`."""

SkipArgs = TypedDict("SkipArgs", {
    "at": NotRequired["str | None"],
})
"""The arguments of `skip`."""

RebuyArgs = TypedDict("RebuyArgs", {
})
"""The arguments of `rebuy`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.poker@819b4c26ac3cb9f9"
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


class InHand(Resource):
    """InHand — `tl.so.tilo.poker.InHand`."""
    __surface__ = "so.tilo.poker@819b4c26ac3cb9f9"
    lmmv5sj5: str  # player
    @property
    def _7qadgu4j(self) -> int:
        """seat (`7qadgu4j` on the wire)."""
        return self.fields["7qadgu4j"]

    @_7qadgu4j.setter
    def _7qadgu4j(self, value: int) -> None:
        self.edit("7qadgu4j", value=value)
    cstpy5cg: int  # stack
    f2j35cui: int  # bet
    @property
    def _62xrdt5x(self) -> int:
        """committed (`62xrdt5x` on the wire)."""
        return self.fields["62xrdt5x"]

    @_62xrdt5x.setter
    def _62xrdt5x(self, value: int) -> None:
        self.edit("62xrdt5x", value=value)
    upvuk57p: list[int]  # cards
    @property
    def _4jhijusz(self) -> bool:
        """folded (`4jhijusz` on the wire)."""
        return self.fields["4jhijusz"]

    @_4jhijusz.setter
    def _4jhijusz(self, value: bool) -> None:
        self.edit("4jhijusz", value=value)
    jt3hhmqs: bool  # acted
    zqefi644: bool  # shown
    bql52yxi: Move  # last


class Award(Resource):
    """Award — `tl.so.tilo.poker.Award`."""
    __surface__ = "so.tilo.poker@819b4c26ac3cb9f9"
    nu36auvm: str  # player
    ym5jj4ap: int  # amount
    jhe7cgkq: Category | None  # category


class Hand(Resource):
    """Hand — `tl.so.tilo.poker.Hand`."""
    __surface__ = "so.tilo.poker@819b4c26ac3cb9f9"
    eqwpzjbn: int  # number
    @property
    def _5s4ds2nn(self) -> Street:
        """street (`5s4ds2nn` on the wire)."""
        return self.fields["5s4ds2nn"]

    @_5s4ds2nn.setter
    def _5s4ds2nn(self, value: Street) -> None:
        self.edit("5s4ds2nn", value=value)
    @property
    def _6wlyoas5(self) -> list[Entry[InHand]]:
        """players (`6wlyoas5` on the wire)."""
        return self.fields["6wlyoas5"]

    @_6wlyoas5.setter
    def _6wlyoas5(self, value: list[Entry[InHand]]) -> None:
        self.edit("6wlyoas5", value=value)
    e5al5sgz: int  # button
    iferrh2h: list[int]  # deck
    @property
    def _4wccktra(self) -> int:
        """next (`4wccktra` on the wire)."""
        return self.fields["4wccktra"]

    @_4wccktra.setter
    def _4wccktra(self, value: int) -> None:
        self.edit("4wccktra", value=value)
    f73z3brr: list[int]  # board
    jvq7xgfb: int | None  # to_act
    ycyrisz2: int  # current_bet
    efpbnvul: int  # min_raise
    z7i5z7cq: str | None  # since
    @property
    def _4l6pd2lu(self) -> list[Entry[Award]]:
        """awards (`4l6pd2lu` on the wire)."""
        return self.fields["4l6pd2lu"]

    @_4l6pd2lu.setter
    def _4l6pd2lu(self, value: list[Entry[Award]]) -> None:
        self.edit("4l6pd2lu", value=value)


class Player(Resource):
    """Player — `tl.so.tilo.poker.Player`."""
    __surface__ = "so.tilo.poker@819b4c26ac3cb9f9"
    uq3kgvxc: str  # person
    noqmxnuc: int  # seat
    l3nts24i: int  # chips
    @property
    def _5d2doysh(self) -> str:
        """joined (`5d2doysh` on the wire)."""
        return self.fields["5d2doysh"]

    @_5d2doysh.setter
    def _5d2doysh(self, value: str) -> None:
        self.edit("5d2doysh", value=value)


class Table(Resource):
    """Table — `tl.so.tilo.poker.Table`."""
    __surface__ = "so.tilo.poker@819b4c26ac3cb9f9"
    waqmzca5: str  # title
    kbxb2nbt: list[Entry[Player]]  # players
    vemnbttj: Hand  # hand
    a4rb7zva: int  # button_seat

    def sit(self,
        *,
        seat: int | None = None,
    ) -> None:
        """Takes a seat at the table, with a buy-in of chips."""
        args: dict[str, Any] = {}
        if seat is not None:
            args["seat"] = seat
        return self.action("sit", **args)

    def sit_many(self, items: list[SitArgs]) -> list[dict[str, Any]]:
        """Runs `sit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("sit", items)

    def leave(self) -> None:
        """Leaves the table."""
        return self.action("leave")

    def leave_many(self, items: list[LeaveArgs]) -> list[dict[str, Any]]:
        """Runs `leave` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("leave", items)

    def deal(self,
        *,
        at: str | None = None,
    ) -> None:
        """Deals the next hand: shuffles, moves the button and posts the blinds."""
        args: dict[str, Any] = {}
        if at is not None:
            args["at"] = at
        return self.action("deal", **args)

    def deal_many(self, items: list[DealArgs]) -> list[dict[str, Any]]:
        """Runs `deal` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("deal", items)

    def advance(self) -> None:
        """Moves the hand on once its street's betting is done: the next player's turn, the next cards dealt, or the hand paid out. Every move calls it; a call of its own changes only what the table's rules have already decided, and refuses while a move is open."""
        return self.action("advance")

    def advance_many(self, items: list[AdvanceArgs]) -> list[dict[str, Any]]:
        """Runs `advance` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("advance", items)

    def close_bets(self) -> None:
        """Puts every player's bet back to zero for the next street: what closing a street's betting leaves behind."""
        return self.action("close_bets")

    def close_bets_many(self, items: list[CloseBetsArgs]) -> list[dict[str, Any]]:
        """Runs `close_bets` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("close_bets", items)

    def settle_hand(self) -> None:
        """Pays the pots and ends the hand. The rules of the table call it once the hand is decided; a call of its own pays what the showdown pays anyway and refuses while betting is open."""
        return self.action("settle_hand")

    def settle_hand_many(self, items: list[SettleHandArgs]) -> list[dict[str, Any]]:
        """Runs `settle_hand` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("settle_hand", items)

    def fold(self) -> None:
        """Folds your hand."""
        return self.action("fold")

    def fold_many(self, items: list[FoldArgs]) -> list[dict[str, Any]]:
        """Runs `fold` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("fold", items)

    def call(self) -> None:
        """Checks when nobody has bet, or calls the bet you face."""
        return self.action("call")

    def call_many(self, items: list[CallArgs]) -> list[dict[str, Any]]:
        """Runs `call` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("call", items)

    def raise_(self, *, to: float) -> None:
        """Bets or raises to a total for this street."""
        return self.action("raise", to=to)

    def raise_many(self, items: list[RaiseArgs]) -> list[dict[str, Any]]:
        """Runs `raise` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("raise", items)

    def all_in(self) -> None:
        """Puts all your chips in."""
        return self.action("all_in")

    def all_in_many(self, items: list[AllInArgs]) -> list[dict[str, Any]]:
        """Runs `all_in` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("all_in", items)

    def show(self) -> None:
        """Shows your cards to the table after the hand."""
        return self.action("show")

    def show_many(self, items: list[ShowArgs]) -> list[dict[str, Any]]:
        """Runs `show` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show", items)

    def skip(self,
        *,
        at: str | None = None,
    ) -> None:
        """Folds the hand of a player who has taken longer than their turn."""
        args: dict[str, Any] = {}
        if at is not None:
            args["at"] = at
        return self.action("skip", **args)

    def skip_many(self, items: list[SkipArgs]) -> list[dict[str, Any]]:
        """Runs `skip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("skip", items)

    def rebuy(self) -> None:
        """Buys back in once you have no chips."""
        return self.action("rebuy")

    def rebuy_many(self, items: list[RebuyArgs]) -> list[dict[str, Any]]:
        """Runs `rebuy` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rebuy", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"sit", "sit_many", "leave", "leave_many", "deal", "deal_many", "advance", "advance_many", "close_bets", "close_bets_many", "settle_hand", "settle_hand_many", "fold", "fold_many", "call", "call_many", "raise_", "raise_many", "all_in", "all_in_many", "show", "show_many", "skip", "skip_many", "rebuy", "rebuy_many"})


class Poker(Resource):
    """Poker — `tl.so.tilo.poker`."""
    __surface__ = "so.tilo.poker@819b4c26ac3cb9f9"

