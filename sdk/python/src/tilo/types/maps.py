"""Types for Maps (`so.tilo.maps`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Workspace` is the app's `maps.workspace` at `maps/<resource>`, read with
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

class Workspace(Resource):
    """Maps — `maps.workspace`."""
    places: list[Place]
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

    def accept_places(self, *, places: list[Place]) -> Run | None:
        """Runs the app's action."""
        return self.action("accept-places", places=places)

    def commit_search(self, *, query: str) -> Run | None:
        """Runs the app's action."""
        return self.action("commit-search", query=query)

    def clear_recents(self) -> Run | None:
        """Runs the app's action."""
        return self.action("clear-recents")

    def accept_route(self,
        *,
        route: Route,
        origin_name: str,
        dest_name: str,
        mode: TravelMode,
        origin: LatLon | None = None,
        dest: LatLon | None = None,
    ) -> Run | None:
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

    def clear_route(self) -> Run | None:
        """Runs the app's action."""
        return self.action("clear-route")

    def accept_transit(self,
        *,
        transit: list[TransitJourney],
        origin_name: str,
        dest_name: str,
        origin: LatLon | None = None,
        dest: LatLon | None = None,
    ) -> Run | None:
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

    def clear_transit(self) -> Run | None:
        """Runs the app's action."""
        return self.action("clear-transit")

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

    def delete_offline_area(self, *, area: str) -> None:
        """Runs the app's action."""
        return self.action("delete-offline-area", area=area)

    def clear_offline_areas(self) -> None:
        """Runs the app's action."""
        return self.action("clear-offline-areas")


class OfflineAreas(Resource):
    """Offline areas — `maps.offline-areas`."""
    devices: dict[str, Any]

    def record(self,
        *,
        device: str,
        region: DownloadedRegion,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("record", device=device, region=region)

    def forget(self,
        *,
        device: str,
        area: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("forget", device=device, area=area)

    def clear(self, *, device: str) -> Run | None:
        """Runs the app's action."""
        return self.action("clear", device=device)


