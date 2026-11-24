"""Types for Video Editor (`so.tilo.video-editor`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `video-editor/<resource>`, read with
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
    "Align",
    "Form",
    "TitleLook",
    "Look",
    "Notice",
    "Media",
    "Effect",
    "Track",
    "Key",
    "Clip",
    "Marker",
    "Project",
    "Editor",
    "Probed",
    "ProbeAsk",
    "StillAsk",
    "StillAnswer",
    "WaveAsk",
    "Peaks",
    "RenderAsk",
    "JobAnswer",
    "JobState",
    "Shot",
    "ShotMedia",
    "ShotTrack",
    "ShotClip",
    "ShotEffect",
    "ShotKey",
    "ThumbAsk",
    "SampleAsk",
    "SampleAnswer",
    "ShotMarker",
    "Saved",
    "SavedMedia",
    "SavedTrack",
    "SavedClip",
    "Copied",
    "CopiedClip",
    "SavedMarker",
    "Drawn",
    "VideoEditor",
    "DrawSwatchArgs",
    "HearArgs",
    "SetTitleStyleArgs",
    "SetTitleLookArgs",
    "RedrawMediaArgs",
    "SetAlignArgs",
    "SetWordsArgs",
    "SetTrackGainArgs",
    "SetTrackPanArgs",
    "SetTrackMutedArgs",
    "SetTrackSoloArgs",
    "SetTrackLockedArgs",
    "ResetMixArgs",
    "SetInterpArgs",
    "SetSpeedArgs",
    "NormalSpeedArgs",
    "SetLookArgs",
    "SetMaskArgs",
    "InvertMaskArgs",
    "SetTransitionArgs",
    "SetReverseArgs",
    "RedrawClipArgs",
    "KeyAtArgs",
    "UnkeyArgs",
    "KeyColourArgs",
    "UnlinkClipArgs",
    "UnplaceSideBySideArgs",
    "AddEffectArgs",
    "RemoveEffectArgs",
    "SetEffectArgs",
    "MoveEffectArgs",
    "ClearEffectsArgs",
    "NoteMarkerArgs",
    "SplitAtArgs",
    "TroubleArgs",
    "AdoptProbeArgs",
    "RefreshSourceArgs",
    "RelinkArgs",
    "MarkMissingArgs",
    "DrawStillArgs",
    "BeginRenderArgs",
    "RenameProjectArgs",
    "ImportFilesArgs",
    "ImportOneArgs",
    "CommitDragArgs",
    "SplitClipArgs",
    "CutAtArgs",
    "RemoveClipsArgs",
    "RippleDeleteArgs",
    "NudgeArgs",
    "TrimStartArgs",
    "TrimEndArgs",
    "PasteClipsArgs",
    "DuplicateClipsArgs",
    "RedrawArgs",
    "SampleKeyArgs",
    "FilmstripArgs",
    "DrawNextStripArgs",
    "RemoveMarkerArgs",
    "MarkInArgs",
    "MarkOutArgs",
    "ClearMarksArgs",
    "SetFrameRateArgs",
    "SetFrameSizeArgs",
    "AdoptFormatArgs",
    "DeleteTrackArgs",
    "LinkClipsArgs",
    "SplitAudioArgs",
    "PlaceSideBySideArgs",
    "DeleteMediaArgs",
    "PreviewArgs",
    "SeekAtArgs",
    "PreviewCutArgs",
    "ExportCutArgs",
    "PollRenderArgs",
    "CancelRenderArgs",
    "OpenProjectArgs",
    "ForgetProjectArgs",
]

Align = Literal["left", "centre", "right"]
"""How a title's lines sit in its box."""

Form = Literal["video", "audio", "image", "title"]
"""What a source is: a picture that moves, a sound, a still, or words this app draws itself."""

TitleLook = Literal["size", "red", "green", "blue", "backdrop"]
"""One look of a title the inspector sets with a slider, drawn again as it changes."""

Look = Literal["opacity", "volume", "speed", "fade_in", "fade_out", "brightness", "contrast", "saturation", "scale_x", "scale_y", "pos_x", "pos_y", "rotation", "crop_left", "crop_right", "crop_top", "crop_bottom", "key_tolerance", "key_softness", "key_spill", "mask_x", "mask_y", "mask_w", "mask_h", "mask_rotation", "mask_feather", "transition_duration"]
"""One look of a clip that the inspector sets with a slider: the clip's own picture and sound, which the preview draws again as the person changes it."""

DrawSwatchArgs = TypedDict("DrawSwatchArgs", {
    "width": "int",
})
"""The arguments of `draw_swatch`."""

HearArgs = TypedDict("HearArgs", {
})
"""The arguments of `hear`."""

SetTitleStyleArgs = TypedDict("SetTitleStyleArgs", {
    "bold": "bool",
    "italic": "bool",
})
"""The arguments of `set_title_style`."""

SetTitleLookArgs = TypedDict("SetTitleLookArgs", {
    "look": "TitleLook",
    "value": "float",
})
"""The arguments of `set_title_look`."""

RedrawMediaArgs = TypedDict("RedrawMediaArgs", {
})
"""The arguments of `redraw_media`."""

SetAlignArgs = TypedDict("SetAlignArgs", {
    "align": "Align",
})
"""The arguments of `set_align`."""

SetWordsArgs = TypedDict("SetWordsArgs", {
    "words": "str",
})
"""The arguments of `set_words`."""

SetTrackGainArgs = TypedDict("SetTrackGainArgs", {
    "db": "float",
})
"""The arguments of `set_track_gain`."""

SetTrackPanArgs = TypedDict("SetTrackPanArgs", {
    "pan": "float",
})
"""The arguments of `set_track_pan`."""

SetTrackMutedArgs = TypedDict("SetTrackMutedArgs", {
    "muted": "bool",
})
"""The arguments of `set_track_muted`."""

SetTrackSoloArgs = TypedDict("SetTrackSoloArgs", {
    "solo": "bool",
})
"""The arguments of `set_track_solo`."""

SetTrackLockedArgs = TypedDict("SetTrackLockedArgs", {
    "locked": "bool",
})
"""The arguments of `set_track_locked`."""

ResetMixArgs = TypedDict("ResetMixArgs", {
})
"""The arguments of `reset_mix`."""

SetInterpArgs = TypedDict("SetInterpArgs", {
    "interp": "str",
})
"""The arguments of `set_interp`."""

SetSpeedArgs = TypedDict("SetSpeedArgs", {
    "speed": "float",
})
"""The arguments of `set_speed`."""

NormalSpeedArgs = TypedDict("NormalSpeedArgs", {
})
"""The arguments of `normal_speed`."""

SetLookArgs = TypedDict("SetLookArgs", {
    "look": "Look",
    "value": "float",
})
"""The arguments of `set_look`."""

SetMaskArgs = TypedDict("SetMaskArgs", {
    "shape": "str",
})
"""The arguments of `set_mask`."""

InvertMaskArgs = TypedDict("InvertMaskArgs", {
    "invert": "bool",
})
"""The arguments of `invert_mask`."""

SetTransitionArgs = TypedDict("SetTransitionArgs", {
    "transition": "str",
    "seconds": "float",
})
"""The arguments of `set_transition`."""

SetReverseArgs = TypedDict("SetReverseArgs", {
    "reverse": "bool",
})
"""The arguments of `set_reverse`."""

RedrawClipArgs = TypedDict("RedrawClipArgs", {
})
"""The arguments of `redraw_clip`."""

KeyAtArgs = TypedDict("KeyAtArgs", {
    "at": "float",
    "param": "str",
    "interp": "str",
})
"""The arguments of `key_at`."""

UnkeyArgs = TypedDict("UnkeyArgs", {
    "k": "Key",
})
"""The arguments of `unkey`."""

KeyColourArgs = TypedDict("KeyColourArgs", {
    "red": "float",
    "green": "float",
    "blue": "float",
})
"""The arguments of `key_colour`."""

UnlinkClipArgs = TypedDict("UnlinkClipArgs", {
})
"""The arguments of `unlink_clip`."""

UnplaceSideBySideArgs = TypedDict("UnplaceSideBySideArgs", {
})
"""The arguments of `unplace_side_by_side`."""

AddEffectArgs = TypedDict("AddEffectArgs", {
    "form": "str",
})
"""The arguments of `add_effect`."""

RemoveEffectArgs = TypedDict("RemoveEffectArgs", {
    "e": "Effect",
})
"""The arguments of `remove_effect`."""

SetEffectArgs = TypedDict("SetEffectArgs", {
    "e": "Effect",
    "amount": "float",
})
"""The arguments of `set_effect`."""

MoveEffectArgs = TypedDict("MoveEffectArgs", {
    "e": "Effect",
    "up": "bool",
})
"""The arguments of `move_effect`."""

ClearEffectsArgs = TypedDict("ClearEffectsArgs", {
})
"""The arguments of `clear_effects`."""

NoteMarkerArgs = TypedDict("NoteMarkerArgs", {
    "note": "str",
})
"""The arguments of `note_marker`."""

SplitAtArgs = TypedDict("SplitAtArgs", {
    "c": "Clip",
    "at": "float",
})
"""The arguments of `split_at`."""

TroubleArgs = TypedDict("TroubleArgs", {
    "why": "str",
})
"""The arguments of `trouble`."""

AdoptProbeArgs = TypedDict("AdoptProbeArgs", {
    "m": "Media",
    "said": "str",
})
"""The arguments of `adopt_probe`."""

RefreshSourceArgs = TypedDict("RefreshSourceArgs", {
    "m": "Media",
})
"""The arguments of `refresh_source`."""

RelinkArgs = TypedDict("RelinkArgs", {
    "m": "Media",
    "file": "Any",
})
"""The arguments of `relink`."""

MarkMissingArgs = TypedDict("MarkMissingArgs", {
    "m": "Media",
    "why": "str",
})
"""The arguments of `mark_missing`."""

DrawStillArgs = TypedDict("DrawStillArgs", {
    "at": "float",
})
"""The arguments of `draw_still`."""

