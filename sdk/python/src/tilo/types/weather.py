"""Types for Weather (`so.tilo.weather`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `weather/<resource>`, read with
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
    "Notice",
    "Hour",
    "Day",
    "Reading",
    "Air",
    "Place",
    "PlaceReading",
    "Refresh",
    "Found",
    "Searching",
    "Locations",
    "Readings",
    "Weather",
    "UseAsDefaultArgs",
    "MoveToTopArgs",
    "MovePlaceArgs",
    "RenamePlaceArgs",
    "RemovePlaceArgs",
    "StopRefreshArgs",
    "RetryPlaceArgs",
    "ReadNextArgs",
    "PollWeatherArgs",
    "LookForPlaceArgs",
    "EndSearchArgs",
    "AcceptFoundArgs",
]

UseAsDefaultArgs = TypedDict("UseAsDefaultArgs", {
    "place": "Place",
})
"""The arguments of `use_as_default`."""

MoveToTopArgs = TypedDict("MoveToTopArgs", {
    "place": "Place",
})
"""The arguments of `move_to_top`."""

MovePlaceArgs = TypedDict("MovePlaceArgs", {
    "ids": "list[str]",
})
"""The arguments of `move_place`."""

RenamePlaceArgs = TypedDict("RenamePlaceArgs", {
    "place": "Place",
    "name": "str",
})
"""The arguments of `rename_place`."""

RemovePlaceArgs = TypedDict("RemovePlaceArgs", {
    "place": "Place",
})
"""The arguments of `remove_place`."""

StopRefreshArgs = TypedDict("StopRefreshArgs", {
})
"""The arguments of `stop_refresh`."""

RetryPlaceArgs = TypedDict("RetryPlaceArgs", {
    "place": "Place",
})
"""The arguments of `retry_place`."""

ReadNextArgs = TypedDict("ReadNextArgs", {
})
"""The arguments of `read_next`."""

PollWeatherArgs = TypedDict("PollWeatherArgs", {
})
"""The arguments of `poll_weather`."""

LookForPlaceArgs = TypedDict("LookForPlaceArgs", {
})
"""The arguments of `look_for_place`."""

EndSearchArgs = TypedDict("EndSearchArgs", {
    "reader": "Searching",
})
"""The arguments of `end_search`."""

AcceptFoundArgs = TypedDict("AcceptFoundArgs", {
    "reader": "Searching",
    "choice": "str",
})
"""The arguments of `accept_found`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.weather@bcadefef8e611e18"
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


class Hour(Resource):
    """Hour — `tl.so.tilo.weather.Hour`."""
    __surface__ = "so.tilo.weather@bcadefef8e611e18"
    hqg5l743: str  # at
    nz766xgs: float  # temperature
    bszivu27: float  # chance
    w4hqkb3u: int  # code
    rx2vbln3: bool  # day
    il4besep: str  # icon


class Day(Resource):
    """Day — `tl.so.tilo.weather.Day`."""
    __surface__ = "so.tilo.weather@bcadefef8e611e18"
    jpx34w56: str  # date
    @property
    def _7yqjq2lc(self) -> int:
        """weekday (`7yqjq2lc` on the wire)."""
        return self.fields["7yqjq2lc"]

    @_7yqjq2lc.setter
    def _7yqjq2lc(self, value: int) -> None:
        self.edit("7yqjq2lc", value=value)
    dyfvgxu3: float  # high
    hgvg5w2j: float  # low
    nle53w7c: float  # chance
    @property
    def _6nrssehp(self) -> int:
        """code (`6nrssehp` on the wire)."""
        return self.fields["6nrssehp"]

    @_6nrssehp.setter
    def _6nrssehp(self, value: int) -> None:
        self.edit("6nrssehp", value=value)
    og6bqxmv: float  # wind
    ttyi3z7y: float  # uv
    zxdugvp7: str  # rise
    f2qdxlbk: str  # set
    scsrpy5m: str  # icon


