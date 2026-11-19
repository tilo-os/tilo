"""Types for Video Editor (`so.tilo.video-editor`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Project` is the app's `video-editor.project` at `video-editor/<resource>`, read with
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
    "ChromaKey",
    "Clip",
    "ClipEffect",
    "ClipPlacement",
    "EffectKind",
    "Filters",
    "Marker",
    "Mask",
    "MaskShape",
    "MediaItem",
    "MediaKind",
    "Speed",
    "TextAlign",
    "TextSource",
    "Track",
    "TrackKind",
    "Transform",
    "Transition",
    "Project",
    "SetFrameSizeArgs",
    "SetFrameRateArgs",
    "SetRenderRangeArgs",
    "MarkInArgs",
    "MarkOutArgs",
    "ClearMarksArgs",
    "AddMarkerArgs",
    "PlaceTwoAnglesArgs",
    "UnplaceTwoAnglesArgs",
    "RemoveMarkerArgs",
    "AddTrackArgs",
    "ResetMixArgs",
    "TrimTracksArgs",
    "DeleteTrackArgs",
    "PlaceClipArgs",
    "DeleteMediaArgs",
    "UseAsProjectFormatArgs",
    "DeleteArgs",
    "RippleDeleteArgs",
    "SplitClipArgs",
    "TrimInClipArgs",
    "TrimOutClipArgs",
    "SetSpeedArgs",
    "ToggleReverseArgs",
    "ResetSpeedArgs",
    "SetParamArgs",
    "AddEffectArgs",
    "RemoveEffectArgs",
    "MoveEffectArgs",
    "SetEffectAmountArgs",
    "ClearEffectsArgs",
    "UnlinkArgs",
    "LinkAudioArgs",
    "LinkWithArgs",
]

class ChromaKey(TypedDict):
    enabled: bool
    color: list[float]
    tolerance: float
    softness: float
    spill: float

class Clip(TypedDict):
    id: str
    track: str
    media: str
    start: float
    in_point: float
    out_point: float
    opacity: float
    volume: float
    transition: Transition
    palette: int
    filters: Filters
    curves: Any
    fade_in: float
    fade_out: float
    transition_duration: float
    effects: list[Entry[ClipEffect]]
    transform: Transform
    speed: Speed
    key: ChromaKey
    mask: Mask
    anim: Any
    markers: list[Entry[Marker]]
    link: str | None
    sound_split: bool
    link_follows: bool

class ClipEffect(TypedDict):
    id: str
    kind: EffectKind
    amount: float

ClipPlacement = TypedDict("ClipPlacement", {
    "media": "str",
    "track": "str | None",
    "at": "float | None",
    "in": "float | None",
    "out": "float | None",
    "fade_in": "float | None",
    "fade_out": "float | None",
})

EffectKind = Literal["Grayscale", "Sepia", "Invert", "Warm", "Cool", "Vignette"]

class Filters(TypedDict):
    brightness: float
    contrast: float
    saturation: float

class Marker(TypedDict):
    id: str
    t: float
    name: str
    palette: int
    note: str

class Mask(TypedDict):
    shape: MaskShape
    x: float
    y: float
    w: float
    h: float
    rotation: float
    feather: float
    invert: bool

MaskShape = Literal["None", "Rect", "Ellipse"]

class MediaItem(TypedDict):
    id: str
    name: str
    kind: MediaKind
    duration: float
    width: int
    height: int
    fps: float
    palette: int
    path: str
    text: TextSource | None
    has_audio: bool

MediaKind = Literal["Video", "Audio", "Image", "Text"]

class Speed(TypedDict):
    rate: float
    reverse: bool

TextAlign = Literal["Left", "Center", "Right"]

class TextSource(TypedDict):
    content: str
    size: float
    color: list[float]
    align: TextAlign
    font: str
    bold: bool
    italic: bool
    backdrop: float

class Track(TypedDict):
    id: str
    name: str
    kind: TrackKind
    index: int
    muted: bool
    locked: bool
    solo: bool
    gain_db: float
    pan: float

TrackKind = Literal["video", "audio"]

class Transform(TypedDict):
    scale_x: float
    scale_y: float
    pos_x: float
    pos_y: float
    rotation: float
    anchor_x: float
    anchor_y: float
    crop_left: float
    crop_right: float
    crop_top: float
    crop_bottom: float

Transition = Literal["None", "CrossDissolve", "FadeIn", "DipToBlack", "Wipe"]

SetFrameSizeArgs = TypedDict("SetFrameSizeArgs", {
    "width": "int",
    "height": "int",
})
"""The arguments of `set-frame-size`."""

SetFrameRateArgs = TypedDict("SetFrameRateArgs", {
    "fps": "float",
})
"""The arguments of `set-frame-rate`."""

SetRenderRangeArgs = TypedDict("SetRenderRangeArgs", {
    "start": "float",
    "end": "float",
})
"""The arguments of `set-render-range`."""

MarkInArgs = TypedDict("MarkInArgs", {
    "at": "float",
})
"""The arguments of `mark-in`."""

MarkOutArgs = TypedDict("MarkOutArgs", {
    "at": "float",
})
"""The arguments of `mark-out`."""

ClearMarksArgs = TypedDict("ClearMarksArgs", {
})
"""The arguments of `clear-marks`."""

AddMarkerArgs = TypedDict("AddMarkerArgs", {
    "at": "float",
    "name": "str",
})
"""The arguments of `add-marker`."""

PlaceTwoAnglesArgs = TypedDict("PlaceTwoAnglesArgs", {
    "at": "float",
})
"""The arguments of `place-two-angles`."""

UnplaceTwoAnglesArgs = TypedDict("UnplaceTwoAnglesArgs", {
    "at": "float",
})
"""The arguments of `unplace-two-angles`."""

RemoveMarkerArgs = TypedDict("RemoveMarkerArgs", {
    "marker": "str",
})
"""The arguments of `remove-marker`."""

AddTrackArgs = TypedDict("AddTrackArgs", {
    "kind": "TrackKind",
})
"""The arguments of `add-track`."""

ResetMixArgs = TypedDict("ResetMixArgs", {
    "track": "str",
})
"""The arguments of `reset-mix`."""

TrimTracksArgs = TypedDict("TrimTracksArgs", {
    "tracks": "list[str]",
    "db": "float",
    "headroom": "float",
})
"""The arguments of `trim-tracks`."""

DeleteTrackArgs = TypedDict("DeleteTrackArgs", {
    "track": "str",
})
"""The arguments of `delete-track`."""

PlaceClipArgs = TypedDict("PlaceClipArgs", {
    "media": "str",
    "track": "str",
    "start": "float",
})
"""The arguments of `place-clip`."""

DeleteMediaArgs = TypedDict("DeleteMediaArgs", {
    "media": "str",
})
"""The arguments of `delete-media`."""

UseAsProjectFormatArgs = TypedDict("UseAsProjectFormatArgs", {
    "media": "str",
})
"""The arguments of `use-as-project-format`."""

DeleteArgs = TypedDict("DeleteArgs", {
    "clip": "str",
})
"""The arguments of `delete`."""

RippleDeleteArgs = TypedDict("RippleDeleteArgs", {
    "clip": "str",
})
"""The arguments of `ripple-delete`."""

SplitClipArgs = TypedDict("SplitClipArgs", {
    "clip": "str",
    "at": "float",
})
"""The arguments of `split-clip`."""

TrimInClipArgs = TypedDict("TrimInClipArgs", {
    "clip": "str",
    "at": "float",
})
"""The arguments of `trim-in-clip`."""

TrimOutClipArgs = TypedDict("TrimOutClipArgs", {
    "clip": "str",
    "at": "float",
})
"""The arguments of `trim-out-clip`."""

SetSpeedArgs = TypedDict("SetSpeedArgs", {
    "clip": "str",
    "rate": "float",
})
"""The arguments of `set-speed`."""

ToggleReverseArgs = TypedDict("ToggleReverseArgs", {
    "clip": "str",
})
"""The arguments of `toggle-reverse`."""

ResetSpeedArgs = TypedDict("ResetSpeedArgs", {
    "clip": "str",
})
"""The arguments of `reset-speed`."""

SetParamArgs = TypedDict("SetParamArgs", {
    "clip": "str",
    "param": "str",
    "at": "float",
    "value": "float",
})
"""The arguments of `set-param`."""

AddEffectArgs = TypedDict("AddEffectArgs", {
    "clip": "str",
    "effect": "EffectKind",
})
"""The arguments of `add-effect`."""

RemoveEffectArgs = TypedDict("RemoveEffectArgs", {
    "clip": "str",
    "effect": "str",
})
"""The arguments of `remove-effect`."""

MoveEffectArgs = TypedDict("MoveEffectArgs", {
    "clip": "str",
    "effect": "str",
    "up": "bool",
})
"""The arguments of `move-effect`."""

SetEffectAmountArgs = TypedDict("SetEffectAmountArgs", {
    "clip": "str",
    "effect": "str",
    "amount": "float",
})
"""The arguments of `set-effect-amount`."""

ClearEffectsArgs = TypedDict("ClearEffectsArgs", {
    "clip": "str",
})
"""The arguments of `clear-effects`."""

UnlinkArgs = TypedDict("UnlinkArgs", {
    "clip": "str",
})
"""The arguments of `unlink`."""

LinkAudioArgs = TypedDict("LinkAudioArgs", {
    "clip": "str",
})
"""The arguments of `link-audio`."""

LinkWithArgs = TypedDict("LinkWithArgs", {
    "clip": "str",
    "other": "str",
})
"""The arguments of `link-with`."""

class Project(Resource):
    """Project — `video-editor.project`."""
    __surface__ = "so.tilo.video-editor@f0631a84d79983e3"
    name: str
    fps: float
    width: int
    height: int
    format_explicit: bool
    mark_in: float | None
    mark_out: float | None
    media: list[Entry[MediaItem]]
    tracks: list[Entry[Track]]
    clips: list[Entry[Clip]]
    markers: list[Entry[Marker]]

    def set_frame_size(self,
        *,
        width: int,
        height: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-frame-size", width=width, height=height)

    def set_frame_size_many(self, items: list[SetFrameSizeArgs]) -> list[dict[str, Any]]:
        """Runs `set-frame-size` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-frame-size", items)

    def set_frame_rate(self, *, fps: float) -> None:
        """Runs the app's action."""
        return self.action("set-frame-rate", fps=fps)

    def set_frame_rate_many(self, items: list[SetFrameRateArgs]) -> list[dict[str, Any]]:
        """Runs `set-frame-rate` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-frame-rate", items)

    def set_render_range(self,
        *,
        start: float,
        end: float,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-render-range", start=start, end=end)

    def set_render_range_many(self, items: list[SetRenderRangeArgs]) -> list[dict[str, Any]]:
        """Runs `set-render-range` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-render-range", items)

    # the app's `mark-in` action is named as a field of this kind: run it with
    # `action("mark-in")`.
    # the app's `mark-out` action is named as a field of this kind: run it with
    # `action("mark-out")`.
    def clear_marks(self) -> None:
        """Runs the app's action."""
        return self.action("clear-marks")

    def clear_marks_many(self, items: list[ClearMarksArgs]) -> list[dict[str, Any]]:
        """Runs `clear-marks` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear-marks", items)

    def add_title(self,
        *,
        text: str,
        colour: str | None = None,
        size: float | None = None,
        align: str | None = None,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["text"] = text
        if colour is not None:
            args["colour"] = colour
        if size is not None:
            args["size"] = size
        if align is not None:
            args["align"] = align
        return self.action("add-title", **args)

    def set_title(self,
        *,
        media: str,
        text: str | None = None,
        colour: str | None = None,
        size: float | None = None,
        align: str | None = None,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["media"] = media
        if text is not None:
            args["text"] = text
        if colour is not None:
            args["colour"] = colour
        if size is not None:
            args["size"] = size
        if align is not None:
            args["align"] = align
        return self.action("set-title", **args)

    def add_clip(self,
        *,
        media: str,
        track: str | None = None,
        at: float | None = None,
        in_: float | None = None,
        out: float | None = None,
        fade_in: float | None = None,
        fade_out: float | None = None,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["media"] = media
        if track is not None:
            args["track"] = track
        if at is not None:
            args["at"] = at
        if in_ is not None:
            args["in"] = in_
        if out is not None:
            args["out"] = out
        if fade_in is not None:
            args["fade_in"] = fade_in
        if fade_out is not None:
            args["fade_out"] = fade_out
        return self.action("add-clip", **args)

    def move_clip(self,
        *,
        clip: str,
        at: float,
        track: str | None = None,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["clip"] = clip
        args["at"] = at
        if track is not None:
            args["track"] = track
        return self.action("move-clip", **args)

    def set_clip(self,
        *,
        clip: str,
        fade_in: float | None = None,
        fade_out: float | None = None,
        opacity: float | None = None,
        volume: float | None = None,
        transition: str | None = None,
        transition_duration: float | None = None,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["clip"] = clip
        if fade_in is not None:
            args["fade_in"] = fade_in
        if fade_out is not None:
            args["fade_out"] = fade_out
        if opacity is not None:
            args["opacity"] = opacity
        if volume is not None:
            args["volume"] = volume
        if transition is not None:
            args["transition"] = transition
        if transition_duration is not None:
            args["transition_duration"] = transition_duration
        return self.action("set-clip", **args)

    def remove_clip(self,
        *,
        clip: str,
        ripple: bool | None = None,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["clip"] = clip
        if ripple is not None:
            args["ripple"] = ripple
        return self.action("remove-clip", **args)

    def add_marker(self,
        *,
        at: float,
        name: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("add-marker", at=at, name=name)

    def add_marker_many(self, items: list[AddMarkerArgs]) -> list[dict[str, Any]]:
        """Runs `add-marker` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-marker", items)

    def place_two_angles(self, *, at: float) -> None:
        """Runs the app's action."""
        return self.action("place-two-angles", at=at)

    def place_two_angles_many(self, items: list[PlaceTwoAnglesArgs]) -> list[dict[str, Any]]:
        """Runs `place-two-angles` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("place-two-angles", items)

    def unplace_two_angles(self, *, at: float) -> None:
        """Runs the app's action."""
        return self.action("unplace-two-angles", at=at)

    def unplace_two_angles_many(self, items: list[UnplaceTwoAnglesArgs]) -> list[dict[str, Any]]:
        """Runs `unplace-two-angles` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unplace-two-angles", items)

    def remove_marker(self, *, marker: str) -> None:
        """Runs the app's action."""
        return self.action("remove-marker", marker=marker)

    def remove_marker_many(self, items: list[RemoveMarkerArgs]) -> list[dict[str, Any]]:
        """Runs `remove-marker` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-marker", items)

    def add_track(self, *, kind: TrackKind) -> None:
        """Runs the app's action."""
        return self.action("add-track", kind=kind)

    def add_track_many(self, items: list[AddTrackArgs]) -> list[dict[str, Any]]:
        """Runs `add-track` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-track", items)

    def reset_mix(self, *, track: str) -> None:
        """Runs the app's action."""
        return self.action("reset-mix", track=track)

    def reset_mix_many(self, items: list[ResetMixArgs]) -> list[dict[str, Any]]:
        """Runs `reset-mix` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reset-mix", items)

    def trim_tracks(self,
        *,
        tracks: list[str],
        db: float,
        headroom: float,
    ) -> None:
        """Runs the app's action."""
        return self.action("trim-tracks", tracks=tracks, db=db, headroom=headroom)

    def trim_tracks_many(self, items: list[TrimTracksArgs]) -> list[dict[str, Any]]:
        """Runs `trim-tracks` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("trim-tracks", items)

    def delete_track(self, *, track: str) -> None:
        """Runs the app's action."""
        return self.action("delete-track", track=track)

    def delete_track_many(self, items: list[DeleteTrackArgs]) -> list[dict[str, Any]]:
        """Runs `delete-track` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-track", items)

    def place_clip(self,
        *,
        media: str,
        track: str,
        start: float,
    ) -> None:
        """Runs the app's action."""
        return self.action("place-clip", media=media, track=track, start=start)

    def place_clip_many(self, items: list[PlaceClipArgs]) -> list[dict[str, Any]]:
        """Runs `place-clip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("place-clip", items)

    def delete_media(self, *, media: str) -> None:
        """Runs the app's action."""
        return self.action("delete-media", media=media)

    def delete_media_many(self, items: list[DeleteMediaArgs]) -> list[dict[str, Any]]:
        """Runs `delete-media` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-media", items)

    def use_as_project_format(self, *, media: str) -> None:
        """Runs the app's action."""
        return self.action("use-as-project-format", media=media)

    def use_as_project_format_many(self, items: list[UseAsProjectFormatArgs]) -> list[dict[str, Any]]:
        """Runs `use-as-project-format` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("use-as-project-format", items)

    def delete(self, *, clip: str) -> None:
        """Runs the app's action."""
        return self.action("delete", clip=clip)

    def delete_many(self, items: list[DeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete", items)

    def ripple_delete(self, *, clip: str) -> None:
        """Runs the app's action."""
        return self.action("ripple-delete", clip=clip)

    def ripple_delete_many(self, items: list[RippleDeleteArgs]) -> list[dict[str, Any]]:
        """Runs `ripple-delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("ripple-delete", items)

    def split_clip(self,
        *,
        clip: str,
        at: float,
    ) -> None:
        """Runs the app's action."""
        return self.action("split-clip", clip=clip, at=at)

    def split_clip_many(self, items: list[SplitClipArgs]) -> list[dict[str, Any]]:
        """Runs `split-clip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("split-clip", items)

    def trim_in_clip(self,
        *,
        clip: str,
        at: float,
    ) -> None:
        """Runs the app's action."""
        return self.action("trim-in-clip", clip=clip, at=at)

    def trim_in_clip_many(self, items: list[TrimInClipArgs]) -> list[dict[str, Any]]:
        """Runs `trim-in-clip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("trim-in-clip", items)

    def trim_out_clip(self,
        *,
        clip: str,
        at: float,
    ) -> None:
        """Runs the app's action."""
        return self.action("trim-out-clip", clip=clip, at=at)

    def trim_out_clip_many(self, items: list[TrimOutClipArgs]) -> list[dict[str, Any]]:
        """Runs `trim-out-clip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("trim-out-clip", items)

    def set_speed(self,
        *,
        clip: str,
        rate: float,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-speed", clip=clip, rate=rate)

    def set_speed_many(self, items: list[SetSpeedArgs]) -> list[dict[str, Any]]:
        """Runs `set-speed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-speed", items)

    def toggle_reverse(self, *, clip: str) -> None:
        """Runs the app's action."""
        return self.action("toggle-reverse", clip=clip)

    def toggle_reverse_many(self, items: list[ToggleReverseArgs]) -> list[dict[str, Any]]:
        """Runs `toggle-reverse` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("toggle-reverse", items)

    def reset_speed(self, *, clip: str) -> None:
        """Runs the app's action."""
        return self.action("reset-speed", clip=clip)

    def reset_speed_many(self, items: list[ResetSpeedArgs]) -> list[dict[str, Any]]:
        """Runs `reset-speed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reset-speed", items)

    def set_param(self,
        *,
        clip: str,
        param: str,
        at: float,
        value: float,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-param", clip=clip, param=param, at=at, value=value)

    def set_param_many(self, items: list[SetParamArgs]) -> list[dict[str, Any]]:
        """Runs `set-param` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-param", items)

    def add_effect(self,
        *,
        clip: str,
        effect: EffectKind,
    ) -> None:
        """Runs the app's action."""
        return self.action("add-effect", clip=clip, effect=effect)

    def add_effect_many(self, items: list[AddEffectArgs]) -> list[dict[str, Any]]:
        """Runs `add-effect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-effect", items)

    def remove_effect(self,
        *,
        clip: str,
        effect: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("remove-effect", clip=clip, effect=effect)

    def remove_effect_many(self, items: list[RemoveEffectArgs]) -> list[dict[str, Any]]:
        """Runs `remove-effect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-effect", items)

    def move_effect(self,
        *,
        clip: str,
        effect: str,
        up: bool,
    ) -> None:
        """Runs the app's action."""
        return self.action("move-effect", clip=clip, effect=effect, up=up)

    def move_effect_many(self, items: list[MoveEffectArgs]) -> list[dict[str, Any]]:
        """Runs `move-effect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move-effect", items)

    def set_effect_amount(self,
        *,
        clip: str,
        effect: str,
        amount: float,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-effect-amount", clip=clip, effect=effect, amount=amount)

    def set_effect_amount_many(self, items: list[SetEffectAmountArgs]) -> list[dict[str, Any]]:
        """Runs `set-effect-amount` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-effect-amount", items)

    def clear_effects(self, *, clip: str) -> None:
        """Runs the app's action."""
        return self.action("clear-effects", clip=clip)

    def clear_effects_many(self, items: list[ClearEffectsArgs]) -> list[dict[str, Any]]:
        """Runs `clear-effects` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear-effects", items)

    def unlink(self, *, clip: str) -> None:
        """Runs the app's action."""
        return self.action("unlink", clip=clip)

    def unlink_many(self, items: list[UnlinkArgs]) -> list[dict[str, Any]]:
        """Runs `unlink` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unlink", items)

    def link_audio(self, *, clip: str) -> None:
        """Runs the app's action."""
        return self.action("link-audio", clip=clip)

    def link_audio_many(self, items: list[LinkAudioArgs]) -> list[dict[str, Any]]:
        """Runs `link-audio` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("link-audio", items)

    def link_with(self,
        *,
        clip: str,
        other: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("link-with", clip=clip, other=other)

    def link_with_many(self, items: list[LinkWithArgs]) -> list[dict[str, Any]]:
        """Runs `link-with` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("link-with", items)

    def export_timeline_content(self) -> None:
        """Runs the app's action."""
        return self.action("export-timeline-content")

    def render_timeline(self,
        *,
        start: float | None = None,
        end: float | None = None,
        format: str | None = None,
        size: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if start is not None:
            args["start"] = start
        if end is not None:
            args["end"] = end
        if format is not None:
            args["format"] = format
        if size is not None:
            args["size"] = size
        return self.action("render-timeline", **args)

    def export_video_content(self,
        *,
        format: str | None = None,
        width: int | None = None,
        height: int | None = None,
        fps: int | None = None,
        wait: int | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if format is not None:
            args["format"] = format
        if width is not None:
            args["width"] = width
        if height is not None:
            args["height"] = height
        if fps is not None:
            args["fps"] = fps
        if wait is not None:
            args["wait"] = wait
        return self.action("export-video-content", **args)

    def export_video_status(self,
        *,
        job: str,
        wait: int | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["job"] = job
        if wait is not None:
            args["wait"] = wait
        return self.action("export-video-status", **args)

    def cancel_video_export(self, *, job: str) -> None:
        """Runs the app's action."""
        return self.action("cancel-video-export", job=job)

    def view_frame_content(self,
        *,
        at: float,
        width: int | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["at"] = at
        if width is not None:
            args["width"] = width
        return self.action("view-frame-content", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"set_frame_size", "set_frame_size_many", "set_frame_rate", "set_frame_rate_many", "set_render_range", "set_render_range_many", "clear_marks", "clear_marks_many", "add_title", "set_title", "add_clip", "move_clip", "set_clip", "remove_clip", "add_marker", "add_marker_many", "place_two_angles", "place_two_angles_many", "unplace_two_angles", "unplace_two_angles_many", "remove_marker", "remove_marker_many", "add_track", "add_track_many", "reset_mix", "reset_mix_many", "trim_tracks", "trim_tracks_many", "delete_track", "delete_track_many", "place_clip", "place_clip_many", "delete_media", "delete_media_many", "use_as_project_format", "use_as_project_format_many", "delete", "delete_many", "ripple_delete", "ripple_delete_many", "split_clip", "split_clip_many", "trim_in_clip", "trim_in_clip_many", "trim_out_clip", "trim_out_clip_many", "set_speed", "set_speed_many", "toggle_reverse", "toggle_reverse_many", "reset_speed", "reset_speed_many", "set_param", "set_param_many", "add_effect", "add_effect_many", "remove_effect", "remove_effect_many", "move_effect", "move_effect_many", "set_effect_amount", "set_effect_amount_many", "clear_effects", "clear_effects_many", "unlink", "unlink_many", "link_audio", "link_audio_many", "link_with", "link_with_many", "export_timeline_content", "render_timeline", "export_video_content", "export_video_status", "cancel_video_export", "view_frame_content"})


