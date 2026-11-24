"""Types for Camera (`so.tilo.camera`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`ScanEntry` is the app's `tl.so.tilo.camera.ScanEntry` at `camera/<resource>`, read with
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
    "CaptureKind",
    "Facing",
    "ScanEntry",
    "SharedCapture",
    "Wifi",
    "Contact",
    "CapturePreferences",
    "ScannedPage",
    "ScanSession",
    "Camera",
    "StillAsk",
    "FrameAsk",
    "CodesAsk",
    "DecodedCode",
    "CodesAnswer",
    "CameraDevice",
    "CamerasAnswer",
    "JobAnswer",
    "JobState",
    "RecordAsk",
    "RecordAskJob",
    "RecordStopped",
    "RecordedClip",
    "ScanAsk",
    "OcrAsk",
    "OcrAnswer",
    "PdfPage",
    "PagesAsk",
    "UrlAnswer",
    "TiloCamera",
    "RegisterCaptureArgs",
    "RecordScanArgs",
    "SetGridArgs",
    "SetLevelArgs",
    "UseModeArgs",
    "UseLensArgs",
    "CycleLensArgs",
    "UseCameraArgs",
    "RetryCameraArgs",
    "SetResolutionArgs",
    "SetPhotoFormatArgs",
    "SetTimerArgs",
    "SetMirrorFrontArgs",
    "SetGeotagArgs",
    "SetHdrArgs",
    "SetDefaultLensArgs",
    "StartLookArgs",
    "StopLookArgs",
    "LookArgs",
    "ReadCodesArgs",
    "ShutterArgs",
    "ArmTimerArgs",
    "CaptureArgs",
    "StartRecordingArgs",
    "StopRecordingArgs",
    "PollArgs",
    "ShutterDueArgs",
    "ImportImageArgs",
    "KeepScanArgs",
    "SetScannerArgs",
    "SetScanResolutionArgs",
    "SetScanColourArgs",
    "SetScanSourceArgs",
    "SetOcrLanguageArgs",
    "CapturePageArgs",
    "SelectPageArgs",
    "RotatePageArgs",
    "RecognizeTextArgs",
    "MovePageArgs",
    "RemovePageArgs",
    "CropPageArgs",
]

CaptureKind = Literal["photo", "video"]
"""A capture is a still picture or a clip."""

Facing = Literal["back", "front", "wide"]
"""Which lens the viewfinder shows: the rear one, one facing the person, or the wide one."""

RegisterCaptureArgs = TypedDict("RegisterCaptureArgs", {
    "capture": "SharedCapture",
})
"""The arguments of `register_capture`."""

RecordScanArgs = TypedDict("RecordScanArgs", {
    "raw": "str",
})
"""The arguments of `record_scan`."""

SetGridArgs = TypedDict("SetGridArgs", {
    "on": "bool",
})
"""The arguments of `set_grid`."""

SetLevelArgs = TypedDict("SetLevelArgs", {
    "on": "bool",
})
"""The arguments of `set_level`."""

UseModeArgs = TypedDict("UseModeArgs", {
    "mode": "str",
})
"""The arguments of `use_mode`."""

UseLensArgs = TypedDict("UseLensArgs", {
    "lens": "Facing",
})
"""The arguments of `use_lens`."""

CycleLensArgs = TypedDict("CycleLensArgs", {
})
"""The arguments of `cycle_lens`."""

UseCameraArgs = TypedDict("UseCameraArgs", {
    "device": "str",
})
"""The arguments of `use_camera`."""

RetryCameraArgs = TypedDict("RetryCameraArgs", {
})
"""The arguments of `retry_camera`."""

SetResolutionArgs = TypedDict("SetResolutionArgs", {
    "resolution": "str",
})
"""The arguments of `set_resolution`."""

SetPhotoFormatArgs = TypedDict("SetPhotoFormatArgs", {
    "format": "str",
})
"""The arguments of `set_photo_format`."""

SetTimerArgs = TypedDict("SetTimerArgs", {
    "seconds": "int",
})
"""The arguments of `set_timer`."""

SetMirrorFrontArgs = TypedDict("SetMirrorFrontArgs", {
    "on": "bool",
})
"""The arguments of `set_mirror_front`."""

SetGeotagArgs = TypedDict("SetGeotagArgs", {
    "on": "bool",
})
"""The arguments of `set_geotag`."""

SetHdrArgs = TypedDict("SetHdrArgs", {
    "on": "bool",
})
"""The arguments of `set_hdr`."""

SetDefaultLensArgs = TypedDict("SetDefaultLensArgs", {
    "lens": "Facing",
})
"""The arguments of `set_default_lens`."""

StartLookArgs = TypedDict("StartLookArgs", {
})
"""The arguments of `start_look`."""

StopLookArgs = TypedDict("StopLookArgs", {
})
"""The arguments of `stop_look`."""

LookArgs = TypedDict("LookArgs", {
})
"""The arguments of `look`."""

ReadCodesArgs = TypedDict("ReadCodesArgs", {
    "url": "str",
})
"""The arguments of `read_codes`."""

ShutterArgs = TypedDict("ShutterArgs", {
})
"""The arguments of `shutter`."""

ArmTimerArgs = TypedDict("ArmTimerArgs", {
})
"""The arguments of `arm_timer`."""

CaptureArgs = TypedDict("CaptureArgs", {
})
"""The arguments of `capture`."""

StartRecordingArgs = TypedDict("StartRecordingArgs", {
})
"""The arguments of `start_recording`."""

StopRecordingArgs = TypedDict("StopRecordingArgs", {
})
"""The arguments of `stop_recording`."""

PollArgs = TypedDict("PollArgs", {
})
"""The arguments of `poll`."""

ShutterDueArgs = TypedDict("ShutterDueArgs", {
})
"""The arguments of `shutter_due`."""

ImportImageArgs = TypedDict("ImportImageArgs", {
    "file": "Any",
})
"""The arguments of `import_image`."""

KeepScanArgs = TypedDict("KeepScanArgs", {
})
"""The arguments of `keep_scan`."""

SetScannerArgs = TypedDict("SetScannerArgs", {
    "device": "str",
})
"""The arguments of `set_scanner`."""

SetScanResolutionArgs = TypedDict("SetScanResolutionArgs", {
    "resolution": "int",
})
"""The arguments of `set_scan_resolution`."""

SetScanColourArgs = TypedDict("SetScanColourArgs", {
    "colour": "str",
})
"""The arguments of `set_scan_colour`."""

SetScanSourceArgs = TypedDict("SetScanSourceArgs", {
    "source": "str",
})
"""The arguments of `set_scan_source`."""

SetOcrLanguageArgs = TypedDict("SetOcrLanguageArgs", {
    "language": "str",
})
"""The arguments of `set_ocr_language`."""

CapturePageArgs = TypedDict("CapturePageArgs", {
})
"""The arguments of `capture_page`."""

SelectPageArgs = TypedDict("SelectPageArgs", {
    "page": "ScannedPage",
})
"""The arguments of `select_page`."""

RotatePageArgs = TypedDict("RotatePageArgs", {
})
"""The arguments of `rotate_page`."""

RecognizeTextArgs = TypedDict("RecognizeTextArgs", {
})
"""The arguments of `recognize_text`."""

MovePageArgs = TypedDict("MovePageArgs", {
    "later": "bool",
})
"""The arguments of `move_page`."""

RemovePageArgs = TypedDict("RemovePageArgs", {
})
"""The arguments of `remove_page`."""

CropPageArgs = TypedDict("CropPageArgs", {
    "left": "float",
    "top": "float",
    "right": "float",
    "bottom": "float",
})
"""The arguments of `crop_page`."""

class ScanEntry(Resource):
    """ScanEntry — `tl.so.tilo.camera.ScanEntry`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    hsfv5zso: str  # raw
    ep2nakxy: str  # ts


