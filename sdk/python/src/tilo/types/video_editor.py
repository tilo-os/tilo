"""Types for Video Editor (`so.tilo.video-editor`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Project` is the app's `video-editor.project` at `video-editor/<resource>`, read with
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
    "ChromaKey",
    "Clip",
    "ClipEffect",
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
    effects: list[ClipEffect]
    transform: Transform
    speed: Speed
    key: ChromaKey
    mask: Mask
    anim: Any
    markers: list[Marker]
    link: str | None
    sound_split: bool
    link_follows: bool

class ClipEffect(TypedDict):
    id: str
    kind: EffectKind
    amount: float

EffectKind = Literal["grayscale", "sepia", "invert", "warm", "cool", "vignette"]

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

MaskShape = Literal["none", "rect", "ellipse"]

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

MediaKind = Literal["video", "audio", "image", "text"]

class Speed(TypedDict):
    rate: float
    reverse: bool

TextAlign = Literal["left", "center", "right"]

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

Transition = Literal["none", "cross_dissolve", "fade_in", "dip_to_black", "wipe"]

class Project(Resource):
    """Project — `video-editor.project`."""
    name: str
    fps: float
    width: int
    height: int
    format_explicit: bool
    mark_in: float | None
    mark_out: float | None
    media: list[MediaItem]
    tracks: list[Track]
    clips: list[Clip]
    markers: list[Marker]

    def set_frame_size(self,
        *,
        width: int,
        height: int,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-frame-size", width=width, height=height)

    def set_frame_rate(self, *, fps: float) -> Run | None:
        """Runs the app's action."""
        return self.action("set-frame-rate", fps=fps)

    def set_render_range(self,
        *,
        start: float,
        end: float,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-render-range", start=start, end=end)

    # the app's `mark-in` action is named as a field of this kind: run it with
    # `action("mark-in")`.
    # the app's `mark-out` action is named as a field of this kind: run it with
    # `action("mark-out")`.
    def clear_marks(self) -> Run | None:
        """Runs the app's action."""
        return self.action("clear-marks")

    def add_title(self,
        *,
        text: str,
        colour: str | None = None,
        size: float | None = None,
        align: str | None = None,
    ) -> Any | Run:
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
    ) -> Any | Run:
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
    ) -> Any | Run:
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
    ) -> Any | Run:
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
    ) -> Any | Run:
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
    ) -> Any | Run:
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
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("add-marker", at=at, name=name)

    def place_two_angles(self, *, at: float) -> Run | None:
        """Runs the app's action."""
        return self.action("place-two-angles", at=at)

    def unplace_two_angles(self, *, at: float) -> Run | None:
        """Runs the app's action."""
        return self.action("unplace-two-angles", at=at)

    def remove_marker(self, *, marker: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-marker", marker=marker)

    def add_track(self, *, kind: TrackKind) -> Run | None:
        """Runs the app's action."""
        return self.action("add-track", kind=kind)

    def reset_mix(self, *, track: str) -> Run | None:
        """Runs the app's action."""
        return self.action("reset-mix", track=track)

    def trim_lanes(self,
        *,
        tracks: list[str],
        db: float,
        headroom: float,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("trim-lanes", tracks=tracks, db=db, headroom=headroom)

    def delete_track(self, *, track: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-track", track=track)

    def place_clip(self,
        *,
        media: str,
        track: str,
        start: float,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("place-clip", media=media, track=track, start=start)

    def delete_media(self, *, media: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-media", media=media)

    def use_as_project_format(self, *, media: str) -> Run | None:
        """Runs the app's action."""
        return self.action("use-as-project-format", media=media)

    def delete(self, *, clip: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete", clip=clip)

    def ripple_delete(self, *, clip: str) -> Run | None:
        """Runs the app's action."""
        return self.action("ripple-delete", clip=clip)

    def split_clip(self,
        *,
        clip: str,
        at: float,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("split-clip", clip=clip, at=at)

    def trim_in_clip(self,
        *,
        clip: str,
        at: float,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("trim-in-clip", clip=clip, at=at)

    def trim_out_clip(self,
        *,
        clip: str,
        at: float,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("trim-out-clip", clip=clip, at=at)

    def set_speed(self,
        *,
        clip: str,
        rate: float,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-speed", clip=clip, rate=rate)

    def toggle_reverse(self, *, clip: str) -> Run | None:
        """Runs the app's action."""
        return self.action("toggle-reverse", clip=clip)

    def reset_speed(self, *, clip: str) -> Run | None:
        """Runs the app's action."""
        return self.action("reset-speed", clip=clip)

    def set_param(self,
        *,
        clip: str,
        param: str,
        at: float,
        value: float,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-param", clip=clip, param=param, at=at, value=value)

    def add_effect(self,
        *,
        clip: str,
        effect: EffectKind,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("add-effect", clip=clip, effect=effect)

    def remove_effect(self,
        *,
        clip: str,
        effect: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-effect", clip=clip, effect=effect)

    def move_effect(self,
        *,
        clip: str,
        effect: str,
        up: bool,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("move-effect", clip=clip, effect=effect, up=up)

    def set_effect_amount(self,
        *,
        clip: str,
        effect: str,
        amount: float,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-effect-amount", clip=clip, effect=effect, amount=amount)

    def clear_effects(self, *, clip: str) -> Run | None:
        """Runs the app's action."""
        return self.action("clear-effects", clip=clip)

    def unlink(self, *, clip: str) -> Run | None:
        """Runs the app's action."""
        return self.action("unlink", clip=clip)

    def link_audio(self, *, clip: str) -> Run | None:
        """Runs the app's action."""
        return self.action("link-audio", clip=clip)

    def link_with(self,
        *,
        clip: str,
        other: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("link-with", clip=clip, other=other)

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


