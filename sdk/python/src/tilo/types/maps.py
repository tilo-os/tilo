"""Types for Maps (`so.tilo.maps`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `maps/<resource>`, read with
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
    "Category",
    "Maneuver",
    "TravelMode",
    "TransitMode",
    "Notice",
    "Place",
    "Step",
    "Route",
    "Leg",
    "Journey",
    "TransitTime",
    "Poi",
    "Recent",
    "KeptArea",
    "KeptAreas",
    "SaveAreaAsk",
    "ForgetAreaAsk",
    "SaveStarted",
    "Saving",
    "Atlas",
    "Scale",
    "Places",
    "Routes",
    "Journeys",
    "Pois",
    "Maps",
    "SavePlaceArgs",
    "RenamePlaceArgs",
    "ForgetFoundArgs",
    "RememberArgs",
    "TakeInArgs",
    "AcceptPlacesArgs",
    "SearchArgs",
    "ClearSearchArgs",
    "ClearRecentsArgs",
    "OpenPlaceArgs",
    "ShowPointArgs",
    "LocateArgs",
    "ZoomMapArgs",
    "SetModeArgs",
    "SetTransitTimeArgs",
    "DirectionsArgs",
    "PlanTransitArgs",
    "AcceptRouteArgs",
    "AnotherRouteArgs",
    "FitMapArgs",
    "ClearRouteArgs",
    "AcceptTransitArgs",
    "AnotherJourneyArgs",
    "ClearTransitArgs",
    "StartNavigationArgs",
    "EndNavigationArgs",
    "FindNearbyArgs",
    "OpenPoiArgs",
    "DownloadViewArgs",
    "DownloadPlaceArgs",
    "KeepAreaArgs",
    "SaveAreaArgs",
    "AreaSavedArgs",
    "StopOfflineAreaArgs",
    "ContinueOfflineAreaArgs",
    "DeleteOfflineAreaArgs",
    "ClearOfflineAreasArgs",
]

Category = Literal["landmark", "cafe", "restaurant", "bar", "museum", "park", "transit", "hotel", "shop", "hospital", "other"]
"""What a place is: the marker's ink, the card's glyph, and the OpenStreetMap class it maps from."""

Maneuver = Literal["depart", "continue_on", "slight_left", "left", "sharp_left", "slight_right", "right", "sharp_right", "uturn", "roundabout", "merging", "arrive"]
"""A turn of a route."""

TravelMode = Literal["car", "transit", "bike", "foot"]
"""How directions travel."""

TransitMode = Literal["walk", "bus", "tram", "subway", "rail", "ferry"]
"""The way one leg of a public transport journey travels."""

SavePlaceArgs = TypedDict("SavePlaceArgs", {
    "saved": NotRequired["bool | None"],
})
"""The arguments of `save_place`."""

RenamePlaceArgs = TypedDict("RenamePlaceArgs", {
    "name": "str",
})
"""The arguments of `rename_place`."""

ForgetFoundArgs = TypedDict("ForgetFoundArgs", {
})
"""The arguments of `forget_found`."""

RememberArgs = TypedDict("RememberArgs", {
    "query": "str",
})
"""The arguments of `remember`."""

TakeInArgs = TypedDict("TakeInArgs", {
    "places": "list[Place]",
})
"""The arguments of `take_in`."""

AcceptPlacesArgs = TypedDict("AcceptPlacesArgs", {
    "places": "list[Place]",
})
"""The arguments of `accept_places`."""

SearchArgs = TypedDict("SearchArgs", {
    "query": "str",
})
"""The arguments of `search`."""

ClearSearchArgs = TypedDict("ClearSearchArgs", {
})
"""The arguments of `clear_search`."""

ClearRecentsArgs = TypedDict("ClearRecentsArgs", {
})
"""The arguments of `clear_recents`."""

OpenPlaceArgs = TypedDict("OpenPlaceArgs", {
    "place": "str",
})
"""The arguments of `open_place`."""

ShowPointArgs = TypedDict("ShowPointArgs", {
    "latitude": "float",
    "longitude": "float",
})
"""The arguments of `show_point`."""

LocateArgs = TypedDict("LocateArgs", {
})
"""The arguments of `locate`."""

ZoomMapArgs = TypedDict("ZoomMapArgs", {
    "direction": "float",
})
"""The arguments of `zoom_map`."""

SetModeArgs = TypedDict("SetModeArgs", {
    "mode": "TravelMode",
})
"""The arguments of `set_mode`."""

SetTransitTimeArgs = TypedDict("SetTransitTimeArgs", {
    "arrive_by": "bool",
    "offset_min": "int",
})
"""The arguments of `set_transit_time`."""

DirectionsArgs = TypedDict("DirectionsArgs", {
    "from_words": "str",
    "to_words": "str",
})
"""The arguments of `directions`."""

PlanTransitArgs = TypedDict("PlanTransitArgs", {
    "from": "Place",
    "to": "Place",
})
"""The arguments of `plan_transit`."""

AcceptRouteArgs = TypedDict("AcceptRouteArgs", {
    "routes": "list[Route]",
    "from_name": "str",
    "to_name": "str",
})
"""The arguments of `accept_route`."""

AnotherRouteArgs = TypedDict("AnotherRouteArgs", {
    "at": NotRequired["int | None"],
})
"""The arguments of `another_route`."""

FitMapArgs = TypedDict("FitMapArgs", {
})
"""The arguments of `fit_map`."""

ClearRouteArgs = TypedDict("ClearRouteArgs", {
})
"""The arguments of `clear_route`."""

AcceptTransitArgs = TypedDict("AcceptTransitArgs", {
    "journeys": "list[Journey]",
    "to_name": "str",
})
"""The arguments of `accept_transit`."""

AnotherJourneyArgs = TypedDict("AnotherJourneyArgs", {
    "at": NotRequired["int | None"],
})
"""The arguments of `another_journey`."""

ClearTransitArgs = TypedDict("ClearTransitArgs", {
})
"""The arguments of `clear_transit`."""

StartNavigationArgs = TypedDict("StartNavigationArgs", {
})
"""The arguments of `start_navigation`."""

EndNavigationArgs = TypedDict("EndNavigationArgs", {
})
"""The arguments of `end_navigation`."""

FindNearbyArgs = TypedDict("FindNearbyArgs", {
})
"""The arguments of `find_nearby`."""

OpenPoiArgs = TypedDict("OpenPoiArgs", {
    "poi": "str",
})
"""The arguments of `open_poi`."""

DownloadViewArgs = TypedDict("DownloadViewArgs", {
    "north": NotRequired["float | None"],
    "south": NotRequired["float | None"],
    "east": NotRequired["float | None"],
    "west": NotRequired["float | None"],
    "least_zoom": NotRequired["int | None"],
    "most_zoom": NotRequired["int | None"],
})
"""The arguments of `download_view`."""

DownloadPlaceArgs = TypedDict("DownloadPlaceArgs", {
    "query": "str",
    "least_zoom": NotRequired["int | None"],
    "most_zoom": NotRequired["int | None"],
})
"""The arguments of `download_place`."""

KeepAreaArgs = TypedDict("KeepAreaArgs", {
    "area": "KeptArea",
})
"""The arguments of `keep_area`."""

SaveAreaArgs = TypedDict("SaveAreaArgs", {
    "area": "KeptArea",
})
"""The arguments of `save_area`."""

AreaSavedArgs = TypedDict("AreaSavedArgs", {
    "area": "KeptArea",
    "outcome": "str",
})
"""The arguments of `area_saved`."""

StopOfflineAreaArgs = TypedDict("StopOfflineAreaArgs", {
    "area": "str",
})
"""The arguments of `stop_offline_area`."""

ContinueOfflineAreaArgs = TypedDict("ContinueOfflineAreaArgs", {
    "area": "str",
})
"""The arguments of `continue_offline_area`."""

DeleteOfflineAreaArgs = TypedDict("DeleteOfflineAreaArgs", {
    "area": "str",
})
"""The arguments of `delete_offline_area`."""

ClearOfflineAreasArgs = TypedDict("ClearOfflineAreasArgs", {
})
"""The arguments of `clear_offline_areas`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.maps@b5067cc77afeef57"
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