class SharedCapture(Resource):
    """SharedCapture — `tl.so.tilo.camera.SharedCapture`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    @property
    def _3w536lga(self) -> Any:
        """file (`3w536lga` on the wire)."""
        return self.fields["3w536lga"]

    @_3w536lga.setter
    def _3w536lga(self, value: Any) -> None:
        self.edit("3w536lga", value=value)
    @property
    def _2h53klqs(self) -> CaptureKind:
        """kind (`2h53klqs` on the wire)."""
        return self.fields["2h53klqs"]

    @_2h53klqs.setter
    def _2h53klqs(self, value: CaptureKind) -> None:
        self.edit("2h53klqs", value=value)
    @property
    def _7enc2ly3(self) -> str:
        """ts (`7enc2ly3` on the wire)."""
        return self.fields["7enc2ly3"]

    @_7enc2ly3.setter
    def _7enc2ly3(self, value: str) -> None:
        self.edit("7enc2ly3", value=value)
    dwoxfhf4: int  # w
    wwonv6b5: int  # h
    cchj7ycb: int  # dur_ms
    ls5awfet: Facing  # facing
    @property
    def _4xq6x7ig(self) -> str:
        """label (`4xq6x7ig` on the wire)."""
        return self.fields["4xq6x7ig"]

    @_4xq6x7ig.setter
    def _4xq6x7ig(self, value: str) -> None:
        self.edit("4xq6x7ig", value=value)


class Wifi(Resource):
    """Wifi — `tl.so.tilo.camera.Wifi`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    lzomebum: str  # ssid
    r4cfx2eg: str  # auth
    vjej4gya: str  # password
    cpvlblyf: bool  # hidden


