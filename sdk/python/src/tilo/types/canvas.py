"""Types for Canvas (`so.tilo.canvas`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `canvas/<resource>`, read with
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
    "Artwork",
    "Layer",
    "Mark",
    "Area",
    "Picture",
    "Medium",
    "Pigment",
    "Reference",
    "Library",
    "LookRequest",
    "ExportRequest",
    "SvgRequest",
    "PlaceRequest",
    "ArtworkJson",
    "LayerJson",
    "MediumJson",
    "PigmentJson",
    "ReferenceJson",
    "MarkJson",
    "AreaJson",
    "MatteJson",
    "GradeAsk",
    "WandAsk",
    "FindAsk",
    "CombineAsk",
    "MeasureAsk",
    "SelectAsk",
    "InspectAsk",
    "RemoveAsk",
    "DiffAsk",
    "CompareAsk",
    "Canvas",
    "RenameLayerArgs",
    "LockLayerArgs",
    "ClipLayerArgs",
    "SetMaskArgs",
    "SetMediumArgs",
    "RemoveMediumArgs",
    "MoveLayerUpArgs",
    "MoveLayerDownArgs",
    "MergeLayerDownArgs",
    "ShowLayerArgs",
    "RemoveLayerArgs",
    "SetArtworkArgs",
    "ExtendStrokeArgs",
    "RemoveMarkArgs",
    "RecolourArgs",
    "ResizeMarkArgs",
    "SetReferenceArgs",
    "ClearReferenceArgs",
    "ClearComparisonArgs",
    "PickMarkArgs",
    "PickMarksArgs",
    "ClearPickedArgs",
    "SelectAreaArgs",
    "ClearSelectionArgs",
    "DrawLookArgs",
    "ClearNoticeArgs",
    "StartPaintArgs",
    "SetLayerGroupArgs",
    "AdjustLayerArgs",
    "GroupMarkArgs",
    "FillGradientArgs",
    "WetMarkArgs",
    "ClearGradientArgs",
    "SetTextArgs",
    "CropPictureArgs",
    "StraightenPictureArgs",
    "FlipPictureArgs",
    "BlendMarkArgs",
    "AdjustMarkArgs",
    "MoveMarkArgs",
    "ScaleMarkArgs",
    "RotateMarkArgs",
    "ClipMarkArgs",
    "FinishMarkArgs",
    "SetCurveArgs",
    "SetDynamicsArgs",
]

RenameLayerArgs = TypedDict("RenameLayerArgs", {
    "layer": "Layer",
    "name": "str",
})
"""The arguments of `rename_layer`."""

LockLayerArgs = TypedDict("LockLayerArgs", {
    "layer": "Layer",
    "locked": NotRequired["bool | None"],
})
"""The arguments of `lock_layer`."""

ClipLayerArgs = TypedDict("ClipLayerArgs", {
    "layer": "Layer",
    "clip": NotRequired["bool | None"],
})
"""The arguments of `clip_layer`."""

SetMaskArgs = TypedDict("SetMaskArgs", {
    "layer": "Layer",
    "base": NotRequired["float | None"],
    "invert": NotRequired["bool | None"],
    "unlinked": NotRequired["bool | None"],
})
"""The arguments of `set_mask`."""

SetMediumArgs = TypedDict("SetMediumArgs", {
    "layer": "Layer",
    "medium": "str",
})
"""The arguments of `set_medium`."""

RemoveMediumArgs = TypedDict("RemoveMediumArgs", {
    "m": "Medium",
})
"""The arguments of `remove_medium`."""

MoveLayerUpArgs = TypedDict("MoveLayerUpArgs", {
    "layer": "Layer",
})
"""The arguments of `move_layer_up`."""

MoveLayerDownArgs = TypedDict("MoveLayerDownArgs", {
    "layer": "Layer",
})
"""The arguments of `move_layer_down`."""

MergeLayerDownArgs = TypedDict("MergeLayerDownArgs", {
    "layer": "Layer",
})
"""The arguments of `merge_layer_down`."""

ShowLayerArgs = TypedDict("ShowLayerArgs", {
    "layer": "Layer",
    "visible": "bool",
})
"""The arguments of `show_layer`."""

RemoveLayerArgs = TypedDict("RemoveLayerArgs", {
    "layer": "Layer",
})
"""The arguments of `remove_layer`."""

SetArtworkArgs = TypedDict("SetArtworkArgs", {
    "title": NotRequired["str | None"],
    "width": NotRequired["float | None"],
    "height": NotRequired["float | None"],
})
"""The arguments of `set_artwork`."""

ExtendStrokeArgs = TypedDict("ExtendStrokeArgs", {
    "m": "Mark",
    "point": "list[float]",
})
"""The arguments of `extend_stroke`."""

RemoveMarkArgs = TypedDict("RemoveMarkArgs", {
    "m": "Mark",
})
"""The arguments of `remove_mark`."""

RecolourArgs = TypedDict("RecolourArgs", {
    "m": "Mark",
    "colour": "str",
})
"""The arguments of `recolour`."""

ResizeMarkArgs = TypedDict("ResizeMarkArgs", {
    "m": "Mark",
    "size": "float",
})
"""The arguments of `resize_mark`."""

SetReferenceArgs = TypedDict("SetReferenceArgs", {
    "file": "Any",
    "opacity": NotRequired["float | None"],
})
"""The arguments of `set_reference`."""

ClearReferenceArgs = TypedDict("ClearReferenceArgs", {
})
"""The arguments of `clear_reference`."""

ClearComparisonArgs = TypedDict("ClearComparisonArgs", {
})
"""The arguments of `clear_comparison`."""

PickMarkArgs = TypedDict("PickMarkArgs", {
    "m": "Mark",
    "on": NotRequired["bool | None"],
})
"""The arguments of `pick_mark`."""

PickMarksArgs = TypedDict("PickMarksArgs", {
    "marks": "list[Mark]",
})
"""The arguments of `pick_marks`."""

ClearPickedArgs = TypedDict("ClearPickedArgs", {
})
"""The arguments of `clear_picked`."""

SelectAreaArgs = TypedDict("SelectAreaArgs", {
    "d": "str",
    "feather": NotRequired["float | None"],
})
"""The arguments of `select_area`."""

ClearSelectionArgs = TypedDict("ClearSelectionArgs", {
})
"""The arguments of `clear_selection`."""

DrawLookArgs = TypedDict("DrawLookArgs", {
    "size": NotRequired["int | None"],
})
"""The arguments of `draw_look`."""

ClearNoticeArgs = TypedDict("ClearNoticeArgs", {
})
"""The arguments of `clear_notice`."""

StartPaintArgs = TypedDict("StartPaintArgs", {
    "tool": "str",
    "brush": "str",
    "layer": NotRequired["Layer | None"],
    "point": "list[float]",
    "size": "float",
    "colour": "str",
})
"""The arguments of `start_paint`."""

SetLayerGroupArgs = TypedDict("SetLayerGroupArgs", {
    "group": "str",
})
"""The arguments of `set_layer_group`."""

AdjustLayerArgs = TypedDict("AdjustLayerArgs", {
    "exposure": NotRequired["float | None"],
    "contrast": NotRequired["float | None"],
    "highlights": NotRequired["float | None"],
    "shadows": NotRequired["float | None"],
    "saturation": NotRequired["float | None"],
    "vibrance": NotRequired["float | None"],
    "hue": NotRequired["float | None"],
    "temperature": NotRequired["float | None"],
    "tint": NotRequired["float | None"],
    "grayscale": NotRequired["bool | None"],
    "invert": NotRequired["bool | None"],
    "blur": NotRequired["float | None"],
    "sharpen": NotRequired["float | None"],
    "vignette": NotRequired["float | None"],
})
"""The arguments of `adjust_layer`."""

GroupMarkArgs = TypedDict("GroupMarkArgs", {
    "group": "str",
})
"""The arguments of `group_mark`."""

FillGradientArgs = TypedDict("FillGradientArgs", {
    "kind": NotRequired["str | None"],
    "x0": "float",
    "y0": "float",
    "x1": "float",
    "y1": "float",
    "offsets": "list[float]",
    "colours": "list[str]",
})
"""The arguments of `fill_gradient`."""

WetMarkArgs = TypedDict("WetMarkArgs", {
    "wet": NotRequired["bool | None"],
})
"""The arguments of `wet_mark`."""

ClearGradientArgs = TypedDict("ClearGradientArgs", {
})
"""The arguments of `clear_gradient`."""

SetTextArgs = TypedDict("SetTextArgs", {
    "text": NotRequired["str | None"],
    "size": NotRequired["float | None"],
    "colour": NotRequired["str | None"],
    "font": NotRequired["str | None"],
    "weight": NotRequired["int | None"],
    "italic": NotRequired["bool | None"],
    "align": NotRequired["str | None"],
    "line_height": NotRequired["float | None"],
    "tracking": NotRequired["float | None"],
    "path": NotRequired["str | None"],
})
"""The arguments of `set_text`."""

CropPictureArgs = TypedDict("CropPictureArgs", {
    "x": NotRequired["float | None"],
    "y": NotRequired["float | None"],
    "width": NotRequired["float | None"],
    "height": NotRequired["float | None"],
})
"""The arguments of `crop_picture`."""

StraightenPictureArgs = TypedDict("StraightenPictureArgs", {
    "degrees": "float",
})
"""The arguments of `straighten_picture`."""

FlipPictureArgs = TypedDict("FlipPictureArgs", {
    "horizontal": NotRequired["bool | None"],
    "vertical": NotRequired["bool | None"],
    "flip_h": NotRequired["bool | None"],
    "flip_v": NotRequired["bool | None"],
})
"""The arguments of `flip_picture`."""

BlendMarkArgs = TypedDict("BlendMarkArgs", {
    "blend": "str",
})
"""The arguments of `blend_mark`."""

AdjustMarkArgs = TypedDict("AdjustMarkArgs", {
    "exposure": NotRequired["float | None"],
    "contrast": NotRequired["float | None"],
    "highlights": NotRequired["float | None"],
    "shadows": NotRequired["float | None"],
    "saturation": NotRequired["float | None"],
    "vibrance": NotRequired["float | None"],
    "hue": NotRequired["float | None"],
    "temperature": NotRequired["float | None"],
    "tint": NotRequired["float | None"],
    "grayscale": NotRequired["bool | None"],
    "invert": NotRequired["bool | None"],
    "blur": NotRequired["float | None"],
    "sharpen": NotRequired["float | None"],
    "vignette": NotRequired["float | None"],
})
"""The arguments of `adjust_mark`."""

MoveMarkArgs = TypedDict("MoveMarkArgs", {
    "dx": "float",
    "dy": "float",
})
"""The arguments of `move_mark`."""

ScaleMarkArgs = TypedDict("ScaleMarkArgs", {
    "cx": "float",
    "cy": "float",
    "factor": "float",
})
"""The arguments of `scale_mark`."""

RotateMarkArgs = TypedDict("RotateMarkArgs", {
    "cx": "float",
    "cy": "float",
    "degrees": "float",
})
"""The arguments of `rotate_mark`."""

ClipMarkArgs = TypedDict("ClipMarkArgs", {
    "by": "str",
})
"""The arguments of `clip_mark`."""

FinishMarkArgs = TypedDict("FinishMarkArgs", {
    "pixelated": NotRequired["bool | None"],
    "grain": NotRequired["float | None"],
    "feather": NotRequired["float | None"],
    "shadow_x": NotRequired["float | None"],
    "shadow_y": NotRequired["float | None"],
    "shadow_blur": NotRequired["float | None"],
    "shadow_colour": NotRequired["str | None"],
    "inner_shadow_x": NotRequired["float | None"],
    "inner_shadow_y": NotRequired["float | None"],
    "inner_shadow_blur": NotRequired["float | None"],
    "inner_shadow_colour": NotRequired["str | None"],
})
"""The arguments of `finish_mark`."""

SetCurveArgs = TypedDict("SetCurveArgs", {
    "curve": "list[list[float]]",
})
"""The arguments of `set_curve`."""

SetDynamicsArgs = TypedDict("SetDynamicsArgs", {
    "flow": NotRequired["float | None"],
    "hardness": NotRequired["float | None"],
    "spacing": NotRequired["float | None"],
    "smoothing": NotRequired["float | None"],
    "taper": NotRequired["float | None"],
})
"""The arguments of `set_dynamics`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
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


