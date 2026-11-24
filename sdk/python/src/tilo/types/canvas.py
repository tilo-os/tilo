"""Types for Canvas (`so.tilo.canvas`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Artwork` is the app's `tl.so.tilo.canvas.Artwork` at `canvas/<resource>`, read with
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
    "Artwork",
    "Layer",
    "Mark",
    "Area",
    "Picture",
    "Library",
    "LookRequest",
    "ExportRequest",
    "SvgRequest",
    "PlaceRequest",
    "ArtworkJson",
    "LayerJson",
    "MarkJson",
    "AreaJson",
    "Canvas",
    "RenameLayerArgs",
    "LockLayerArgs",
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
    "SelectAreaArgs",
    "ClearSelectionArgs",
    "DrawLookArgs",
    "ClearNoticeArgs",
    "StartPaintArgs",
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

class Artwork(Resource):
    """Artwork — `tl.so.tilo.canvas.Artwork`."""
    __surface__ = "so.tilo.canvas@34cab672aa7431c3"
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
    def _4wmc4l7b(self) -> list[Entry[Picture]]:
        """pictures (`4wmc4l7b` on the wire)."""
        return self.fields["4wmc4l7b"]

    @_4wmc4l7b.setter
    def _4wmc4l7b(self, value: list[Entry[Picture]]) -> None:
        self.edit("4wmc4l7b", value=value)
    ob6rlf7s: Area | None  # selection
    k2slfiuy: str  # look
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
        return self.action("finish_paint", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"add_layer", "rename_layer", "rename_layer_many", "lock_layer", "lock_layer_many", "move_layer_up", "move_layer_up_many", "move_layer_down", "move_layer_down_many", "duplicate_layer", "merge_layer_down", "merge_layer_down_many", "show_layer", "show_layer_many", "remove_layer", "remove_layer_many", "set_artwork", "set_artwork_many", "add_stroke", "extend_stroke", "extend_stroke_many", "top_layer", "add_shape", "remove_mark", "remove_mark_many", "recolour", "recolour_many", "resize_mark", "resize_mark_many", "place_picture", "select_area", "select_area_many", "clear_selection", "clear_selection_many", "fill_selection", "draw_look", "draw_look_many", "export_picture", "export_svg", "save_artwork", "import_svg", "clear_notice", "clear_notice_many", "start_paint", "start_paint_many", "continue_paint", "finish_paint"})


class Layer(Resource):
    """Layer — `tl.so.tilo.canvas.Layer`."""
    __surface__ = "so.tilo.canvas@34cab672aa7431c3"
    bplq2q5j: str  # name
    ilxjjxxz: str  # medium
    smoxtxmk: float  # opacity
    evne54yw: bool  # visible
    nagaw3zw: bool  # locked
    yzvcnjuy: float  # texture


class Mark(Resource):
    """Mark — `tl.so.tilo.canvas.Mark`."""
    __surface__ = "so.tilo.canvas@34cab672aa7431c3"
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


class Area(Resource):
    """Area — `tl.so.tilo.canvas.Area`."""
    __surface__ = "so.tilo.canvas@34cab672aa7431c3"
    k26xq4t7: str  # d
    enongrrr: float  # feather


class Picture(Resource):
    """Picture — `tl.so.tilo.canvas.Picture`."""
    __surface__ = "so.tilo.canvas@34cab672aa7431c3"
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


class Library(Resource):
    """Library — `tl.so.tilo.canvas.Library`."""
    __surface__ = "so.tilo.canvas@34cab672aa7431c3"
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
    __surface__ = "so.tilo.canvas@34cab672aa7431c3"
    hhu6bz5c: int  # size
    ubid4c7s: str  # format


class ExportRequest(Resource):
    """ExportRequest — `tl.so.tilo.canvas.ExportRequest`."""
    __surface__ = "so.tilo.canvas@34cab672aa7431c3"
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
    __surface__ = "so.tilo.canvas@34cab672aa7431c3"
    c35hmrf6: list[str] | None  # ids
    gqfdwswn: bool | None  # outline_text


class PlaceRequest(Resource):
    """PlaceRequest — `tl.so.tilo.canvas.PlaceRequest`."""
    __surface__ = "so.tilo.canvas@34cab672aa7431c3"
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
    __surface__ = "so.tilo.canvas@34cab672aa7431c3"
    j3bzgr3f: str  # title
    dowdfhi2: float  # width
    vlrgzghe: float  # height
    lotrow4q: str  # ground_texture
    mpyvrw7z: str  # ground_colour
    xubd3xon: list[LayerJson]  # layers
    lowvdtkg: list[MarkJson]  # marks
    @property
    def _5h6vjvk2(self) -> AreaJson | None:
        """selection (`5h6vjvk2` on the wire)."""
        return self.fields["5h6vjvk2"]

    @_5h6vjvk2.setter
    def _5h6vjvk2(self, value: AreaJson | None) -> None:
        self.edit("5h6vjvk2", value=value)


class LayerJson(Resource):
    """LayerJson — `tl.so.tilo.canvas.LayerJson`."""
    __surface__ = "so.tilo.canvas@34cab672aa7431c3"
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


class MarkJson(Resource):
    """MarkJson — `tl.so.tilo.canvas.MarkJson`."""
    __surface__ = "so.tilo.canvas@34cab672aa7431c3"
    @property
    def _67wvmkkq(self) -> str:
        """name (`67wvmkkq` on the wire)."""
        return self.fields["67wvmkkq"]

    @_67wvmkkq.setter
    def _67wvmkkq(self, value: str) -> None:
        self.edit("67wvmkkq", value=value)
    jk7lnwfw: LayerJson  # layer
    xy2goji2: str  # kind
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


class AreaJson(Resource):
    """AreaJson — `tl.so.tilo.canvas.AreaJson`."""
    __surface__ = "so.tilo.canvas@34cab672aa7431c3"
    nxcluiyf: str  # d
    jxpwvxya: float  # feather


class Canvas(Resource):
    """Canvas — `tl.so.tilo.canvas`."""
    __surface__ = "so.tilo.canvas@34cab672aa7431c3"