BeginRenderArgs = TypedDict("BeginRenderArgs", {
    "render_kind": "str",
    "resolution": "str",
    "container": "str",
    "fps": "int",
})
"""The arguments of `begin_render`."""

RenameProjectArgs = TypedDict("RenameProjectArgs", {
    "name": "str",
})
"""The arguments of `rename_project`."""

ImportFilesArgs = TypedDict("ImportFilesArgs", {
    "files": "list[Any]",
})
"""The arguments of `import_files`."""

ImportOneArgs = TypedDict("ImportOneArgs", {
    "file": "Any",
})
"""The arguments of `import_one`."""

CommitDragArgs = TypedDict("CommitDragArgs", {
    "grab": "str",
    "clip": "str",
    "from": "float",
    "at": "float",
    "with": "list[Clip]",
})
"""The arguments of `commit_drag`."""

SplitClipArgs = TypedDict("SplitClipArgs", {
    "c": "Clip",
    "at": "float",
})
"""The arguments of `split_clip`."""

CutAtArgs = TypedDict("CutAtArgs", {
    "at": "float",
})
"""The arguments of `cut_at`."""

RemoveClipsArgs = TypedDict("RemoveClipsArgs", {
    "cs": "list[Clip]",
})
"""The arguments of `remove_clips`."""

RippleDeleteArgs = TypedDict("RippleDeleteArgs", {
    "cs": "list[Clip]",
})
"""The arguments of `ripple_delete`."""

NudgeArgs = TypedDict("NudgeArgs", {
    "cs": "list[Clip]",
    "frames": "int",
})
"""The arguments of `nudge`."""

TrimStartArgs = TypedDict("TrimStartArgs", {
    "cs": "list[Clip]",
    "at": "float",
})
"""The arguments of `trim_start`."""

TrimEndArgs = TypedDict("TrimEndArgs", {
    "cs": "list[Clip]",
    "at": "float",
})
"""The arguments of `trim_end`."""

PasteClipsArgs = TypedDict("PasteClipsArgs", {
    "at": "float",
})
"""The arguments of `paste_clips`."""

DuplicateClipsArgs = TypedDict("DuplicateClipsArgs", {
    "cs": "list[Clip]",
})
"""The arguments of `duplicate_clips`."""

RedrawArgs = TypedDict("RedrawArgs", {
})
"""The arguments of `redraw`."""

SampleKeyArgs = TypedDict("SampleKeyArgs", {
    "c": "Clip",
    "at": "float",
    "x": "float",
    "y": "float",
})
"""The arguments of `sample_key`."""

FilmstripArgs = TypedDict("FilmstripArgs", {
    "c": "Clip",
})
"""The arguments of `filmstrip`."""

DrawNextStripArgs = TypedDict("DrawNextStripArgs", {
})
"""The arguments of `draw_next_strip`."""

RemoveMarkerArgs = TypedDict("RemoveMarkerArgs", {
    "k": "Marker",
})
"""The arguments of `remove_marker`."""

MarkInArgs = TypedDict("MarkInArgs", {
    "at": "float",
})
"""The arguments of `mark_in`."""

MarkOutArgs = TypedDict("MarkOutArgs", {
    "at": "float",
})
"""The arguments of `mark_out`."""

ClearMarksArgs = TypedDict("ClearMarksArgs", {
})
"""The arguments of `clear_marks`."""

SetFrameRateArgs = TypedDict("SetFrameRateArgs", {
    "fps": "float",
})
"""The arguments of `set_frame_rate`."""

SetFrameSizeArgs = TypedDict("SetFrameSizeArgs", {
    "width": "int",
    "height": "int",
})
"""The arguments of `set_frame_size`."""

AdoptFormatArgs = TypedDict("AdoptFormatArgs", {
    "m": "Media",
})
"""The arguments of `adopt_format`."""

DeleteTrackArgs = TypedDict("DeleteTrackArgs", {
    "t": "Track",
})
"""The arguments of `delete_track`."""

LinkClipsArgs = TypedDict("LinkClipsArgs", {
    "a": "Clip",
    "b": "Clip",
})
"""The arguments of `link_clips`."""

SplitAudioArgs = TypedDict("SplitAudioArgs", {
    "c": "Clip",
})
"""The arguments of `split_audio`."""

PlaceSideBySideArgs = TypedDict("PlaceSideBySideArgs", {
    "left": "Clip",
    "right": "Clip",
})
"""The arguments of `place_side_by_side`."""

DeleteMediaArgs = TypedDict("DeleteMediaArgs", {
    "m": "Media",
})
"""The arguments of `delete_media`."""

PreviewArgs = TypedDict("PreviewArgs", {
    "at": "float",
})
"""The arguments of `preview`."""

SeekAtArgs = TypedDict("SeekAtArgs", {
    "x": "float",
    "span": "float",
    "scroll": "float",
})
"""The arguments of `seek_at`."""

PreviewCutArgs = TypedDict("PreviewCutArgs", {
})
"""The arguments of `preview_cut`."""

ExportCutArgs = TypedDict("ExportCutArgs", {
    "resolution": "str",
    "container": "str",
    "fps": "int",
})
"""The arguments of `export_cut`."""

PollRenderArgs = TypedDict("PollRenderArgs", {
})
"""The arguments of `poll_render`."""

CancelRenderArgs = TypedDict("CancelRenderArgs", {
})
"""The arguments of `cancel_render`."""

OpenProjectArgs = TypedDict("OpenProjectArgs", {
    "p": "Project",
})
"""The arguments of `open_project`."""

ForgetProjectArgs = TypedDict("ForgetProjectArgs", {
    "p": "Project",
})
"""The arguments of `forget_project`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
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


class Media(Resource):
    """Media — `tl.so.tilo.video-editor.Media`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    efejoszz: str  # name
    pcamqw4n: Form  # form
    wj2aohfd: float  # duration
    hpkuwe3q: int  # width
    krrrznuy: int  # height
    vmxn3cwg: float  # fps
    foxa6zt7: int  # palette
    zltur6gc: str  # path
    ap457hnd: bool  # has_audio
    @property
    def _3vgrlwgv(self) -> str:
        """still (`3vgrlwgv` on the wire)."""
        return self.fields["3vgrlwgv"]

    @_3vgrlwgv.setter
    def _3vgrlwgv(self, value: str) -> None:
        self.edit("3vgrlwgv", value=value)
    vddvspwk: list[float]  # peaks
    v4yfudyw: bool  # missing
    e7atusb5: str  # words
    qrihyxs3: float  # size
    x2yorlvh: float  # red
    @property
    def _5nv4umhz(self) -> float:
        """green (`5nv4umhz` on the wire)."""
        return self.fields["5nv4umhz"]

    @_5nv4umhz.setter
    def _5nv4umhz(self, value: float) -> None:
        self.edit("5nv4umhz", value=value)
    ktanswey: float  # blue
    lqpu2kuo: float  # alpha
    @property
    def _3j5mkeir(self) -> Align:
        """align (`3j5mkeir` on the wire)."""
        return self.fields["3j5mkeir"]

    @_3j5mkeir.setter
    def _3j5mkeir(self, value: Align) -> None:
        self.edit("3j5mkeir", value=value)
    u37npdvi: bool  # bold
    wzbpbjsf: bool  # italic
    hlbhw6tg: float  # backdrop

    def draw_swatch(self, *, width: int) -> None:
        """The source's picture as the bin shows it."""
        return self.action("draw_swatch", width=width)

    def draw_swatch_many(self, items: list[DrawSwatchArgs]) -> list[dict[str, Any]]:
        """Runs `draw_swatch` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("draw_swatch", items)

    def hear(self) -> None:
        """The source's loudness across the file, as the waveform draws it."""
        return self.action("hear")

    def hear_many(self, items: list[HearArgs]) -> list[dict[str, Any]]:
        """Runs `hear` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("hear", items)

    def set_title_style(self,
        *,
        bold: bool,
        italic: bool,
    ) -> None:
        """One style of a title's lines, drawn again as it changes."""
        return self.action("set_title_style", bold=bold, italic=italic)

    def set_title_style_many(self, items: list[SetTitleStyleArgs]) -> list[dict[str, Any]]:
        """Runs `set_title_style` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_title_style", items)

    def set_title_look(self,
        *,
        look: TitleLook,
        value: float,
    ) -> None:
        """Runs the app's action."""
        return self.action("set_title_look", look=look, value=value)

    def set_title_look_many(self, items: list[SetTitleLookArgs]) -> list[dict[str, Any]]:
        """Runs `set_title_look` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_title_look", items)

    def redraw_media(self) -> None:
        """Runs the app's action."""
        return self.action("redraw_media")

    def redraw_media_many(self, items: list[RedrawMediaArgs]) -> list[dict[str, Any]]:
        """Runs `redraw_media` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("redraw_media", items)

    def set_align(self, *, align: Align) -> None:
        """Sets where a title's lines sit in its box."""
        return self.action("set_align", align=align)

    def set_align_many(self, items: list[SetAlignArgs]) -> list[dict[str, Any]]:
        """Runs `set_align` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_align", items)

    def set_words(self, *, words: str) -> None:
        """What the title's lines say."""
        return self.action("set_words", words=words)

    def set_words_many(self, items: list[SetWordsArgs]) -> list[dict[str, Any]]:
        """Runs `set_words` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_words", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"draw_swatch", "draw_swatch_many", "hear", "hear_many", "set_title_style", "set_title_style_many", "set_title_look", "set_title_look_many", "redraw_media", "redraw_media_many", "set_align", "set_align_many", "set_words", "set_words_many"})


class Effect(Resource):
    """Effect — `tl.so.tilo.video-editor.Effect`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    ktghxc36: str  # form
    lj3ggezn: float  # amount