class Contact(Resource):
    """Contact — `tl.so.tilo.camera.Contact`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    vqb7vg46: str  # name
    m3wjb2ux: str  # phone
    ozre57rz: str  # email
    @property
    def _66g4od3j(self) -> str:
        """org (`66g4od3j` on the wire)."""
        return self.fields["66g4od3j"]

    @_66g4od3j.setter
    def _66g4od3j(self, value: str) -> None:
        self.edit("66g4od3j", value=value)


class CapturePreferences(Resource):
    """CapturePreferences — `tl.so.tilo.camera.CapturePreferences`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    kbzf222p: str  # resolution
    zqovr7j4: str  # photo_format
    qv6u7j3k: int  # timer
    @property
    def _7htbqsa6(self) -> bool:
        """grid (`7htbqsa6` on the wire)."""
        return self.fields["7htbqsa6"]

    @_7htbqsa6.setter
    def _7htbqsa6(self, value: bool) -> None:
        self.edit("7htbqsa6", value=value)
    xpbujo45: bool  # level
    ly3zxilj: bool  # mirror_front
    b4n265en: bool  # geotag
    uucfkei5: bool  # hdr
    nfm4slrd: Facing  # default_lens
    cfihwt6m: str  # mode
    v5w2qmvn: Facing  # lens
    zaeshfry: str  # device
    ffavn4zg: str  # preview
    b4frgf3q: str  # stream
    s45visew: str  # found
    v7ncmrtp: float  # found_x
    frttwlrr: float  # found_y
    di5cyk72: float  # found_w
    @property
    def _3adwwuks(self) -> float:
        """found_h (`3adwwuks` on the wire)."""
        return self.fields["3adwwuks"]

    @_3adwwuks.setter
    def _3adwwuks(self, value: float) -> None:
        self.edit("3adwwuks", value=value)
    fnu3wna2: str  # recording
    bkfl557v: str | None  # shutter_at
    zu2gt2mk: str  # trouble


class ScannedPage(Resource):
    """ScannedPage — `tl.so.tilo.camera.ScannedPage`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    @property
    def _5tzdwbr2(self) -> Any:
        """file (`5tzdwbr2` on the wire)."""
        return self.fields["5tzdwbr2"]

    @_5tzdwbr2.setter
    def _5tzdwbr2(self, value: Any) -> None:
        self.edit("5tzdwbr2", value=value)
    noe6wvhn: str  # text
    a6zvslgh: int  # order


class ScanSession(Resource):
    """ScanSession — `tl.so.tilo.camera.ScanSession`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    r3wtymsq: str  # scanner
    @property
    def _7wujh6qk(self) -> int:
        """resolution (`7wujh6qk` on the wire)."""
        return self.fields["7wujh6qk"]

    @_7wujh6qk.setter
    def _7wujh6qk(self, value: int) -> None:
        self.edit("7wujh6qk", value=value)
    mmi6goha: str  # colour
    jmjue7md: str  # source
    auvo6snv: str  # language
    pgl6ysyg: list[Entry[ScannedPage]]  # pages
    @property
    def _2yr4wc6x(self) -> str:
        """job (`2yr4wc6x` on the wire)."""
        return self.fields["2yr4wc6x"]

    @_2yr4wc6x.setter
    def _2yr4wc6x(self, value: str) -> None:
        self.edit("2yr4wc6x", value=value)
    @property
    def _7mup54lj(self) -> str:
        """failed (`7mup54lj` on the wire)."""
        return self.fields["7mup54lj"]

    @_7mup54lj.setter
    def _7mup54lj(self, value: str) -> None:
        self.edit("7mup54lj", value=value)
    yn4bzy6x: int  # selected