class Place(Resource):
    """Place — `tl.so.tilo.maps.Place`."""
    __surface__ = "so.tilo.maps@b5067cc77afeef57"
    rcalop4o: str  # name
    m5b55osu: str  # key
    lozw4dq7: Category  # category
    opf2vbzc: str  # address
    fiavc35r: float  # latitude
    w552wzze: float  # longitude
    s2ed7lsi: str  # phone
    wgimrb5u: str  # website
    qoo35pg5: bool  # saved
    sewsxq7v: bool  # found

    def save_place(self,
        *,
        saved: bool | None = None,
    ) -> None:
        """Keeps a place in the person's places, or takes it out."""
        args: dict[str, Any] = {}
        if saved is not None:
            args["saved"] = saved
        return self.action("save_place", **args)

    def save_place_many(self, items: list[SavePlaceArgs]) -> list[dict[str, Any]]:
        """Runs `save_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("save_place", items)

    def rename_place(self, *, name: str) -> None:
        """Renames a saved place; the name a search brought is the place's until the person gives it another."""
        return self.action("rename_place", name=name)

    def rename_place_many(self, items: list[RenamePlaceArgs]) -> list[dict[str, Any]]:
        """Runs `rename_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename_place", items)

    def call_place(self) -> None:
        """Calls the place's phone number, through the system's own open action."""
        return self.action("call_place")

    def open_site(self) -> None:
        """Opens the place's website in the app that opens web addresses."""
        return self.action("open_site")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"save_place", "save_place_many", "rename_place", "rename_place_many", "call_place", "open_site"})


