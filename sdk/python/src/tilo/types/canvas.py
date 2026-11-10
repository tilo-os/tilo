"""Types for Canvas (`so.tilo.canvas`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Artwork` is the app's `canvas.artwork` at `canvas/<resource>`, read with
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
    "Adjust",
    "Ground",
    "Layer",
    "Mark",
    "Shape",
    "Style",
    "Recent",
    "Artwork",
    "RecentArtworks",
]

class Adjust(TypedDict):
    exposure: float | None
    contrast: float | None
    highlights: float | None
    shadows: float | None
    saturation: float | None
    vibrance: float | None
    hue: float | None
    temperature: float | None
    tint: float | None
    curve: list[list[float]] | None
    grayscale: bool | None
    invert: bool | None
    blur: float | None
    sharpen: float | None
    vignette: float | None

class Ground(TypedDict):
    texture: str
    colour: str

class Layer(TypedDict):
    id: str
    name: str
    group: str
    order: str
    medium: str
    opacity: float
    visible: bool
    texture: float
    locked: bool
    adjust: Adjust | None
    mask_of: str
    base: float
    invert: bool
    clip: bool
    unlinked: bool

class Mark(TypedDict):
    id: str
    layer: str
    order: str
    name: str
    shape: Shape
    style: Style
    transform: list[float] | None
    group: str
    clip: str
    area: str

Shape = dict[str, Any]
"""One of rect, ellipse, polygon, line, path, text, stroke, image: an object whose case the app tags it with."""

class Style(TypedDict):
    pixelated: bool
    fill: str | None
    gradient: Any | None
    stroke: str | None
    stroke_width: float
    opacity: float | None
    brush: str | None
    brush_size: float | None
    angle: float | None
    wet: bool
    params: dict[str, float]
    dash: list[float]
    cap: str | None
    join: str | None
    blend: str | None
    shadow: Any | None
    inner_shadow: Any | None
    grain: float | None
    feather: float | None
    adjust: Adjust | None

class Recent(TypedDict):
    path: str
    title: str
    width: float
    height: float
    at_ms: int
    thumbnail: str | None

class Artwork(Resource):
    """Artwork — `canvas.artwork`."""
    title: str
    width: float
    height: float
    ground: Ground
    layers: list[Layer]
    marks: list[Mark]
    media: dict[str, Any]
    reference: Any | None
    selection: Any | None

    def paint(self,
        *,
        strokes: list[Any],
        layer: str | None = None,
        group: str | None = None,
    ) -> Any | Run:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if layer is not None:
            args["layer"] = layer
        args["strokes"] = strokes
        if group is not None:
            args["group"] = group
        return self.action("paint", **args)

    def draw(self,
        *,
        marks: list[Any],
        layer: str | None = None,
    ) -> Any | Run:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if layer is not None:
            args["layer"] = layer
        args["marks"] = marks
        return self.action("draw", **args)

    def chart(self,
        *,
        labels: list[str],
        values: list[float],
        kind: str | None = None,
        box: list[float] | None = None,
        max: float | None = None,
        bar_width: float | None = None,
        colour: str | None = None,
        label_size: float | None = None,
        values_shown: bool | None = None,
        title: str | None = None,
        id: str | None = None,
        layer: str | None = None,
    ) -> Any | Run:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["labels"] = labels
        args["values"] = values
        if kind is not None:
            args["kind"] = kind
        if box is not None:
            args["box"] = box
        if max is not None:
            args["max"] = max
        if bar_width is not None:
            args["bar_width"] = bar_width
        if colour is not None:
            args["colour"] = colour
        if label_size is not None:
            args["label_size"] = label_size
        if values_shown is not None:
            args["values_shown"] = values_shown
        if title is not None:
            args["title"] = title
        if id is not None:
            args["id"] = id
        if layer is not None:
            args["layer"] = layer
        return self.action("chart", **args)

    def edit_marks(self,
        *,
        ids: list[str],
        pixelated: bool | None = None,
        name: str | None = None,
        shape: Any | None = None,
        dash: list[float] | None = None,
        cap: str | None = None,
        join: str | None = None,
        blend: str | None = None,
        shadow: Any | None = None,
        inner_shadow: Any | None = None,
        grain: float | None = None,
        feather: float | None = None,
        group: str | None = None,
        clip: str | None = None,
        fill: str | None = None,
        gradient: Any | None = None,
        stroke: str | None = None,
        stroke_width: float | None = None,
        opacity: float | None = None,
        brush: str | None = None,
        size: float | None = None,
        brush_size: float | None = None,
        angle: float | None = None,
        wet: bool | None = None,
        params: Any | None = None,
        move_by: list[float] | None = None,
        scale: float | None = None,
        rotate: float | None = None,
        layer: str | None = None,
        transform: list[float] | None = None,
        flip: str | None = None,
        crop: list[float] | None = None,
        straighten: float | None = None,
        adjust: Any | None = None,
        smooth: float | None = None,
        simplify: float | None = None,
        extend: list[list[float]] | None = None,
        split: list[float] | None = None,
        merge: bool | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if pixelated is not None:
            args["pixelated"] = pixelated
        args["ids"] = ids
        if name is not None:
            args["name"] = name
        if shape is not None:
            args["shape"] = shape
        if dash is not None:
            args["dash"] = dash
        if cap is not None:
            args["cap"] = cap
        if join is not None:
            args["join"] = join
        if blend is not None:
            args["blend"] = blend
        if shadow is not None:
            args["shadow"] = shadow
        if inner_shadow is not None:
            args["inner_shadow"] = inner_shadow
        if grain is not None:
            args["grain"] = grain
        if feather is not None:
            args["feather"] = feather
        if group is not None:
            args["group"] = group
        if clip is not None:
            args["clip"] = clip
        if fill is not None:
            args["fill"] = fill
        if gradient is not None:
            args["gradient"] = gradient
        if stroke is not None:
            args["stroke"] = stroke
        if stroke_width is not None:
            args["stroke_width"] = stroke_width
        if opacity is not None:
            args["opacity"] = opacity
        if brush is not None:
            args["brush"] = brush
        if size is not None:
            args["size"] = size
        if brush_size is not None:
            args["brush_size"] = brush_size
        if angle is not None:
            args["angle"] = angle
        if wet is not None:
            args["wet"] = wet
        if params is not None:
            args["params"] = params
        if move_by is not None:
            args["move_by"] = move_by
        if scale is not None:
            args["scale"] = scale
        if rotate is not None:
            args["rotate"] = rotate
        if layer is not None:
            args["layer"] = layer
        if transform is not None:
            args["transform"] = transform
        if flip is not None:
            args["flip"] = flip
        if crop is not None:
            args["crop"] = crop
        if straighten is not None:
            args["straighten"] = straighten
        if adjust is not None:
            args["adjust"] = adjust
        if smooth is not None:
            args["smooth"] = smooth
        if simplify is not None:
            args["simplify"] = simplify
        if extend is not None:
            args["extend"] = extend
        if split is not None:
            args["split"] = split
        if merge is not None:
            args["merge"] = merge
        return self.action("edit-marks", **args)

    def remove_marks(self,
        *,
        ids: list[str] | None = None,
        layer: str | None = None,
        along: list[list[float]] | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if ids is not None:
            args["ids"] = ids
        if layer is not None:
            args["layer"] = layer
        if along is not None:
            args["along"] = along
        return self.action("remove-marks", **args)

    def arrange(self,
        *,
        ids: list[str],
        to: str,
        of: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["ids"] = ids
        args["to"] = to
        if of is not None:
            args["of"] = of
        return self.action("arrange", **args)

    def add_layer(self,
        *,
        name: str,
        id: str | None = None,
        medium: str | None = None,
        opacity: float | None = None,
        above: str | None = None,
        texture: float | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if id is not None:
            args["id"] = id
        args["name"] = name
        if medium is not None:
            args["medium"] = medium
        if opacity is not None:
            args["opacity"] = opacity
        if above is not None:
            args["above"] = above
        if texture is not None:
            args["texture"] = texture
        return self.action("add-layer", **args)

    def edit_layer(self,
        *,
        id: str,
        name: str | None = None,
        group: str | None = None,
        medium: str | None = None,
        opacity: float | None = None,
        visible: bool | None = None,
        locked: bool | None = None,
        to: str | None = None,
        texture: float | None = None,
        adjust: Any | None = None,
        mask: str | None = None,
        invert: bool | None = None,
        clip: bool | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["id"] = id
        if name is not None:
            args["name"] = name
        if group is not None:
            args["group"] = group
        if medium is not None:
            args["medium"] = medium
        if opacity is not None:
            args["opacity"] = opacity
        if visible is not None:
            args["visible"] = visible
        if locked is not None:
            args["locked"] = locked
        if to is not None:
            args["to"] = to
        if texture is not None:
            args["texture"] = texture
        if adjust is not None:
            args["adjust"] = adjust
        if mask is not None:
            args["mask"] = mask
        if invert is not None:
            args["invert"] = invert
        if clip is not None:
            args["clip"] = clip
        return self.action("edit-layer", **args)

    def remove_layer(self, *, id: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-layer", id=id)

    def set_artwork(self,
        *,
        title: str | None = None,
        width: float | None = None,
        height: float | None = None,
        texture: str | None = None,
        ground_colour: str | None = None,
        crop: list[float] | None = None,
        fit: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if title is not None:
            args["title"] = title
        if width is not None:
            args["width"] = width
        if height is not None:
            args["height"] = height
        if texture is not None:
            args["texture"] = texture
        if ground_colour is not None:
            args["ground_colour"] = ground_colour
        if crop is not None:
            args["crop"] = crop
        if fit is not None:
            args["fit"] = fit
        return self.action("set-artwork", **args)

    def save_medium(self, *, medium: Any) -> Run | None:
        """Runs the app's action."""
        return self.action("save-medium", medium=medium)

    def remove_medium(self, *, id: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-medium", id=id)

    def align(self,
        *,
        ids: list[str],
        edge: str | None = None,
        distribute: str | None = None,
        to: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["ids"] = ids
        if edge is not None:
            args["edge"] = edge
        if distribute is not None:
            args["distribute"] = distribute
        if to is not None:
            args["to"] = to
        return self.action("align", **args)

    def set_reference(self,
        *,
        image: str | None = None,
        frame: list[float] | None = None,
        opacity: float | None = None,
        visible: bool | None = None,
        remove: bool | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if image is not None:
            args["image"] = image
        if frame is not None:
            args["frame"] = frame
        if opacity is not None:
            args["opacity"] = opacity
        if visible is not None:
            args["visible"] = visible
        if remove is not None:
            args["remove"] = remove
        return self.action("set-reference", **args)

    def import_svg(self,
        *,
        svg: str,
        layer: str | None = None,
        group: str | None = None,
        fit: list[float] | None = None,
    ) -> Any | Run:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["svg"] = svg
        if layer is not None:
            args["layer"] = layer
        if group is not None:
            args["group"] = group
        if fit is not None:
            args["fit"] = fit
        return self.action("import-svg", **args)

    def open_picture(self,
        *,
        path: str,
        title: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["path"] = path
        if title is not None:
            args["title"] = title
        return self.action("open-picture", **args)

    def new_artwork(self,
        *,
        title: str | None = None,
        size: str | None = None,
        landscape: bool | None = None,
        width: float | None = None,
        height: float | None = None,
        start: str | None = None,
        texture: str | None = None,
        ground_colour: str | None = None,
        medium: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if title is not None:
            args["title"] = title
        if size is not None:
            args["size"] = size
        if landscape is not None:
            args["landscape"] = landscape
        if width is not None:
            args["width"] = width
        if height is not None:
            args["height"] = height
        if start is not None:
            args["start"] = start
        if texture is not None:
            args["texture"] = texture
        if ground_colour is not None:
            args["ground_colour"] = ground_colour
        if medium is not None:
            args["medium"] = medium
        return self.action("new-artwork", **args)

    def open_artwork(self, *, file: str) -> Run | None:
        """Runs the app's action."""
        return self.action("open-artwork", file=file)

    def select(self,
        *,
        rect: list[float] | None = None,
        ellipse: list[float] | None = None,
        points: list[list[float]] | None = None,
        d: str | None = None,
        marks: list[str] | None = None,
        matte: Any | None = None,
        mode: str | None = None,
        feather: float | None = None,
        grow: float | None = None,
        invert: bool | None = None,
    ) -> Any | Run:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if rect is not None:
            args["rect"] = rect
        if ellipse is not None:
            args["ellipse"] = ellipse
        if points is not None:
            args["points"] = points
        if d is not None:
            args["d"] = d
        if marks is not None:
            args["marks"] = marks
        if matte is not None:
            args["matte"] = matte
        if mode is not None:
            args["mode"] = mode
        if feather is not None:
            args["feather"] = feather
        if grow is not None:
            args["grow"] = grow
        if invert is not None:
            args["invert"] = invert
        return self.action("select", **args)

    def use_selection(self,
        *,
        do: str,
        layer: str | None = None,
        colour: str | None = None,
        width: float | None = None,
        id: str | None = None,
        defringe: bool | None = None,
        cleaned: list[list[str]] | None = None,
    ) -> Any | Run:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["do"] = do
        if layer is not None:
            args["layer"] = layer
        if colour is not None:
            args["colour"] = colour
        if width is not None:
            args["width"] = width
        if id is not None:
            args["id"] = id
        if defringe is not None:
            args["defringe"] = defringe
        if cleaned is not None:
            args["cleaned"] = cleaned
        return self.action("use-selection", **args)

    def combine(self,
        *,
        ids: list[str],
        op: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("combine", ids=ids, op=op)

    def look(self,
        *,
        region: list[float] | None = None,
        size: int | None = None,
        format: str | None = None,
        grid: bool | None = None,
        layers: list[str] | None = None,
        version: str | None = None,
        reference: bool | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if region is not None:
            args["region"] = region
        if size is not None:
            args["size"] = size
        if format is not None:
            args["format"] = format
        if grid is not None:
            args["grid"] = grid
        if layers is not None:
            args["layers"] = layers
        if version is not None:
            args["version"] = version
        if reference is not None:
            args["reference"] = reference
        return self.action("look", **args)

    def measure(self,
        *,
        ids: list[str],
        version: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["ids"] = ids
        if version is not None:
            args["version"] = version
        return self.action("measure", **args)

    def export(self,
        *,
        transparent: bool | None = None,
        scale: float | None = None,
        format: str | None = None,
        quality: int | None = None,
        path: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if transparent is not None:
            args["transparent"] = transparent
        if scale is not None:
            args["scale"] = scale
        if format is not None:
            args["format"] = format
        if quality is not None:
            args["quality"] = quality
        if path is not None:
            args["path"] = path
        return self.action("export", **args)

    def compare_versions(self,
        *,
        from_: str,
        to: str | None = None,
        look: bool | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["from"] = from_
        if to is not None:
            args["to"] = to
        if look is not None:
            args["look"] = look
        return self.action("compare-versions", **args)

    def export_svg(self,
        *,
        ids: list[str] | None = None,
        outline_text: bool | None = None,
        path: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if ids is not None:
            args["ids"] = ids
        if outline_text is not None:
            args["outline_text"] = outline_text
        if path is not None:
            args["path"] = path
        return self.action("export-svg", **args)

    def compare(self,
        *,
        region: list[float] | None = None,
        mode: str | None = None,
        size: int | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if region is not None:
            args["region"] = region
        if mode is not None:
            args["mode"] = mode
        if size is not None:
            args["size"] = size
        return self.action("compare", **args)

    def save_artwork(self,
        *,
        path: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if path is not None:
            args["path"] = path
        return self.action("save-artwork", **args)


class RecentArtworks(Resource):
    """Recent artworks — `canvas.recent-artworks`."""
    devices: dict[str, Any]

    def remember(self,
        *,
        device: str,
        recent: Recent,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("remember", device=device, recent=recent)

    def forget(self,
        *,
        device: str,
        path: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("forget", device=device, path=path)