class Artwork(Resource):
    """Artwork — `tl.so.tilo.canvas.Artwork`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    zp2ztcwq: str  # title
    izpefh4x: float  # width
    @property
    def _5ui5uh6r(self) -> float:
        """height (`5ui5uh6r` on the wire)."""
        return self.fields["5ui5uh6r"]

    @_5ui5uh6r.setter
    def _5ui5uh6r(self, value: float) -> None:
        self.edit("5ui5uh6r", value=value)
    fh35sews: str  # ground_texture
    zdv3lz52: str  # ground_colour
    xu4h6ew2: list[Entry[Layer]]  # layers
    btwv4xq5: list[Entry[Mark]]  # marks
    @property
    def _4bht3tzh(self) -> list[Entry[Medium]]:
        """media (`4bht3tzh` on the wire)."""
        return self.fields["4bht3tzh"]

    @_4bht3tzh.setter
    def _4bht3tzh(self, value: list[Entry[Medium]]) -> None:
        self.edit("4bht3tzh", value=value)
    @property
    def _4wmc4l7b(self) -> list[Entry[Picture]]:
        """pictures (`4wmc4l7b` on the wire)."""
        return self.fields["4wmc4l7b"]

    @_4wmc4l7b.setter
    def _4wmc4l7b(self, value: list[Entry[Picture]]) -> None:
        self.edit("4wmc4l7b", value=value)
    ob6rlf7s: Area | None  # selection
    hrqejear: list[Mark]  # picked
    exp6h3eo: Reference | None  # reference
    k2slfiuy: str  # look
    mksntvfu: str  # comparison
    jmoqryks: str  # version_note
    fydl4rsk: str  # version_overlay
    i5yildir: list[float]  # version_region
    meooqdog: str  # notice

    def add_layer(self,
        *,
        name: str | None = None,
    ) -> Layer:
        """Adds a layer on top of the artwork."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        return self.action("add_layer", **args)

    def rename_layer(self,
        *,
        layer: Layer,
        name: str,
    ) -> None:
        """Renames a layer. Two layers never share a name: a mark names its layer by it."""
        return self.action("rename_layer", layer=layer, name=name)

    def rename_layer_many(self, items: list[RenameLayerArgs]) -> list[dict[str, Any]]:
        """Runs `rename_layer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename_layer", items)

    def lock_layer(self,
        *,
        layer: Layer,
        locked: bool | None = None,
    ) -> None:
        """Holds a layer still, so the pointer passes through its marks, or lets its marks be picked again."""
        args: dict[str, Any] = {}
        args["layer"] = layer
        if locked is not None:
            args["locked"] = locked
        return self.action("lock_layer", **args)

    def lock_layer_many(self, items: list[LockLayerArgs]) -> list[dict[str, Any]]:
        """Runs `lock_layer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("lock_layer", items)

    def clip_layer(self,
        *,
        layer: Layer,
        clip: bool | None = None,
    ) -> None:
        """Holds a layer to the paint of the one under it, or lets it stand free."""
        args: dict[str, Any] = {}
        args["layer"] = layer
        if clip is not None:
            args["clip"] = clip
        return self.action("clip_layer", **args)

    def clip_layer_many(self, items: list[ClipLayerArgs]) -> list[dict[str, Any]]:
        """Runs `clip_layer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clip_layer", items)

    def add_mask(self, *, layer: Layer) -> Layer:
        """Adds a mask to a layer: a layer of greys the layer is seen through. Its marks are made with the brushes as any layer's are, light showing the layer and dark hiding it."""
        return self.action("add_mask", layer=layer)

    def set_mask(self,
        *,
        layer: Layer,
        base: float | None = None,
        invert: bool | None = None,
        unlinked: bool | None = None,
    ) -> None:
        """Sets a mask's grey before any mark, whether it hides where it is light, and whether it stays put when its layer's marks move."""
        args: dict[str, Any] = {}
        args["layer"] = layer
        if base is not None:
            args["base"] = base
        if invert is not None:
            args["invert"] = invert
        if unlinked is not None:
            args["unlinked"] = unlinked
        return self.action("set_mask", **args)

    def set_mask_many(self, items: list[SetMaskArgs]) -> list[dict[str, Any]]:
        """Runs `set_mask` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_mask", items)

    def add_text(self,
        *,
        text: str,
        layer: Layer | None = None,
        x: float | None = None,
        y: float | None = None,
        size: float | None = None,
        colour: str | None = None,
        font: str | None = None,
        weight: int | None = None,
        italic: bool | None = None,
        align: str | None = None,
        line_height: float | None = None,
        tracking: float | None = None,
        path: str | None = None,
    ) -> Mark:
        """Adds a text mark: words set in the artwork's own fonts, at `x`, `y` (its first baseline) in artwork units."""
        args: dict[str, Any] = {}
        if layer is not None:
            args["layer"] = layer
        args["text"] = text
        if x is not None:
            args["x"] = x
        if y is not None:
            args["y"] = y
        if size is not None:
            args["size"] = size
        if colour is not None:
            args["colour"] = colour
        if font is not None:
            args["font"] = font
        if weight is not None:
            args["weight"] = weight
        if italic is not None:
            args["italic"] = italic
        if align is not None:
            args["align"] = align
        if line_height is not None:
            args["line_height"] = line_height
        if tracking is not None:
            args["tracking"] = tracking
        if path is not None:
            args["path"] = path
        return self.action("add_text", **args)

    def set_medium(self,
        *,
        layer: Layer,
        medium: str,
    ) -> None:
        """Paints a layer with a medium: `oil`, `watercolour`, a preset saved in the artwork by its id, or empty for plain vector paint."""
        return self.action("set_medium", layer=layer, medium=medium)

    def set_medium_many(self, items: list[SetMediumArgs]) -> list[dict[str, Any]]:
        """Runs `set_medium` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_medium", items)

    def save_medium(self,
        *,
        id: str,
        label: str,
        engine: str | None = None,
        description: str | None = None,
        ground: str | None = None,
        params: dict[str, float] | None = None,
        palette: list[Pigment] | None = None,
    ) -> Medium:
        """Saves a medium in the artwork, so its layers name it and every device paints it the same. A medium of the same id is replaced."""
        args: dict[str, Any] = {}
        args["id"] = id
        args["label"] = label
        if engine is not None:
            args["engine"] = engine
        if description is not None:
            args["description"] = description
        if ground is not None:
            args["ground"] = ground
        if params is not None:
            args["params"] = params
        if palette is not None:
            args["palette"] = palette
        return self.action("save_medium", **args)

    def remove_medium(self, *, m: Medium) -> None:
        """Runs the app's action."""
        return self.action("remove_medium", m=m)

    def remove_medium_many(self, items: list[RemoveMediumArgs]) -> list[dict[str, Any]]:
        """Runs `remove_medium` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_medium", items)

    def move_layer_up(self, *, layer: Layer) -> None:
        """Moves a layer one place up in the stack."""
        return self.action("move_layer_up", layer=layer)

    def move_layer_up_many(self, items: list[MoveLayerUpArgs]) -> list[dict[str, Any]]:
        """Runs `move_layer_up` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_layer_up", items)

    def move_layer_down(self, *, layer: Layer) -> None:
        """Moves a layer one place down in the stack."""
        return self.action("move_layer_down", layer=layer)

    def move_layer_down_many(self, items: list[MoveLayerDownArgs]) -> list[dict[str, Any]]:
        """Runs `move_layer_down` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_layer_down", items)

    def duplicate_layer(self, *, layer: Layer) -> Layer:
        """Copies a layer and everything on it, above the original."""
        return self.action("duplicate_layer", layer=layer)

    def merge_layer_down(self, *, layer: Layer) -> None:
        """Folds a layer into the one under it: its marks join that layer, and it is gone."""
        return self.action("merge_layer_down", layer=layer)

    def merge_layer_down_many(self, items: list[MergeLayerDownArgs]) -> list[dict[str, Any]]:
        """Runs `merge_layer_down` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("merge_layer_down", items)

    def show_layer(self,
        *,
        layer: Layer,
        visible: bool,
    ) -> None:
        """Shows or hides a layer."""
        return self.action("show_layer", layer=layer, visible=visible)

    def show_layer_many(self, items: list[ShowLayerArgs]) -> list[dict[str, Any]]:
        """Runs `show_layer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show_layer", items)

    def remove_layer(self, *, layer: Layer) -> None:
        """Runs the app's action."""
        return self.action("remove_layer", layer=layer)

    def remove_layer_many(self, items: list[RemoveLayerArgs]) -> list[dict[str, Any]]:
        """Runs `remove_layer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_layer", items)

    def set_artwork(self,
        *,
        title: str | None = None,
        width: float | None = None,
        height: float | None = None,
    ) -> None:
        """Sets what the artwork is called and how big it is."""
        args: dict[str, Any] = {}
        if title is not None:
            args["title"] = title
        if width is not None:
            args["width"] = width
        if height is not None:
            args["height"] = height
        return self.action("set_artwork", **args)

    def set_artwork_many(self, items: list[SetArtworkArgs]) -> list[dict[str, Any]]:
        """Runs `set_artwork` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_artwork", items)

    def add_stroke(self,
        *,
        point: list[float],
        size: float,
        colour: str,
        brush: str,
        layer: Layer | None = None,
    ) -> Mark:
        """Lays one brush stroke where the hand went, its point `[x, y, pressure]` in artwork units."""
        args: dict[str, Any] = {}
        if layer is not None:
            args["layer"] = layer
        args["point"] = point
        args["size"] = size
        args["colour"] = colour
        args["brush"] = brush
        return self.action("add_stroke", **args)

    def extend_stroke(self,
        *,
        m: Mark,
        point: list[float],
    ) -> None:
        """Adds the next point to the stroke being painted."""
        return self.action("extend_stroke", m=m, point=point)

    def extend_stroke_many(self, items: list[ExtendStrokeArgs]) -> list[dict[str, Any]]:
        """Runs `extend_stroke` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("extend_stroke", items)

    def top_layer(self) -> Layer:
        """The layer a stroke lands on when none is chosen: the top one that shows and is not held still. An artwork whose layers are all hidden or locked gains one to paint on."""
        return self.action("top_layer")

    def add_shape(self,
        *,
        d: str,
        colour: str,
        layer: Layer | None = None,
    ) -> Mark:
        """Draws a shape, its SVG path data in artwork units."""
        args: dict[str, Any] = {}
        if layer is not None:
            args["layer"] = layer
        args["d"] = d
        args["colour"] = colour
        return self.action("add_shape", **args)

    def remove_mark(self, *, m: Mark) -> None:
        """Runs the app's action."""
        return self.action("remove_mark", m=m)

    def remove_mark_many(self, items: list[RemoveMarkArgs]) -> list[dict[str, Any]]:
        """Runs `remove_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_mark", items)

    def recolour(self,
        *,
        m: Mark,
        colour: str,
    ) -> None:
        """Recolours a mark."""
        return self.action("recolour", m=m, colour=colour)

    def recolour_many(self, items: list[RecolourArgs]) -> list[dict[str, Any]]:
        """Runs `recolour` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("recolour", items)

    def resize_mark(self,
        *,
        m: Mark,
        size: float,
    ) -> None:
        """Makes a mark bigger or smaller."""
        return self.action("resize_mark", m=m, size=size)

    def resize_mark_many(self, items: list[ResizeMarkArgs]) -> list[dict[str, Any]]:
        """Runs `resize_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("resize_mark", items)

    def place_picture(self, *, file: Any) -> Mark:
        """Places a picture the person chose on the top layer, fitted inside the artwork and centred: the picture stays its own object, so it moves, crops and syncs like paint."""
        return self.action("place_picture", file=file)

    def select_like(self,
        *,
        at: list[float] | None = None,
        colour: str | None = None,
        tolerance: float | None = None,
        everywhere: bool | None = None,
    ) -> str | None:
        """Selects what the colour at `at` touches in the artwork's picture, or that colour everywhere: the magic wand. The area comes back as a selection the fill, mask and outline use."""
        args: dict[str, Any] = {}
        if at is not None:
            args["at"] = at
        if colour is not None:
            args["colour"] = colour
        if tolerance is not None:
            args["tolerance"] = tolerance
        if everywhere is not None:
            args["everywhere"] = everywhere
        return self.action("select_like", **args)

    def select_subject(self,
        *,
        refine: float | None = None,
    ) -> str | None:
        """Selects the picture's subject, found by the on-device model the person set up, with its edge fitted to the picture's own edges over `refine` units where that is asked."""
        args: dict[str, Any] = {}
        if refine is not None:
            args["refine"] = refine
        return self.action("select_subject", **args)

    def refine_selection(self,
        *,
        refine: float | None = None,
    ) -> str | None:
        """Refines the selection's edge to the picture's own edges over `refine` units, keeping fur and hair soft."""
        args: dict[str, Any] = {}
        if refine is not None:
            args["refine"] = refine
        return self.action("refine_selection", **args)

    def combine_marks(self,
        *,
        marks: list[Mark],
        op: str | None = None,
    ) -> Mark | None:
        """Combines the marks into one path, in the bottom one's place and colour: shapes united, subtracted, intersected or excluded. The engine works out the outline; the artwork keeps one mark where there were several."""
        args: dict[str, Any] = {}
        args["marks"] = marks
        if op is not None:
            args["op"] = op
        return self.action("combine_marks", **args)

    def measure_marks(self, *, marks: list[Mark]) -> str | None:
        """Weighs the marks' ink as the eye sees it: each one's ink box, ink area, centre of mass and its offset from the box's middle, so two icons balance when their areas and offsets match. The answer is the engine's JSON; the window says it in words."""
        return self.action("measure_marks", marks=marks)

    def set_reference(self,
        *,
        file: Any,
        opacity: float | None = None,
    ) -> None:
        """Sets the picture to work from, fitted inside the artwork and centred. It is not part of the work: comparing shows the artwork beside, over or against it, and export never includes it."""
        args: dict[str, Any] = {}
        args["file"] = file
        if opacity is not None:
            args["opacity"] = opacity
        return self.action("set_reference", **args)

    def set_reference_many(self, items: list[SetReferenceArgs]) -> list[dict[str, Any]]:
        """Runs `set_reference` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_reference", items)

    def clear_reference(self) -> None:
        """Takes the reference picture away."""
        return self.action("clear_reference")

    def clear_reference_many(self, items: list[ClearReferenceArgs]) -> list[dict[str, Any]]:
        """Runs `clear_reference` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_reference", items)

    def compare_reference(self,
        *,
        mode: str | None = None,
        size: int | None = None,
    ) -> Any | None:
        """Shows the artwork beside (`side-by-side`), over (`overlay`) or against (`difference`) the reference picture, as a new file, with how far apart they are."""
        args: dict[str, Any] = {}
        if mode is not None:
            args["mode"] = mode
        if size is not None:
            args["size"] = size
        return self.action("compare_reference", **args)

    def compare_versions(self, *, change: str) -> str | None:
        """Compares the artwork with the version history names, overlaid on the surface: what changed between the two, and the changed region as it now stands where it stands."""
        return self.action("compare_versions", change=change)

    def clear_comparison(self) -> None:
        """Takes the version comparison away from the surface."""
        return self.action("clear_comparison")

    def clear_comparison_many(self, items: list[ClearComparisonArgs]) -> list[dict[str, Any]]:
        """Runs `clear_comparison` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_comparison", items)

    def ask_selection(self, *, ask: str) -> str | None:
        """Asks the engine for a selection, and keeps the area it answers."""
        return self.action("ask_selection", ask=ask)

    def select_shape(self,
        *,
        kind: str,
        from_: list[float],
        to: list[float],
        mode: str | None = None,
        feather: float | None = None,
        grow: float | None = None,
        invert: bool | None = None,
    ) -> str | None:
        """Selects a rectangle or an ellipse from one corner of a drag to the other, meeting the area there is by `mode` (`replace`, `add`, `subtract` or `intersect`)."""
        args: dict[str, Any] = {}
        args["kind"] = kind
        args["from"] = from_
        args["to"] = to
        if mode is not None:
            args["mode"] = mode
        if feather is not None:
            args["feather"] = feather
        if grow is not None:
            args["grow"] = grow
        if invert is not None:
            args["invert"] = invert
        return self.action("select_shape", **args)

    def select_lasso(self,
        *,
        points: list[list[float]],
        mode: str | None = None,
        feather: float | None = None,
        grow: float | None = None,
        invert: bool | None = None,
    ) -> str | None:
        """Selects the area a lasso traces, closed, meeting the area there is by `mode`."""
        args: dict[str, Any] = {}
        args["points"] = points
        if mode is not None:
            args["mode"] = mode
        if feather is not None:
            args["feather"] = feather
        if grow is not None:
            args["grow"] = grow
        if invert is not None:
            args["invert"] = invert
        return self.action("select_lasso", **args)

    def select_outline(self,
        *,
        d: str,
        mode: str | None = None,
        feather: float | None = None,
        grow: float | None = None,
        invert: bool | None = None,
    ) -> str | None:
        """Selects the area an outline names, meeting the area there is by `mode`."""
        args: dict[str, Any] = {}
        args["d"] = d
        if mode is not None:
            args["mode"] = mode
        if feather is not None:
            args["feather"] = feather
        if grow is not None:
            args["grow"] = grow
        if invert is not None:
            args["invert"] = invert
        return self.action("select_outline", **args)

    def grow_selection(self, *, by: float) -> str | None:
        """Grows (or shrinks, when negative) the selection's edge by `by` units."""
        return self.action("grow_selection", by=by)

    def invert_selection(self) -> str | None:
        """Turns the selection inside out: everything in the artwork but what it held."""
        return self.action("invert_selection")

    def select_marks(self,
        *,
        ids: list[str],
        mode: str | None = None,
    ) -> str | None:
        """Selects the shape of the marks or groups `ids` name, meeting the area there is by `mode`."""
        args: dict[str, Any] = {}
        args["ids"] = ids
        if mode is not None:
            args["mode"] = mode
        return self.action("select_marks", **args)

    def pick_mark(self,
        *,
        m: Mark,
        on: bool | None = None,
    ) -> None:
        """Picks a mark, or lets it go: combine and measure act on the picked marks."""
        args: dict[str, Any] = {}
        args["m"] = m
        if on is not None:
            args["on"] = on
        return self.action("pick_mark", **args)

    def pick_mark_many(self, items: list[PickMarkArgs]) -> list[dict[str, Any]]:
        """Runs `pick_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("pick_mark", items)

    def pick_marks(self, *, marks: list[Mark]) -> None:
        """Picks the marks, adding any that were not picked."""
        return self.action("pick_marks", marks=marks)

    def pick_marks_many(self, items: list[PickMarksArgs]) -> list[dict[str, Any]]:
        """Runs `pick_marks` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("pick_marks", items)

    def clear_picked(self) -> None:
        """Lets every picked mark go."""
        return self.action("clear_picked")

    def clear_picked_many(self, items: list[ClearPickedArgs]) -> list[dict[str, Any]]:
        """Runs `clear_picked` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_picked", items)

    def remove_along(self,
        *,
        points: list[list[float]],
        layer: Layer | None = None,
    ) -> int:
        """Removes the brush strokes a path crosses, on `layer` or every layer: what a stroke eraser does. Answers how many went."""
        args: dict[str, Any] = {}
        args["points"] = points
        if layer is not None:
            args["layer"] = layer
        return self.action("remove_along", **args)

    def inspect_artwork(self,
        *,
        palette: bool | None = None,
        limit: int | None = None,
        layer: str | None = None,
        region: list[float] | None = None,
    ) -> str | None:
        """Lists what the artwork holds: its layers and newest marks, with ids, bounds and colours, and with `palette` the colours it paints with. The answer is the engine's JSON."""
        args: dict[str, Any] = {}
        if palette is not None:
            args["palette"] = palette
        if limit is not None:
            args["limit"] = limit
        if layer is not None:
            args["layer"] = layer
        if region is not None:
            args["region"] = region
        return self.action("inspect_artwork", **args)

    def select_area(self,
        *,
        d: str,
        feather: float | None = None,
    ) -> None:
        """Selects an area, its outline as SVG path data."""
        args: dict[str, Any] = {}
        args["d"] = d
        if feather is not None:
            args["feather"] = feather
        return self.action("select_area", **args)

    def select_area_many(self, items: list[SelectAreaArgs]) -> list[dict[str, Any]]:
        """Runs `select_area` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("select_area", items)

    def clear_selection(self) -> None:
        """Lets go of the selection."""
        return self.action("clear_selection")

    def clear_selection_many(self, items: list[ClearSelectionArgs]) -> list[dict[str, Any]]:
        """Runs `clear_selection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_selection", items)

    def fill_selection(self,
        *,
        colour: str,
        layer: Layer | None = None,
    ) -> Mark | None:
        """Fills the selection as a path mark on a layer."""
        args: dict[str, Any] = {}
        if layer is not None:
            args["layer"] = layer
        args["colour"] = colour
        return self.action("fill_selection", **args)

    def draw_look(self,
        *,
        size: int | None = None,
    ) -> None:
        """The picture of the artwork as it stands, kept on it, so the surface shows it under the hand."""
        args: dict[str, Any] = {}
        if size is not None:
            args["size"] = size
        return self.action("draw_look", **args)

    def draw_look_many(self, items: list[DrawLookArgs]) -> list[dict[str, Any]]:
        """Runs `draw_look` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("draw_look", items)

    def export_picture(self,
        *,
        format: str | None = None,
        scale: float | None = None,
    ) -> Any | None:
        """Writes the artwork at full size as a PNG, JPEG or WebP picture."""
        args: dict[str, Any] = {}
        if format is not None:
            args["format"] = format
        if scale is not None:
            args["scale"] = scale
        return self.action("export_picture", **args)

    def export_svg(self) -> str:
        """Writes the artwork as an SVG document."""
        return self.action("export_svg")

    def save_artwork(self) -> Any | None:
        """Saves the artwork as a `.artwork` package carrying its pictures."""
        return self.action("save_artwork")

    def import_svg(self, *, markup: str) -> str:
        """Imports an SVG document as marks on a layer: each shape the device's drawing engine read from it becomes a mark, so the import is drawn, saved and shown like paint the person laid."""
        return self.action("import_svg", markup=markup)

    def clear_notice(self) -> None:
        """Clears the notice a person has read."""
        return self.action("clear_notice")

    def clear_notice_many(self, items: list[ClearNoticeArgs]) -> list[dict[str, Any]]:
        """Runs `clear_notice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_notice", items)

    def start_paint(self,
        *,
        tool: str,
        brush: str,
        point: list[float],
        size: float,
        colour: str,
        layer: Layer | None = None,
    ) -> None:
        """Starts what the hand lays down at `point`: a stroke with the paint loaded, or nothing when the tool draws a shape, whose outline only exists once the hand lifts. It lands on `layer`, the one the person chose, or the top one that takes paint."""
        args: dict[str, Any] = {}
        args["tool"] = tool
        args["brush"] = brush
        if layer is not None:
            args["layer"] = layer
        args["point"] = point
        args["size"] = size
        args["colour"] = colour
        return self.action("start_paint", **args)

    def start_paint_many(self, items: list[StartPaintArgs]) -> list[dict[str, Any]]:
        """Runs `start_paint` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("start_paint", items)

    def continue_paint(self,
        *,
        tool: str,
        brush: str,
        point: list[float],
        size: float,
        colour: str,
    ) -> Mark | None:
        """Takes the next point of the stroke being painted. A shape tool paints nothing while the hand moves: its outline is drawn when the hand lifts."""
        return self.action("continue_paint", tool=tool, brush=brush, point=point, size=size, colour=colour)

    def finish_paint(self,
        *,
        tool: str,
        brush: str,
        from_: list[float],
        point: list[float],
        size: float,
        colour: str,
        layer: Layer | None = None,
        lasso: list[list[float]] | None = None,
    ) -> Mark | None:
        """Ends what the hand laid down at `point`: the stroke takes its last point and closes, or a shape is drawn across the drag that made it."""
        args: dict[str, Any] = {}
        args["tool"] = tool
        args["brush"] = brush
        if layer is not None:
            args["layer"] = layer
        args["from"] = from_
        args["point"] = point
        args["size"] = size
        args["colour"] = colour
        if lasso is not None:
            args["lasso"] = lasso
        return self.action("finish_paint", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"add_layer", "rename_layer", "rename_layer_many", "lock_layer", "lock_layer_many", "clip_layer", "clip_layer_many", "add_mask", "set_mask", "set_mask_many", "add_text", "set_medium", "set_medium_many", "save_medium", "remove_medium", "remove_medium_many", "move_layer_up", "move_layer_up_many", "move_layer_down", "move_layer_down_many", "duplicate_layer", "merge_layer_down", "merge_layer_down_many", "show_layer", "show_layer_many", "remove_layer", "remove_layer_many", "set_artwork", "set_artwork_many", "add_stroke", "extend_stroke", "extend_stroke_many", "top_layer", "add_shape", "remove_mark", "remove_mark_many", "recolour", "recolour_many", "resize_mark", "resize_mark_many", "place_picture", "select_like", "select_subject", "refine_selection", "combine_marks", "measure_marks", "set_reference", "set_reference_many", "clear_reference", "clear_reference_many", "compare_reference", "compare_versions", "clear_comparison", "clear_comparison_many", "ask_selection", "select_shape", "select_lasso", "select_outline", "grow_selection", "invert_selection", "select_marks", "pick_mark", "pick_mark_many", "pick_marks", "pick_marks_many", "clear_picked", "clear_picked_many", "remove_along", "inspect_artwork", "select_area", "select_area_many", "clear_selection", "clear_selection_many", "fill_selection", "draw_look", "draw_look_many", "export_picture", "export_svg", "save_artwork", "import_svg", "clear_notice", "clear_notice_many", "start_paint", "start_paint_many", "continue_paint", "finish_paint"})


class Layer(Resource):
    """Layer — `tl.so.tilo.canvas.Layer`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    bplq2q5j: str  # name
    ilxjjxxz: str  # medium
    smoxtxmk: float  # opacity
    evne54yw: bool  # visible
    nagaw3zw: bool  # locked
    yzvcnjuy: float  # texture
    cwenklqw: str  # group
    rxz73i7f: str  # mask_of
    umkqodij: float  # base
    owktdb7d: bool  # invert
    n43pvuyc: bool  # clip
    @property
    def _2rtbmdqf(self) -> bool:
        """unlinked (`2rtbmdqf` on the wire)."""
        return self.fields["2rtbmdqf"]

    @_2rtbmdqf.setter
    def _2rtbmdqf(self, value: bool) -> None:
        self.edit("2rtbmdqf", value=value)
    zrxehlis: float  # exposure
    erzbebsl: float  # contrast
    qcpp23hg: float  # highlights
    xr5c7gly: float  # shadows
    cxp7piot: float  # saturation
    frr4f7qk: float  # vibrance
    @property
    def _7iz5iokq(self) -> float:
        """hue (`7iz5iokq` on the wire)."""
        return self.fields["7iz5iokq"]

    @_7iz5iokq.setter
    def _7iz5iokq(self, value: float) -> None:
        self.edit("7iz5iokq", value=value)
    lqesc6ol: float  # temperature
    @property
    def _4hpqnjer(self) -> float:
        """tint (`4hpqnjer` on the wire)."""
        return self.fields["4hpqnjer"]

    @_4hpqnjer.setter
    def _4hpqnjer(self, value: float) -> None:
        self.edit("4hpqnjer", value=value)
    @property
    def _2zsbapjy(self) -> bool:
        """adjust_grayscale (`2zsbapjy` on the wire)."""
        return self.fields["2zsbapjy"]

    @_2zsbapjy.setter
    def _2zsbapjy(self, value: bool) -> None:
        self.edit("2zsbapjy", value=value)
    giajnlvh: bool  # adjust_invert
    @property
    def _76aq4pc3(self) -> float:
        """blur (`76aq4pc3` on the wire)."""
        return self.fields["76aq4pc3"]

    @_76aq4pc3.setter
    def _76aq4pc3(self, value: float) -> None:
        self.edit("76aq4pc3", value=value)
    nraansa6: float  # sharpen
    yxn6dvna: float  # vignette
    mvgiivbx: list[list[float]]  # curve

    def set_layer_group(self, *, group: str) -> None:
        """Puts a layer in the folder `group`, or takes it out when that is empty."""
        return self.action("set_layer_group", group=group)

    def set_layer_group_many(self, items: list[SetLayerGroupArgs]) -> list[dict[str, Any]]:
        """Runs `set_layer_group` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_layer_group", items)

    def adjust_layer(self,
        *,
        exposure: float | None = None,
        contrast: float | None = None,
        highlights: float | None = None,
        shadows: float | None = None,
        saturation: float | None = None,
        vibrance: float | None = None,
        hue: float | None = None,
        temperature: float | None = None,
        tint: float | None = None,
        grayscale: bool | None = None,
        invert: bool | None = None,
        blur: float | None = None,
        sharpen: float | None = None,
        vignette: float | None = None,
    ) -> None:
        """Adjusts everything on a layer, after each mark's own adjustments."""
        args: dict[str, Any] = {}
        if exposure is not None:
            args["exposure"] = exposure
        if contrast is not None:
            args["contrast"] = contrast
        if highlights is not None:
            args["highlights"] = highlights
        if shadows is not None:
            args["shadows"] = shadows
        if saturation is not None:
            args["saturation"] = saturation
        if vibrance is not None:
            args["vibrance"] = vibrance
        if hue is not None:
            args["hue"] = hue
        if temperature is not None:
            args["temperature"] = temperature
        if tint is not None:
            args["tint"] = tint
        if grayscale is not None:
            args["grayscale"] = grayscale
        if invert is not None:
            args["invert"] = invert
        if blur is not None:
            args["blur"] = blur
        if sharpen is not None:
            args["sharpen"] = sharpen
        if vignette is not None:
            args["vignette"] = vignette
        return self.action("adjust_layer", **args)

    def adjust_layer_many(self, items: list[AdjustLayerArgs]) -> list[dict[str, Any]]:
        """Runs `adjust_layer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("adjust_layer", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"set_layer_group", "set_layer_group_many", "adjust_layer", "adjust_layer_many"})


class Mark(Resource):
    """Mark — `tl.so.tilo.canvas.Mark`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    @property
    def _3mgqcu4c(self) -> str:
        """name (`3mgqcu4c` on the wire)."""
        return self.fields["3mgqcu4c"]

    @_3mgqcu4c.setter
    def _3mgqcu4c(self, value: str) -> None:
        self.edit("3mgqcu4c", value=value)
    xeahkwbd: Layer  # layer
    pwxdvgoe: str  # kind
    @property
    def _4eq3fpao(self) -> list[float]:
        """points (`4eq3fpao` on the wire)."""
        return self.fields["4eq3fpao"]

    @_4eq3fpao.setter
    def _4eq3fpao(self, value: list[float]) -> None:
        self.edit("4eq3fpao", value=value)
    qxx46dqp: str  # d
    vmqyvhnc: float  # size
    fnl5j4dd: str  # colour
    omem6tkf: str  # brush
    kytiqfia: float  # opacity
    y5teghkv: bool  # open
    hyumfmpg: str  # group
    lu2gjrfu: bool  # wet
    s2tjjp2p: dict[str, float]  # params
    cdvrgjan: str  # src
    vniyw6t3: float  # x
    v3wlgndz: float  # y
    @property
    def _4nv5sha3(self) -> float:
        """width (`4nv5sha3` on the wire)."""
        return self.fields["4nv5sha3"]

    @_4nv5sha3.setter
    def _4nv5sha3(self, value: float) -> None:
        self.edit("4nv5sha3", value=value)
    hlvljkif: float  # height
    o4zghm7q: str  # blend
    wpv66rxy: float  # crop_x
    sz6fzruq: float  # crop_y
    ax7jjgo5: float  # crop_width
    @property
    def _7jtq2b7o(self) -> float:
        """crop_height (`7jtq2b7o` on the wire)."""
        return self.fields["7jtq2b7o"]

    @_7jtq2b7o.setter
    def _7jtq2b7o(self, value: float) -> None:
        self.edit("7jtq2b7o", value=value)
    mzyxo2mo: float  # straighten
    xndatwwg: bool  # flip_h
    vyaxfcb2: bool  # flip_v
    @property
    def _37ailkzg(self) -> float:
        """exposure (`37ailkzg` on the wire)."""
        return self.fields["37ailkzg"]

    @_37ailkzg.setter
    def _37ailkzg(self, value: float) -> None:
        self.edit("37ailkzg", value=value)
    g3ifa5op: float  # contrast
    @property
    def _3taebyjo(self) -> float:
        """highlights (`3taebyjo` on the wire)."""
        return self.fields["3taebyjo"]

    @_3taebyjo.setter
    def _3taebyjo(self, value: float) -> None:
        self.edit("3taebyjo", value=value)
    @property
    def _5d6dfhop(self) -> float:
        """shadows (`5d6dfhop` on the wire)."""
        return self.fields["5d6dfhop"]

    @_5d6dfhop.setter
    def _5d6dfhop(self, value: float) -> None:
        self.edit("5d6dfhop", value=value)
    nebzyt3a: float  # saturation
    @property
    def _6ib7oqy4(self) -> float:
        """vibrance (`6ib7oqy4` on the wire)."""
        return self.fields["6ib7oqy4"]

    @_6ib7oqy4.setter
    def _6ib7oqy4(self, value: float) -> None:
        self.edit("6ib7oqy4", value=value)
    vzagvyr3: float  # hue
    bdxzjdby: float  # temperature
    skvl4xui: float  # tint
    @property
    def _3tuoc3tu(self) -> bool:
        """grayscale (`3tuoc3tu` on the wire)."""
        return self.fields["3tuoc3tu"]

    @_3tuoc3tu.setter
    def _3tuoc3tu(self, value: bool) -> None:
        self.edit("3tuoc3tu", value=value)
    tr7kuqc5: bool  # invert
    qe7z2w5c: float  # blur
    h6rqzqu5: float  # sharpen
    glowmwgx: float  # vignette
    jzqxuy4d: str  # gradient
    @property
    def _2afylrq3(self) -> float:
        """gradient_x0 (`2afylrq3` on the wire)."""
        return self.fields["2afylrq3"]

    @_2afylrq3.setter
    def _2afylrq3(self, value: float) -> None:
        self.edit("2afylrq3", value=value)
    skgor2yj: float  # gradient_y0
    ctpi3sbl: float  # gradient_x1
    nosksv3s: float  # gradient_y1
    n4zfbtzn: list[float] | None  # gradient_offsets
    @property
    def _7vai53br(self) -> list[str] | None:
        """gradient_colours (`7vai53br` on the wire)."""
        return self.fields["7vai53br"]

    @_7vai53br.setter
    def _7vai53br(self, value: list[str] | None) -> None:
        self.edit("7vai53br", value=value)
    @property
    def _63yhsfnf(self) -> str:
        """text (`63yhsfnf` on the wire)."""
        return self.fields["63yhsfnf"]

    @_63yhsfnf.setter
    def _63yhsfnf(self, value: str) -> None:
        self.edit("63yhsfnf", value=value)
    xbmipp4j: str  # font
    ymnuknae: int  # weight
    u62kvwpy: bool  # italic
    mj3rz5s2: str  # align
    i7zjtca3: float  # line_height
    sc445r7q: float  # tracking
    e7l24fze: str  # text_path
    @property
    def _5lpu3cr5(self) -> list[float]:
        """transform (`5lpu3cr5` on the wire)."""
        return self.fields["5lpu3cr5"]

    @_5lpu3cr5.setter
    def _5lpu3cr5(self, value: list[float]) -> None:
        self.edit("5lpu3cr5", value=value)
    @property
    def _7kaylfaa(self) -> str:
        """clip (`7kaylfaa` on the wire)."""
        return self.fields["7kaylfaa"]

    @_7kaylfaa.setter
    def _7kaylfaa(self, value: str) -> None:
        self.edit("7kaylfaa", value=value)
    adangnyz: bool  # pixelated
    p45w3ec2: float  # grain
    gdv4nrn7: float  # feather
    dvxs4mkf: float  # shadow_x
    ebyye73f: float  # shadow_y
    lpv4a2ao: float  # shadow_blur
    j3x5co4v: str  # shadow_colour
    @property
    def _4d36wjcg(self) -> float:
        """inner_shadow_x (`4d36wjcg` on the wire)."""
        return self.fields["4d36wjcg"]

    @_4d36wjcg.setter
    def _4d36wjcg(self, value: float) -> None:
        self.edit("4d36wjcg", value=value)
    aukevlpf: float  # inner_shadow_y
    wz2pco6l: float  # inner_shadow_blur
    @property
    def _3hhduagr(self) -> str:
        """inner_shadow_colour (`3hhduagr` on the wire)."""
        return self.fields["3hhduagr"]

    @_3hhduagr.setter
    def _3hhduagr(self, value: str) -> None:
        self.edit("3hhduagr", value=value)
    @property
    def _66bccfgi(self) -> list[list[float]]:
        """curve (`66bccfgi` on the wire)."""
        return self.fields["66bccfgi"]

    @_66bccfgi.setter
    def _66bccfgi(self, value: list[list[float]]) -> None:
        self.edit("66bccfgi", value=value)

    def group_mark(self, *, group: str) -> None:
        """Puts a mark in the folder `group`, or takes it out when that is empty. Marks of a group are addressed, exported and shown together."""
        return self.action("group_mark", group=group)

    def group_mark_many(self, items: list[GroupMarkArgs]) -> list[dict[str, Any]]:
        """Runs `group_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("group_mark", items)

    def fill_gradient(self,
        *,
        x0: float,
        y0: float,
        x1: float,
        y1: float,
        offsets: list[float],
        colours: list[str],
        kind: str | None = None,
    ) -> None:
        """Fills a mark with a gradient from `from` to `to`, its stops given as offsets 0 to 1 beside their colours."""
        args: dict[str, Any] = {}
        if kind is not None:
            args["kind"] = kind
        args["x0"] = x0
        args["y0"] = y0
        args["x1"] = x1
        args["y1"] = y1
        args["offsets"] = offsets
        args["colours"] = colours
        return self.action("fill_gradient", **args)

    def fill_gradient_many(self, items: list[FillGradientArgs]) -> list[dict[str, Any]]:
        """Runs `fill_gradient` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("fill_gradient", items)

    def wet_mark(self,
        *,
        wet: bool | None = None,
    ) -> None:
        """Paints a mark into what is still wet, or over dry paint."""
        args: dict[str, Any] = {}
        if wet is not None:
            args["wet"] = wet
        return self.action("wet_mark", **args)

    def wet_mark_many(self, items: list[WetMarkArgs]) -> list[dict[str, Any]]:
        """Runs `wet_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("wet_mark", items)

    def clear_gradient(self) -> None:
        """Clears a mark's gradient, leaving the plain colour."""
        return self.action("clear_gradient")

    def clear_gradient_many(self, items: list[ClearGradientArgs]) -> list[dict[str, Any]]:
        """Runs `clear_gradient` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_gradient", items)

    def set_text(self,
        *,
        text: str | None = None,
        size: float | None = None,
        colour: str | None = None,
        font: str | None = None,
        weight: int | None = None,
        italic: bool | None = None,
        align: str | None = None,
        line_height: float | None = None,
        tracking: float | None = None,
        path: str | None = None,
    ) -> None:
        """Sets a text mark's words, its type and its setting. A parameter left out stays as it is."""
        args: dict[str, Any] = {}
        if text is not None:
            args["text"] = text
        if size is not None:
            args["size"] = size
        if colour is not None:
            args["colour"] = colour
        if font is not None:
            args["font"] = font
        if weight is not None:
            args["weight"] = weight
        if italic is not None:
            args["italic"] = italic
        if align is not None:
            args["align"] = align
        if line_height is not None:
            args["line_height"] = line_height
        if tracking is not None:
            args["tracking"] = tracking
        if path is not None:
            args["path"] = path
        return self.action("set_text", **args)

    def set_text_many(self, items: list[SetTextArgs]) -> list[dict[str, Any]]:
        """Runs `set_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_text", items)

    def crop_picture(self,
        *,
        x: float | None = None,
        y: float | None = None,
        width: float | None = None,
        height: float | None = None,
    ) -> None:
        """Shows part of the picture: the frame `x`, `y`, `width`, `height` of its own pixels, each a fraction of them. The picture stays put and the frame follows, so a crop is an edit of the mark and never of the pixels."""
        args: dict[str, Any] = {}
        if x is not None:
            args["x"] = x
        if y is not None:
            args["y"] = y
        if width is not None:
            args["width"] = width
        if height is not None:
            args["height"] = height
        return self.action("crop_picture", **args)

    def crop_picture_many(self, items: list[CropPictureArgs]) -> list[dict[str, Any]]:
        """Runs `crop_picture` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("crop_picture", items)

    def straighten_picture(self, *, degrees: float) -> None:
        """Turns the picture inside its frame, in degrees, enlarging it so no corner shows."""
        return self.action("straighten_picture", degrees=degrees)

    def straighten_picture_many(self, items: list[StraightenPictureArgs]) -> list[dict[str, Any]]:
        """Runs `straighten_picture` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("straighten_picture", items)

    def flip_picture(self,
        *,
        horizontal: bool | None = None,
        vertical: bool | None = None,
        flip_h: bool | None = None,
        flip_v: bool | None = None,
    ) -> None:
        """Mirrors the picture across its middle."""
        args: dict[str, Any] = {}
        if horizontal is not None:
            args["horizontal"] = horizontal
        if vertical is not None:
            args["vertical"] = vertical
        if flip_h is not None:
            args["flip_h"] = flip_h
        if flip_v is not None:
            args["flip_v"] = flip_v
        return self.action("flip_picture", **args)

    def flip_picture_many(self, items: list[FlipPictureArgs]) -> list[dict[str, Any]]:
        """Runs `flip_picture` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("flip_picture", items)

    def blend_mark(self, *, blend: str) -> None:
        """Sets how a mark mixes with what is under it."""
        return self.action("blend_mark", blend=blend)

    def blend_mark_many(self, items: list[BlendMarkArgs]) -> list[dict[str, Any]]:
        """Runs `blend_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("blend_mark", items)

    def adjust_mark(self,
        *,
        exposure: float | None = None,
        contrast: float | None = None,
        highlights: float | None = None,
        shadows: float | None = None,
        saturation: float | None = None,
        vibrance: float | None = None,
        hue: float | None = None,
        temperature: float | None = None,
        tint: float | None = None,
        grayscale: bool | None = None,
        invert: bool | None = None,
        blur: float | None = None,
        sharpen: float | None = None,
        vignette: float | None = None,
    ) -> None:
        """Grades a mark without touching its pixels: exposure, contrast and the rest are kept on the mark and applied wherever the artwork is shown or exported. A parameter left out stays as it is, so an agent changes one step at a time."""
        args: dict[str, Any] = {}
        if exposure is not None:
            args["exposure"] = exposure
        if contrast is not None:
            args["contrast"] = contrast
        if highlights is not None:
            args["highlights"] = highlights
        if shadows is not None:
            args["shadows"] = shadows
        if saturation is not None:
            args["saturation"] = saturation
        if vibrance is not None:
            args["vibrance"] = vibrance
        if hue is not None:
            args["hue"] = hue
        if temperature is not None:
            args["temperature"] = temperature
        if tint is not None:
            args["tint"] = tint
        if grayscale is not None:
            args["grayscale"] = grayscale
        if invert is not None:
            args["invert"] = invert
        if blur is not None:
            args["blur"] = blur
        if sharpen is not None:
            args["sharpen"] = sharpen
        if vignette is not None:
            args["vignette"] = vignette
        return self.action("adjust_mark", **args)

    def adjust_mark_many(self, items: list[AdjustMarkArgs]) -> list[dict[str, Any]]:
        """Runs `adjust_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("adjust_mark", items)

    def move_mark(self,
        *,
        dx: float,
        dy: float,
    ) -> None:
        """Moves a mark by `dx`, `dy` artwork units."""
        return self.action("move_mark", dx=dx, dy=dy)

    def move_mark_many(self, items: list[MoveMarkArgs]) -> list[dict[str, Any]]:
        """Runs `move_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_mark", items)

    def scale_mark(self,
        *,
        cx: float,
        cy: float,
        factor: float,
    ) -> None:
        """Scales a mark about `cx`, `cy` by `factor`."""
        return self.action("scale_mark", cx=cx, cy=cy, factor=factor)

    def scale_mark_many(self, items: list[ScaleMarkArgs]) -> list[dict[str, Any]]:
        """Runs `scale_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("scale_mark", items)

    def rotate_mark(self,
        *,
        cx: float,
        cy: float,
        degrees: float,
    ) -> None:
        """Turns a mark about `cx`, `cy` by `degrees`."""
        return self.action("rotate_mark", cx=cx, cy=cy, degrees=degrees)

    def rotate_mark_many(self, items: list[RotateMarkArgs]) -> list[dict[str, Any]]:
        """Runs `rotate_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rotate_mark", items)

    def clip_mark(self, *, by: str) -> None:
        """Cuts a mark to the shape of another mark or a group, or lets it stand whole."""
        return self.action("clip_mark", by=by)

    def clip_mark_many(self, items: list[ClipMarkArgs]) -> list[dict[str, Any]]:
        """Runs `clip_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clip_mark", items)

    def finish_mark(self,
        *,
        pixelated: bool | None = None,
        grain: float | None = None,
        feather: float | None = None,
        shadow_x: float | None = None,
        shadow_y: float | None = None,
        shadow_blur: float | None = None,
        shadow_colour: str | None = None,
        inner_shadow_x: float | None = None,
        inner_shadow_y: float | None = None,
        inner_shadow_blur: float | None = None,
        inner_shadow_colour: str | None = None,
    ) -> None:
        """Sets how a mark is finished: nearest-neighbour pixels, grain, a feathered edge, and its shadows. A parameter left out stays as it is."""
        args: dict[str, Any] = {}
        if pixelated is not None:
            args["pixelated"] = pixelated
        if grain is not None:
            args["grain"] = grain
        if feather is not None:
            args["feather"] = feather
        if shadow_x is not None:
            args["shadow_x"] = shadow_x
        if shadow_y is not None:
            args["shadow_y"] = shadow_y
        if shadow_blur is not None:
            args["shadow_blur"] = shadow_blur
        if shadow_colour is not None:
            args["shadow_colour"] = shadow_colour
        if inner_shadow_x is not None:
            args["inner_shadow_x"] = inner_shadow_x
        if inner_shadow_y is not None:
            args["inner_shadow_y"] = inner_shadow_y
        if inner_shadow_blur is not None:
            args["inner_shadow_blur"] = inner_shadow_blur
        if inner_shadow_colour is not None:
            args["inner_shadow_colour"] = inner_shadow_colour
        return self.action("finish_mark", **args)

    def finish_mark_many(self, items: list[FinishMarkArgs]) -> list[dict[str, Any]]:
        """Runs `finish_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("finish_mark", items)

    def set_curve(self, *, curve: list[list[float]]) -> None:
        """Sets a mark's tone curve, `[[in, out], …]` from 0 to 1."""
        return self.action("set_curve", curve=curve)

    def set_curve_many(self, items: list[SetCurveArgs]) -> list[dict[str, Any]]:
        """Runs `set_curve` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_curve", items)

    def set_dynamics(self,
        *,
        flow: float | None = None,
        hardness: float | None = None,
        spacing: float | None = None,
        smoothing: float | None = None,
        taper: float | None = None,
    ) -> None:
        """Sets how the brush lays a mark: its flow, hardness, spacing, smoothing and taper, each 0 to 1. A parameter left out stays as it is."""
        args: dict[str, Any] = {}
        if flow is not None:
            args["flow"] = flow
        if hardness is not None:
            args["hardness"] = hardness
        if spacing is not None:
            args["spacing"] = spacing
        if smoothing is not None:
            args["smoothing"] = smoothing
        if taper is not None:
            args["taper"] = taper
        return self.action("set_dynamics", **args)

    def set_dynamics_many(self, items: list[SetDynamicsArgs]) -> list[dict[str, Any]]:
        """Runs `set_dynamics` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_dynamics", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"group_mark", "group_mark_many", "fill_gradient", "fill_gradient_many", "wet_mark", "wet_mark_many", "clear_gradient", "clear_gradient_many", "set_text", "set_text_many", "crop_picture", "crop_picture_many", "straighten_picture", "straighten_picture_many", "flip_picture", "flip_picture_many", "blend_mark", "blend_mark_many", "adjust_mark", "adjust_mark_many", "move_mark", "move_mark_many", "scale_mark", "scale_mark_many", "rotate_mark", "rotate_mark_many", "clip_mark", "clip_mark_many", "finish_mark", "finish_mark_many", "set_curve", "set_curve_many", "set_dynamics", "set_dynamics_many"})