class Track(Resource):
    """Track — `tl.so.tilo.video-editor.Track`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    hyiqyr2d: str  # name
    voeh7uw2: Form  # form
    h5vj7qoc: int  # index
    o7i5df56: bool  # muted
    azpfbjsn: bool  # solo
    wowizwqg: bool  # locked
    rofejlx6: float  # gain_db
    ovon2nm7: float  # pan

    def set_track_gain(self, *, db: float) -> None:
        """One track's level, where the sound of everything on it sits in the mix."""
        return self.action("set_track_gain", db=db)

    def set_track_gain_many(self, items: list[SetTrackGainArgs]) -> list[dict[str, Any]]:
        """Runs `set_track_gain` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_track_gain", items)

    def set_track_pan(self, *, pan: float) -> None:
        """Where a track's sound sits across the stereo field: -1 hard left, 1 hard right."""
        return self.action("set_track_pan", pan=pan)

    def set_track_pan_many(self, items: list[SetTrackPanArgs]) -> list[dict[str, Any]]:
        """Runs `set_track_pan` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_track_pan", items)

    def set_track_muted(self, *, muted: bool) -> None:
        """Runs the app's action."""
        return self.action("set_track_muted", muted=muted)

    def set_track_muted_many(self, items: list[SetTrackMutedArgs]) -> list[dict[str, Any]]:
        """Runs `set_track_muted` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_track_muted", items)

    def set_track_solo(self, *, solo: bool) -> None:
        """Runs the app's action."""
        return self.action("set_track_solo", solo=solo)

    def set_track_solo_many(self, items: list[SetTrackSoloArgs]) -> list[dict[str, Any]]:
        """Runs `set_track_solo` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_track_solo", items)

    def set_track_locked(self, *, locked: bool) -> None:
        """Runs the app's action."""
        return self.action("set_track_locked", locked=locked)

    def set_track_locked_many(self, items: list[SetTrackLockedArgs]) -> list[dict[str, Any]]:
        """Runs `set_track_locked` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_track_locked", items)

    def reset_mix(self) -> None:
        """Puts a track back at unity and the centre."""
        return self.action("reset_mix")

    def reset_mix_many(self, items: list[ResetMixArgs]) -> list[dict[str, Any]]:
        """Runs `reset_mix` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reset_mix", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"set_track_gain", "set_track_gain_many", "set_track_pan", "set_track_pan_many", "set_track_muted", "set_track_muted_many", "set_track_solo", "set_track_solo_many", "set_track_locked", "set_track_locked_many", "reset_mix", "reset_mix_many"})


