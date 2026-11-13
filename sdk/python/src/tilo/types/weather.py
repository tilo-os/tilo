"""Types for Weather (`so.tilo.weather`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Locations` is the app's `weather.locations` at `weather/<resource>`, read with
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
    "Place",
    "PlaceDetails",
    "Locations",
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

class Locations(Resource):
    """Weather — `weather.locations`."""
    __surface__ = "so.tilo.weather@3150b872b91bc7e1"
    places: list[Place]
    default_place: str

    def save_location(self,
        *,
        place: PlaceDetails,
        make_default: bool | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["place"] = place
        if make_default is not None:
            args["make_default"] = make_default
        return self.action("save-location", **args)

    def use_as_default(self, *, place: str) -> Run | None:
        """Runs the app's action."""
        return self.action("use-as-default", place=place)

    def move_to_top(self, *, place: str) -> Run | None:
        """Runs the app's action."""
        return self.action("move-to-top", place=place)

    # the app's `remove` action is the SDK's own: run it with `action("remove")`.
    # the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
    def refresh_location(self, *, location: str) -> None:
        """Runs the app's action."""
        return self.action("refresh-location", location=location)

    def open_location(self, *, location: str) -> None:
        """Runs the app's action."""
        return self.action("open-location", location=location)


