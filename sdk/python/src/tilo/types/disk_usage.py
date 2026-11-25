"""Types for Disk Usage Analyzer (`so.tilo.disk-usage`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `disk-usage/<resource>`, read with
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
    "Measure",
    "Recent",
    "Analyzer",
    "ScanAsk",
    "ScanState",
    "EntriesAsk",
    "Scanned",
    "DiskUsage",
    "AnalyzeArgs",
    "DrillArgs",
    "OpenPlaceArgs",
    "BackArgs",
    "HomeArgs",
    "ChooseArgs",
    "GoArgs",
    "PickArgs",
    "CancelChooseArgs",
    "ReadPickerArgs",
    "ScanMovedArgs",
    "ScanEndedArgs",
]

AnalyzeArgs = TypedDict("AnalyzeArgs", {
    "path": "str",
    "label": "str",
})
"""The arguments of `analyze`."""

DrillArgs = TypedDict("DrillArgs", {
    "m": "Measure",
})
"""The arguments of `drill`."""

OpenPlaceArgs = TypedDict("OpenPlaceArgs", {
    "path": "str",
})
"""The arguments of `open_place`."""

BackArgs = TypedDict("BackArgs", {
})
"""The arguments of `back`."""

HomeArgs = TypedDict("HomeArgs", {
})
"""The arguments of `home`."""

ChooseArgs = TypedDict("ChooseArgs", {
})
"""The arguments of `choose`."""

GoArgs = TypedDict("GoArgs", {
    "path": "str",
})
"""The arguments of `go`."""

PickArgs = TypedDict("PickArgs", {
})
"""The arguments of `pick`."""

CancelChooseArgs = TypedDict("CancelChooseArgs", {
})
"""The arguments of `cancel_choose`."""

ReadPickerArgs = TypedDict("ReadPickerArgs", {
})
"""The arguments of `read_picker`."""

ScanMovedArgs = TypedDict("ScanMovedArgs", {
    "j": "Any",
})
"""The arguments of `scan_moved`."""

ScanEndedArgs = TypedDict("ScanEndedArgs", {
    "j": "Any",
})
"""The arguments of `scan_ended`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.disk-usage@b1e8bfb751fc3d15"
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


class Measure(Resource):
    """Measure — `tl.so.tilo.disk-usage.Measure`."""
    __surface__ = "so.tilo.disk-usage@b1e8bfb751fc3d15"
    dgpcdvfo: str  # name
    hr7pdtyh: str  # path
    @property
    def _5ejkceq4(self) -> int:
        """bytes (`5ejkceq4` on the wire)."""
        return self.fields["5ejkceq4"]

    @_5ejkceq4.setter
    def _5ejkceq4(self, value: int) -> None:
        self.edit("5ejkceq4", value=value)
    h6ycnyru: int  # files


class Recent(Resource):
    """Recent — `tl.so.tilo.disk-usage.Recent`."""
    __surface__ = "so.tilo.disk-usage@b1e8bfb751fc3d15"
    mks7bf3q: str  # path
    w6urofdz: str  # label
    rrwtdebb: str  # seen


