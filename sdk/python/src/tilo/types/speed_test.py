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
    "TiloSpeedTest",
    "StartArgs",
    "StopArgs",
    "PollArgs",
    "UploadStageArgs",
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

UploadStageArgs = TypedDict("UploadStageArgs", {
    "run": "Run",
})
"""The arguments of `upload_stage`."""

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
    __surface__ = "so.tilo.speed-test@596ac912f06684e0"
    hpwi5yye: list[Entry[Measurement]]  # results
    phw7nc7e: list[Entry[Run]]  # runs
    ckq5elpb: str | None  # next_poll

    def start(self) -> None:
        """Starts a run: the window measures latency against the nearest Cloudflare test server, then the download and the upload on the seconds after, and shows the dial moving as it goes."""
        return self.action("start")

    def start_many(self, items: list[StartArgs]) -> list[dict[str, Any]]:
        """Runs `start` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("start", items)

    def stop(self) -> None:
        """Stops the run in flight: the measurement stops and the dial goes back to ready. Nothing of a stopped run is kept."""
        return self.action("stop")

    def stop_many(self, items: list[StopArgs]) -> list[dict[str, Any]]:
        """Runs `stop` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("stop", items)

    def poll(self) -> None:
        """Measures the next stage of the run on the page: the download, then the upload, which records what the run measured. A stage that fails ends the run with its words."""
        return self.action("poll")

    def poll_many(self, items: list[PollArgs]) -> list[dict[str, Any]]:
        """Runs `poll` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("poll", items)

    def upload_stage(self, *, run: Run) -> None:
        """Uploads a run's bytes and records what the whole run measured. A refusal fails the stage."""
        return self.action("upload_stage", run=run)

    def upload_stage_many(self, items: list[UploadStageArgs]) -> list[dict[str, Any]]:
        """Runs `upload_stage` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("upload_stage", items)

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
    __actions__ = frozenset({"start", "start_many", "stop", "stop_many", "poll", "poll_many", "upload_stage", "upload_stage_many", "record", "record_many", "clear", "clear_many"})


class Measurement(Resource):
    """Measurement — `tl.so.tilo.speed-test.Measurement`."""
    __surface__ = "so.tilo.speed-test@596ac912f06684e0"
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
    __surface__ = "so.tilo.speed-test@596ac912f06684e0"
    fbbyyume: str  # phase
    vufvfoij: float  # live
    stw2oyj6: int  # bytes
    nwtvekek: str  # server
    t3padrs4: float  # ping
    vejad7bv: float  # jitter
    @property
    def _2kviwxr6(self) -> float:
        """download (`2kviwxr6` on the wire)."""
        return self.fields["2kviwxr6"]

    @_2kviwxr6.setter
    def _2kviwxr6(self, value: float) -> None:
        self.edit("2kviwxr6", value=value)
    wkhwlknc: str  # error

    def ping_stage(self) -> bool:
        """Times a few round trips and keeps the server the first one found. The dial shows the last reading while the run measures."""
        return self.action("ping_stage")

    def download_stage(self) -> bool:
        """Downloads a run's bytes in chunks of a reply the language holds whole, timing them together, and keeps the rate they came at."""
        return self.action("download_stage")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"ping_stage", "download_stage"})


class TiloSpeedTest(Resource):
    """Speed Test — `tl.so.tilo.speed-test`."""
    __surface__ = "so.tilo.speed-test@596ac912f06684e0"