class Area(Resource):
    """Area — `tl.so.tilo.canvas.Area`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    k26xq4t7: str  # d
    enongrrr: float  # feather
    j7jomf5b: str  # matte_src
    oeihhduj: float  # matte_x
    iqzt5i5r: float  # matte_y
    l6zpym3q: float  # matte_width
    to7qczkv: float  # matte_height


class Picture(Resource):
    """Picture — `tl.so.tilo.canvas.Picture`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    di5h6qir: str  # name
    @property
    def _4be6ojwm(self) -> str:
        """url (`4be6ojwm` on the wire)."""
        return self.fields["4be6ojwm"]

    @_4be6ojwm.setter
    def _4be6ojwm(self, value: str) -> None:
        self.edit("4be6ojwm", value=value)
    rp5ikqj7: int  # width
    @property
    def _4r7lfx7t(self) -> int:
        """height (`4r7lfx7t` on the wire)."""
        return self.fields["4r7lfx7t"]

    @_4r7lfx7t.setter
    def _4r7lfx7t(self, value: int) -> None:
        self.edit("4r7lfx7t", value=value)


class Medium(Resource):
    """Medium — `tl.so.tilo.canvas.Medium`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    bevph2qk: str  # key
    cgytyo4j: str  # label
    @property
    def _2opuzdzr(self) -> str:
        """engine (`2opuzdzr` on the wire)."""
        return self.fields["2opuzdzr"]

    @_2opuzdzr.setter
    def _2opuzdzr(self, value: str) -> None:
        self.edit("2opuzdzr", value=value)
    mgi6schv: str  # description
    @property
    def _6r6opsjz(self) -> str:
        """ground (`6r6opsjz` on the wire)."""
        return self.fields["6r6opsjz"]

    @_6r6opsjz.setter
    def _6r6opsjz(self, value: str) -> None:
        self.edit("6r6opsjz", value=value)
    hids323q: dict[str, float]  # params
    rdqoi6b4: list[Pigment]  # palette


class Pigment(Resource):
    """Pigment — `tl.so.tilo.canvas.Pigment`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    ph4qfvlv: str  # name
    @property
    def _6heumgwe(self) -> str:
        """colour (`6heumgwe` on the wire)."""
        return self.fields["6heumgwe"]

    @_6heumgwe.setter
    def _6heumgwe(self, value: str) -> None:
        self.edit("6heumgwe", value=value)
    tm7hljog: float  # granulation
    t2xyhkxn: float  # strength