class Step(Resource):
    """Step — `tl.so.tilo.maps.Step`."""
    __surface__ = "so.tilo.maps@b5067cc77afeef57"
    nfgqw3um: str  # instruction
    bvjnbo5z: float  # distance_m
    a4qv6woo: Maneuver  # maneuver


class Route(Resource):
    """Route — `tl.so.tilo.maps.Route`."""
    __surface__ = "so.tilo.maps@b5067cc77afeef57"
    ymsz4m34: TravelMode  # mode
    ty6bfuoo: float  # distance_m
    wc6ysmvx: float  # duration_s
    uyrheaim: list[float]  # geometry
    fyf6eckz: str  # from_name
    bwd4zoxi: str  # to_name
    yufxlbcl: float  # from_latitude
    ws5q4ft3: float  # from_longitude
    pepwxqg3: float  # to_latitude
    @property
    def _3lxyvouf(self) -> float:
        """to_longitude (`3lxyvouf` on the wire)."""
        return self.fields["3lxyvouf"]

    @_3lxyvouf.setter
    def _3lxyvouf(self, value: float) -> None:
        self.edit("3lxyvouf", value=value)
    ql4xlomv: list[Step]  # steps


class Leg(Resource):
    """Leg — `tl.so.tilo.maps.Leg`."""
    __surface__ = "so.tilo.maps@b5067cc77afeef57"
    @property
    def _3hgwl46g(self) -> TransitMode:
        """mode (`3hgwl46g` on the wire)."""
        return self.fields["3hgwl46g"]

    @_3hgwl46g.setter
    def _3hgwl46g(self, value: TransitMode) -> None:
        self.edit("3hgwl46g", value=value)
    oivvgw5v: str  # line
    dk4ne74p: str  # headsign
    sd7j2mow: str  # from_name
    zsmxifsc: str  # to_name
    @property
    def _4pplnxwl(self) -> str:
        """dep (`4pplnxwl` on the wire)."""
        return self.fields["4pplnxwl"]

    @_4pplnxwl.setter
    def _4pplnxwl(self, value: str) -> None:
        self.edit("4pplnxwl", value=value)
    a6ukygsr: str  # arr
    agzaego5: int  # duration_min
    ptjnyoqs: int  # num_stops
    i5zg4biq: str  # colour
    g4fcmpin: str  # text_colour
    rkrkjx3z: str  # agency
    @property
    def _7wrlvcuf(self) -> bool:
        """realtime (`7wrlvcuf` on the wire)."""
        return self.fields["7wrlvcuf"]

    @_7wrlvcuf.setter
    def _7wrlvcuf(self, value: bool) -> None:
        self.edit("7wrlvcuf", value=value)
    e6sm3pwz: list[float]  # geometry


class Journey(Resource):
    """Journey — `tl.so.tilo.maps.Journey`."""
    __surface__ = "so.tilo.maps@b5067cc77afeef57"
    xjeu7nif: str  # dep
    bhvk42wt: str  # arr
    sgpde2bq: int  # duration_min
    ixfei2g5: int  # transfers
    ica6larv: list[Leg]  # legs