class Camera(Resource):
    """Camera — `tl.so.tilo.camera.Camera`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    mbpsgka6: list[Entry[ScanEntry]]  # scans
    rx3qy2lc: SharedCapture | None  # recent
    vdbaklqk: list[Entry[CapturePreferences]]  # preferences
    @property
    def _5q2zbyut(self) -> list[Entry[ScanSession]]:
        """sessions (`5q2zbyut` on the wire)."""
        return self.fields["5q2zbyut"]

    @_5q2zbyut.setter
    def _5q2zbyut(self, value: list[Entry[ScanSession]]) -> None:
        self.edit("5q2zbyut", value=value)

    def register_capture(self, *, capture: SharedCapture) -> None:
        """Takes what the engine kept as the capture the app hands on."""
        return self.action("register_capture", capture=capture)

    def register_capture_many(self, items: list[RegisterCaptureArgs]) -> list[dict[str, Any]]:
        """Runs `register_capture` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("register_capture", items)

    def record_scan(self, *, raw: str) -> None:
        """Keeps a scanned code in the history; the same code seen again moves to the front rather than repeating."""
        return self.action("record_scan", raw=raw)

    def record_scan_many(self, items: list[RecordScanArgs]) -> list[dict[str, Any]]:
        """Runs `record_scan` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("record_scan", items)

    def set_grid(self, *, on: bool) -> None:
        """Shows the composition grid, or takes it away."""
        return self.action("set_grid", on=on)

    def set_grid_many(self, items: list[SetGridArgs]) -> list[dict[str, Any]]:
        """Runs `set_grid` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_grid", items)

    def set_level(self, *, on: bool) -> None:
        """Shows the level guide, or takes it away."""
        return self.action("set_level", on=on)

    def set_level_many(self, items: list[SetLevelArgs]) -> list[dict[str, Any]]:
        """Runs `set_level` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_level", items)

    def prefs_of(self) -> CapturePreferences:
        """The person's preferences, made the first time they are written."""
        return self.action("prefs_of")

    def session_of(self) -> ScanSession:
        """The person's document, made the first time it is written."""
        return self.action("session_of")

    def use_mode(self, *, mode: str) -> None:
        """Opens the viewfinder. Opens the code scanner. Opens the document scanner. Opens the capture settings. Switches the viewfinder between photos and clips."""
        return self.action("use_mode", mode=mode)

    def use_mode_many(self, items: list[UseModeArgs]) -> list[dict[str, Any]]:
        """Runs `use_mode` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("use_mode", items)

    def use_lens(self, *, lens: Facing) -> None:
        """Shows one lens."""
        return self.action("use_lens", lens=lens)

    def use_lens_many(self, items: list[UseLensArgs]) -> list[dict[str, Any]]:
        """Runs `use_lens` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("use_lens", items)

    def cycle_lens(self) -> None:
        """Switches to the next lens."""
        return self.action("cycle_lens")

    def cycle_lens_many(self, items: list[CycleLensArgs]) -> list[dict[str, Any]]:
        """Runs `cycle_lens` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("cycle_lens", items)

    def use_camera(self, *, device: str) -> None:
        """Uses the camera at this device node."""
        return self.action("use_camera", device=device)

    def use_camera_many(self, items: list[UseCameraArgs]) -> list[dict[str, Any]]:
        """Runs `use_camera` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("use_camera", items)

    def retry_camera(self) -> None:
        """Forgets the chosen camera and asks the engine for the cameras again."""
        return self.action("retry_camera")

    def retry_camera_many(self, items: list[RetryCameraArgs]) -> list[dict[str, Any]]:
        """Runs `retry_camera` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("retry_camera", items)

    def set_resolution(self, *, resolution: str) -> None:
        """Runs the app's action."""
        return self.action("set_resolution", resolution=resolution)

    def set_resolution_many(self, items: list[SetResolutionArgs]) -> list[dict[str, Any]]:
        """Runs `set_resolution` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_resolution", items)

    def set_photo_format(self, *, format: str) -> None:
        """Runs the app's action."""
        return self.action("set_photo_format", format=format)

    def set_photo_format_many(self, items: list[SetPhotoFormatArgs]) -> list[dict[str, Any]]:
        """Runs `set_photo_format` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_photo_format", items)

    def set_timer(self, *, seconds: int) -> None:
        """Runs the app's action."""
        return self.action("set_timer", seconds=seconds)

    def set_timer_many(self, items: list[SetTimerArgs]) -> list[dict[str, Any]]:
        """Runs `set_timer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_timer", items)

    def set_mirror_front(self, *, on: bool) -> None:
        """Runs the app's action."""
        return self.action("set_mirror_front", on=on)

    def set_mirror_front_many(self, items: list[SetMirrorFrontArgs]) -> list[dict[str, Any]]:
        """Runs `set_mirror_front` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_mirror_front", items)

    def set_geotag(self, *, on: bool) -> None:
        """Runs the app's action."""
        return self.action("set_geotag", on=on)

    def set_geotag_many(self, items: list[SetGeotagArgs]) -> list[dict[str, Any]]:
        """Runs `set_geotag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_geotag", items)

    def set_hdr(self, *, on: bool) -> None:
        """Runs the app's action."""
        return self.action("set_hdr", on=on)

    def set_hdr_many(self, items: list[SetHdrArgs]) -> list[dict[str, Any]]:
        """Runs `set_hdr` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_hdr", items)

    def set_default_lens(self, *, lens: Facing) -> None:
        """Runs the app's action."""
        return self.action("set_default_lens", lens=lens)

    def set_default_lens_many(self, items: list[SetDefaultLensArgs]) -> list[dict[str, Any]]:
        """Runs `set_default_lens` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_default_lens", items)

    def start_look(self) -> None:
        """Starts the camera's moving picture: the window paints the newest frame as it arrives."""
        return self.action("start_look")

    def start_look_many(self, items: list[StartLookArgs]) -> list[dict[str, Any]]:
        """Runs `start_look` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("start_look", items)

    def stop_look(self) -> None:
        """Stops the camera's moving picture, letting the device go."""
        return self.action("stop_look")

    def stop_look_many(self, items: list[StopLookArgs]) -> list[dict[str, Any]]:
        """Runs `stop_look` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("stop_look", items)

    def look(self) -> None:
        """Reads the codes the newest frame holds, keeping the first one the scanner view shows."""
        return self.action("look")

    def look_many(self, items: list[LookArgs]) -> list[dict[str, Any]]:
        """Runs `look` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("look", items)

    def read_codes(self, *, url: str) -> None:
        """Decodes the codes in the picture at `url`, keeping the first one the scanner view shows."""
        return self.action("read_codes", url=url)

    def read_codes_many(self, items: list[ReadCodesArgs]) -> list[dict[str, Any]]:
        """Runs `read_codes` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("read_codes", items)

    def shutter(self) -> None:
        """The shutter: takes a photo now, starts the self-timer, or records."""
        return self.action("shutter")

    def shutter_many(self, items: list[ShutterArgs]) -> list[dict[str, Any]]:
        """Runs `shutter` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("shutter", items)

    def arm_timer(self) -> None:
        """Arms the self-timer the person chose, or cancels the one counting down."""
        return self.action("arm_timer")

    def arm_timer_many(self, items: list[ArmTimerArgs]) -> list[dict[str, Any]]:
        """Runs `arm_timer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("arm_timer", items)

    def capture(self) -> None:
        """Takes a photo of the frame the camera shows, and hands it on."""
        return self.action("capture")

    def capture_many(self, items: list[CaptureArgs]) -> list[dict[str, Any]]:
        """Runs `capture` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("capture", items)

    def start_recording(self) -> None:
        """Starts recording from the frames the camera's engine thread takes."""
        return self.action("start_recording")

    def start_recording_many(self, items: list[StartRecordingArgs]) -> list[dict[str, Any]]:
        """Runs `start_recording` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("start_recording", items)

    def stop_recording(self) -> None:
        """Stops the clip recording and hands it on once the engine has muxed it."""
        return self.action("stop_recording")

    def stop_recording_many(self, items: list[StopRecordingArgs]) -> list[dict[str, Any]]:
        """Runs `stop_recording` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("stop_recording", items)

    def poll(self) -> None:
        """Asks how far the recording and the document scan have got, finishing what is done."""
        return self.action("poll")

    def poll_many(self, items: list[PollArgs]) -> list[dict[str, Any]]:
        """Runs `poll` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("poll", items)

    def shutter_due(self) -> None:
        """The self-timer's moment came: take the photo it counted down to, unless it was cancelled."""
        return self.action("shutter_due")

    def shutter_due_many(self, items: list[ShutterDueArgs]) -> list[dict[str, Any]]:
        """Runs `shutter_due` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("shutter_due", items)

    def import_image(self, *, file: Any) -> None:
        """Takes a chosen image as the code to read, without importing it."""
        return self.action("import_image", file=file)

    def import_image_many(self, items: list[ImportImageArgs]) -> list[dict[str, Any]]:
        """Runs `import_image` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import_image", items)

    def copy_code(self, *, which: str) -> None:
        """Copies what the scanned code says, or its password or link."""
        return self.action("copy_code", which=which)

    def keep_scan(self) -> None:
        """Keeps the scanned code in the history."""
        return self.action("keep_scan")

    def keep_scan_many(self, items: list[KeepScanArgs]) -> list[dict[str, Any]]:
        """Runs `keep_scan` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep_scan", items)

    def set_scanner(self, *, device: str) -> None:
        """Chooses the scanner the document scans with."""
        return self.action("set_scanner", device=device)

    def set_scanner_many(self, items: list[SetScannerArgs]) -> list[dict[str, Any]]:
        """Runs `set_scanner` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_scanner", items)

    def set_scan_resolution(self, *, resolution: int) -> None:
        """Runs the app's action."""
        return self.action("set_scan_resolution", resolution=resolution)

    def set_scan_resolution_many(self, items: list[SetScanResolutionArgs]) -> list[dict[str, Any]]:
        """Runs `set_scan_resolution` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_scan_resolution", items)

    def set_scan_colour(self, *, colour: str) -> None:
        """Runs the app's action."""
        return self.action("set_scan_colour", colour=colour)

    def set_scan_colour_many(self, items: list[SetScanColourArgs]) -> list[dict[str, Any]]:
        """Runs `set_scan_colour` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_scan_colour", items)

    def set_scan_source(self, *, source: str) -> None:
        """Runs the app's action."""
        return self.action("set_scan_source", source=source)

    def set_scan_source_many(self, items: list[SetScanSourceArgs]) -> list[dict[str, Any]]:
        """Runs `set_scan_source` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_scan_source", items)

    def set_ocr_language(self, *, language: str) -> None:
        """Runs the app's action."""
        return self.action("set_ocr_language", language=language)

    def set_ocr_language_many(self, items: list[SetOcrLanguageArgs]) -> list[dict[str, Any]]:
        """Runs `set_ocr_language` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_ocr_language", items)

    def capture_page(self) -> None:
        """Captures one page with the chosen scanner and settings."""
        return self.action("capture_page")

    def capture_page_many(self, items: list[CapturePageArgs]) -> list[dict[str, Any]]:
        """Runs `capture_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("capture_page", items)

    def select_page(self, *, page: ScannedPage) -> None:
        """Runs the app's action."""
        return self.action("select_page", page=page)

    def select_page_many(self, items: list[SelectPageArgs]) -> list[dict[str, Any]]:
        """Runs `select_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("select_page", items)

    def rotate_page(self) -> None:
        """Turns the selected page a quarter clockwise; its text is read again when the person asks."""
        return self.action("rotate_page")

    def rotate_page_many(self, items: list[RotatePageArgs]) -> list[dict[str, Any]]:
        """Runs `rotate_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rotate_page", items)

    def recognize_text(self) -> None:
        """Reads the text off the selected page in the chosen language."""
        return self.action("recognize_text")

    def recognize_text_many(self, items: list[RecognizeTextArgs]) -> list[dict[str, Any]]:
        """Runs `recognize_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("recognize_text", items)

    def copy_text(self) -> None:
        """Copies the selected page's recognized text."""
        return self.action("copy_text")

    def move_page(self, *, later: bool) -> None:
        """Moves the selected page one place earlier, or later."""
        return self.action("move_page", later=later)

    def move_page_many(self, items: list[MovePageArgs]) -> list[dict[str, Any]]:
        """Runs `move_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_page", items)

    def remove_page(self) -> None:
        """Throws the selected page away."""
        return self.action("remove_page")

    def remove_page_many(self, items: list[RemovePageArgs]) -> list[dict[str, Any]]:
        """Runs `remove_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_page", items)

    def crop_source(self) -> Any | None:
        """The selected page's picture, as the crop frame works on it."""
        return self.action("crop_source")

    def crop_page(self,
        *,
        left: float,
        top: float,
        right: float,
        bottom: float,
    ) -> None:
        """Crops the selected page to the part the person kept; its text is read again when they ask."""
        return self.action("crop_page", left=left, top=top, right=right, bottom=bottom)

    def crop_page_many(self, items: list[CropPageArgs]) -> list[dict[str, Any]]:
        """Runs `crop_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("crop_page", items)

    def document_pdf(self) -> Any:
        """The document as one searchable PDF: pages with recognized text carry it, the rest are pictures alone."""
        return self.action("document_pdf")

    def open_recent(self) -> None:
        """Opens the capture that was taken last in the app that owns its bytes."""
        return self.action("open_recent")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"register_capture", "register_capture_many", "record_scan", "record_scan_many", "set_grid", "set_grid_many", "set_level", "set_level_many", "prefs_of", "session_of", "use_mode", "use_mode_many", "use_lens", "use_lens_many", "cycle_lens", "cycle_lens_many", "use_camera", "use_camera_many", "retry_camera", "retry_camera_many", "set_resolution", "set_resolution_many", "set_photo_format", "set_photo_format_many", "set_timer", "set_timer_many", "set_mirror_front", "set_mirror_front_many", "set_geotag", "set_geotag_many", "set_hdr", "set_hdr_many", "set_default_lens", "set_default_lens_many", "start_look", "start_look_many", "stop_look", "stop_look_many", "look", "look_many", "read_codes", "read_codes_many", "shutter", "shutter_many", "arm_timer", "arm_timer_many", "capture", "capture_many", "start_recording", "start_recording_many", "stop_recording", "stop_recording_many", "poll", "poll_many", "shutter_due", "shutter_due_many", "import_image", "import_image_many", "copy_code", "keep_scan", "keep_scan_many", "set_scanner", "set_scanner_many", "set_scan_resolution", "set_scan_resolution_many", "set_scan_colour", "set_scan_colour_many", "set_scan_source", "set_scan_source_many", "set_ocr_language", "set_ocr_language_many", "capture_page", "capture_page_many", "select_page", "select_page_many", "rotate_page", "rotate_page_many", "recognize_text", "recognize_text_many", "copy_text", "move_page", "move_page_many", "remove_page", "remove_page_many", "crop_source", "crop_page", "crop_page_many", "document_pdf", "open_recent"})


class StillAsk(Resource):
    """StillAsk — `tl.so.tilo.camera.StillAsk`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    tbd3ic3b: str  # device
    ijab2zhm: int  # width
    zyjznh7y: int  # height
    w2ygpawb: str  # format