class Reference(Resource):
    """Reference — `tl.so.tilo.canvas.Reference`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    @property
    def _3ze3qk7e(self) -> str:
        """image (`3ze3qk7e` on the wire)."""
        return self.fields["3ze3qk7e"]

    @_3ze3qk7e.setter
    def _3ze3qk7e(self, value: str) -> None:
        self.edit("3ze3qk7e", value=value)
    ky6t7zk4: float  # x
    k5uohe26: float  # y
    vyzji3vv: float  # width
    peuuxvxb: float  # height
    decytxpt: float  # opacity
    j3jxn7s3: bool  # visible


class Library(Resource):
    """Library — `tl.so.tilo.canvas.Library`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    @property
    def _5gxbqk6v(self) -> list[Entry[Artwork]]:
        """artworks (`5gxbqk6v` on the wire)."""
        return self.fields["5gxbqk6v"]

    @_5gxbqk6v.setter
    def _5gxbqk6v(self, value: list[Entry[Artwork]]) -> None:
        self.edit("5gxbqk6v", value=value)

    def open_artwork(self, *, file: Any) -> Artwork:
        """Opens a saved artwork from the file a person, Files or another app hands over."""
        return self.action("open_artwork", file=file)

    def blank_with(self, *, why: str) -> Artwork:
        """A blank artwork begun with a problem to show, so a file that cannot be opened still lands where a person can read why."""
        return self.action("blank_with", why=why)

    def new_artwork(self,
        *,
        title: str,
        width: float,
        height: float,
    ) -> Artwork:
        """Starts a blank artwork with one plain layer."""
        return self.action("new_artwork", title=title, width=width, height=height)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"open_artwork", "blank_with", "new_artwork"})


