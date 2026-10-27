# Canvas

Vector drawing, painting in real media and photo editing, as fluent for an
agent as for a person.

An artwork is layers of marks: shapes, paths, text, brush strokes and
pictures, each an object with a stable id. A plain layer draws its marks as vector art.
A layer with a medium (`oil`, `watercolour` and their presets, or one saved in
the artwork) paints them through `tilo-wet-media`, re-simulated from the marks
on every machine, so the painting syncs, merges and undoes as data.

## The vocabulary

Everything the pointer does is one of the artwork's actions, and so is
everything an agent, a script or a screen reader does:

| Action | What it does |
| --- | --- |
| `paint` | a passage of brush strokes in one step, with a brush preset or the eraser |
| `draw` | shapes (rect, ellipse, polygon, line, SVG path, text, stroke, image) |
| `edit-marks` | recolour, move, scale, rotate, flip or re-layer marks by id; re-brush, smooth, simplify, extend, split and merge strokes; crop, straighten and adjust pictures |
| `remove-marks`, `arrange` | remove (by id, or the strokes a path crosses), restack |
| `add-layer`, `edit-layer`, `remove-layer` | layers and their media |
| `select`, `use-selection` | an area: shapes, lasso, marks; fill, outline, mask, clear |
| `find-area` | an area a picture of the artwork shows: magic wand, colour, a photo's subject, a refined edge |
| `combine` | shapes united, subtracted, intersected or excluded into one path |
| `set-artwork` | title, size, ground texture and colour; crop to a region or a mark |
| `open` | a PNG, JPEG or WebP as a new artwork the size of the picture |
| `save-medium`, `remove-medium` | media presets saved in the artwork |
| `look` | a PNG of the artwork or a region, with an optional coordinate grid |
| `inspect`, `media`, `history`, `export` | what is on it, what it can use, how it got here; PNG, JPEG or WebP at any scale |
| `measure`, `compare`, `compare-versions`, `export-svg` | where marks' ink weighs, how close it is to the reference, what changed, SVG |

What reads a picture of the artwork (`look`, `export`, `measure`, `compare`,
`compare-versions`, `export-svg`, `find-area`) answers as a job: at once where
the picture can be rendered at once, and a few frames later in a browser
page, where the frame loop renders it.

On a plain layer a stroke is drawn by its brush preset, worked out from the
stroke mark alone: `pen` (crisp ink, pressure swells the line), `pencil`
(grainy, pressure darkens it), `marker` (flat, see-through, overlaps
darken), `airbrush` (soft spray that builds up), `charcoal` (ragged, catching
the ground's tooth) and `calligraphy` (a broad nib at the stroke's `angle`).
A stroke's `params` tune its flow, hardness, spacing, smoothing and taper.
The `eraser` is a mark too: it clears the paint beneath it on its layer,
plain or painted, and paint laid after it shows; `remove-marks` with `along`
takes whole strokes instead. A pen tablet's pressure goes into the stroke's
points; with a mouse the hand's speed stands in for it.

Lettering uses the fonts Tilo ships (`media` lists them), so it looks the
same on every machine: any weight between the face's lightest and boldest,
tracking, line height, alignment, and words set along a path. A mark can
carry film `grain`, an `inner_shadow` (a light one is a highlight) and a
`feather`ed edge; a layer's `texture` lets the ground's tooth show through.
`measure` reports where a mark's ink weighs (its ink box,
area and centre of mass) and each letter's box, for balancing icons and
type by eye. Groups nest (`icons/mail`), and SVG export and import keep
them, their shadows and text set along a path.

## Pictures

A picture is a mark like any other: placed from a file, a content address or
a `data:` URI and kept by its content, so it syncs, merges and undoes as
data. Its crop (fractions of the picture; the picture stays put and the
frame follows), its straightening (a turn inside the frame, enlarged so no
corner shows), a flip, opacity, blend and a clip by any mark or group are
all edits of the mark, never of the pixels.

Adjustments are non-destructive too: exposure, contrast, highlights and
shadows, saturation and vibrance, hue, temperature and tint, a tone
curve, black and white, invert, blur, sharpen and vignette. A mark's
`adjust` grades its picture (or a shape's colours); a layer's grades
everything on it, after each mark's own, the same way a layer's medium or
texture changes how its marks look. One pipeline (`adjust.rs`) grades for
the canvas, `look`, `export` and the thumbnails alike; the canvas grades a
copy at the resolution it shows, so a slider drags live on a 12-megapixel
photo, and an export grades the full picture.

A person crops on the canvas (Enter, C or double-click on a picture; the
turn handle straightens), drags the inspector's sliders with the result
shown while dragging, and adds a picture from the header: on an empty
sheet it becomes the artwork, sized to the picture.
The selection is an area kept with the artwork, like the reference: it
syncs and undoes, and is never drawn into it. Rectangles, ellipses, lassos,
path data and marks' shapes select it; `find-area`'s magic wand and colour
find it in the artwork as the window shows it, every layer kind alike, and
select the plain geometry they found, so every replica applies the same
area. Shift adds, Alt subtracts, both intersect; grow, feather and
invert refine it. What is made of it is ordinary data: a fill or outline is a
path mark, paint kept inside it carries that area with it (`clip:
"selection"`, and the brush does it while there is a selection; `inspect`
shows the box it is kept `inside`, and `edit-marks` with `clip: ""` lets it
out), and
`mask`/`clear` draw white or black into the layer's mask. A mask is a layer of
grey marks (`<layer>-mask`) the layer is shown through, so brushes, shapes and
selections all edit it; it inverts, switches off and goes with its layer. A
layer can clip to the one below. `combine` keeps curves exact: where the
result's edges lie on the original outlines they are those curves again.

Refusals name the mistake and the valid choices (the layers that exist, the
pigments of the layer's palette), so an agent can correct itself from the
message.

## Testing what an agent experiences

```bash
scripts/agent-loop/session.py start apps/canvas   # headless, isolated, rebuilt on save
scripts/agent-loop/session.py mcp canvas          # the MCP endpoint (see .cursor/mcp.json)
scripts/agent-loop/session.py report canvas       # history, inspection and a final look
scripts/agent-loop/session.py stop canvas
```

Besides the actions, the session's agent can use the window like a person:
`computer` with `app_id: "so.tilo.canvas"` clicks the tools, drags strokes and
handles, types and Ctrl+scrolls in the window's pixels (a point of `render`
plus its `window_origin`) and answers with a screenshot of the window.

`tests/agent-surface.json` is what an agent sees of the app; a change to the
vocabulary shows there as a diff (`tests/agent_surface.rs` explains updating
it). Screenshots: `TILO_SHOTS_THEME=light ./target/debug/canvas --shots
out.png --scenario landscape`.