class Key(Resource):
    """Key — `tl.so.tilo.video-editor.Key`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    @property
    def _362ioruy(self) -> str:
        """param (`362ioruy` on the wire)."""
        return self.fields["362ioruy"]

    @_362ioruy.setter
    def _362ioruy(self, value: str) -> None:
        self.edit("362ioruy", value=value)
    ksxcsp7k: float  # t
    al7ic2wh: float  # v
    @property
    def _4rmokhsj(self) -> str:
        """interp (`4rmokhsj` on the wire)."""
        return self.fields["4rmokhsj"]

    @_4rmokhsj.setter
    def _4rmokhsj(self, value: str) -> None:
        self.edit("4rmokhsj", value=value)

    def set_interp(self, *, interp: str) -> None:
        """How a key's curve leaves it."""
        return self.action("set_interp", interp=interp)

    def set_interp_many(self, items: list[SetInterpArgs]) -> list[dict[str, Any]]:
        """Runs `set_interp` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_interp", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"set_interp", "set_interp_many"})


class Clip(Resource):
    """Clip — `tl.so.tilo.video-editor.Clip`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    azbufhus: str  # name
    tpxg6k4q: Track  # track
    y4qdvmxb: Media  # media
    vle43xl6: float  # start
    tmrpa5lj: float  # in_point
    ijg7zj64: float  # out_point
    mzmglpkg: float  # opacity
    hb6kydcz: float  # volume
    @property
    def _5lwntoqr(self) -> str:
        """transition (`5lwntoqr` on the wire)."""
        return self.fields["5lwntoqr"]

    @_5lwntoqr.setter
    def _5lwntoqr(self, value: str) -> None:
        self.edit("5lwntoqr", value=value)
    ihl7lqbw: float  # transition_duration
    fmhqhubf: float  # fade_in
    bygbi2tw: float  # fade_out
    jrsfcsej: int  # palette
    @property
    def _55seasjt(self) -> float:
        """speed_rate (`55seasjt` on the wire)."""
        return self.fields["55seasjt"]

    @_55seasjt.setter
    def _55seasjt(self, value: float) -> None:
        self.edit("55seasjt", value=value)
    dmthewcr: bool  # reverse
    ttu42344: float  # brightness
    sosquagj: float  # contrast
    t6mekjo2: float  # saturation
    kikxhud5: float  # scale_x
    @property
    def _67q6ewpx(self) -> float:
        """scale_y (`67q6ewpx` on the wire)."""
        return self.fields["67q6ewpx"]

    @_67q6ewpx.setter
    def _67q6ewpx(self, value: float) -> None:
        self.edit("67q6ewpx", value=value)
    nu7xhxvx: float  # pos_x
    vrqmvalw: float  # pos_y
    @property
    def _3ru7tn4m(self) -> float:
        """rotation (`3ru7tn4m` on the wire)."""
        return self.fields["3ru7tn4m"]

    @_3ru7tn4m.setter
    def _3ru7tn4m(self, value: float) -> None:
        self.edit("3ru7tn4m", value=value)
    kx3c765z: float  # crop_left
    @property
    def _4rl2tdsz(self) -> float:
        """crop_right (`4rl2tdsz` on the wire)."""
        return self.fields["4rl2tdsz"]

    @_4rl2tdsz.setter
    def _4rl2tdsz(self, value: float) -> None:
        self.edit("4rl2tdsz", value=value)
    d4ryk5ir: float  # crop_top
    r27hvfe7: float  # crop_bottom
    @property
    def _7uz3gyat(self) -> list[Entry[Effect]]:
        """effects (`7uz3gyat` on the wire)."""
        return self.fields["7uz3gyat"]

    @_7uz3gyat.setter
    def _7uz3gyat(self, value: list[Entry[Effect]]) -> None:
        self.edit("7uz3gyat", value=value)
    tskiyma5: bool  # key_enabled
    nflhw3wz: float  # key_tolerance
    gk3fgv4a: float  # key_softness
    zxs2bxu6: float  # key_spill
    ag6tnt5w: list[Entry[Key]]  # keys
    @property
    def _54ikwabs(self) -> list[str]:
        """strips (`54ikwabs` on the wire)."""
        return self.fields["54ikwabs"]

    @_54ikwabs.setter
    def _54ikwabs(self, value: list[str]) -> None:
        self.edit("54ikwabs", value=value)
    bvukpc5x: str  # strips_key
    dhh754gx: float  # key_red
    podots7u: float  # key_green
    tgpgd3hb: float  # key_blue
    hfnhir7p: str  # mask_shape
    hclucc6k: float  # mask_x
    g2n4egth: float  # mask_y
    lwnm5qmy: float  # mask_w
    casg7gsr: float  # mask_h
    u3oht37u: float  # mask_rotation
    iraobc5s: float  # mask_feather
    @property
    def _6njmwsei(self) -> bool:
        """mask_invert (`6njmwsei` on the wire)."""
        return self.fields["6njmwsei"]

    @_6njmwsei.setter
    def _6njmwsei(self, value: bool) -> None:
        self.edit("6njmwsei", value=value)
    vmc7izdi: str  # link
    wovh5j2b: bool  # link_follows
    poy7f6dc: bool  # sound_split

    def set_speed(self, *, speed: float) -> None:
        """Sets the rate a clip plays at: 2 twice as fast and half as long, 0.5 in slow motion."""
        return self.action("set_speed", speed=speed)

    def set_speed_many(self, items: list[SetSpeedArgs]) -> list[dict[str, Any]]:
        """Runs `set_speed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_speed", items)

    def normal_speed(self) -> None:
        """Runs the app's action."""
        return self.action("normal_speed")

    def normal_speed_many(self, items: list[NormalSpeedArgs]) -> list[dict[str, Any]]:
        """Runs `normal_speed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("normal_speed", items)

    def set_look(self,
        *,
        look: Look,
        value: float,
    ) -> None:
        """Sets one look of the clip, and draws the frame the preview shows again, so an edit is seen as it is made rather than at the next move of the playhead."""
        return self.action("set_look", look=look, value=value)

    def set_look_many(self, items: list[SetLookArgs]) -> list[dict[str, Any]]:
        """Runs `set_look` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_look", items)

    def set_mask(self, *, shape: str) -> None:
        """Sets which shape limits where a clip shows: `off`, `rect` or `ellipse`."""
        return self.action("set_mask", shape=shape)

    def set_mask_many(self, items: list[SetMaskArgs]) -> list[dict[str, Any]]:
        """Runs `set_mask` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_mask", items)

    def invert_mask(self, *, invert: bool) -> None:
        """Keeps the inside of the clip's mask, or the outside of it."""
        return self.action("invert_mask", invert=invert)

    def invert_mask_many(self, items: list[InvertMaskArgs]) -> list[dict[str, Any]]:
        """Runs `invert_mask` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("invert_mask", items)

    def set_transition(self,
        *,
        transition: str,
        seconds: float,
    ) -> None:
        """Sets the incoming transition and how long it runs."""
        return self.action("set_transition", transition=transition, seconds=seconds)

    def set_transition_many(self, items: list[SetTransitionArgs]) -> list[dict[str, Any]]:
        """Runs `set_transition` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_transition", items)

    def set_reverse(self, *, reverse: bool) -> None:
        """Plays the clip's source backwards, or forwards again, and draws the frame it holds now."""
        return self.action("set_reverse", reverse=reverse)

    def set_reverse_many(self, items: list[SetReverseArgs]) -> list[dict[str, Any]]:
        """Runs `set_reverse` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_reverse", items)

    def redraw_clip(self) -> None:
        """Runs the app's action."""
        return self.action("redraw_clip")

    def redraw_clip_many(self, items: list[RedrawClipArgs]) -> list[dict[str, Any]]:
        """Runs `redraw_clip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("redraw_clip", items)

    def key_at(self,
        *,
        at: float,
        param: str,
        interp: str,
    ) -> None:
        """Sets a key of `param` at `at` (timeline seconds), at the value the field holds now: the value a curve changes from, as the inspection shows it."""
        return self.action("key_at", at=at, param=param, interp=interp)

    def key_at_many(self, items: list[KeyAtArgs]) -> list[dict[str, Any]]:
        """Runs `key_at` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("key_at", items)

    def unkey(self, *, k: Key) -> None:
        """Removes a key."""
        return self.action("unkey", k=k)

    def unkey_many(self, items: list[UnkeyArgs]) -> list[dict[str, Any]]:
        """Runs `unkey` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unkey", items)

    def key_colour(self,
        *,
        red: float,
        green: float,
        blue: float,
    ) -> None:
        """Sets the colour the key removes to one of the usual screens."""
        return self.action("key_colour", red=red, green=green, blue=blue)

    def key_colour_many(self, items: list[KeyColourArgs]) -> list[dict[str, Any]]:
        """Runs `key_colour` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("key_colour", items)

    def thumb_frame(self,
        *,
        at: float,
        width: int,
        height: int,
    ) -> str:
        """One frame of a source at `at` seconds in it, kept as a filmstrip picture."""
        return self.action("thumb_frame", at=at, width=width, height=height)

    def unlink_clip(self) -> None:
        """Runs the app's action."""
        return self.action("unlink_clip")

    def unlink_clip_many(self, items: list[UnlinkClipArgs]) -> list[dict[str, Any]]:
        """Runs `unlink_clip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unlink_clip", items)

    def unplace_side_by_side(self) -> None:
        """Runs the app's action."""
        return self.action("unplace_side_by_side")

    def unplace_side_by_side_many(self, items: list[UnplaceSideBySideArgs]) -> list[dict[str, Any]]:
        """Runs `unplace_side_by_side` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unplace_side_by_side", items)

    def add_effect(self, *, form: str) -> None:
        """Runs the app's action."""
        return self.action("add_effect", form=form)

    def add_effect_many(self, items: list[AddEffectArgs]) -> list[dict[str, Any]]:
        """Runs `add_effect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_effect", items)

    def remove_effect(self, *, e: Effect) -> None:
        """Runs the app's action."""
        return self.action("remove_effect", e=e)

    def remove_effect_many(self, items: list[RemoveEffectArgs]) -> list[dict[str, Any]]:
        """Runs `remove_effect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_effect", items)

    def set_effect(self,
        *,
        e: Effect,
        amount: float,
    ) -> None:
        """How strongly one effect of the clip applies, applied to the picture at once."""
        return self.action("set_effect", e=e, amount=amount)

    def set_effect_many(self, items: list[SetEffectArgs]) -> list[dict[str, Any]]:
        """Runs `set_effect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_effect", items)

    def move_effect(self,
        *,
        e: Effect,
        up: bool,
    ) -> None:
        """Moves an effect up or down the clip's stack: the order decides what the picture goes through first."""
        return self.action("move_effect", e=e, up=up)

    def move_effect_many(self, items: list[MoveEffectArgs]) -> list[dict[str, Any]]:
        """Runs `move_effect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_effect", items)

    def clear_effects(self) -> None:
        """Takes every effect off a clip."""
        return self.action("clear_effects")

    def clear_effects_many(self, items: list[ClearEffectsArgs]) -> list[dict[str, Any]]:
        """Runs `clear_effects` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_effects", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"set_speed", "set_speed_many", "normal_speed", "normal_speed_many", "set_look", "set_look_many", "set_mask", "set_mask_many", "invert_mask", "invert_mask_many", "set_transition", "set_transition_many", "set_reverse", "set_reverse_many", "redraw_clip", "redraw_clip_many", "key_at", "key_at_many", "unkey", "unkey_many", "key_colour", "key_colour_many", "thumb_frame", "unlink_clip", "unlink_clip_many", "unplace_side_by_side", "unplace_side_by_side_many", "add_effect", "add_effect_many", "remove_effect", "remove_effect_many", "set_effect", "set_effect_many", "move_effect", "move_effect_many", "clear_effects", "clear_effects_many"})


class Marker(Resource):
    """Marker — `tl.so.tilo.video-editor.Marker`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    @property
    def _6gy3r5jn(self) -> str:
        """name (`6gy3r5jn` on the wire)."""
        return self.fields["6gy3r5jn"]

    @_6gy3r5jn.setter
    def _6gy3r5jn(self, value: str) -> None:
        self.edit("6gy3r5jn", value=value)
    @property
    def _43eckzwd(self) -> float:
        """t (`43eckzwd` on the wire)."""
        return self.fields["43eckzwd"]

    @_43eckzwd.setter
    def _43eckzwd(self, value: float) -> None:
        self.edit("43eckzwd", value=value)
    jvjfto7y: int  # palette
    h2qmhyqy: str  # note

    def note_marker(self, *, note: str) -> None:
        """Writes what a mark says."""
        return self.action("note_marker", note=note)

    def note_marker_many(self, items: list[NoteMarkerArgs]) -> list[dict[str, Any]]:
        """Runs `note_marker` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("note_marker", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"note_marker", "note_marker_many"})


class Project(Resource):
    """Project — `tl.so.tilo.video-editor.Project`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    hdhd7ufr: str  # name
    sggyvq5o: float  # fps
    rv7tdvl6: int  # width
    z7cmqzxo: int  # height
    m7e2vebr: bool  # explicit_format
    enmbeevc: float  # mark_in
    vfq5phqm: float  # mark_out
    f7xzwsye: str  # format
    qm4juanr: list[Entry[Media]]  # media
    @property
    def _4z3qlwar(self) -> list[Entry[Track]]:
        """tracks (`4z3qlwar` on the wire)."""
        return self.fields["4z3qlwar"]

    @_4z3qlwar.setter
    def _4z3qlwar(self, value: list[Entry[Track]]) -> None:
        self.edit("4z3qlwar", value=value)
    @property
    def _74vaoosx(self) -> list[Entry[Clip]]:
        """clips (`74vaoosx` on the wire)."""
        return self.fields["74vaoosx"]

    @_74vaoosx.setter
    def _74vaoosx(self, value: list[Entry[Clip]]) -> None:
        self.edit("74vaoosx", value=value)
    qr6qe3cp: list[Entry[Marker]]  # markers
    @property
    def _47vsdgaf(self) -> str:
        """still (`47vsdgaf` on the wire)."""
        return self.fields["47vsdgaf"]

    @_47vsdgaf.setter
    def _47vsdgaf(self, value: str) -> None:
        self.edit("47vsdgaf", value=value)
    dppbe5en: float  # still_at
    gkp4va33: str  # proxy
    xa5fpldf: str  # job
    dkhcppi4: str  # job_kind
    myjdbgx2: int  # job_done
    @property
    def _63bjrnm4(self) -> int:
        """job_total (`63bjrnm4` on the wire)."""
        return self.fields["63bjrnm4"]

    @_63bjrnm4.setter
    def _63bjrnm4(self, value: int) -> None:
        self.edit("63bjrnm4", value=value)
    rhjs5vir: str  # exported
    @property
    def _4jky56y7(self) -> str:
        """trouble (`4jky56y7` on the wire)."""
        return self.fields["4jky56y7"]

    @_4jky56y7.setter
    def _4jky56y7(self, value: str) -> None:
        self.edit("4jky56y7", value=value)

    def restore_clip(self,
        *,
        saved: str,
        track: Track,
        media: Media,
        start: float,
        link: str,
    ) -> Clip | None:
        """Makes the clip `c` keeps on `track`, of `media`, at `start`, with its keys and effects."""
        return self.action("restore_clip", saved=saved, track=track, media=media, start=start, link=link)

    def split_at(self,
        *,
        c: Clip,
        at: float,
    ) -> None:
        """Cuts a clip in two at `at`, each half keeping the source its own part shows."""
        return self.action("split_at", c=c, at=at)

    def split_at_many(self, items: list[SplitAtArgs]) -> list[dict[str, Any]]:
        """Runs `split_at` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("split_at", items)

    def shifted(self,
        *,
        c: Clip,
        start: float,
    ) -> bool:
        """Moves `c` (and its linked sound with it) to `start`, refusing an overlap."""
        return self.action("shifted", c=c, start=start)

    def trouble(self, *, why: str) -> None:
        """Runs the app's action."""
        return self.action("trouble", why=why)

    def trouble_many(self, items: list[TroubleArgs]) -> list[dict[str, Any]]:
        """Runs `trouble` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("trouble", items)

    def absorb(self, *, file: Any) -> str:
        """Asks the device what a file is, adds it as a source and draws its swatch and waveform. What the device said when it could not be added, or empty when it was: a batch names each file that did not arrive and why, so a person knows what to do about it."""
        return self.action("absorb", file=file)

    def adopt_probe(self,
        *,
        m: Media,
        said: str,
    ) -> None:
        """Writes what the device read of a source onto it, as the engine said it: what it is, how long it runs, its size and rate, and whether it carries sound. An action is handed the engine's words rather than the decoded `Probed`, which is a value no resource holds."""
        return self.action("adopt_probe", m=m, said=said)

    def adopt_probe_many(self, items: list[AdoptProbeArgs]) -> list[dict[str, Any]]:
        """Runs `adopt_probe` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("adopt_probe", items)

    def refresh_source(self, *, m: Media) -> None:
        """Checks that a source's bytes are here: reads what they are again, draws its swatch and waveform, and marks it not on this device when the engine cannot reach them."""
        return self.action("refresh_source", m=m)

    def refresh_source_many(self, items: list[RefreshSourceArgs]) -> list[dict[str, Any]]:
        """Runs `refresh_source` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("refresh_source", items)

    def relink(self,
        *,
        m: Media,
        file: Any,
    ) -> None:
        """Points a source at a file the person chose: the clips are cuts of the source, so they keep it, and the source reads what its new file says."""
        return self.action("relink", m=m, file=file)

    def relink_many(self, items: list[RelinkArgs]) -> list[dict[str, Any]]:
        """Runs `relink` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("relink", items)

    def mark_missing(self,
        *,
        m: Media,
        why: str,
    ) -> None:
        """Says the source's bytes are not on this device, and why the engine could not read them."""
        return self.action("mark_missing", m=m, why=why)

    def mark_missing_many(self, items: list[MarkMissingArgs]) -> list[dict[str, Any]]:
        """Runs `mark_missing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("mark_missing", items)

    def draw_still(self, *, at: float) -> None:
        """The frame the preview shows at `at`, drawn by the engine."""
        return self.action("draw_still", at=at)

    def draw_still_many(self, items: list[DrawStillArgs]) -> list[dict[str, Any]]:
        """Runs `draw_still` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("draw_still", items)

    def begin_render(self,
        *,
        render_kind: str,
        resolution: str,
        container: str,
        fps: int,
    ) -> None:
        """Starts a render: `preview` keeps the cut small for the window to play, `export` writes the file a person saves."""
        return self.action("begin_render", render_kind=render_kind, resolution=resolution, container=container, fps=fps)

    def begin_render_many(self, items: list[BeginRenderArgs]) -> list[dict[str, Any]]:
        """Runs `begin_render` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("begin_render", items)

    def rename_project(self, *, name: str) -> None:
        """Runs the app's action."""
        return self.action("rename_project", name=name)

    def rename_project_many(self, items: list[RenameProjectArgs]) -> list[dict[str, Any]]:
        """Runs `rename_project` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename_project", items)

    def import_files(self, *, files: list[Any]) -> None:
        """Adds every file a person chose or dropped as a source, and says which files did not arrive and what the device said of each."""
        return self.action("import_files", files=files)

    def import_files_many(self, items: list[ImportFilesArgs]) -> list[dict[str, Any]]:
        """Runs `import_files` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import_files", items)

    def import_one(self, *, file: Any) -> None:
        """Runs the app's action."""
        return self.action("import_one", file=file)

    def import_one_many(self, items: list[ImportOneArgs]) -> list[dict[str, Any]]:
        """Runs `import_one` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import_one", items)

    def save_project(self) -> Any:
        """The cut as a file a person keeps: its sources and their places, where they stand on their own device."""
        return self.action("save_project")

    def export_file(self) -> Any:
        """The cut as a film: what an export made."""
        return self.action("export_file")

    def place(self,
        *,
        m: Media,
        track: Track,
        at: float,
    ) -> Clip:
        """Places `m` on `track` at `at`, in the first place it fits."""
        return self.action("place", m=m, track=track, at=at)

    def place_media(self,
        *,
        m: Media,
        at: float,
    ) -> Clip | None:
        """Places the bin's source on the first track of its form, at `at`."""
        return self.action("place_media", m=m, at=at)

    def commit_drag(self,
        *,
        grab: str,
        clip: str,
        from_: float,
        at: float,
        with_: list[Clip],
    ) -> None:
        """What a drag of the timeline committed: a clip moved or trimmed. A clip moved among `with`, the clips chosen, moves them all."""
        args: dict[str, Any] = {}
        args["grab"] = grab
        args["clip"] = clip
        args["from"] = from_
        args["at"] = at
        args["with"] = with_
        return self.action("commit_drag", **args)

    def commit_drag_many(self, items: list[CommitDragArgs]) -> list[dict[str, Any]]:
        """Runs `commit_drag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("commit_drag", items)

    def moved(self,
        *,
        cs: list[Clip],
        by: float,
    ) -> bool:
        """Moves the clips `cs`, and the sound linked to them, by `by` seconds together, refusing an overlap with a clip that stays."""
        return self.action("moved", cs=cs, by=by)

    def split_clip(self,
        *,
        c: Clip,
        at: float,
    ) -> None:
        """Cuts the clip at the playhead in two."""
        return self.action("split_clip", c=c, at=at)

    def split_clip_many(self, items: list[SplitClipArgs]) -> list[dict[str, Any]]:
        """Runs `split_clip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("split_clip", items)

    def cut_at(self, *, at: float) -> None:
        """Cuts every clip the playhead stands on."""
        return self.action("cut_at", at=at)

    def cut_at_many(self, items: list[CutAtArgs]) -> list[dict[str, Any]]:
        """Runs `cut_at` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("cut_at", items)

    def remove_clips(self, *, cs: list[Clip]) -> None:
        """Deletes the clips, and the sound linked to them with them."""
        return self.action("remove_clips", cs=cs)

    def remove_clips_many(self, items: list[RemoveClipsArgs]) -> list[dict[str, Any]]:
        """Runs `remove_clips` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_clips", items)

    def ripple_delete(self, *, cs: list[Clip]) -> None:
        """Deletes the clips and closes the gap each leaves behind it on its track."""
        return self.action("ripple_delete", cs=cs)

    def ripple_delete_many(self, items: list[RippleDeleteArgs]) -> list[dict[str, Any]]:
        """Runs `ripple_delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("ripple_delete", items)

    def nudge(self,
        *,
        cs: list[Clip],
        frames: int,
    ) -> None:
        """Slips the clips along the timeline by frames."""
        return self.action("nudge", cs=cs, frames=frames)

    def nudge_many(self, items: list[NudgeArgs]) -> list[dict[str, Any]]:
        """Runs `nudge` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("nudge", items)

    def trim_start(self,
        *,
        cs: list[Clip],
        at: float,
    ) -> None:
        """Trims the start of each clip the playhead stands in to the playhead."""
        return self.action("trim_start", cs=cs, at=at)

    def trim_start_many(self, items: list[TrimStartArgs]) -> list[dict[str, Any]]:
        """Runs `trim_start` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("trim_start", items)

    def trim_end(self,
        *,
        cs: list[Clip],
        at: float,
    ) -> None:
        """Trims the end of each clip the playhead stands in to the playhead."""
        return self.action("trim_end", cs=cs, at=at)

    def trim_end_many(self, items: list[TrimEndArgs]) -> list[dict[str, Any]]:
        """Runs `trim_end` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("trim_end", items)

    def copy_clips(self, *, cs: list[Clip]) -> None:
        """Puts the clips, with their looks, keys and effects, on the person's clipboard."""
        return self.action("copy_clips", cs=cs)

    def cut_clips(self, *, cs: list[Clip]) -> None:
        """Puts the clips on the person's clipboard and takes them out of the cut."""
        return self.action("cut_clips", cs=cs)

    def paste_track(self,
        *,
        track: str,
        form_named: str,
    ) -> Track:
        """The track a pasted clip goes on: the one it came from while it is in this cut and open, else the first open one of its form, else a new one."""
        return self.action("paste_track", track=track, form_named=form_named)

    def paste_clips(self, *, at: float) -> None:
        """Places copied clips from `at`, the earliest there and the rest keeping their distance from it; clips linked to each other stay linked, to each other alone."""
        return self.action("paste_clips", at=at)

    def paste_clips_many(self, items: list[PasteClipsArgs]) -> list[dict[str, Any]]:
        """Runs `paste_clips` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("paste_clips", items)

    def duplicate_clips(self, *, cs: list[Clip]) -> None:
        """Puts a copy of the clips right after them, on the same tracks."""
        return self.action("duplicate_clips", cs=cs)

    def duplicate_clips_many(self, items: list[DuplicateClipsArgs]) -> list[dict[str, Any]]:
        """Runs `duplicate_clips` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("duplicate_clips", items)

    def redraw(self) -> None:
        """Draws the frame the preview last showed again, so an edit made after it is seen at once. A cut whose frame the window never showed draws nothing: there is no place to draw it at."""
        return self.action("redraw")

    def redraw_many(self, items: list[RedrawArgs]) -> list[dict[str, Any]]:
        """Runs `redraw` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("redraw", items)

    def sample_key(self,
        *,
        c: Clip,
        at: float,
        x: float,
        y: float,
    ) -> None:
        """Takes the colour of the cut's frame where the person pointed, as the key that removes it."""
        return self.action("sample_key", c=c, at=at, x=x, y=y)

    def sample_key_many(self, items: list[SampleKeyArgs]) -> list[dict[str, Any]]:
        """Runs `sample_key` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("sample_key", items)

    def filmstrip(self, *, c: Clip) -> None:
        """Draws eight frames of the clip across its own part, kept until a trim asks for them again."""
        return self.action("filmstrip", c=c)

    def filmstrip_many(self, items: list[FilmstripArgs]) -> list[dict[str, Any]]:
        """Runs `filmstrip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("filmstrip", items)

    def draw_next_strip(self) -> None:
        """Draws the filmstrip of the first clip whose frames are missing or stale."""
        return self.action("draw_next_strip")

    def draw_next_strip_many(self, items: list[DrawNextStripArgs]) -> list[dict[str, Any]]:
        """Runs `draw_next_strip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("draw_next_strip", items)

    def add_title(self,
        *,
        words: str,
        at: float,
    ) -> Media:
        """Adds a title to the bin and places it at `at`."""
        return self.action("add_title", words=words, at=at)

    def add_marker(self, *, at: float) -> Marker:
        """Adds a marker at `at`."""
        return self.action("add_marker", at=at)

    def remove_marker(self, *, k: Marker) -> None:
        """Runs the app's action."""
        return self.action("remove_marker", k=k)

    def remove_marker_many(self, items: list[RemoveMarkerArgs]) -> list[dict[str, Any]]:
        """Runs `remove_marker` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_marker", items)

    def mark_in(self, *, at: float) -> None:
        """Runs the app's action."""
        return self.action("mark_in", at=at)

    def mark_in_many(self, items: list[MarkInArgs]) -> list[dict[str, Any]]:
        """Runs `mark_in` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("mark_in", items)

    def mark_out(self, *, at: float) -> None:
        """Runs the app's action."""
        return self.action("mark_out", at=at)

    def mark_out_many(self, items: list[MarkOutArgs]) -> list[dict[str, Any]]:
        """Runs `mark_out` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("mark_out", items)

    def clear_marks(self) -> None:
        """Runs the app's action."""
        return self.action("clear_marks")

    def clear_marks_many(self, items: list[ClearMarksArgs]) -> list[dict[str, Any]]:
        """Runs `clear_marks` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_marks", items)

    def set_frame_rate(self, *, fps: float) -> None:
        """Runs the app's action."""
        return self.action("set_frame_rate", fps=fps)

    def set_frame_rate_many(self, items: list[SetFrameRateArgs]) -> list[dict[str, Any]]:
        """Runs `set_frame_rate` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_frame_rate", items)

    def set_frame_size(self,
        *,
        width: int,
        height: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("set_frame_size", width=width, height=height)

    def set_frame_size_many(self, items: list[SetFrameSizeArgs]) -> list[dict[str, Any]]:
        """Runs `set_frame_size` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_frame_size", items)

    def adopt_format(self, *, m: Media) -> None:
        """Takes its shape from a source: what 'Use its format' does."""
        return self.action("adopt_format", m=m)

    def adopt_format_many(self, items: list[AdoptFormatArgs]) -> list[dict[str, Any]]:
        """Runs `adopt_format` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("adopt_format", items)

    def add_track(self, *, form: Form) -> Track:
        """Runs the app's action."""
        return self.action("add_track", form=form)

    def delete_track(self, *, t: Track) -> None:
        """Runs the app's action."""
        return self.action("delete_track", t=t)

    def delete_track_many(self, items: list[DeleteTrackArgs]) -> list[dict[str, Any]]:
        """Runs `delete_track` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_track", items)

    def link_clips(self,
        *,
        a: Clip,
        b: Clip,
    ) -> None:
        """Makes two clips one shot's picture and sound: they move and cut together."""
        return self.action("link_clips", a=a, b=b)

    def link_clips_many(self, items: list[LinkClipsArgs]) -> list[dict[str, Any]]:
        """Runs `link_clips` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("link_clips", items)

    def split_audio(self, *, c: Clip) -> None:
        """Moves a picture clip's own sound onto an audio clip of its own, linked to it."""
        return self.action("split_audio", c=c)

    def split_audio_many(self, items: list[SplitAudioArgs]) -> list[dict[str, Any]]:
        """Runs `split_audio` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("split_audio", items)

    def place_side_by_side(self,
        *,
        left: Clip,
        right: Clip,
    ) -> None:
        """Puts two clips side by side: each half the frame, one left and one right."""
        return self.action("place_side_by_side", left=left, right=right)

    def place_side_by_side_many(self, items: list[PlaceSideBySideArgs]) -> list[dict[str, Any]]:
        """Runs `place_side_by_side` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("place_side_by_side", items)

    def delete_media(self, *, m: Media) -> None:
        """Takes a source out of the bin, and every clip that showed it with it."""
        return self.action("delete_media", m=m)

    def delete_media_many(self, items: list[DeleteMediaArgs]) -> list[dict[str, Any]]:
        """Runs `delete_media` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_media", items)

    def preview(self, *, at: float) -> None:
        """Draws the frame the preview shows at `at`."""
        return self.action("preview", at=at)

    def preview_many(self, items: list[PreviewArgs]) -> list[dict[str, Any]]:
        """Runs `preview` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("preview", items)

    def seek_at(self,
        *,
        x: float,
        span: float,
        scroll: float,
    ) -> None:
        """Draws the frame at the ruler's place `x`, as a drag along it asks for."""
        return self.action("seek_at", x=x, span=span, scroll=scroll)

    def seek_at_many(self, items: list[SeekAtArgs]) -> list[dict[str, Any]]:
        """Runs `seek_at` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("seek_at", items)

    def preview_cut(self) -> None:
        """Renders the cut small, so the window plays it."""
        return self.action("preview_cut")

    def preview_cut_many(self, items: list[PreviewCutArgs]) -> list[dict[str, Any]]:
        """Runs `preview_cut` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("preview_cut", items)

    def export_cut(self,
        *,
        resolution: str,
        container: str,
        fps: int,
    ) -> None:
        """Renders the cut to a file."""
        return self.action("export_cut", resolution=resolution, container=container, fps=fps)

    def export_cut_many(self, items: list[ExportCutArgs]) -> list[dict[str, Any]]:
        """Runs `export_cut` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("export_cut", items)

    def poll_render(self) -> None:
        """Asks how far the running render has got."""
        return self.action("poll_render")

    def poll_render_many(self, items: list[PollRenderArgs]) -> list[dict[str, Any]]:
        """Runs `poll_render` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("poll_render", items)

    def cancel_render(self) -> None:
        """Runs the app's action."""
        return self.action("cancel_render")

    def cancel_render_many(self, items: list[CancelRenderArgs]) -> list[dict[str, Any]]:
        """Runs `cancel_render` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("cancel_render", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"restore_clip", "split_at", "split_at_many", "shifted", "trouble", "trouble_many", "absorb", "adopt_probe", "adopt_probe_many", "refresh_source", "refresh_source_many", "relink", "relink_many", "mark_missing", "mark_missing_many", "draw_still", "draw_still_many", "begin_render", "begin_render_many", "rename_project", "rename_project_many", "import_files", "import_files_many", "import_one", "import_one_many", "save_project", "export_file", "place", "place_media", "commit_drag", "commit_drag_many", "moved", "split_clip", "split_clip_many", "cut_at", "cut_at_many", "remove_clips", "remove_clips_many", "ripple_delete", "ripple_delete_many", "nudge", "nudge_many", "trim_start", "trim_start_many", "trim_end", "trim_end_many", "copy_clips", "cut_clips", "paste_track", "paste_clips", "paste_clips_many", "duplicate_clips", "duplicate_clips_many", "redraw", "redraw_many", "sample_key", "sample_key_many", "filmstrip", "filmstrip_many", "draw_next_strip", "draw_next_strip_many", "add_title", "add_marker", "remove_marker", "remove_marker_many", "mark_in", "mark_in_many", "mark_out", "mark_out_many", "clear_marks", "clear_marks_many", "set_frame_rate", "set_frame_rate_many", "set_frame_size", "set_frame_size_many", "adopt_format", "adopt_format_many", "add_track", "delete_track", "delete_track_many", "link_clips", "link_clips_many", "split_audio", "split_audio_many", "place_side_by_side", "place_side_by_side_many", "delete_media", "delete_media_many", "preview", "preview_many", "seek_at", "seek_at_many", "preview_cut", "preview_cut_many", "export_cut", "export_cut_many", "poll_render", "poll_render_many", "cancel_render", "cancel_render_many"})


class Editor(Resource):
    """Editor — `tl.so.tilo.video-editor.Editor`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    ws67tq4i: list[Entry[Project]]  # projects
    i3vrs6nt: Project | None  # open
    qyx2uds6: list[Entry[Any]]  # sessions
    @property
    def _3kenkvdb(self) -> Any | None:
        """playing (`3kenkvdb` on the wire)."""
        return self.fields["3kenkvdb"]

    @_3kenkvdb.setter
    def _3kenkvdb(self, value: Any | None) -> None:
        self.edit("3kenkvdb", value=value)

    def new_project(self) -> Project:
        """Starts a cut: its shape and its first two tracks."""
        return self.action("new_project")

    def open_project(self, *, p: Project) -> None:
        """Opens the cut the window shows, and draws its first frame where the window has not shown one yet: a cut opens on its picture, not on an empty preview."""
        return self.action("open_project", p=p)

    def open_project_many(self, items: list[OpenProjectArgs]) -> list[dict[str, Any]]:
        """Runs `open_project` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open_project", items)

    def session(self) -> Any:
        """The session this window plays through: the one it has, or one made now."""
        return self.action("session")

    def forget_project(self, *, p: Project) -> None:
        """Runs the app's action."""
        return self.action("forget_project", p=p)

    def forget_project_many(self, items: list[ForgetProjectArgs]) -> list[dict[str, Any]]:
        """Runs `forget_project` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget_project", items)

    def open_media(self, *, file: Any) -> Project:
        """Opens a video, a sound or a picture in a cut of its own: what Files and the person's own system offer the editor for."""
        return self.action("open_media", file=file)

    def open_project_file(self, *, file: Any) -> Project:
        """Opens a project file a person chose: one undoable edit that makes the cut it holds."""
        return self.action("open_project_file", file=file)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_project", "open_project", "open_project_many", "session", "forget_project", "forget_project_many", "open_media", "open_project_file"})