class LookRequest(Resource):
    """LookRequest — `tl.so.tilo.canvas.LookRequest`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    hhu6bz5c: int  # size
    ubid4c7s: str  # format


class ExportRequest(Resource):
    """ExportRequest — `tl.so.tilo.canvas.ExportRequest`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    ottkj6jd: str  # format
    @property
    def _5mutx32x(self) -> float:
        """scale (`5mutx32x` on the wire)."""
        return self.fields["5mutx32x"]

    @_5mutx32x.setter
    def _5mutx32x(self, value: float) -> None:
        self.edit("5mutx32x", value=value)


class SvgRequest(Resource):
    """SvgRequest — `tl.so.tilo.canvas.SvgRequest`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    c35hmrf6: list[str] | None  # ids
    gqfdwswn: bool | None  # outline_text


class PlaceRequest(Resource):
    """PlaceRequest — `tl.so.tilo.canvas.PlaceRequest`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    @property
    def _2w4n3dr2(self) -> str | None:
        """layer (`2w4n3dr2` on the wire)."""
        return self.fields["2w4n3dr2"]

    @_2w4n3dr2.setter
    def _2w4n3dr2(self, value: str | None) -> None:
        self.edit("2w4n3dr2", value=value)
    zxu35ztu: str | None  # group
    ulngw3pf: list[float] | None  # fit