class Analyzer(Resource):
    """Analyzer — `tl.so.tilo.disk-usage.Analyzer`."""
    __surface__ = "so.tilo.disk-usage@b1e8bfb751fc3d15"
    myw6qh3s: str  # folder
    thlese6a: str  # label
    w37l5txv: list[Entry[Measure]]  # entries
    of6rtqrq: int  # total
    h7sbqkay: int  # files
    qgfvwvqs: int  # unreadable
    ciznalbq: str  # trouble
    qirhun4g: str  # notice
    xpqqxnzx: str  # job
    bborqgwr: int  # seen_bytes
    jycgqaau: int  # seen_files
    tby3fdb6: str  # root
    neoxlbvt: list[Entry[Recent]]  # recents
    jwzq6fuq: bool  # choosing
    eynuk6dc: str  # picker
    o5hekdfu: list[Entry[Measure]]  # picks

    def analyze(self,
        *,
        path: str,
        label: str,
    ) -> None:
        """The folder the person chose, made the folder measured now and its own chain's start."""
        return self.action("analyze", path=path, label=label)

    def analyze_many(self, items: list[AnalyzeArgs]) -> list[dict[str, Any]]:
        """Runs `analyze` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("analyze", items)

    def drill(self, *, m: Measure) -> None:
        """Measures a folder inside the one measured, keeping the way back."""
        return self.action("drill", m=m)

    def drill_many(self, items: list[DrillArgs]) -> list[dict[str, Any]]:
        """Runs `drill` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("drill", items)

    def open_place(self, *, path: str) -> None:
        """Measures a folder offered on the places page, starting a chain of its own."""
        return self.action("open_place", path=path)

    def open_place_many(self, items: list[OpenPlaceArgs]) -> list[dict[str, Any]]:
        """Runs `open_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open_place", items)

    def stop(self) -> None:
        """Stops the measuring under way."""
        return self.action("stop")

    def back(self) -> None:
        """Goes back one folder, and from the folder the chain started at to the places page."""
        return self.action("back")

    def back_many(self, items: list[BackArgs]) -> list[dict[str, Any]]:
        """Runs `back` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("back", items)

    def home(self) -> None:
        """Shows the places, on this device and measured lately."""
        return self.action("home")

    def home_many(self, items: list[HomeArgs]) -> list[dict[str, Any]]:
        """Runs `home` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("home", items)

    def choose(self) -> None:
        """Asks the person which folder to measure, showing the folders of this device."""
        return self.action("choose")

    def choose_many(self, items: list[ChooseArgs]) -> list[dict[str, Any]]:
        """Runs `choose` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("choose", items)

    def go(self, *, path: str) -> None:
        """Walks the picker to `path` and shows the folders in it."""
        return self.action("go", path=path)

    def go_many(self, items: list[GoArgs]) -> list[dict[str, Any]]:
        """Runs `go` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("go", items)

    def pick(self) -> None:
        """Measures the folder the picker shows."""
        return self.action("pick")

    def pick_many(self, items: list[PickArgs]) -> list[dict[str, Any]]:
        """Runs `pick` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("pick", items)

    def cancel_choose(self) -> None:
        """Leaves the chooser without measuring anything."""
        return self.action("cancel_choose")

    def cancel_choose_many(self, items: list[CancelChooseArgs]) -> list[dict[str, Any]]:
        """Runs `cancel_choose` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("cancel_choose", items)

    def read_picker(self) -> None:
        """Reads the folders of the one the picker shows."""
        return self.action("read_picker")

    def read_picker_many(self, items: list[ReadPickerArgs]) -> list[dict[str, Any]]:
        """Runs `read_picker` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("read_picker", items)

    def scan_moved(self, *, j: Any) -> None:
        """The measuring moved on: how much of the folder it has seen so far."""
        return self.action("scan_moved", j=j)

    def scan_moved_many(self, items: list[ScanMovedArgs]) -> list[dict[str, Any]]:
        """Runs `scan_moved` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("scan_moved", items)

    def scan_ended(self, *, j: Any) -> None:
        """The measuring ended: the folders it found, why it could not read them, or that it was stopped."""
        return self.action("scan_ended", j=j)

    def scan_ended_many(self, items: list[ScanEndedArgs]) -> list[dict[str, Any]]:
        """Runs `scan_ended` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("scan_ended", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"analyze", "analyze_many", "drill", "drill_many", "open_place", "open_place_many", "stop", "back", "back_many", "home", "home_many", "choose", "choose_many", "go", "go_many", "pick", "pick_many", "cancel_choose", "cancel_choose_many", "read_picker", "read_picker_many", "scan_moved", "scan_moved_many", "scan_ended", "scan_ended_many"})


class ScanAsk(Resource):
    """ScanAsk — `tl.so.tilo.disk-usage.ScanAsk`."""
    __surface__ = "so.tilo.disk-usage@b1e8bfb751fc3d15"
    fzzlfxy7: str  # path


class ScanState(Resource):
    """ScanState — `tl.so.tilo.disk-usage.ScanState`."""
    __surface__ = "so.tilo.disk-usage@b1e8bfb751fc3d15"
    qaev2xl3: str  # state
    dmlnkvla: str  # path
    k2iebb3k: int  # bytes
    zzb64yrs: int  # files
    @property
    def _7gwl2int(self) -> int:
        """total (`7gwl2int` on the wire)."""
        return self.fields["7gwl2int"]

    @_7gwl2int.setter
    def _7gwl2int(self, value: int) -> None:
        self.edit("7gwl2int", value=value)
    uvlehuak: int  # unreadable
    f7hqirco: str  # error


class EntriesAsk(Resource):
    """EntriesAsk — `tl.so.tilo.disk-usage.EntriesAsk`."""
    __surface__ = "so.tilo.disk-usage@b1e8bfb751fc3d15"
    hxfyfs5q: str  # job


class Scanned(Resource):
    """Scanned — `tl.so.tilo.disk-usage.Scanned`."""
    __surface__ = "so.tilo.disk-usage@b1e8bfb751fc3d15"
    @property
    def _5asfkn6k(self) -> str:
        """name (`5asfkn6k` on the wire)."""
        return self.fields["5asfkn6k"]

    @_5asfkn6k.setter
    def _5asfkn6k(self, value: str) -> None:
        self.edit("5asfkn6k", value=value)
    @property
    def _7xc3ifdt(self) -> str:
        """path (`7xc3ifdt` on the wire)."""
        return self.fields["7xc3ifdt"]

    @_7xc3ifdt.setter
    def _7xc3ifdt(self, value: str) -> None:
        self.edit("7xc3ifdt", value=value)
    elhtrr33: int  # bytes
    ktl6erww: int  # files


class DiskUsage(Resource):
    """Disk Usage Analyzer — `tl.so.tilo.disk-usage`."""
    __surface__ = "so.tilo.disk-usage@b1e8bfb751fc3d15"
    def open_disks(self) -> None:
        """Opens the Disks app, where the device's drives and their use are."""
        return self.action("open_disks")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"open_disks"})


