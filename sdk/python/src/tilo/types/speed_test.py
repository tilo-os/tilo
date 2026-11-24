"""Types for Speed Test (`so.tilo.speed-test`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`SpeedTest` is the app's `tl.so.tilo.speed-test.SpeedTest` at `speed-test/<resource>`, read with
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
    "SpeedTest",
    "Measurement",
    "Run",
    "RunState",
    "Started",
    "TiloSpeedTest",
    "StartArgs",
    "StopArgs",
    "PollArgs",
    "RecordArgs",
    "ClearArgs",
]

StartArgs = TypedDict("StartArgs", {
})
"""The arguments of `start`."""

StopArgs = TypedDict("StopArgs", {
})
"""The arguments of `stop`."""

PollArgs = TypedDict("PollArgs", {
})
"""The arguments of `poll`."""

RecordArgs = TypedDict("RecordArgs", {
    "ping": "float",
    "jitter": "float",
    "download": "float",
    "upload": "float",
    "server": "str",
    "device": "str",
})
"""The arguments of `record`."""

ClearArgs = TypedDict("ClearArgs", {
})
"""The arguments of `clear`."""

class SpeedTest(Resource):
    """SpeedTest — `tl.so.tilo.speed-test.SpeedTest`."""
    __surface__ = "so.tilo.speed-test@b0a00bf56855af32"
    hpwi5yye: list[Entry[Measurement]]  # results
    phw7nc7e: list[Entry[Run]]  # runs
    ckq5elpb: str | None  # next_poll

    def start(self) -> None:
        """Starts a run: the device measures latency, download and upload against the nearest Cloudflare test server, and the window follows it as it goes."""
        return self.action("start")

    def start_many(self, items: list[StartArgs]) -> list[dict[str, Any]]:
        """Runs `start` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("start", items)

    def stop(self) -> None:
        """Stops the run in flight: the device stops measuring and the dial goes back to ready. Nothing of a stopped run is kept."""
        return self.action("stop")

    def stop_many(self, items: list[StopArgs]) -> list[dict[str, Any]]:
        """Runs `stop` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("stop", items)

    def poll(self) -> None:
        """Asks the device how far the run has got: the dial follows the reading of the stage, and what the run measured is kept when it ends. A device that no longer answers the run ends it without a measurement, as closing the app used to."""
        return self.action("poll")

    def poll_many(self, items: list[PollArgs]) -> list[dict[str, Any]]:
        """Runs `poll` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("poll", items)

    def record(self,
        *,
        ping: float,
        jitter: float,
        download: float,
        upload: float,
        server: str,
        device: str,
    ) -> None:
        """Records a finished measurement, newest first, and lets the oldest go beyond what the workspace keeps: an agent or a script may record one too."""
        return self.action("record", ping=ping, jitter=jitter, download=download, upload=upload, server=server, device=device)

    def record_many(self, items: list[RecordArgs]) -> list[dict[str, Any]]:
        """Runs `record` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("record", items)

    def clear(self) -> None:
        """Forgets every measurement."""
        return self.action("clear")

    def clear_many(self, items: list[ClearArgs]) -> list[dict[str, Any]]:
        """Runs `clear` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"start", "start_many", "stop", "stop_many", "poll", "poll_many", "record", "record_many", "clear", "clear_many"})


class Measurement(Resource):
    """Measurement — `tl.so.tilo.speed-test.Measurement`."""
    __surface__ = "so.tilo.speed-test@b0a00bf56855af32"
    @property
    def _5f54c4ou(self) -> float:
        """ping (`5f54c4ou` on the wire)."""
        return self.fields["5f54c4ou"]

    @_5f54c4ou.setter
    def _5f54c4ou(self, value: float) -> None:
        self.edit("5f54c4ou", value=value)
    q5jjysbc: float  # jitter
    kded6ppq: float  # download
    x6xgknqc: float  # upload
    vcibj65j: str  # server
    vbch5rhv: str  # device


class Run(Resource):
    """Run — `tl.so.tilo.speed-test.Run`."""
    __surface__ = "so.tilo.speed-test@b0a00bf56855af32"
    fbbyyume: str  # phase
    @property
    def _376ly3mp(self) -> str:
        """job (`376ly3mp` on the wire)."""
        return self.fields["376ly3mp"]

    @_376ly3mp.setter
    def _376ly3mp(self, value: str) -> None:
        self.edit("376ly3mp", value=value)
    vufvfoij: float  # live
    stw2oyj6: int  # bytes
    nwtvekek: str  # server
    wkhwlknc: str  # error


class RunState(Resource):
    """RunState — `tl.so.tilo.speed-test.RunState`."""
    __surface__ = "so.tilo.speed-test@b0a00bf56855af32"
    @property
    def _4xzztkvp(self) -> str:
        """state (`4xzztkvp` on the wire)."""
        return self.fields["4xzztkvp"]

    @_4xzztkvp.setter
    def _4xzztkvp(self, value: str) -> None:
        self.edit("4xzztkvp", value=value)
    os4ndogp: str  # phase
    @property
    def _7eyrzvhi(self) -> float:
        """live (`7eyrzvhi` on the wire)."""
        return self.fields["7eyrzvhi"]

    @_7eyrzvhi.setter
    def _7eyrzvhi(self, value: float) -> None:
        self.edit("7eyrzvhi", value=value)
    phil2ezm: int  # bytes
    @property
    def _5szkqkfk(self) -> str:
        """server (`5szkqkfk` on the wire)."""
        return self.fields["5szkqkfk"]

    @_5szkqkfk.setter
    def _5szkqkfk(self, value: str) -> None:
        self.edit("5szkqkfk", value=value)
    hcm22tea: float  # ping
    esi4hzhw: float  # jitter
    gmjsrtmo: float  # download
    q4qvdsia: float  # upload
    eyu5ljwd: str  # device
    c6eopm2r: str  # failure


class Started(Resource):
    """Started — `tl.so.tilo.speed-test.Started`."""
    __surface__ = "so.tilo.speed-test@b0a00bf56855af32"
    fltipwm6: str  # job


class TiloSpeedTest(Resource):
    """Speed Test — `tl.so.tilo.speed-test`."""
    __surface__ = "so.tilo.speed-test@b0a00bf56855af32"