class ArtworkJson(Resource):
    """ArtworkJson — `tl.so.tilo.canvas.ArtworkJson`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    j3bzgr3f: str  # title
    dowdfhi2: float  # width
    vlrgzghe: float  # height
    lotrow4q: str  # ground_texture
    mpyvrw7z: str  # ground_colour
    xubd3xon: list[LayerJson]  # layers
    lowvdtkg: list[MarkJson]  # marks
    hoix2o4s: list[MediumJson]  # media
    @property
    def _5h6vjvk2(self) -> AreaJson | None:
        """selection (`5h6vjvk2` on the wire)."""
        return self.fields["5h6vjvk2"]

    @_5h6vjvk2.setter
    def _5h6vjvk2(self, value: AreaJson | None) -> None:
        self.edit("5h6vjvk2", value=value)
    n77pojfm: ReferenceJson | None  # reference


class LayerJson(Resource):
    """LayerJson — `tl.so.tilo.canvas.LayerJson`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    xunw6qmv: str  # name
    sxka2uxh: str  # medium
    aka7ql3b: float  # opacity
    @property
    def _4h7cxhog(self) -> bool:
        """visible (`4h7cxhog` on the wire)."""
        return self.fields["4h7cxhog"]

    @_4h7cxhog.setter
    def _4h7cxhog(self, value: bool) -> None:
        self.edit("4h7cxhog", value=value)
    @property
    def _2r4nberb(self) -> bool:
        """locked (`2r4nberb` on the wire)."""
        return self.fields["2r4nberb"]

    @_2r4nberb.setter
    def _2r4nberb(self, value: bool) -> None:
        self.edit("2r4nberb", value=value)
    xlvw3rkz: float  # texture
    km2u7cyx: str  # group
    qhmnxju4: str  # mask_of
    @property
    def _6oqanygz(self) -> float:
        """base (`6oqanygz` on the wire)."""
        return self.fields["6oqanygz"]

    @_6oqanygz.setter
    def _6oqanygz(self, value: float) -> None:
        self.edit("6oqanygz", value=value)
    y6iyulut: bool  # invert
    k2r2z54p: bool  # clip
    ppv32nn6: bool  # unlinked
    z56o2ag2: float  # exposure
    ui3khk6w: float  # contrast
    yynr6bsh: float  # highlights
    @property
    def _3kml5egf(self) -> float:
        """shadows (`3kml5egf` on the wire)."""
        return self.fields["3kml5egf"]

    @_3kml5egf.setter
    def _3kml5egf(self, value: float) -> None:
        self.edit("3kml5egf", value=value)
    @property
    def _5c4xwrcg(self) -> float:
        """saturation (`5c4xwrcg` on the wire)."""
        return self.fields["5c4xwrcg"]

    @_5c4xwrcg.setter
    def _5c4xwrcg(self, value: float) -> None:
        self.edit("5c4xwrcg", value=value)
    gi73p5cu: float  # vibrance
    @property
    def _7r6f5gkz(self) -> float:
        """hue (`7r6f5gkz` on the wire)."""
        return self.fields["7r6f5gkz"]

    @_7r6f5gkz.setter
    def _7r6f5gkz(self, value: float) -> None:
        self.edit("7r6f5gkz", value=value)
    tfdtaklm: float  # temperature
    oanzkyda: float  # tint
    aj3mmig4: bool  # grayscale
    @property
    def _2cktvie6(self) -> bool:
        """invert_adjust (`2cktvie6` on the wire)."""
        return self.fields["2cktvie6"]

    @_2cktvie6.setter
    def _2cktvie6(self, value: bool) -> None:
        self.edit("2cktvie6", value=value)
    zkir2g3c: float  # blur
    frk7zwuc: float  # sharpen
    pftddo22: float  # vignette
    eenyv7e2: list[list[float]]  # curve