class Probed(Resource):
    """Probed — `tl.so.tilo.video-editor.Probed`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    bc2o5yk6: str  # kind
    jb7opba6: float  # duration
    duh5etpe: int  # width
    ibouz53w: int  # height
    eci2btbm: float  # fps
    cg3roaa5: bool  # has_audio
    r72d7i4q: str  # codec


class ProbeAsk(Resource):
    """ProbeAsk — `tl.so.tilo.video-editor.ProbeAsk`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    dx4zjyh5: str  # url


class StillAsk(Resource):
    """StillAsk — `tl.so.tilo.video-editor.StillAsk`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    itbf6wf4: str  # project
    @property
    def _5klklgrm(self) -> float:
        """at (`5klklgrm` on the wire)."""
        return self.fields["5klklgrm"]

    @_5klklgrm.setter
    def _5klklgrm(self, value: float) -> None:
        self.edit("5klklgrm", value=value)
    zrjp2ayo: int  # width
    c7hx2nde: int  # height


class StillAnswer(Resource):
    """StillAnswer — `tl.so.tilo.video-editor.StillAnswer`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    fqmupsg5: str  # url


class WaveAsk(Resource):
    """WaveAsk — `tl.so.tilo.video-editor.WaveAsk`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    hv2hn45e: str  # url
    u2gsuh7h: int  # buckets


class Peaks(Resource):
    """Peaks — `tl.so.tilo.video-editor.Peaks`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    xatqduw2: list[float]  # peaks


