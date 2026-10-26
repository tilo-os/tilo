# Canvas: an agent-fluent vector and wet-media editor

The app lives in `apps/canvas`.

## Intent

Tilo needs an image/vector editor that an AI uses as fluently as a person:
an agent should be able to paint something good, look at it, and refine it,
through the same action model every other caller uses. The same work builds
a reusable loop for testing the *agent experience* of any Tilo app while it
is being written.

Lessons taken from prior art (stillwet.art, "Training AI to Paint with
Code", Surya, 2026):

- A painting as a program of marks is editable, attributable and mergeable;
  pixels are not. Every mark here is a vector object with a stable id.
- A short, opinionated vocabulary beats a long API. The action surface is
  that vocabulary; its schemas are tight and its descriptions name units.
- Painters improve by looking: paint a passage, step back, look. Looking is
  an action that returns an image tied to a revision, so a look is never
  stale or ambiguous.
- Good looks come from the medium, not the model: strokes are simple data,
  a wet-media simulation renders them as oil or watercolour.
- Batching matters: one action carries a whole passage of strokes and is
  one undo step and one attributed history entry.
- The painter gets the easel's tools only: the app's actions through the
  ordinary runtime door.

## Decisions

- **Vector first, media first-class.** Layers hold marks (shapes, paths,
  text, brush strokes). A layer's *medium* decides how its marks are
  realised: a plain layer draws crisp vector (strokes become variable-width
  ink); an oil or watercolour layer runs a simulation (shapes become washes
  or flat paint, strokes become bristle or wet strokes).
- **Strokes are the state; paint is derived.** The document stores marks;
  each machine re-simulates locally into a per-layer raster cache, with
  checkpoints so appending a stroke costs one stroke. Simulation is CPU,
  deterministic (seeded by mark id), parallel by rows, so every collaborator
  sees the same painting.
- **Media are engine + preset.** Built-in engines: `plain`, `watercolour`,
  `oil`. A preset is shareable data (engine id, parameters, ground, named
  pigment palette) stored in the document like any resource and synced. The
  `MediumEngine` trait is the plug-in boundary; third-party engines
  (sandboxed WGSL/WASM modules) are designed for but not built yet.
- **One interface.** Agents use `tilo-runtime-mcp` and the app's actions,
  nothing else. No private test ops, no second CLI.

## Document (`canvas.artwork`, hand-written `Resource`)

Keyed maps so each action writes only what it touches and reads refresh
incrementally:

- `title` (text), `width`/`height` (artboard units, origin top-left, y down),
  `ground` (colour + texture: `paper`, `cold-press`, `linen`, `none`).
- `layers.<id>`: name, fractional `order`, `medium` (preset id or empty for
  plain), opacity, visible, locked.
- `marks.<id>`: layer id, fractional `order`, name, shape, style (fill,
  stroke, opacity, blend), transform. A shape is a data enum: `$variant`
  names `rect`, `ellipse`, `polygon`, `line`, `path`, `text`, `stroke` or
  `image`, and that variant's fields live beside it (`shape.rect.width`,
  `shape.text.x`, …), so position and size controls merge rather than
  replacing one another. A variant change is one write of the name; fields
  the new variant does not carry stay as unused history.
- `media.<id>`: user presets (JSON).

## Vocabulary

Named document actions (palette, agents, scripts; all undoable):
`add-layer`, `edit-layer`, `remove-layer`, `draw` (batch of shapes),
`paint` (batch of brush strokes), `edit-marks` (style/transform/rename on a
set of ids), `remove-marks`, `reorder-marks`, `set-artwork` (title, size,
ground), `define-medium` (save a preset). The generic `apply` carries the
same typed values with a published JSON Schema.

Effect/view operations (`canvas.view` resource): `look` (PNG of the whole
artwork or a region at a bounded size, with `{revision, width, height,
scale}`), `inspect` (JSON of layers and marks in a region with ids, bounds
and colours), `media` (built-in and user presets with their pigments),
`export` (full-resolution PNG content).

Colours: `#rrggbb[aa]`, or `pigment:<name>` from the layer preset's
palette. Strokes are smoothed (centripetal Catmull-Rom) so a handful of
points describe a curve; pressure is optional per point.

## Agent loop (reusable by every app)

1. `scripts/agent-loop/session.py <app>` starts an isolated session: a
   headless Tilo compositor as the Wayland display (nothing on the user's
   desktop), the runtime daemon on a fixed profile under
   `target/agent-loop/`, and `tilo-app dev` for the app. Rebuilds restart
   the app; the daemon and MCP session survive.
2. `.cursor/mcp.json` connects the coding agent to that profile through
   `tilo-runtime-mcp --connect`. External agents use the same endpoint.
3. `tilo-runtime-mcp` returns an MCP `image` block when a terminal
   invocation names image content, read through the profile's media host
   with the app's own scoped capability; `tilo_invoke`/`tilo_status` take a
   bounded `wait_ms` so a painter is not forced to poll.
4. The record is the document's own history (`agent-run` groups); the
   session script writes looks and a summary to `target/agent-runs/`.
5. Tier 1: a golden snapshot of the app's agent-visible surface (resource
   tree, actions, schemas, byte size) runs in plain `cargo test`.

## UI

Tool rail (select, rectangle, ellipse, path pen, brush), canvas viewport
(shared `ViewportState` zoom/pan, artboard with ground, selection with
shared transform handles, collaborator presence), inspector (layers, medium
preset picker, colour picker with pigment swatches, brush size). All
colours from `Theme`, all strings Fluent keys, every control bound to an
action.

## Testing

`Replica`-level tests for every action, deterministic engine tests (same
input, same pixels; stroke append equals full replay), `HeadlessApp` tests
for undo/permissions, the surface snapshot, shots in light, dark and high
contrast, and a live painting session through MCP.