class FrameAsk(Resource):
    """FrameAsk — `tl.so.tilo.camera.FrameAsk`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    ycmsih2o: str  # device
    yvbln5ns: int  # width
    kzl4t5dn: int  # height


class CodesAsk(Resource):
    """CodesAsk — `tl.so.tilo.camera.CodesAsk`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    vzfp47qk: str  # url
    v7y3lgcv: int  # width


class DecodedCode(Resource):
    """DecodedCode — `tl.so.tilo.camera.DecodedCode`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    fzwv4d4j: str  # text
    lsk2z7xu: float  # x
    ukafx4ho: float  # y
    hqwha6oq: float  # w
    mc2k2zbw: float  # h


class CodesAnswer(Resource):
    """CodesAnswer — `tl.so.tilo.camera.CodesAnswer`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    etfbstke: list[DecodedCode]  # codes


class CameraDevice(Resource):
    """CameraDevice — `tl.so.tilo.camera.CameraDevice`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    cowbxfqn: str  # id
    umm5rpj4: str  # label


class CamerasAnswer(Resource):
    """CamerasAnswer — `tl.so.tilo.camera.CamerasAnswer`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    @property
    def _3sxueu6f(self) -> list[CameraDevice]:
        """devices (`3sxueu6f` on the wire)."""
        return self.fields["3sxueu6f"]

    @_3sxueu6f.setter
    def _3sxueu6f(self, value: list[CameraDevice]) -> None:
        self.edit("3sxueu6f", value=value)
    rnqfpsmo: bool  # prompt


