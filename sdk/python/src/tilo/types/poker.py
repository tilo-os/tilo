"""Types for Poker (`so.tilo.poker`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Table` is the app's `so.tilo.poker.table` at `poker/<resource>`, read with
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
    "Table",
]

class Table(Resource):
    """Poker table — `so.tilo.poker.table`."""
    players: Any
    hand: Any
    button_seat: Any

    def sit(self, *, seat: int) -> Run | None:
        """Runs the app's action."""
        return self.action("sit", seat=seat)

    def leave(self) -> Run | None:
        """Runs the app's action."""
        return self.action("leave")

    def deal(self) -> Run | None:
        """Runs the app's action."""
        return self.action("deal")

    def fold(self) -> Run | None:
        """Runs the app's action."""
        return self.action("fold")

    def call(self) -> Run | None:
        """Runs the app's action."""
        return self.action("call")

    def raise_(self, *, to: float) -> Run | None:
        """Runs the app's action."""
        return self.action("raise", to=to)

    def all_in(self) -> Run | None:
        """Runs the app's action."""
        return self.action("all-in")

    def show(self) -> Run | None:
        """Runs the app's action."""
        return self.action("show")

    def skip(self) -> Run | None:
        """Runs the app's action."""
        return self.action("skip")

    def rebuy(self) -> Run | None:
        """Runs the app's action."""
        return self.action("rebuy")