class RenderAsk(Resource):
    """RenderAsk — `tl.so.tilo.video-editor.RenderAsk`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    @property
    def _4xhy5qpi(self) -> str:
        """project (`4xhy5qpi` on the wire)."""
        return self.fields["4xhy5qpi"]

    @_4xhy5qpi.setter
    def _4xhy5qpi(self, value: str) -> None:
        self.edit("4xhy5qpi", value=value)
    nlg2n4yg: str  # resolution
    n3bktwt4: str  # container
    ylbeg2vl: int  # fps
    mqmxnv4s: str  # name


class JobAnswer(Resource):
    """JobAnswer — `tl.so.tilo.video-editor.JobAnswer`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    @property
    def _2qflgu2y(self) -> str:
        """job (`2qflgu2y` on the wire)."""
        return self.fields["2qflgu2y"]

    @_2qflgu2y.setter
    def _2qflgu2y(self, value: str) -> None:
        self.edit("2qflgu2y", value=value)


class JobState(Resource):
    """JobState — `tl.so.tilo.video-editor.JobState`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    @property
    def _2lqqe2u4(self) -> str:
        """state (`2lqqe2u4` on the wire)."""
        return self.fields["2lqqe2u4"]

    @_2lqqe2u4.setter
    def _2lqqe2u4(self, value: str) -> None:
        self.edit("2lqqe2u4", value=value)
    wddixskv: int  # done
    yfo7z67v: int  # total
    ivfsfn6q: str  # url
    tlb73mjx: str  # error


class Shot(Resource):
    """Shot — `tl.so.tilo.video-editor.Shot`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    fagho4gr: str  # name
    fsxs4zep: float  # fps
    ebteysdk: int  # width
    a4torznk: int  # height
    v6ck4d4l: float  # mark_in
    hzxujhk3: float  # mark_out
    yjp3sehi: list[str]  # media
    q3ecfgq3: list[str]  # tracks
    wgoywspl: list[str]  # clips
    w26d474g: list[str]  # markers


