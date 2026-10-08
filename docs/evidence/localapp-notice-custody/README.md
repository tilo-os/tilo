# Saved local application notice custody

A saved native application window is represented by a LocalApp notice until its
new surface arrives. The shell group previously bound that notice to a nonexistent
scratch document, masking its actual launch state with Document unavailable.

TileHost now excludes symbolic notices and external surfaces from host scratch
custody. Explicit document-creation refusals remain refused across ordinary
rebinds; replacing a document with a notice retires its history owner without
deleting the saved record. Native package timeouts and asynchronous launch
failures settle only matching, still-opening LocalApp notices. Their saved tab
and local app-record association remain, and answered cards do not relaunch
every frame. A windowless clean-parent handoff reports its expiry once;
retirement of an already claimed placement produces no false launch failure.
Mapped Foreign windows and unrelated refusals remain intact.

Status and document-error cards reuse the toolkit EmptyState painter. The first
200% text captures exposed clipped recovery sentences in the old custom painter;
the final shared stack wraps the complete explanation within the tile.

## Qualification

The regression uses the actual shell FrameBuilder, group binding, application
pump, layout, paint and accessibility paths with a synthetic saved Browser tab.
These captures show a shell-rendered notice, **not a live Browser app**. The
three original-binding runs use the inspected base cec43c355 and the added capture
fixture; each fails the intended status assertion and captures Document
unavailable. Final dark, light at 200% text, and high-contrast at 200% text runs
capture opening, failed and timeout states. No screenshot golden is updated.

Production regressions cover notice rebinding/creation, outgoing document
history, genuine document unavailability, asynchronous launch-event draining,
native and host-app timeouts, repeated drains and preservation of saved records.
The shared EmptyState suite covers clipping/fit and pointer, keyboard and
semantic activation. Shell test compilation and strict all-target Clippy are
required before publication.

Original before/final PNGs and their SHA-256 manifest are attached to the
delivering PR. Coordinated installed verification against the preserved failing
records remains a separate gate. This increment does not claim to resolve slow
native rendering, the independent action-lifetime reproduction, or every
remaining desktop surface.