class Reading(Resource):
    """Reading — `tl.so.tilo.weather.Reading`."""
    __surface__ = "so.tilo.weather@bcadefef8e611e18"
    wz5kgppi: str  # at
    nfn3o6kh: float  # temperature
    npx3rbww: float  # apparent
    adkhdcmi: float  # humidity
    z25jhm5f: float  # precipitation
    pexond7v: int  # code
    zdiyx5q6: float  # wind
    tcofpdw2: float  # wind_from
    ulscmhjp: float  # pressure
    h2zd2igi: float  # uv
    hilw2sek: bool  # day
    @property
    def _3imkwm63(self) -> str:
        """icon (`3imkwm63` on the wire)."""
        return self.fields["3imkwm63"]

    @_3imkwm63.setter
    def _3imkwm63(self, value: str) -> None:
        self.edit("3imkwm63", value=value)


class Air(Resource):
    """Air — `tl.so.tilo.weather.Air`."""
    __surface__ = "so.tilo.weather@bcadefef8e611e18"
    qhxusqd7: str  # at
    b43at2ai: float | None  # european
    rwv4mbrr: float | None  # us
    fpf3tzoo: float | None  # pm2_5
    we4qtcij: float | None  # pm10
    vjey5w7m: float | None  # no2
    @property
    def _5pnfxgbq(self) -> float | None:
        """ozone (`5pnfxgbq` on the wire)."""
        return self.fields["5pnfxgbq"]

    @_5pnfxgbq.setter
    def _5pnfxgbq(self, value: float | None) -> None:
        self.edit("5pnfxgbq", value=value)


class Place(Resource):
    """Place — `tl.so.tilo.weather.Place`."""
    __surface__ = "so.tilo.weather@bcadefef8e611e18"
    bacjedt3: str  # name
    aohkzfz3: str  # region
    j6rsr6zo: str  # country
    @property
    def _26l3gl47(self) -> str:
        """country_code (`26l3gl47` on the wire)."""
        return self.fields["26l3gl47"]

    @_26l3gl47.setter
    def _26l3gl47(self, value: str) -> None:
        self.edit("26l3gl47", value=value)
    ax54oez6: float  # latitude
    ewmohhie: float  # longitude
    y5lbxwn7: str  # timezone


class PlaceReading(Resource):
    """PlaceReading — `tl.so.tilo.weather.PlaceReading`."""
    __surface__ = "so.tilo.weather@bcadefef8e611e18"
    uzxnvv67: str  # name
    hfv2phzu: str  # place
    kgqshwck: bool  # read
    eecg6i33: Reading  # current
    yc3wt5yt: list[Entry[Hour]]  # hours
    @property
    def _7xtuywip(self) -> list[Entry[Day]]:
        """days (`7xtuywip` on the wire)."""
        return self.fields["7xtuywip"]

    @_7xtuywip.setter
    def _7xtuywip(self, value: list[Entry[Day]]) -> None:
        self.edit("7xtuywip", value=value)
    @property
    def _7eu3357v(self) -> Air:
        """air (`7eu3357v` on the wire)."""
        return self.fields["7eu3357v"]

    @_7eu3357v.setter
    def _7eu3357v(self, value: Air) -> None:
        self.edit("7eu3357v", value=value)
    gqv7rwej: bool  # has_air
    caagd74f: str | None  # fetched
    juxtrvon: bool  # stale
    losawnjy: str  # trouble

    def apply_forecast(self, *, body: str) -> bool:
        """The forecast a service answered, written into this device's reading of the place: its current reading, a day of hours and a week of days. False when the body is not a forecast the service could have sent."""
        return self.action("apply_forecast", body=body)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"apply_forecast"})


class Refresh(Resource):
    """Refresh — `tl.so.tilo.weather.Refresh`."""
    __surface__ = "so.tilo.weather@bcadefef8e611e18"
    @property
    def _6a665z7q(self) -> str:
        """state (`6a665z7q` on the wire)."""
        return self.fields["6a665z7q"]

    @_6a665z7q.setter
    def _6a665z7q(self, value: str) -> None:
        self.edit("6a665z7q", value=value)
    vawmqqsv: list[str]  # waiting
    umztfj5e: int  # done
    @property
    def _6an5syvf(self) -> int:
        """total (`6an5syvf` on the wire)."""
        return self.fields["6an5syvf"]

    @_6an5syvf.setter
    def _6an5syvf(self, value: int) -> None:
        self.edit("6an5syvf", value=value)
    lrbilhih: int  # failed
    bf4xzg4n: str  # note