class JobAnswer(Resource):
    """JobAnswer — `tl.so.tilo.camera.JobAnswer`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    @property
    def _3uzmfe3a(self) -> str:
        """job (`3uzmfe3a` on the wire)."""
        return self.fields["3uzmfe3a"]

    @_3uzmfe3a.setter
    def _3uzmfe3a(self, value: str) -> None:
        self.edit("3uzmfe3a", value=value)


class JobState(Resource):
    """JobState — `tl.so.tilo.camera.JobState`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    @property
    def _5c2s6yqj(self) -> str:
        """state (`5c2s6yqj` on the wire)."""
        return self.fields["5c2s6yqj"]

    @_5c2s6yqj.setter
    def _5c2s6yqj(self, value: str) -> None:
        self.edit("5c2s6yqj", value=value)
    b4mxjneo: str  # url
    i7ypqcia: str  # error


class RecordAsk(Resource):
    """RecordAsk — `tl.so.tilo.camera.RecordAsk`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    zm52cba6: str  # device
    @property
    def _2vmgzw6b(self) -> int:
        """width (`2vmgzw6b` on the wire)."""
        return self.fields["2vmgzw6b"]

    @_2vmgzw6b.setter
    def _2vmgzw6b(self, value: int) -> None:
        self.edit("2vmgzw6b", value=value)
    @property
    def _2oehkbmq(self) -> int:
        """height (`2oehkbmq` on the wire)."""
        return self.fields["2oehkbmq"]

    @_2oehkbmq.setter
    def _2oehkbmq(self, value: int) -> None:
        self.edit("2oehkbmq", value=value)


class RecordAskJob(Resource):
    """RecordAskJob — `tl.so.tilo.camera.RecordAskJob`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    vkx7nslm: str  # job