class TransitTime(Resource):
    """TransitTime — `tl.so.tilo.maps.TransitTime`."""
    __surface__ = "so.tilo.maps@b5067cc77afeef57"
    bv3pklkc: bool  # arrive_by
    @property
    def _5hirpmkr(self) -> int:
        """offset_min (`5hirpmkr` on the wire)."""
        return self.fields["5hirpmkr"]

    @_5hirpmkr.setter
    def _5hirpmkr(self, value: int) -> None:
        self.edit("5hirpmkr", value=value)


class Poi(Resource):
    """Poi — `tl.so.tilo.maps.Poi`."""
    __surface__ = "so.tilo.maps@b5067cc77afeef57"
    kghw4vpb: str  # name
    osqgf2vf: str  # key
    ndtgscii: Category  # category
    ilnfptos: float  # latitude
    fdly24ym: float  # longitude
    pdpenkqy: str  # address
    vraa6lbe: str  # hours
    t3erfdxp: str  # website
    hxlh5wj4: str  # phone
    gdtnxbln: str  # kind


class Recent(Resource):
    """Recent — `tl.so.tilo.maps.Recent`."""
    __surface__ = "so.tilo.maps@b5067cc77afeef57"
    dozhur5v: str  # query
    eyiulpwh: str  # at


class KeptArea(Resource):
    """KeptArea — `tl.so.tilo.maps.KeptArea`."""
    __surface__ = "so.tilo.maps@b5067cc77afeef57"
    ibpdrk45: str  # name
    ukxlhhqi: str  # key
    mxxu653h: float  # north
    qniubcbe: float  # south
    yaeyddn2: float  # east
    z3kr346y: float  # west
    wygwldfb: int  # least_zoom
    @property
    def _3fo6bsu2(self) -> int:
        """most_zoom (`3fo6bsu2` on the wire)."""
        return self.fields["3fo6bsu2"]

    @_3fo6bsu2.setter
    def _3fo6bsu2(self, value: int) -> None:
        self.edit("3fo6bsu2", value=value)
    @property
    def _2gfef4uc(self) -> int:
        """tiles (`2gfef4uc` on the wire)."""
        return self.fields["2gfef4uc"]

    @_2gfef4uc.setter
    def _2gfef4uc(self, value: int) -> None:
        self.edit("2gfef4uc", value=value)
    ukhwybsm: int  # done
    x6eit2xc: bool  # stopped
    @property
    def _7s2dgers(self) -> str:
        """job (`7s2dgers` on the wire)."""
        return self.fields["7s2dgers"]

    @_7s2dgers.setter
    def _7s2dgers(self, value: str) -> None:
        self.edit("7s2dgers", value=value)


class KeptAreas(Resource):
    """KeptAreas — `tl.so.tilo.maps.KeptAreas`."""
    __surface__ = "so.tilo.maps@b5067cc77afeef57"
    id2odelq: list[Entry[KeptArea]]  # kept


class SaveAreaAsk(Resource):
    """SaveAreaAsk — `tl.so.tilo.maps.SaveAreaAsk`."""
    __surface__ = "so.tilo.maps@b5067cc77afeef57"
    uebjyxpv: str  # id
    bomeo3ju: str  # name
    @property
    def _3p6krsqd(self) -> float:
        """north (`3p6krsqd` on the wire)."""
        return self.fields["3p6krsqd"]

    @_3p6krsqd.setter
    def _3p6krsqd(self, value: float) -> None:
        self.edit("3p6krsqd", value=value)
    qa5rfbrz: float  # south
    kn7knr7e: float  # east
    tgv5i2z6: float  # west
    u4sjgx7w: int  # least_zoom
    xqoyoobw: int  # most_zoom
    z3a4l3gz: str  # basemap


class ForgetAreaAsk(Resource):
    """ForgetAreaAsk — `tl.so.tilo.maps.ForgetAreaAsk`."""
    __surface__ = "so.tilo.maps@b5067cc77afeef57"
    atq5mwsp: str  # id


class SaveStarted(Resource):
    """SaveStarted — `tl.so.tilo.maps.SaveStarted`."""
    __surface__ = "so.tilo.maps@b5067cc77afeef57"
    kix2eeda: str  # job


class Saving(Resource):
    """Saving — `tl.so.tilo.maps.Saving`."""
    __surface__ = "so.tilo.maps@b5067cc77afeef57"
    sbuvlugc: str  # state
    krb5phdh: int  # saved
    j6wtgiqo: int  # total
    nsfzbnns: str  # error


