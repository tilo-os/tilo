"""Types for Weather (`so.tilo.weather`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Hour` is the app's `tl.so.tilo.weather.Hour` at `weather/<resource>`, read with
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
    "Hour",
    "Day",
    "Reading",
    "Air",
    "Place",
    "Refresh",
    "Found",
    "Searching",
    "Locations",
    "Ask",
    "AskPlace",
    "SearchAsk",
    "TakeAsk",
    "Job",
    "Taken",
    "Answer",
    "Weather",
    "UseAsDefaultArgs",
    "MoveToTopArgs",
    "RemovePlaceArgs",
    "StopRefreshArgs",
    "RetryPlaceArgs",
    "PollWeatherArgs",
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

PollWeatherArgs = TypedDict("PollWeatherArgs", {
})
"""The arguments of `poll_weather`."""

EndSearchArgs = TypedDict("EndSearchArgs", {
    "reader": "Searching",
})
"""The arguments of `end_search`."""

AcceptFoundArgs = TypedDict("AcceptFoundArgs", {
    "reader": "Searching",
    "choice": "str",
})
"""The arguments of `accept_found`."""

class Hour(Resource):
    """Hour — `tl.so.tilo.weather.Hour`."""
    __surface__ = "so.tilo.weather@e5dcf21ad5da326c"
    hqg5l743: str  # at
    nz766xgs: float  # temperature
    bszivu27: float  # chance
    w4hqkb3u: int  # code
    rx2vbln3: bool  # day
    il4besep: str  # icon


class Day(Resource):
    """Day — `tl.so.tilo.weather.Day`."""
    __surface__ = "so.tilo.weather@e5dcf21ad5da326c"
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
    __surface__ = "so.tilo.weather@e5dcf21ad5da326c"
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
    __surface__ = "so.tilo.weather@e5dcf21ad5da326c"
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
    __surface__ = "so.tilo.weather@e5dcf21ad5da326c"
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
    ikazv44r: bool  # read
    gbrotevm: Reading  # current
    qt6zc3me: list[Entry[Hour]]  # hours
    fjbo2bs7: list[Entry[Day]]  # days
    qz7n6v2n: Air  # air
    uthtg3cy: bool  # has_air
    @property
    def _42oai4b2(self) -> str | None:
        """fetched (`42oai4b2` on the wire)."""
        return self.fields["42oai4b2"]

    @_42oai4b2.setter
    def _42oai4b2(self, value: str | None) -> None:
        self.edit("42oai4b2", value=value)
    lkrdcuut: bool  # stale
    ckqlainl: str  # trouble


class Refresh(Resource):
    """Refresh — `tl.so.tilo.weather.Refresh`."""
    __surface__ = "so.tilo.weather@e5dcf21ad5da326c"
    j4qvmrkj: str  # job
    @property
    def _6a665z7q(self) -> str:
        """state (`6a665z7q` on the wire)."""
        return self.fields["6a665z7q"]

    @_6a665z7q.setter
    def _6a665z7q(self, value: str) -> None:
        self.edit("6a665z7q", value=value)
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
    __surface__ = "so.tilo.weather@e5dcf21ad5da326c"
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
    __surface__ = "so.tilo.weather@e5dcf21ad5da326c"
    fmhvvp6l: str  # wanted
    o4whnz6t: str  # asked
    xx2oi6v5: str  # job
    cqmaksmt: bool  # searching
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
    __surface__ = "so.tilo.weather@e5dcf21ad5da326c"
    b2qn6gqw: list[Entry[Place]]  # places
    vqfze2cq: Place | None  # default
    esvv4ymu: Refresh  # refresh
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

    def remove_place(self, *, place: Place) -> None:
        """Removes a place from the list; what was read of it goes with it. Undo brings it back."""
        return self.action("remove_place", place=place)

    def remove_place_many(self, items: list[RemovePlaceArgs]) -> list[dict[str, Any]]:
        """Runs `remove_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_place", items)

    # the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
    def stop_refresh(self) -> None:
        """Stops an update that is running."""
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

    def poll_weather(self) -> None:
        """Takes what the device has read so far: each place's answer as it lands, and what the service says of it. What an update reads is nobody's step of their own, so landing it says nothing and offers no undo of its own. A second read of the same place replaces the first."""
        return self.action("poll_weather")

    def poll_weather_many(self, items: list[PollWeatherArgs]) -> list[dict[str, Any]]:
        """Runs `poll_weather` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("poll_weather", items)

    def begin_search(self) -> Searching:
        """Looks for places by name. The device asks the service away from the frame; the window takes the rows as they land."""
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
    __actions__ = frozenset({"save_place", "use_as_default", "use_as_default_many", "move_to_top", "move_to_top_many", "remove_place", "remove_place_many", "stop_refresh", "stop_refresh_many", "retry_place", "retry_place_many", "poll_weather", "poll_weather_many", "begin_search", "end_search", "end_search_many", "add_found", "accept_found", "accept_found_many"})


class Ask(Resource):
    """Ask — `tl.so.tilo.weather.Ask`."""
    __surface__ = "so.tilo.weather@e5dcf21ad5da326c"
    wf3zmolm: list[AskPlace]  # places


class AskPlace(Resource):
    """AskPlace — `tl.so.tilo.weather.AskPlace`."""
    __surface__ = "so.tilo.weather@e5dcf21ad5da326c"
    gdy7duy6: str  # place_id
    @property
    def _5btxbecx(self) -> float:
        """latitude (`5btxbecx` on the wire)."""
        return self.fields["5btxbecx"]

    @_5btxbecx.setter
    def _5btxbecx(self, value: float) -> None:
        self.edit("5btxbecx", value=value)
    dcshjxzm: float  # longitude


class SearchAsk(Resource):
    """SearchAsk — `tl.so.tilo.weather.SearchAsk`."""
    __surface__ = "so.tilo.weather@e5dcf21ad5da326c"
    @property
    def _4opjitfa(self) -> str:
        """query (`4opjitfa` on the wire)."""
        return self.fields["4opjitfa"]

    @_4opjitfa.setter
    def _4opjitfa(self, value: str) -> None:
        self.edit("4opjitfa", value=value)


class TakeAsk(Resource):
    """TakeAsk — `tl.so.tilo.weather.TakeAsk`."""
    __surface__ = "so.tilo.weather@e5dcf21ad5da326c"
    @property
    def _6vnqzh4l(self) -> str:
        """job (`6vnqzh4l` on the wire)."""
        return self.fields["6vnqzh4l"]

    @_6vnqzh4l.setter
    def _6vnqzh4l(self, value: str) -> None:
        self.edit("6vnqzh4l", value=value)


class Job(Resource):
    """Job — `tl.so.tilo.weather.Job`."""
    __surface__ = "so.tilo.weather@e5dcf21ad5da326c"
    lwhvatgx: str  # state
    ka2h5wk7: int  # done
    lbqaguom: int  # total


class Taken(Resource):
    """Taken — `tl.so.tilo.weather.Taken`."""
    __surface__ = "so.tilo.weather@e5dcf21ad5da326c"
    gbqubsmc: list[str]  # answers
    d4onixyo: list[str]  # places
    ra4o5i2z: str  # error


class Answer(Resource):
    """Answer — `tl.so.tilo.weather.Answer`."""
    __surface__ = "so.tilo.weather@e5dcf21ad5da326c"
    @property
    def _5prprzwo(self) -> str:
        """place_id (`5prprzwo` on the wire)."""
        return self.fields["5prprzwo"]

    @_5prprzwo.setter
    def _5prprzwo(self, value: str) -> None:
        self.edit("5prprzwo", value=value)
    papzs3jy: bool  # ok
    @property
    def _2t7oi34p(self) -> str:
        """reason (`2t7oi34p` on the wire)."""
        return self.fields["2t7oi34p"]

    @_2t7oi34p.setter
    def _2t7oi34p(self, value: str) -> None:
        self.edit("2t7oi34p", value=value)
    opdrm37r: int  # status
    oh3swlwe: str  # message
    szxryqtq: int  # fetched
    udln4y7a: str  # time
    bt4m22bz: float  # temperature
    @property
    def _5wz5vx2n(self) -> float:
        """apparent (`5wz5vx2n` on the wire)."""
        return self.fields["5wz5vx2n"]

    @_5wz5vx2n.setter
    def _5wz5vx2n(self, value: float) -> None:
        self.edit("5wz5vx2n", value=value)
    kk67hrwa: float  # humidity
    wssxi3hh: float  # precipitation
    z2xmdn5s: int  # code
    @property
    def _45ln3j6b(self) -> float:
        """wind (`45ln3j6b` on the wire)."""
        return self.fields["45ln3j6b"]

    @_45ln3j6b.setter
    def _45ln3j6b(self, value: float) -> None:
        self.edit("45ln3j6b", value=value)
    @property
    def _7sdt6b3s(self) -> float:
        """wind_from (`7sdt6b3s` on the wire)."""
        return self.fields["7sdt6b3s"]

    @_7sdt6b3s.setter
    def _7sdt6b3s(self, value: float) -> None:
        self.edit("7sdt6b3s", value=value)
    @property
    def _6pbvus4v(self) -> float:
        """pressure (`6pbvus4v` on the wire)."""
        return self.fields["6pbvus4v"]

    @_6pbvus4v.setter
    def _6pbvus4v(self, value: float) -> None:
        self.edit("6pbvus4v", value=value)
    @property
    def _7twngswb(self) -> float:
        """uv (`7twngswb` on the wire)."""
        return self.fields["7twngswb"]

    @_7twngswb.setter
    def _7twngswb(self, value: float) -> None:
        self.edit("7twngswb", value=value)
    gfeyv7li: bool  # is_day
    do7cloel: list[str]  # hour_at
    adqc7myi: list[float]  # hour_temp
    m4tdmh3g: list[float]  # hour_chance
    @property
    def _6w6giapb(self) -> list[int]:
        """hour_code (`6w6giapb` on the wire)."""
        return self.fields["6w6giapb"]

    @_6w6giapb.setter
    def _6w6giapb(self, value: list[int]) -> None:
        self.edit("6w6giapb", value=value)
    ercv5gi6: list[bool]  # hour_day
    @property
    def _6vlbpjn2(self) -> list[str]:
        """day_date (`6vlbpjn2` on the wire)."""
        return self.fields["6vlbpjn2"]

    @_6vlbpjn2.setter
    def _6vlbpjn2(self, value: list[str]) -> None:
        self.edit("6vlbpjn2", value=value)
    xpkpnifa: list[int]  # day_weekday
    @property
    def _7mza74v3(self) -> list[float]:
        """day_high (`7mza74v3` on the wire)."""
        return self.fields["7mza74v3"]

    @_7mza74v3.setter
    def _7mza74v3(self, value: list[float]) -> None:
        self.edit("7mza74v3", value=value)
    v5etkrnq: list[float]  # day_low
    lsdyx245: list[float]  # day_chance
    hw7afts4: list[int]  # day_code
    rv4jwm5h: list[float]  # day_wind
    eksw2wiu: list[float]  # day_uv
    cl5k7le3: list[str]  # day_rise
    zjervfc5: list[str]  # day_set
    @property
    def _4kcw2w6l(self) -> bool:
        """air_ok (`4kcw2w6l` on the wire)."""
        return self.fields["4kcw2w6l"]

    @_4kcw2w6l.setter
    def _4kcw2w6l(self, value: bool) -> None:
        self.edit("4kcw2w6l", value=value)
    clhldsl6: str  # air_at
    fxpfimss: float | None  # air_european
    zhjhkay7: float | None  # air_us
    @property
    def _55ge75je(self) -> float | None:
        """air_pm2_5 (`55ge75je` on the wire)."""
        return self.fields["55ge75je"]

    @_55ge75je.setter
    def _55ge75je(self, value: float | None) -> None:
        self.edit("55ge75je", value=value)
    @property
    def _363wfecw(self) -> float | None:
        """air_pm10 (`363wfecw` on the wire)."""
        return self.fields["363wfecw"]

    @_363wfecw.setter
    def _363wfecw(self, value: float | None) -> None:
        self.edit("363wfecw", value=value)
    w77tgu5z: float | None  # air_no2
    qo2ngnib: float | None  # air_ozone


class Weather(Resource):
    """Weather — `tl.so.tilo.weather`."""
    __surface__ = "so.tilo.weather@e5dcf21ad5da326c"