class RecordStopped(Resource):
    """RecordStopped — `tl.so.tilo.camera.RecordStopped`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    tm6dvkh2: RecordedClip  # video


class RecordedClip(Resource):
    """RecordedClip — `tl.so.tilo.camera.RecordedClip`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    tzmimu3l: str  # name
    @property
    def _7znademx(self) -> int:
        """duration_ms (`7znademx` on the wire)."""
        return self.fields["7znademx"]

    @_7znademx.setter
    def _7znademx(self, value: int) -> None:
        self.edit("7znademx", value=value)
    iwtkarql: int  # w
    zmsyrjzf: int  # h
    zgalivtz: int  # size


class ScanAsk(Resource):
    """ScanAsk — `tl.so.tilo.camera.ScanAsk`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    @property
    def _3m6nff75(self) -> str:
        """scanner (`3m6nff75` on the wire)."""
        return self.fields["3m6nff75"]

    @_3m6nff75.setter
    def _3m6nff75(self, value: str) -> None:
        self.edit("3m6nff75", value=value)
    o4hag5xq: int  # resolution
    hvk7vnxj: str  # colour
    @property
    def _7zqczcaa(self) -> str:
        """source (`7zqczcaa` on the wire)."""
        return self.fields["7zqczcaa"]

    @_7zqczcaa.setter
    def _7zqczcaa(self, value: str) -> None:
        self.edit("7zqczcaa", value=value)
    sq2qwcuy: str  # name


class OcrAsk(Resource):
    """OcrAsk — `tl.so.tilo.camera.OcrAsk`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    @property
    def _35d3ajgx(self) -> str:
        """url (`35d3ajgx` on the wire)."""
        return self.fields["35d3ajgx"]

    @_35d3ajgx.setter
    def _35d3ajgx(self, value: str) -> None:
        self.edit("35d3ajgx", value=value)
    tlhgxi3u: str  # language