class MediumJson(Resource):
    """MediumJson — `tl.so.tilo.canvas.MediumJson`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    iztu5hbq: str  # id
    shen6puc: str  # label
    pcwg6twm: str  # engine
    tucyloxp: str  # description
    @property
    def _3g5vtp7l(self) -> str:
        """ground (`3g5vtp7l` on the wire)."""
        return self.fields["3g5vtp7l"]

    @_3g5vtp7l.setter
    def _3g5vtp7l(self, value: str) -> None:
        self.edit("3g5vtp7l", value=value)
    athamtu3: dict[str, float]  # params
    pq2ipocw: list[PigmentJson]  # palette


class PigmentJson(Resource):
    """PigmentJson — `tl.so.tilo.canvas.PigmentJson`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    dgwlsywk: str  # name
    b74p3dso: str  # colour
    rfus746j: float  # granulation
    veeu7fpc: float  # strength


class ReferenceJson(Resource):
    """ReferenceJson — `tl.so.tilo.canvas.ReferenceJson`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    go76fdwt: str  # image
    ax3jntcc: list[float] | None  # frame
    urv4gnep: float  # opacity
    zbqd3iad: bool  # visible


class MarkJson(Resource):
    """MarkJson — `tl.so.tilo.canvas.MarkJson`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    @property
    def _67wvmkkq(self) -> str:
        """name (`67wvmkkq` on the wire)."""
        return self.fields["67wvmkkq"]

    @_67wvmkkq.setter
    def _67wvmkkq(self, value: str) -> None:
        self.edit("67wvmkkq", value=value)
    jk7lnwfw: LayerJson  # layer
    xy2goji2: str  # kind
    lrfhvryb: str  # group
    ysn6onwt: list[float]  # points
    vmvvarxw: str  # d
    hajhmux3: float  # size
    gtn2xdwh: str  # colour
    pqrpx7k3: str  # brush
    kolk77nz: float  # opacity
    @property
    def _7hgp7nbk(self) -> str:
        """src (`7hgp7nbk` on the wire)."""
        return self.fields["7hgp7nbk"]

    @_7hgp7nbk.setter
    def _7hgp7nbk(self, value: str) -> None:
        self.edit("7hgp7nbk", value=value)
    vtafbavc: float  # x
    el3izuuy: float  # y
    jzpg25ot: float  # width
    k7jjvary: float  # height
    f2yfdpsw: str  # blend
    gbswtfkm: float  # crop_x
    cnv33zk4: float  # crop_y
    yzjgjp5q: float  # crop_width
    d2tduo5n: float  # crop_height
    j7tormbq: float  # straighten
    vmes3q7g: bool  # flip_h
    @property
    def _4tv2wnsi(self) -> bool:
        """flip_v (`4tv2wnsi` on the wire)."""
        return self.fields["4tv2wnsi"]

    @_4tv2wnsi.setter
    def _4tv2wnsi(self, value: bool) -> None:
        self.edit("4tv2wnsi", value=value)
    tnsniv2d: float  # exposure
    xsuhepir: float  # contrast
    rhmkylpp: float  # highlights
    qqdl7hij: float  # shadows
    asuq3lgp: float  # saturation
    fxjyqy3l: float  # vibrance
    rf2rek22: float  # hue
    oqo6d26l: float  # temperature
    suw3mw5t: float  # tint
    prqekzin: bool  # grayscale
    ulnewmko: bool  # invert
    hhwbutye: float  # blur
    aooc5sn5: float  # sharpen
    y32qfhyc: float  # vignette
    r77f22g6: str  # gradient_kind
    nuba72na: float  # gradient_x0
    yvaatile: float  # gradient_y0
    jdbhzols: float  # gradient_x1
    tv6gyxgx: float  # gradient_y1
    ih74f5za: list[float]  # gradient_offsets
    zvcp7y2l: list[str]  # gradient_colours
    xub3tsun: str  # text
    fkp53ei3: str  # font
    @property
    def _7nusoc27(self) -> int:
        """weight (`7nusoc27` on the wire)."""
        return self.fields["7nusoc27"]

    @_7nusoc27.setter
    def _7nusoc27(self, value: int) -> None:
        self.edit("7nusoc27", value=value)
    n3izluzh: bool  # italic
    @property
    def _5dk4ceey(self) -> str:
        """align (`5dk4ceey` on the wire)."""
        return self.fields["5dk4ceey"]

    @_5dk4ceey.setter
    def _5dk4ceey(self, value: str) -> None:
        self.edit("5dk4ceey", value=value)
    gnfltu6g: float  # line_height
    u2tmalez: float  # tracking
    hyqcnmz5: str  # text_path
    q73msd2q: bool  # wet
    gfq7akbn: dict[str, float]  # params
    uk5tcnwm: list[float]  # transform
    w4ew2p2r: str  # clip
    @property
    def _3bhfidei(self) -> bool:
        """pixelated (`3bhfidei` on the wire)."""
        return self.fields["3bhfidei"]

    @_3bhfidei.setter
    def _3bhfidei(self, value: bool) -> None:
        self.edit("3bhfidei", value=value)
    @property
    def _6emj7eax(self) -> float:
        """grain (`6emj7eax` on the wire)."""
        return self.fields["6emj7eax"]

    @_6emj7eax.setter
    def _6emj7eax(self, value: float) -> None:
        self.edit("6emj7eax", value=value)
    tcpy73ad: float  # feather
    @property
    def _3oq5rjv3(self) -> float:
        """shadow_x (`3oq5rjv3` on the wire)."""
        return self.fields["3oq5rjv3"]

    @_3oq5rjv3.setter
    def _3oq5rjv3(self, value: float) -> None:
        self.edit("3oq5rjv3", value=value)
    ho5hmfdi: float  # shadow_y
    @property
    def _7v73mzgx(self) -> float:
        """shadow_blur (`7v73mzgx` on the wire)."""
        return self.fields["7v73mzgx"]

    @_7v73mzgx.setter
    def _7v73mzgx(self, value: float) -> None:
        self.edit("7v73mzgx", value=value)
    aktlep6h: str  # shadow_colour
    aayx2zu4: float  # inner_shadow_x
    @property
    def _52usozk2(self) -> float:
        """inner_shadow_y (`52usozk2` on the wire)."""
        return self.fields["52usozk2"]

    @_52usozk2.setter
    def _52usozk2(self, value: float) -> None:
        self.edit("52usozk2", value=value)
    @property
    def _2h5mxl46(self) -> float:
        """inner_shadow_blur (`2h5mxl46` on the wire)."""
        return self.fields["2h5mxl46"]

    @_2h5mxl46.setter
    def _2h5mxl46(self, value: float) -> None:
        self.edit("2h5mxl46", value=value)
    f7cd75z6: str  # inner_shadow_colour
    zalrvie7: list[list[float]]  # curve