class Atlas(Resource):
    """Atlas — `tl.so.tilo.maps.Atlas`."""
    __surface__ = "so.tilo.maps@b5067cc77afeef57"
    q3ezxhmv: list[Entry[Place]]  # places
    vaus6gd3: list[Entry[Recent]]  # recents
    bdrkt5dv: list[Entry[Poi]]  # pois
    sxa3os5i: list[Entry[Route]]  # routes
    ia244ab6: int  # route_at
    xbpiwexi: list[Entry[Journey]]  # journeys
    aok7qtg7: int  # journey_at
    jzvaaswo: TransitTime  # transit_time
    ywuwhj6m: TravelMode  # mode
    @property
    def _7i6mpbfw(self) -> str:
        """query (`7i6mpbfw` on the wire)."""
        return self.fields["7i6mpbfw"]

    @_7i6mpbfw.setter
    def _7i6mpbfw(self, value: str) -> None:
        self.edit("7i6mpbfw", value=value)
    onljlflv: str  # status
    woyw2cxx: float  # centre_latitude
    zwui54r7: float  # centre_longitude
    @property
    def _4azkiaic(self) -> float:
        """centre_zoom (`4azkiaic` on the wire)."""
        return self.fields["4azkiaic"]

    @_4azkiaic.setter
    def _4azkiaic(self, value: float) -> None:
        self.edit("4azkiaic", value=value)
    d54rah5t: bool  # navigating

    def forget_found(self) -> None:
        """Marks every place a search found as no longer found, and forgets the ones nobody kept."""
        return self.action("forget_found")

    def forget_found_many(self, items: list[ForgetFoundArgs]) -> list[dict[str, Any]]:
        """Runs `forget_found` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget_found", items)

    def remember(self, *, query: str) -> None:
        """Remembers a search, newest first, at most eight."""
        return self.action("remember", query=query)

    def remember_many(self, items: list[RememberArgs]) -> list[dict[str, Any]]:
        """Runs `remember` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remember", items)

    def take_in(self, *, places: list[Place]) -> None:
        """Adds the places a search found to the map, keeping one the person already kept as theirs."""
        return self.action("take_in", places=places)

    def take_in_many(self, items: list[TakeInArgs]) -> list[dict[str, Any]]:
        """Runs `take_in` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("take_in", items)

    def accept_places(self, *, places: list[Place]) -> None:
        """Takes the places another app or an agent found into the map."""
        return self.action("accept_places", places=places)

    def accept_places_many(self, items: list[AcceptPlacesArgs]) -> list[dict[str, Any]]:
        """Runs `accept_places` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("accept_places", items)

    def search(self, *, query: str) -> None:
        """Searches the map for the places `query` names, and shows what it found."""
        return self.action("search", query=query)

    def search_many(self, items: list[SearchArgs]) -> list[dict[str, Any]]:
        """Runs `search` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("search", items)

    def clear_search(self) -> None:
        """Takes the last search's results away."""
        return self.action("clear_search")

    def clear_search_many(self, items: list[ClearSearchArgs]) -> list[dict[str, Any]]:
        """Runs `clear_search` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_search", items)

    def clear_recents(self) -> None:
        """Takes the searches the person made away."""
        return self.action("clear_recents")

    def clear_recents_many(self, items: list[ClearRecentsArgs]) -> list[dict[str, Any]]:
        """Runs `clear_recents` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_recents", items)

    def open_place(self, *, place: str) -> None:
        """Shows the place `place` on the map: the map opens on it."""
        return self.action("open_place", place=place)

    def open_place_many(self, items: list[OpenPlaceArgs]) -> list[dict[str, Any]]:
        """Runs `open_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open_place", items)

    def show_point(self,
        *,
        latitude: float,
        longitude: float,
    ) -> None:
        """Shows a point on the map, as a link or another app names it."""
        return self.action("show_point", latitude=latitude, longitude=longitude)

    def show_point_many(self, items: list[ShowPointArgs]) -> list[dict[str, Any]]:
        """Runs `show_point` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show_point", items)

    def locate(self) -> None:
        """Finds the device and shows where it is."""
        return self.action("locate")

    def locate_many(self, items: list[LocateArgs]) -> list[dict[str, Any]]:
        """Runs `locate` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("locate", items)

    def zoom_map(self, *, direction: float) -> None:
        """Zooms the map a level in or out: a level of imagery is the camera's zoom doubled or halved."""
        return self.action("zoom_map", direction=direction)

    def zoom_map_many(self, items: list[ZoomMapArgs]) -> list[dict[str, Any]]:
        """Runs `zoom_map` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("zoom_map", items)

    def set_mode(self, *, mode: TravelMode) -> None:
        """How the map's directions travel."""
        return self.action("set_mode", mode=mode)

    def set_mode_many(self, items: list[SetModeArgs]) -> list[dict[str, Any]]:
        """Runs `set_mode` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_mode", items)

    def set_transit_time(self,
        *,
        arrive_by: bool,
        offset_min: int,
    ) -> None:
        """When public transport journeys leave, or when they must arrive."""
        return self.action("set_transit_time", arrive_by=arrive_by, offset_min=offset_min)

    def set_transit_time_many(self, items: list[SetTransitTimeArgs]) -> list[dict[str, Any]]:
        """Runs `set_transit_time` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_transit_time", items)

    def directions(self,
        *,
        from_words: str,
        to_words: str,
    ) -> None:
        """Plans a route between two places the person named, by the mode directions travel, and shows it."""
        return self.action("directions", from_words=from_words, to_words=to_words)

    def directions_many(self, items: list[DirectionsArgs]) -> list[dict[str, Any]]:
        """Runs `directions` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("directions", items)

    def plan_transit(self,
        *,
        from_: Place,
        to: Place,
    ) -> None:
        """Finds the journeys public transport offers between two places."""
        args: dict[str, Any] = {}
        args["from"] = from_
        args["to"] = to
        return self.action("plan_transit", **args)

    def plan_transit_many(self, items: list[PlanTransitArgs]) -> list[dict[str, Any]]:
        """Runs `plan_transit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("plan_transit", items)

    def accept_route(self,
        *,
        routes: list[Route],
        from_name: str,
        to_name: str,
    ) -> None:
        """Takes the routes directions found as the map's, and shows the first."""
        return self.action("accept_route", routes=routes, from_name=from_name, to_name=to_name)

    def accept_route_many(self, items: list[AcceptRouteArgs]) -> list[dict[str, Any]]:
        """Runs `accept_route` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("accept_route", items)

    def another_route(self,
        *,
        at: int | None = None,
    ) -> None:
        """Shows the next route directions offered for the same journey."""
        args: dict[str, Any] = {}
        if at is not None:
            args["at"] = at
        return self.action("another_route", **args)

    def another_route_many(self, items: list[AnotherRouteArgs]) -> list[dict[str, Any]]:
        """Runs `another_route` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("another_route", items)

    def fit_map(self) -> None:
        """Shows everything the map holds: the route it draws, or every place it knows."""
        return self.action("fit_map")

    def fit_map_many(self, items: list[FitMapArgs]) -> list[dict[str, Any]]:
        """Runs `fit_map` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("fit_map", items)

    def clear_route(self) -> None:
        """Takes the route away."""
        return self.action("clear_route")

    def clear_route_many(self, items: list[ClearRouteArgs]) -> list[dict[str, Any]]:
        """Runs `clear_route` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_route", items)

    def accept_transit(self,
        *,
        journeys: list[Journey],
        to_name: str,
    ) -> None:
        """Takes the journeys public transport offered as the map's, and shows the first."""
        return self.action("accept_transit", journeys=journeys, to_name=to_name)

    def accept_transit_many(self, items: list[AcceptTransitArgs]) -> list[dict[str, Any]]:
        """Runs `accept_transit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("accept_transit", items)

    def another_journey(self,
        *,
        at: int | None = None,
    ) -> None:
        """Shows the next journey public transport offered."""
        args: dict[str, Any] = {}
        if at is not None:
            args["at"] = at
        return self.action("another_journey", **args)

    def another_journey_many(self, items: list[AnotherJourneyArgs]) -> list[dict[str, Any]]:
        """Runs `another_journey` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("another_journey", items)

    def clear_transit(self) -> None:
        """Takes the transit journeys away."""
        return self.action("clear_transit")

    def clear_transit_many(self, items: list[ClearTransitArgs]) -> list[dict[str, Any]]:
        """Runs `clear_transit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_transit", items)

    def start_navigation(self) -> None:
        """Follows the route the map draws: the live card says the turn ahead."""
        return self.action("start_navigation")

    def start_navigation_many(self, items: list[StartNavigationArgs]) -> list[dict[str, Any]]:
        """Runs `start_navigation` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("start_navigation", items)

    def end_navigation(self) -> None:
        """Stops following the route."""
        return self.action("end_navigation")

    def end_navigation_many(self, items: list[EndNavigationArgs]) -> list[dict[str, Any]]:
        """Runs `end_navigation` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("end_navigation", items)

    def find_nearby(self) -> None:
        """Shows what stands around the map, from OpenStreetMap."""
        return self.action("find_nearby")

    def find_nearby_many(self, items: list[FindNearbyArgs]) -> list[dict[str, Any]]:
        """Runs `find_nearby` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("find_nearby", items)

    def open_poi(self, *, poi: str) -> None:
        """Shows a point of interest the map found, and opens the map on it."""
        return self.action("open_poi", poi=poi)

    def open_poi_many(self, items: list[OpenPoiArgs]) -> list[dict[str, Any]]:
        """Runs `open_poi` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open_poi", items)

    def download_view(self,
        *,
        north: float | None = None,
        south: float | None = None,
        east: float | None = None,
        west: float | None = None,
        least_zoom: int | None = None,
        most_zoom: int | None = None,
    ) -> None:
        """Saves what the map shows for offline use: the box on screen and the zoom levels whose tiles it keeps. A caller names the box itself where it has one (a press, an agent): without one it is the place the map is centred on."""
        args: dict[str, Any] = {}
        if north is not None:
            args["north"] = north
        if south is not None:
            args["south"] = south
        if east is not None:
            args["east"] = east
        if west is not None:
            args["west"] = west
        if least_zoom is not None:
            args["least_zoom"] = least_zoom
        if most_zoom is not None:
            args["most_zoom"] = most_zoom
        return self.action("download_view", **args)

    def download_view_many(self, items: list[DownloadViewArgs]) -> list[dict[str, Any]]:
        """Runs `download_view` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("download_view", items)

    def download_place(self,
        *,
        query: str,
        least_zoom: int | None = None,
        most_zoom: int | None = None,
    ) -> None:
        """Saves the area around the place `query` names for offline use, with the zoom levels whose tiles it keeps."""
        args: dict[str, Any] = {}
        args["query"] = query
        if least_zoom is not None:
            args["least_zoom"] = least_zoom
        if most_zoom is not None:
            args["most_zoom"] = most_zoom
        return self.action("download_place", **args)

    def download_place_many(self, items: list[DownloadPlaceArgs]) -> list[dict[str, Any]]:
        """Runs `download_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("download_place", items)

    def keep_area(self, *, area: KeptArea) -> None:
        """Keeps `area` on this device, replacing an area of the same footprint or name, and starts its download."""
        return self.action("keep_area", area=area)

    def keep_area_many(self, items: list[KeepAreaArgs]) -> list[dict[str, Any]]:
        """Runs `keep_area` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep_area", items)

    def save_area(self, *, area: KeptArea) -> None:
        """Starts keeping `area`'s tiles, as a job whose record says how far it has come. A tile already kept is not fetched again, so this also continues a stopped area."""
        return self.action("save_area", area=area)

    def save_area_many(self, items: list[SaveAreaArgs]) -> list[dict[str, Any]]:
        """Runs `save_area` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("save_area", items)

    def area_saved(self,
        *,
        area: KeptArea,
        outcome: str,
    ) -> None:
        """What an area's download ended with: what it kept, or that the person stopped it."""
        return self.action("area_saved", area=area, outcome=outcome)

    def area_saved_many(self, items: list[AreaSavedArgs]) -> list[dict[str, Any]]:
        """Runs `area_saved` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("area_saved", items)

    def stop_offline_area(self, *, area: str) -> None:
        """Stops an area's download: the tiles kept so far stay available offline."""
        return self.action("stop_offline_area", area=area)

    def stop_offline_area_many(self, items: list[StopOfflineAreaArgs]) -> list[dict[str, Any]]:
        """Runs `stop_offline_area` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("stop_offline_area", items)

    def continue_offline_area(self, *, area: str) -> None:
        """Starts a stopped area's download again, keeping what it already has."""
        return self.action("continue_offline_area", area=area)

    def continue_offline_area_many(self, items: list[ContinueOfflineAreaArgs]) -> list[dict[str, Any]]:
        """Runs `continue_offline_area` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("continue_offline_area", items)

    def delete_offline_area(self, *, area: str) -> None:
        """Takes an area this device saved away, and the tiles of it no other kept area needs."""
        return self.action("delete_offline_area", area=area)

    def delete_offline_area_many(self, items: list[DeleteOfflineAreaArgs]) -> list[dict[str, Any]]:
        """Runs `delete_offline_area` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_offline_area", items)

    def clear_offline_areas(self) -> None:
        """Takes every area this device saved away."""
        return self.action("clear_offline_areas")

    def clear_offline_areas_many(self, items: list[ClearOfflineAreasArgs]) -> list[dict[str, Any]]:
        """Runs `clear_offline_areas` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_offline_areas", items)

    def open_link(self, *, link: str) -> Any:
        """Opens a `geo:` link: its search (`geo:0,0?q=cafe`), else its point (`geo:52.52,13.40`)."""
        return self.action("open_link", link=link)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"forget_found", "forget_found_many", "remember", "remember_many", "take_in", "take_in_many", "accept_places", "accept_places_many", "search", "search_many", "clear_search", "clear_search_many", "clear_recents", "clear_recents_many", "open_place", "open_place_many", "show_point", "show_point_many", "locate", "locate_many", "zoom_map", "zoom_map_many", "set_mode", "set_mode_many", "set_transit_time", "set_transit_time_many", "directions", "directions_many", "plan_transit", "plan_transit_many", "accept_route", "accept_route_many", "another_route", "another_route_many", "fit_map", "fit_map_many", "clear_route", "clear_route_many", "accept_transit", "accept_transit_many", "another_journey", "another_journey_many", "clear_transit", "clear_transit_many", "start_navigation", "start_navigation_many", "end_navigation", "end_navigation_many", "find_nearby", "find_nearby_many", "open_poi", "open_poi_many", "download_view", "download_view_many", "download_place", "download_place_many", "keep_area", "keep_area_many", "save_area", "save_area_many", "area_saved", "area_saved_many", "stop_offline_area", "stop_offline_area_many", "continue_offline_area", "continue_offline_area_many", "delete_offline_area", "delete_offline_area_many", "clear_offline_areas", "clear_offline_areas_many", "open_link"})


class Scale(Resource):
    """Scale — `tl.so.tilo.maps.Scale`."""
    __surface__ = "so.tilo.maps@b5067cc77afeef57"
    izgrst56: str  # label
    o2aq6igf: float  # pixels


class Places(Resource):
    """Places — `tl.so.tilo.maps.Places`."""
    __surface__ = "so.tilo.maps@b5067cc77afeef57"
    @property
    def _734zo2xo(self) -> list[Place]:
        """found (`734zo2xo` on the wire)."""
        return self.fields["734zo2xo"]

    @_734zo2xo.setter
    def _734zo2xo(self, value: list[Place]) -> None:
        self.edit("734zo2xo", value=value)
    @property
    def _73qevr6q(self) -> str:
        """why (`73qevr6q` on the wire)."""
        return self.fields["73qevr6q"]

    @_73qevr6q.setter
    def _73qevr6q(self, value: str) -> None:
        self.edit("73qevr6q", value=value)


class Routes(Resource):
    """Routes — `tl.so.tilo.maps.Routes`."""
    __surface__ = "so.tilo.maps@b5067cc77afeef57"
    q2tnydqa: list[Route]  # found
    kmhzghib: str  # why


class Journeys(Resource):
    """Journeys — `tl.so.tilo.maps.Journeys`."""
    __surface__ = "so.tilo.maps@b5067cc77afeef57"
    ix3p2v56: list[Journey]  # found
    cddmwu37: str  # why


class Pois(Resource):
    """Pois — `tl.so.tilo.maps.Pois`."""
    __surface__ = "so.tilo.maps@b5067cc77afeef57"
    fsnuwhad: list[Poi]  # found
    @property
    def _3yeey62c(self) -> str:
        """why (`3yeey62c` on the wire)."""
        return self.fields["3yeey62c"]

    @_3yeey62c.setter
    def _3yeey62c(self, value: str) -> None:
        self.edit("3yeey62c", value=value)


class Maps(Resource):
    """Maps — `tl.so.tilo.maps`."""
    __surface__ = "so.tilo.maps@b5067cc77afeef57"

