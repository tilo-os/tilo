"""Types for Weather (`so.tilo.weather`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Locations` is the app's `weather.locations` at `weather/<resource>`, read with
`await ….load()`: its fields are the app's fields (read-only ones without a setter)
and its methods are its actions. A method answers what the app declares; an action
that takes longer answers a `Run`. A record is the dict the wire carries, and an
enum whose cases carry fields is an object whose tag the app's schema names. An
action whose name is one the class already has is run with `action("…")`, which
the file says where it is. An undoable action that answers nothing also has a
`<action>_many`, which applies it to every argument object of a list in a few
all-or-nothing steps, all of them one run.
"""

from __future__ import annotations

from typing import Any, Literal, NotRequired, TypedDict
from tilo import Entry, Resource, Run

__all__ = [
    "Place",
    "PlaceDetails",
    "Locations",
    "SaveLocationArgs",
    "UseAsDefaultArgs",
    "MoveToTopArgs",
]

class Place(TypedDict):
    name: str
    region: str
    country: str
    country_code: str
    latitude: float
    longitude: float
    timezone: str

class PlaceDetails(TypedDict):
    name: str
    country: str
    latitude: float
    longitude: float
    region: str | None
    country_code: str | None
    timezone: str | None

SaveLocationArgs = TypedDict("SaveLocationArgs", {
    "place": "PlaceDetails",
    "make_default": NotRequired["bool | None"],
})
"""The arguments of `save-location`."""

UseAsDefaultArgs = TypedDict("UseAsDefaultArgs", {
    "place": "str",
})
"""The arguments of `use-as-default`."""

MoveToTopArgs = TypedDict("MoveToTopArgs", {
    "place": "str",
})
"""The arguments of `move-to-top`."""

class Locations(Resource):
    """Weather — `weather.locations`."""
    __surface__ = "so.tilo.weather@3150b872b91bc7e1"
    places: list[Entry[Place]]
    default_place: str

    def save_location(self,
        *,
        place: PlaceDetails,
        make_default: bool | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["place"] = place
        if make_default is not None:
            args["make_default"] = make_default
        return self.action("save-location", **args)

    def save_location_many(self, items: list[SaveLocationArgs]) -> list[dict[str, Any]]:
        """Runs `save-location` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("save-location", items)

    def use_as_default(self, *, place: str) -> None:
        """Runs the app's action."""
        return self.action("use-as-default", place=place)

    def use_as_default_many(self, items: list[UseAsDefaultArgs]) -> list[dict[str, Any]]:
        """Runs `use-as-default` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("use-as-default", items)

    def move_to_top(self, *, place: str) -> None:
        """Runs the app's action."""
        return self.action("move-to-top", place=place)

    def move_to_top_many(self, items: list[MoveToTopArgs]) -> list[dict[str, Any]]:
        """Runs `move-to-top` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("move-to-top", items)

    # the app's `remove` action is the SDK's own: run it with `action("remove")`.
    # the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
    def refresh_location(self, *, location: str) -> None:
        """Runs the app's action."""
        return self.action("refresh-location", location=location)

    def open_location(self, *, location: str) -> None:
        """Runs the app's action."""
        return self.action("open-location", location=location)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"save_location", "save_location_many", "use_as_default", "use_as_default_many", "move_to_top", "move_to_top_many", "refresh_location", "open_location"})