class AreaJson(Resource):
    """AreaJson — `tl.so.tilo.canvas.AreaJson`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    nxcluiyf: str  # d
    jxpwvxya: float  # feather
    algnlk7t: MatteJson | None  # matte


class MatteJson(Resource):
    """MatteJson — `tl.so.tilo.canvas.MatteJson`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    @property
    def _7s7g6rha(self) -> str:
        """src (`7s7g6rha` on the wire)."""
        return self.fields["7s7g6rha"]

    @_7s7g6rha.setter
    def _7s7g6rha(self, value: str) -> None:
        self.edit("7s7g6rha", value=value)
    erbjejvt: list[float]  # rect


class GradeAsk(Resource):
    """GradeAsk — `tl.so.tilo.canvas.GradeAsk`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    fy7u7ntb: float  # exposure
    h7ykiuc5: float  # contrast
    akjbo2e5: float  # highlights
    gx2o2imz: float  # shadows
    gwfjuivz: float  # saturation
    y7wzyyqq: float  # vibrance
    pkwjhrnw: float  # hue
    gvwo4klz: float  # temperature
    @property
    def _6bogm2iw(self) -> float:
        """tint (`6bogm2iw` on the wire)."""
        return self.fields["6bogm2iw"]

    @_6bogm2iw.setter
    def _6bogm2iw(self, value: float) -> None:
        self.edit("6bogm2iw", value=value)
    mz4xapep: bool  # grayscale
    @property
    def _7pnhizf2(self) -> bool:
        """invert (`7pnhizf2` on the wire)."""
        return self.fields["7pnhizf2"]

    @_7pnhizf2.setter
    def _7pnhizf2(self, value: bool) -> None:
        self.edit("7pnhizf2", value=value)
    vzbhth5d: float  # blur
    ayg5wxga: float  # sharpen
    bporvvfd: float  # vignette


class WandAsk(Resource):
    """WandAsk — `tl.so.tilo.canvas.WandAsk`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    yyqmyjfc: list[float] | None  # at
    etjvwbve: str | None  # colour
    giesycja: float | None  # tolerance
    vry7txyu: bool | None  # everywhere
    e3v56jsz: list[float] | None  # frame


class FindAsk(Resource):
    """FindAsk — `tl.so.tilo.canvas.FindAsk`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    @property
    def _3ug7cc6p(self) -> bool | None:
        """subject (`3ug7cc6p` on the wire)."""
        return self.fields["3ug7cc6p"]

    @_3ug7cc6p.setter
    def _3ug7cc6p(self, value: bool | None) -> None:
        self.edit("3ug7cc6p", value=value)
    mgf542kb: float | None  # refine
    oehqnmnq: list[float] | None  # at
    t76lt6eq: str | None  # colour
    rk2wxm5k: float | None  # tolerance
    ipz3kxni: str | None  # layer
    @property
    def _5ijkh76y(self) -> float | None:
        """feather (`5ijkh76y` on the wire)."""
        return self.fields["5ijkh76y"]

    @_5ijkh76y.setter
    def _5ijkh76y(self, value: float | None) -> None:
        self.edit("5ijkh76y", value=value)


class CombineAsk(Resource):
    """CombineAsk — `tl.so.tilo.canvas.CombineAsk`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    bp3zj5os: list[str]  # ids
    @property
    def _4kf2vu7l(self) -> str:
        """op (`4kf2vu7l` on the wire)."""
        return self.fields["4kf2vu7l"]

    @_4kf2vu7l.setter
    def _4kf2vu7l(self, value: str) -> None:
        self.edit("4kf2vu7l", value=value)


class MeasureAsk(Resource):
    """MeasureAsk — `tl.so.tilo.canvas.MeasureAsk`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    @property
    def _2tk6phpq(self) -> list[str]:
        """ids (`2tk6phpq` on the wire)."""
        return self.fields["2tk6phpq"]

    @_2tk6phpq.setter
    def _2tk6phpq(self, value: list[str]) -> None:
        self.edit("2tk6phpq", value=value)


class SelectAsk(Resource):
    """SelectAsk — `tl.so.tilo.canvas.SelectAsk`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    @property
    def _7omk45ql(self) -> list[float] | None:
        """rect (`7omk45ql` on the wire)."""
        return self.fields["7omk45ql"]

    @_7omk45ql.setter
    def _7omk45ql(self, value: list[float] | None) -> None:
        self.edit("7omk45ql", value=value)
    krhe2i56: list[float] | None  # ellipse
    @property
    def _5eu7ff7r(self) -> list[list[float]] | None:
        """points (`5eu7ff7r` on the wire)."""
        return self.fields["5eu7ff7r"]

    @_5eu7ff7r.setter
    def _5eu7ff7r(self, value: list[list[float]] | None) -> None:
        self.edit("5eu7ff7r", value=value)
    mgxsjhfz: str | None  # d
    iqzzaywp: list[str] | None  # marks
    l4imaa4l: str | None  # mode
    @property
    def _7zgzz7h2(self) -> float | None:
        """feather (`7zgzz7h2` on the wire)."""
        return self.fields["7zgzz7h2"]

    @_7zgzz7h2.setter
    def _7zgzz7h2(self, value: float | None) -> None:
        self.edit("7zgzz7h2", value=value)
    ohfhq5pm: float | None  # grow
    ojp4xz4a: bool | None  # invert


class InspectAsk(Resource):
    """InspectAsk — `tl.so.tilo.canvas.InspectAsk`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    @property
    def _3uypu22a(self) -> bool | None:
        """palette (`3uypu22a` on the wire)."""
        return self.fields["3uypu22a"]

    @_3uypu22a.setter
    def _3uypu22a(self, value: bool | None) -> None:
        self.edit("3uypu22a", value=value)
    veyz6j2q: list[float] | None  # region
    ylddsw2e: str | None  # layer
    @property
    def _6nfqt3pd(self) -> int | None:
        """limit (`6nfqt3pd` on the wire)."""
        return self.fields["6nfqt3pd"]

    @_6nfqt3pd.setter
    def _6nfqt3pd(self, value: int | None) -> None:
        self.edit("6nfqt3pd", value=value)


class RemoveAsk(Resource):
    """RemoveAsk — `tl.so.tilo.canvas.RemoveAsk`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    js4zslbp: list[list[float]]  # along
    besel4qy: str | None  # layer


class DiffAsk(Resource):
    """DiffAsk — `tl.so.tilo.canvas.DiffAsk`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    eubkyqqd: str  # from
    mit27xah: str | None  # to
    eiocv26o: bool | None  # look
    rgii66ho: bool | None  # overlay


class CompareAsk(Resource):
    """CompareAsk — `tl.so.tilo.canvas.CompareAsk`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    mztro4wb: list[float] | None  # region
    @property
    def _67462gfa(self) -> str | None:
        """mode (`67462gfa` on the wire)."""
        return self.fields["67462gfa"]

    @_67462gfa.setter
    def _67462gfa(self, value: str | None) -> None:
        self.edit("67462gfa", value=value)
    hbu4rn3a: int | None  # size


class Canvas(Resource):
    """Canvas — `tl.so.tilo.canvas`."""
    __surface__ = "so.tilo.canvas@59ab3d6f91f4513c"
    def grade_picture(self,
        *,
        file: Any,
        exposure: float | None = None,
        contrast: float | None = None,
        highlights: float | None = None,
        shadows: float | None = None,
        saturation: float | None = None,
        vibrance: float | None = None,
        hue: float | None = None,
        temperature: float | None = None,
        tint: float | None = None,
        grayscale: bool | None = None,
        invert: bool | None = None,
        blur: float | None = None,
        sharpen: float | None = None,
        vignette: float | None = None,
    ) -> Any | None:
        """A picture with those steps applied to its pixels, as a new file: what a person or an agent asks for when the graded picture itself is wanted, rather than the artwork that shows it."""
        args: dict[str, Any] = {}
        args["file"] = file
        if exposure is not None:
            args["exposure"] = exposure
        if contrast is not None:
            args["contrast"] = contrast
        if highlights is not None:
            args["highlights"] = highlights
        if shadows is not None:
            args["shadows"] = shadows
        if saturation is not None:
            args["saturation"] = saturation
        if vibrance is not None:
            args["vibrance"] = vibrance
        if hue is not None:
            args["hue"] = hue
        if temperature is not None:
            args["temperature"] = temperature
        if tint is not None:
            args["tint"] = tint
        if grayscale is not None:
            args["grayscale"] = grayscale
        if invert is not None:
            args["invert"] = invert
        if blur is not None:
            args["blur"] = blur
        if sharpen is not None:
            args["sharpen"] = sharpen
        if vignette is not None:
            args["vignette"] = vignette
        return self.action("grade_picture", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"grade_picture"})