class ShotMedia(Resource):
    """ShotMedia — `tl.so.tilo.video-editor.ShotMedia`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    jws44pym: str  # id
    @property
    def _6irok5cm(self) -> str:
        """name (`6irok5cm` on the wire)."""
        return self.fields["6irok5cm"]

    @_6irok5cm.setter
    def _6irok5cm(self, value: str) -> None:
        self.edit("6irok5cm", value=value)
    @property
    def _2yxap4ne(self) -> str:
        """kind (`2yxap4ne` on the wire)."""
        return self.fields["2yxap4ne"]

    @_2yxap4ne.setter
    def _2yxap4ne(self, value: str) -> None:
        self.edit("2yxap4ne", value=value)
    j3rpcgim: float  # duration
    pwfziujj: int  # width
    @property
    def _7cokd24o(self) -> int:
        """height (`7cokd24o` on the wire)."""
        return self.fields["7cokd24o"]

    @_7cokd24o.setter
    def _7cokd24o(self, value: int) -> None:
        self.edit("7cokd24o", value=value)
    ldk5mv2m: float  # fps
    @property
    def _5gvee3he(self) -> int:
        """palette (`5gvee3he` on the wire)."""
        return self.fields["5gvee3he"]

    @_5gvee3he.setter
    def _5gvee3he(self, value: int) -> None:
        self.edit("5gvee3he", value=value)
    betxnhkz: str  # path
    @property
    def _7ppacpgk(self) -> bool:
        """has_audio (`7ppacpgk` on the wire)."""
        return self.fields["7ppacpgk"]

    @_7ppacpgk.setter
    def _7ppacpgk(self, value: bool) -> None:
        self.edit("7ppacpgk", value=value)
    cosd36fu: str  # content
    tmz6b4vk: float  # size
    uk5cqaxq: list[float]  # color
    a5zwa247: str  # align
    mjhc4g63: str  # font
    lykfeg5x: bool  # bold
    npxtqckt: bool  # italic
    @property
    def _4b6rl7fz(self) -> float:
        """backdrop (`4b6rl7fz` on the wire)."""
        return self.fields["4b6rl7fz"]

    @_4b6rl7fz.setter
    def _4b6rl7fz(self, value: float) -> None:
        self.edit("4b6rl7fz", value=value)


class ShotTrack(Resource):
    """ShotTrack — `tl.so.tilo.video-editor.ShotTrack`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    wzjt7soo: str  # id
    @property
    def _2phnzuqk(self) -> str:
        """name (`2phnzuqk` on the wire)."""
        return self.fields["2phnzuqk"]

    @_2phnzuqk.setter
    def _2phnzuqk(self, value: str) -> None:
        self.edit("2phnzuqk", value=value)
    mb6mtsoz: str  # kind
    jr2eyeqg: int  # index
    wyqopmjv: bool  # muted
    @property
    def _3enbjrqj(self) -> bool:
        """solo (`3enbjrqj` on the wire)."""
        return self.fields["3enbjrqj"]

    @_3enbjrqj.setter
    def _3enbjrqj(self, value: bool) -> None:
        self.edit("3enbjrqj", value=value)
    lr3pdaah: bool  # locked
    zse3uh5c: float  # gain_db
    aix6cf7x: float  # pan


class ShotClip(Resource):
    """ShotClip — `tl.so.tilo.video-editor.ShotClip`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    jeeiaroo: str  # id
    akxlh23d: str  # track
    pptlby5v: str  # media
    @property
    def _3dgm7b2b(self) -> float:
        """start (`3dgm7b2b` on the wire)."""
        return self.fields["3dgm7b2b"]

    @_3dgm7b2b.setter
    def _3dgm7b2b(self, value: float) -> None:
        self.edit("3dgm7b2b", value=value)
    zbz5amfa: float  # in_point
    pkiaohdd: float  # out_point
    xi2qtarh: float  # opacity
    chhkku2t: float  # volume
    s2fdck6n: str  # transition
    rrf4qn3g: float  # transition_duration
    xubtbe5w: float  # fade_in
    jllwurew: float  # fade_out
    clwasyxv: int  # palette
    uge3tmju: float  # speed_rate
    rxbq7fqf: bool  # speed_reverse
    @property
    def _5utmqjdy(self) -> float:
        """brightness (`5utmqjdy` on the wire)."""
        return self.fields["5utmqjdy"]

    @_5utmqjdy.setter
    def _5utmqjdy(self, value: float) -> None:
        self.edit("5utmqjdy", value=value)
    @property
    def _5kf4asry(self) -> float:
        """contrast (`5kf4asry` on the wire)."""
        return self.fields["5kf4asry"]

    @_5kf4asry.setter
    def _5kf4asry(self, value: float) -> None:
        self.edit("5kf4asry", value=value)
    ugpps35t: float  # saturation
    ueazn2zi: float  # scale_x
    qxxmhhdr: float  # scale_y
    hl2h6f76: float  # pos_x
    nhestloi: float  # pos_y
    ckydsf7y: float  # rotation
    ttpiryuy: float  # crop_left
    dazt7wt5: float  # crop_right
    o5353t6h: float  # crop_top
    lsgktf3d: float  # crop_bottom
    mq4hgil7: list[str]  # effects
    @property
    def _6ahtwc73(self) -> list[str]:
        """keys (`6ahtwc73` on the wire)."""
        return self.fields["6ahtwc73"]

    @_6ahtwc73.setter
    def _6ahtwc73(self, value: list[str]) -> None:
        self.edit("6ahtwc73", value=value)
    @property
    def _2i54q4ly(self) -> bool:
        """key_enabled (`2i54q4ly` on the wire)."""
        return self.fields["2i54q4ly"]

    @_2i54q4ly.setter
    def _2i54q4ly(self, value: bool) -> None:
        self.edit("2i54q4ly", value=value)
    hcm6xmnu: list[float]  # key_color
    ss3pryvp: float  # key_tolerance
    odmxpb6p: float  # key_softness
    vwxtiqh4: float  # key_spill
    v6saof6k: str  # mask_shape
    drrsji3r: float  # mask_x
    q63mhtzf: float  # mask_y
    w3owjsm6: float  # mask_w
    lrguhlws: float  # mask_h
    u4sfvd6p: float  # mask_rotation
    zteue6rc: float  # mask_feather
    ah7jtdcc: bool  # mask_invert
    @property
    def _2wno4yo2(self) -> str:
        """link (`2wno4yo2` on the wire)."""
        return self.fields["2wno4yo2"]

    @_2wno4yo2.setter
    def _2wno4yo2(self, value: str) -> None:
        self.edit("2wno4yo2", value=value)
    @property
    def _3rzhv2vf(self) -> bool:
        """link_follows (`3rzhv2vf` on the wire)."""
        return self.fields["3rzhv2vf"]

    @_3rzhv2vf.setter
    def _3rzhv2vf(self, value: bool) -> None:
        self.edit("3rzhv2vf", value=value)
    hulnlv2t: bool  # sound_split


class ShotEffect(Resource):
    """ShotEffect — `tl.so.tilo.video-editor.ShotEffect`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    ai7thy2p: str  # id
    seoccvzw: str  # form
    y2yjk3ou: float  # amount


class ShotKey(Resource):
    """ShotKey — `tl.so.tilo.video-editor.ShotKey`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    fpeyg4lm: str  # param
    frbcwy5c: float  # t
    hxcqnoii: float  # v
    pf3f6zou: str  # interp


class ThumbAsk(Resource):
    """ThumbAsk — `tl.so.tilo.video-editor.ThumbAsk`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    zbk6etmx: str  # url
    mom4w6zg: float  # at
    qdxfi3dv: int  # width
    a4wvyrpr: int  # height


class SampleAsk(Resource):
    """SampleAsk — `tl.so.tilo.video-editor.SampleAsk`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    @property
    def _47t7hgmm(self) -> str:
        """project (`47t7hgmm` on the wire)."""
        return self.fields["47t7hgmm"]

    @_47t7hgmm.setter
    def _47t7hgmm(self, value: str) -> None:
        self.edit("47t7hgmm", value=value)
    lph6xeeo: float  # at
    hpbggrop: float  # x
    nklgpqyv: float  # y


class SampleAnswer(Resource):
    """SampleAnswer — `tl.so.tilo.video-editor.SampleAnswer`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    twwt37df: float  # red
    @property
    def _6j7i6jgf(self) -> float:
        """green (`6j7i6jgf` on the wire)."""
        return self.fields["6j7i6jgf"]

    @_6j7i6jgf.setter
    def _6j7i6jgf(self, value: float) -> None:
        self.edit("6j7i6jgf", value=value)
    qbxkptqm: float  # blue


