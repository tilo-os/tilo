"""Types for Maps (`so.tilo.maps`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Workspace` is the app's `maps.workspace` at `maps/<resource>`, read with
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
    "Category",
    "LatLon",
    "Maneuver",
    "Place",
    "Route",
    "RouteStep",
    "TransitJourney",
    "TransitLeg",
    "TransitMode",
    "TravelMode",
    "DownloadedRegion",
    "Workspace",
    "OfflineAreas",
    "AcceptPlacesArgs",
    "CommitSearchArgs",
    "ClearRecentsArgs",
    "AcceptRouteArgs",
    "ClearRouteArgs",
    "AcceptTransitArgs",
    "ClearTransitArgs",
    "RecordArgs",
    "ForgetArgs",
    "ClearArgs",
]

Category = Literal["landmark", "cafe", "restaurant", "bar", "museum", "park", "transit", "hotel", "shop", "hospital", "other"]

class LatLon(TypedDict):
    lat: float
    lon: float

Maneuver = Literal["Depart", "Continue", "SlightLeft", "Left", "SharpLeft", "SlightRight", "Right", "SharpRight", "Uturn", "Roundabout", "Merge", "Arrive"]

class Place(TypedDict):
    id: str
    name: str
    category: Category
    address: str
    pos: LatLon
    saved: bool

Route = TypedDict("Route", {
    "mode": "TravelMode",
    "distance_m": "float",
    "duration_s": "float",
    "geometry": "list[LatLon]",
    "steps": "list[RouteStep]",
    "from": "LatLon",
    "to": "LatLon",
})

class RouteStep(TypedDict):
    instruction: str
    distance_m: float
    maneuver: Maneuver

class TransitJourney(TypedDict):
    dep: str
    arr: str
    duration_min: int
    transfers: int
    legs: list[TransitLeg]

class TransitLeg(TypedDict):
    mode: TransitMode
    line: str
    headsign: str
    from_name: str
    to_name: str
    dep: str
    arr: str
    duration_min: int
    num_stops: int
    geometry: list[LatLon]
    color: str | None
    text_color: str | None
    agency: str
    realtime: bool

TransitMode = Literal["Walk", "Bus", "Tram", "Subway", "Rail", "Ferry"]

TravelMode = Literal["car", "transit", "bike", "foot"]

class DownloadedRegion(TypedDict):
    id: str
    name: str
    min: LatLon
    max: LatLon
    z0: int
    z1: int
    tiles: int

AcceptPlacesArgs = TypedDict("AcceptPlacesArgs", {
    "places": "list[Place]",
})
"""The arguments of `accept-places`."""

CommitSearchArgs = TypedDict("CommitSearchArgs", {
    "query": "str",
})
"""The arguments of `commit-search`."""

ClearRecentsArgs = TypedDict("ClearRecentsArgs", {
})
"""The arguments of `clear-recents`."""

AcceptRouteArgs = TypedDict("AcceptRouteArgs", {
    "route": "Route",
    "origin": NotRequired["LatLon | None"],
    "origin_name": "str",
    "dest": NotRequired["LatLon | None"],
    "dest_name": "str",
    "mode": "TravelMode",
})
"""The arguments of `accept-route`."""

ClearRouteArgs = TypedDict("ClearRouteArgs", {
})
"""The arguments of `clear-route`."""

AcceptTransitArgs = TypedDict("AcceptTransitArgs", {
    "transit": "list[TransitJourney]",
    "origin": NotRequired["LatLon | None"],
    "origin_name": "str",
    "dest": NotRequired["LatLon | None"],
    "dest_name": "str",
})
"""The arguments of `accept-transit`."""

ClearTransitArgs = TypedDict("ClearTransitArgs", {
})
"""The arguments of `clear-transit`."""

RecordArgs = TypedDict("RecordArgs", {
    "device": "str",
    "region": "DownloadedRegion",
})
"""The arguments of `record`."""

ForgetArgs = TypedDict("ForgetArgs", {
    "device": "str",
    "area": "str",
})
"""The arguments of `forget`."""

ClearArgs = TypedDict("ClearArgs", {
    "device": "str",
})
"""The arguments of `clear`."""

class Workspace(Resource):
    """Maps — `maps.workspace`."""
    __surface__ = "so.tilo.maps@bff2580c0be19545"
    places: list[Entry[Place]]
    @property
    def recents(self) -> Any:
        """Read only."""
        return self.fields["recents"]
    mode: TravelMode
    transit_time: Any
    @property
    def route_plan(self) -> Any:
        """Read only."""
        return self.fields["route_plan"]
    @property
    def transit_plan(self) -> Any:
        """Read only."""
        return self.fields["transit_plan"]

    def accept_places(self, *, places: list[Place]) -> None:
        """Runs the app's action."""
        return self.action("accept-places", places=places)

    def accept_places_many(self, items: list[AcceptPlacesArgs]) -> list[dict[str, Any]]:
        """Runs `accept-places` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("accept-places", items)

    def commit_search(self, *, query: str) -> None:
        """Runs the app's action."""
        return self.action("commit-search", query=query)

    def commit_search_many(self, items: list[CommitSearchArgs]) -> list[dict[str, Any]]:
        """Runs `commit-search` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("commit-search", items)

    def clear_recents(self) -> None:
        """Runs the app's action."""
        return self.action("clear-recents")

    def clear_recents_many(self, items: list[ClearRecentsArgs]) -> list[dict[str, Any]]:
        """Runs `clear-recents` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("clear-recents", items)

    def accept_route(self,
        *,
        route: Route,
        origin_name: str,
        dest_name: str,
        mode: TravelMode,
        origin: LatLon | None = None,
        dest: LatLon | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["route"] = route
        if origin is not None:
            args["origin"] = origin
        args["origin_name"] = origin_name
        if dest is not None:
            args["dest"] = dest
        args["dest_name"] = dest_name
        args["mode"] = mode
        return self.action("accept-route", **args)

    def accept_route_many(self, items: list[AcceptRouteArgs]) -> list[dict[str, Any]]:
        """Runs `accept-route` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("accept-route", items)

    def clear_route(self) -> None:
        """Runs the app's action."""
        return self.action("clear-route")

    def clear_route_many(self, items: list[ClearRouteArgs]) -> list[dict[str, Any]]:
        """Runs `clear-route` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("clear-route", items)

    def accept_transit(self,
        *,
        transit: list[TransitJourney],
        origin_name: str,
        dest_name: str,
        origin: LatLon | None = None,
        dest: LatLon | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["transit"] = transit
        if origin is not None:
            args["origin"] = origin
        args["origin_name"] = origin_name
        if dest is not None:
            args["dest"] = dest
        args["dest_name"] = dest_name
        return self.action("accept-transit", **args)

    def accept_transit_many(self, items: list[AcceptTransitArgs]) -> list[dict[str, Any]]:
        """Runs `accept-transit` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("accept-transit", items)

    def clear_transit(self) -> None:
        """Runs the app's action."""
        return self.action("clear-transit")

    def clear_transit_many(self, items: list[ClearTransitArgs]) -> list[dict[str, Any]]:
        """Runs `clear-transit` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("clear-transit", items)

    def search(self, *, query: str) -> None:
        """Runs the app's action."""
        return self.action("search", query=query)

    def locate(self) -> None:
        """Runs the app's action."""
        return self.action("locate")

    def toggle_traffic(self) -> None:
        """Runs the app's action."""
        return self.action("toggle-traffic")

    def open_place(self, *, place: str) -> None:
        """Runs the app's action."""
        return self.action("open-place", place=place)

    def directions(self, *, place: str) -> None:
        """Runs the app's action."""
        return self.action("directions", place=place)

    def open_poi(self, *, poi: str) -> None:
        """Runs the app's action."""
        return self.action("open-poi", poi=poi)

    def poi_directions(self, *, poi: str) -> None:
        """Runs the app's action."""
        return self.action("poi-directions", poi=poi)

    def show_point(self,
        *,
        lat: float,
        lon: float,
    ) -> None:
        """Runs the app's action."""
        return self.action("show-point", lat=lat, lon=lon)

    def delete_offline_area(self, *, area: str) -> None:
        """Runs the app's action."""
        return self.action("delete-offline-area", area=area)

    def clear_offline_areas(self) -> None:
        """Runs the app's action."""
        return self.action("clear-offline-areas")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"accept_places", "accept_places_many", "commit_search", "commit_search_many", "clear_recents", "clear_recents_many", "accept_route", "accept_route_many", "clear_route", "clear_route_many", "accept_transit", "accept_transit_many", "clear_transit", "clear_transit_many", "search", "locate", "toggle_traffic", "open_place", "directions", "open_poi", "poi_directions", "show_point", "delete_offline_area", "clear_offline_areas"})


class OfflineAreas(Resource):
    """Offline areas — `maps.offline-areas`."""
    __surface__ = "so.tilo.maps@bff2580c0be19545"
    devices: dict[str, Any]

    def record(self,
        *,
        device: str,
        region: DownloadedRegion,
    ) -> None:
        """Runs the app's action."""
        return self.action("record", device=device, region=region)

    def record_many(self, items: list[RecordArgs]) -> list[dict[str, Any]]:
        """Runs `record` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("record", items)

    def forget(self,
        *,
        device: str,
        area: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("forget", device=device, area=area)

    def forget_many(self, items: list[ForgetArgs]) -> list[dict[str, Any]]:
        """Runs `forget` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("forget", items)

    def clear(self, *, device: str) -> None:
        """Runs the app's action."""
        return self.action("clear", device=device)

    def clear_many(self, items: list[ClearArgs]) -> list[dict[str, Any]]:
        """Runs `clear` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("clear", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"record", "record_many", "forget", "forget_many", "clear", "clear_many"})