class Found(Resource):
    """Found — `tl.so.tilo.weather.Found`."""
    __surface__ = "so.tilo.weather@bcadefef8e611e18"
    mgz6jurw: str  # name
    @property
    def _6jas4neh(self) -> float:
        """latitude (`6jas4neh` on the wire)."""
        return self.fields["6jas4neh"]

    @_6jas4neh.setter
    def _6jas4neh(self, value: float) -> None:
        self.edit("6jas4neh", value=value)
    gvppngv7: float  # longitude
    kx6bm63e: str  # country
    ubamq752: str  # country_code
    a2wm7g5t: str  # region
    @property
    def _5hvczxlz(self) -> str:
        """timezone (`5hvczxlz` on the wire)."""
        return self.fields["5hvczxlz"]

    @_5hvczxlz.setter
    def _5hvczxlz(self, value: str) -> None:
        self.edit("5hvczxlz", value=value)


class Searching(Resource):
    """Searching — `tl.so.tilo.weather.Searching`."""
    __surface__ = "so.tilo.weather@bcadefef8e611e18"
    fmhvvp6l: str  # wanted
    o4whnz6t: str  # asked
    jwbxovvp: str  # pending
    @property
    def _5o5rohu5(self) -> list[Entry[Found]]:
        """found (`5o5rohu5` on the wire)."""
        return self.fields["5o5rohu5"]

    @_5o5rohu5.setter
    def _5o5rohu5(self, value: list[Entry[Found]]) -> None:
        self.edit("5o5rohu5", value=value)
    wgepvfwu: str  # note