class ShotMarker(Resource):
    """ShotMarker — `tl.so.tilo.video-editor.ShotMarker`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    @property
    def _5evudwdu(self) -> str:
        """id (`5evudwdu` on the wire)."""
        return self.fields["5evudwdu"]

    @_5evudwdu.setter
    def _5evudwdu(self, value: str) -> None:
        self.edit("5evudwdu", value=value)
    bh2yiei5: float  # t
    ajyxetst: str  # name
    @property
    def _2wvqaalc(self) -> int:
        """palette (`2wvqaalc` on the wire)."""
        return self.fields["2wvqaalc"]

    @_2wvqaalc.setter
    def _2wvqaalc(self, value: int) -> None:
        self.edit("2wvqaalc", value=value)
    ueznappo: str  # note


class Saved(Resource):
    """Saved — `tl.so.tilo.video-editor.Saved`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    y5f2zb7y: str  # name
    pfj7sab5: float  # fps
    @property
    def _4nw6mpgl(self) -> int:
        """width (`4nw6mpgl` on the wire)."""
        return self.fields["4nw6mpgl"]

    @_4nw6mpgl.setter
    def _4nw6mpgl(self, value: int) -> None:
        self.edit("4nw6mpgl", value=value)
    tyqdqd4o: int  # height
    idvvsvmm: float  # mark_in
    dadn3d73: float  # mark_out
    hzulw2ku: list[str]  # media
    sgtdffn3: list[str]  # tracks
    tgtkyvzv: list[str]  # clips
    mbkqck5v: list[str]  # markers


class SavedMedia(Resource):
    """SavedMedia — `tl.so.tilo.video-editor.SavedMedia`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    y4ivogi5: str  # name
    @property
    def _4yybplx2(self) -> str:
        """form (`4yybplx2` on the wire)."""
        return self.fields["4yybplx2"]

    @_4yybplx2.setter
    def _4yybplx2(self, value: str) -> None:
        self.edit("4yybplx2", value=value)
    lrk7dehr: float  # duration
    zzc32v7l: int  # width
    qj5gefuh: int  # height
    @property
    def _23dpu2sh(self) -> float:
        """fps (`23dpu2sh` on the wire)."""
        return self.fields["23dpu2sh"]

    @_23dpu2sh.setter
    def _23dpu2sh(self, value: float) -> None:
        self.edit("23dpu2sh", value=value)
    soogrhvl: int  # palette
    ooln2ibo: str  # path
    ydskn4p7: bool  # has_audio
    im5ank6o: str  # words
    tmrphs4y: float  # size
    kqelnlf5: float  # red
    vikauvue: float  # green
    l67cmsns: float  # blue
    ozd3qrh5: float  # alpha
    ytkxp4t4: str  # align
    oecwbkzv: bool  # bold
    qefzzmnw: bool  # italic
    npetd5wt: float  # backdrop


class SavedTrack(Resource):
    """SavedTrack — `tl.so.tilo.video-editor.SavedTrack`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    hahxd2bf: str  # name
    @property
    def _6eogjiom(self) -> str:
        """form (`6eogjiom` on the wire)."""
        return self.fields["6eogjiom"]

    @_6eogjiom.setter
    def _6eogjiom(self, value: str) -> None:
        self.edit("6eogjiom", value=value)
    kpfeeasv: int  # index
    objfxiib: bool  # muted
    xucxzrou: bool  # solo
    ziybhwce: bool  # locked
    mddfj6qk: float  # gain_db
    kmg2laxv: float  # pan


class SavedClip(Resource):
    """SavedClip — `tl.so.tilo.video-editor.SavedClip`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    xelrculp: str  # name
    nakmpr6q: int  # track
    b6xll7vm: int  # media
    @property
    def _3styztut(self) -> float:
        """start (`3styztut` on the wire)."""
        return self.fields["3styztut"]

    @_3styztut.setter
    def _3styztut(self, value: float) -> None:
        self.edit("3styztut", value=value)
    fosnfakw: float  # in_point
    fyyrf7ca: float  # out_point
    kl2xarv5: float  # opacity
    xjyxglh4: float  # volume
    @property
    def _3ayzf2x5(self) -> str:
        """transition (`3ayzf2x5` on the wire)."""
        return self.fields["3ayzf2x5"]

    @_3ayzf2x5.setter
    def _3ayzf2x5(self, value: str) -> None:
        self.edit("3ayzf2x5", value=value)
    dqqoukz2: float  # transition_duration
    ked6w3da: float  # fade_in
    gs542y2n: float  # fade_out
    xuo76wt5: float  # speed_rate
    joyeavyw: bool  # reverse
    y7jv62ls: float  # brightness
    g5rdmkth: float  # contrast
    zgf5atrf: float  # saturation
    r4zzuhna: float  # scale_x
    u3nbauiz: float  # scale_y
    @property
    def _7rxjc3mu(self) -> float:
        """pos_x (`7rxjc3mu` on the wire)."""
        return self.fields["7rxjc3mu"]

    @_7rxjc3mu.setter
    def _7rxjc3mu(self, value: float) -> None:
        self.edit("7rxjc3mu", value=value)
    @property
    def _4fkenwwm(self) -> float:
        """pos_y (`4fkenwwm` on the wire)."""
        return self.fields["4fkenwwm"]

    @_4fkenwwm.setter
    def _4fkenwwm(self, value: float) -> None:
        self.edit("4fkenwwm", value=value)
    n2rqdsw7: float  # rotation
    fycn5pvk: float  # crop_left
    @property
    def _2tb4h5uc(self) -> float:
        """crop_right (`2tb4h5uc` on the wire)."""
        return self.fields["2tb4h5uc"]

    @_2tb4h5uc.setter
    def _2tb4h5uc(self, value: float) -> None:
        self.edit("2tb4h5uc", value=value)
    @property
    def _7madt25j(self) -> float:
        """crop_top (`7madt25j` on the wire)."""
        return self.fields["7madt25j"]

    @_7madt25j.setter
    def _7madt25j(self, value: float) -> None:
        self.edit("7madt25j", value=value)
    @property
    def _3wy2xkbg(self) -> float:
        """crop_bottom (`3wy2xkbg` on the wire)."""
        return self.fields["3wy2xkbg"]

    @_3wy2xkbg.setter
    def _3wy2xkbg(self, value: float) -> None:
        self.edit("3wy2xkbg", value=value)
    @property
    def _2ui2wpyh(self) -> bool:
        """key_enabled (`2ui2wpyh` on the wire)."""
        return self.fields["2ui2wpyh"]

    @_2ui2wpyh.setter
    def _2ui2wpyh(self, value: bool) -> None:
        self.edit("2ui2wpyh", value=value)
    cxqyoowf: float  # key_tolerance
    rjd3q7ct: float  # key_softness
    avubfu74: float  # key_spill
    smqr4d4n: str  # mask_shape
    yr4ncz4w: float  # mask_x
    @property
    def _7g7aeosa(self) -> float:
        """mask_y (`7g7aeosa` on the wire)."""
        return self.fields["7g7aeosa"]

    @_7g7aeosa.setter
    def _7g7aeosa(self, value: float) -> None:
        self.edit("7g7aeosa", value=value)
    jasgkfww: float  # mask_w
    f4dke5o2: float  # mask_h
    fdrernpm: float  # mask_rotation
    to3xcmob: float  # mask_feather
    @property
    def _6dr3jtxx(self) -> bool:
        """mask_invert (`6dr3jtxx` on the wire)."""
        return self.fields["6dr3jtxx"]

    @_6dr3jtxx.setter
    def _6dr3jtxx(self, value: bool) -> None:
        self.edit("6dr3jtxx", value=value)
    pdhb3rjf: str  # link
    d6v4or6p: int  # palette
    nayfykgr: float  # key_red
    yl3a4tsd: float  # key_green
    zwo6h7ka: float  # key_blue
    @property
    def _6oop3g3z(self) -> list[str]:
        """keys (`6oop3g3z` on the wire)."""
        return self.fields["6oop3g3z"]

    @_6oop3g3z.setter
    def _6oop3g3z(self, value: list[str]) -> None:
        self.edit("6oop3g3z", value=value)
    igcwp2fn: list[str]  # effects


class Copied(Resource):
    """Copied — `tl.so.tilo.video-editor.Copied`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    vnb2e4hr: str  # kind
    jrywinsx: list[str]  # clips


class CopiedClip(Resource):
    """CopiedClip — `tl.so.tilo.video-editor.CopiedClip`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    fazzk3sz: SavedClip  # clip
    @property
    def _636l34ow(self) -> str:
        """media (`636l34ow` on the wire)."""
        return self.fields["636l34ow"]

    @_636l34ow.setter
    def _636l34ow(self, value: str) -> None:
        self.edit("636l34ow", value=value)
    ebsvhwog: str  # path
    uuax3sh3: str  # track
    ep4wqebq: str  # form


class SavedMarker(Resource):
    """SavedMarker — `tl.so.tilo.video-editor.SavedMarker`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    s6k7ckvj: str  # name
    bglc57r5: float  # t
    @property
    def _2zmyfxy2(self) -> int:
        """palette (`2zmyfxy2` on the wire)."""
        return self.fields["2zmyfxy2"]

    @_2zmyfxy2.setter
    def _2zmyfxy2(self, value: int) -> None:
        self.edit("2zmyfxy2", value=value)
    uqvihrf3: str  # note


class Drawn(Resource):
    """Drawn — `tl.so.tilo.video-editor.Drawn`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"
    jaecw4vv: list[Any]  # shapes
    msyhaonn: list[str]  # owners


class VideoEditor(Resource):
    """Video Editor — `tl.so.tilo.video-editor`."""
    __surface__ = "so.tilo.video-editor@a6a9c3696deccca9"

