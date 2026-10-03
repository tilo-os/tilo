# Canvas

Vector drawing and painting in real media, as fluent for an agent as for a
person.

An artwork is layers of marks: shapes, paths, text and brush strokes, each a
vector object with a stable id. A plain layer draws its marks as vector art.
A layer with a medium (`oil`, `watercolour` and their presets, or one saved in
the artwork) paints them through `tilo-wet-media`, re-simulated from the marks
on every machine, so the painting syncs, merges and undoes as data.

## The vocabulary

Everything the pointer does is one of the artwork's actions, and so is
everything an agent, a script or a screen reader does:

| Action | What it does |
| --- | --- |
| `paint` | a passage of brush strokes in one step |
| `draw` | shapes (rect, ellipse, polygon, line, SVG path, text, stroke) |
| `edit-marks` | recolour, move, scale, rotate or re-layer marks by id |
| `remove-marks`, `arrange` | remove, restack |
| `add-layer`, `edit-layer`, `remove-layer` | layers and their media |
| `set-artwork` | title, size, ground texture and colour |
| `save-medium`, `remove-medium` | media presets saved in the artwork |
| `look` | a PNG of the artwork or a region, with an optional coordinate grid |
| `inspect`, `media`, `history`, `export` | what is on it, what it can use, how it got here |

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