class Locations(Resource):
    """Locations — `tl.so.tilo.weather.Locations`."""
    __surface__ = "so.tilo.weather@bcadefef8e611e18"
    b2qn6gqw: list[Entry[Place]]  # places
    vqfze2cq: Place | None  # default
    ms5dkio6: list[Entry[Searching]]  # searches
    @property
    def _73zdj7wy(self) -> str:
        """today (`73zdj7wy` on the wire)."""
        return self.fields["73zdj7wy"]

    @_73zdj7wy.setter
    def _73zdj7wy(self, value: str) -> None:
        self.edit("73zdj7wy", value=value)
    rusdri25: str  # tomorrow

    def save_place(self,
        *,
        name: str,
        latitude: float,
        longitude: float,
        region: str | None = None,
        country: str | None = None,
        country_code: str | None = None,
        timezone: str | None = None,
    ) -> Place:
        """Saves a place a person follows. A place near one already saved is that place, not a second entry, and a new place is the one windows show first."""
        args: dict[str, Any] = {}
        args["name"] = name
        args["latitude"] = latitude
        args["longitude"] = longitude
        if region is not None:
            args["region"] = region
        if country is not None:
            args["country"] = country
        if country_code is not None:
            args["country_code"] = country_code
        if timezone is not None:
            args["timezone"] = timezone
        return self.action("save_place", **args)

    def use_as_default(self, *, place: Place) -> None:
        """Shows a place's forecast first whenever the app opens."""
        return self.action("use_as_default", place=place)

    def use_as_default_many(self, items: list[UseAsDefaultArgs]) -> list[dict[str, Any]]:
        """Runs `use_as_default` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("use_as_default", items)

    def move_to_top(self, *, place: Place) -> None:
        """Moves a place to the top of the list."""
        return self.action("move_to_top", place=place)

    def move_to_top_many(self, items: list[MoveToTopArgs]) -> list[dict[str, Any]]:
        """Runs `move_to_top` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_to_top", items)

    def move_place(self, *, ids: list[str]) -> None:
        """Moves the place a person dragged, before the one it was dropped on: the list's reorder reports the two ids in that order."""
        return self.action("move_place", ids=ids)

    def move_place_many(self, items: list[MovePlaceArgs]) -> list[dict[str, Any]]:
        """Runs `move_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_place", items)

    def rename_place(self,
        *,
        place: Place,
        name: str,
    ) -> None:
        """Renames a place the person follows."""
        return self.action("rename_place", place=place, name=name)

    def rename_place_many(self, items: list[RenamePlaceArgs]) -> list[dict[str, Any]]:
        """Runs `rename_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename_place", items)

    def remove_place(self, *, place: Place) -> None:
        """Removes a place from the list; what was read of it goes with it. Undo brings it back."""
        return self.action("remove_place", place=place)

    def remove_place_many(self, items: list[RemovePlaceArgs]) -> list[dict[str, Any]]:
        """Runs `remove_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_place", items)

    # the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
    def stop_refresh(self) -> None:
        """Stops an update that is running; the places it had not read keep what they had."""
        return self.action("stop_refresh")

    def stop_refresh_many(self, items: list[StopRefreshArgs]) -> list[dict[str, Any]]:
        """Runs `stop_refresh` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("stop_refresh", items)

    def retry_place(self, *, place: Place) -> None:
        """Updates a place whose last read failed, once the person asks again."""
        return self.action("retry_place", place=place)

    def retry_place_many(self, items: list[RetryPlaceArgs]) -> list[dict[str, Any]]:
        """Runs `retry_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("retry_place", items)

    def read_next(self) -> None:
        """Reads one place an update has still to read, and says when the update has read them all."""
        return self.action("read_next")

    def read_next_many(self, items: list[ReadNextArgs]) -> list[dict[str, Any]]:
        """Runs `read_next` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("read_next", items)

    def poll_weather(self) -> None:
        """Takes what the last read found: a place's reading and its hours and days land together, and the search's rows beside the field. What an update reads is nobody's step of their own, so landing it says nothing and offers no undo of its own. A second read of the same place replaces the first."""
        return self.action("poll_weather")

    def poll_weather_many(self, items: list[PollWeatherArgs]) -> list[dict[str, Any]]:
        """Runs `poll_weather` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("poll_weather", items)

    def look_for_place(self) -> None:
        """Looks for what the person typed once it has stopped changing: the query waits one look, and the device is asked for the last query only."""
        return self.action("look_for_place")

    def look_for_place_many(self, items: list[LookForPlaceArgs]) -> list[dict[str, Any]]:
        """Runs `look_for_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("look_for_place", items)

    def begin_search(self) -> Searching:
        """Looks for places by name: the window asks the service once per query as it is typed."""
        return self.action("begin_search")

    def end_search(self, *, reader: Searching) -> None:
        """Empties the field and stops looking."""
        return self.action("end_search", reader=reader)

    def end_search_many(self, items: list[EndSearchArgs]) -> list[dict[str, Any]]:
        """Runs `end_search` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("end_search", items)

    def add_found(self,
        *,
        reader: Searching,
        at: int,
    ) -> Place:
        """Saves the place a search found, by its place in the list of what it found, and shows it."""
        return self.action("add_found", reader=reader, at=at)

    def accept_found(self,
        *,
        reader: Searching,
        choice: str,
    ) -> None:
        """Takes the row a person picked from the search: saves the place and empties the search."""
        return self.action("accept_found", reader=reader, choice=choice)

    def accept_found_many(self, items: list[AcceptFoundArgs]) -> list[dict[str, Any]]:
        """Runs `accept_found` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("accept_found", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"save_place", "use_as_default", "use_as_default_many", "move_to_top", "move_to_top_many", "move_place", "move_place_many", "rename_place", "rename_place_many", "remove_place", "remove_place_many", "stop_refresh", "stop_refresh_many", "retry_place", "retry_place_many", "read_next", "read_next_many", "poll_weather", "poll_weather_many", "look_for_place", "look_for_place_many", "begin_search", "end_search", "end_search_many", "add_found", "accept_found", "accept_found_many"})


class Readings(Resource):
    """Readings — `tl.so.tilo.weather.Readings`."""
    __surface__ = "so.tilo.weather@bcadefef8e611e18"
    @property
    def _6fze2ppw(self) -> list[Entry[PlaceReading]]:
        """of (`6fze2ppw` on the wire)."""
        return self.fields["6fze2ppw"]

    @_6fze2ppw.setter
    def _6fze2ppw(self, value: list[Entry[PlaceReading]]) -> None:
        self.edit("6fze2ppw", value=value)
    vf63zjb2: Refresh  # refresh

    def reading_for(self, *, place: Place) -> PlaceReading:
        """What this device read of `place`, made the first time something writes it."""
        return self.action("reading_for", place=place)

    def read_place(self, *, place: Place) -> bool:
        """Reads one place's forecast and air quality. One failing leaves the other's answer standing, and a place the service did not answer keeps what it had and says why."""
        return self.action("read_place", place=place)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"reading_for", "read_place"})


class Weather(Resource):
    """Weather — `tl.so.tilo.weather`."""
    __surface__ = "so.tilo.weather@bcadefef8e611e18"