class OcrAnswer(Resource):
    """OcrAnswer — `tl.so.tilo.camera.OcrAnswer`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    @property
    def _3hu3iumn(self) -> str:
        """text (`3hu3iumn` on the wire)."""
        return self.fields["3hu3iumn"]

    @_3hu3iumn.setter
    def _3hu3iumn(self, value: str) -> None:
        self.edit("3hu3iumn", value=value)
    k2lbevvr: int  # words
    nof45z64: float  # deskew


class PdfPage(Resource):
    """PdfPage — `tl.so.tilo.camera.PdfPage`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    siogqny4: str  # url
    anpoq2j7: str  # text


class PagesAsk(Resource):
    """PagesAsk — `tl.so.tilo.camera.PagesAsk`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    mmydep5v: list[PdfPage]  # pages
    rclpsfrb: str  # name


class UrlAnswer(Resource):
    """UrlAnswer — `tl.so.tilo.camera.UrlAnswer`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    llffazxv: str  # url


class TiloCamera(Resource):
    """Camera — `tl.so.tilo.camera`."""
    __surface__ = "so.tilo.camera@51618a1446d5beba"
    def open_photos(self) -> None:
        """Opens the library the captures go to."""
        return self.action("open_photos")

    def open_camera_privacy(self) -> None:
        """Opens this device's camera privacy settings."""
        return self.action("open_camera_privacy")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"open_photos", "open_camera_privacy"})


