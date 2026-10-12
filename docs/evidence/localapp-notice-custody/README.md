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

## Exact saved-window recovery

An opening card now requires a live launch placement for its exact saved tab.
A missing or expired placement settles into a factual interrupted state, without
an automatic process retry. A live sibling of the same app does not keep an
orphan card opening. The existing sibling-launch serialization remains intact.

Failed cards reuse the existing install-card wrapper and shared EmptyState
action: Reopen window. The wrapper owns its layout/action address and a durable
saved-tab accessibility key while the toolkit owns measurement, painting and
input. Keyboard, pointer and the actual native resource/action door all reach
the same guarded recovery request. Escape does not submit; resizing preserves
the action. Repeated requests coalesce, all distinct requests are drained, and
closed, changed-app or already mapped cards cannot reopen from a stale action.

Opening an app while its matching failed saved card is focused now recovers
that window and retains its focus address. Recovery stages its original local
record and adopts the arriving surface into that same tab; sibling records
remain distinct. An unrelated focused window keeps ordinary launch behavior.

Before captures use merge baseline 83c7a3a72 with the production frame fixture.
The repair adds interrupted, failure and timeout recovery captures in dark,
light at 200% text and high contrast at 200% text, plus shared-card keyboard focus
and resized views. These are synthetic source fixtures, not live client frames.
The first high-contrast opening capture omits chrome on both baseline and repair;
it is retained as an initial-frame observation and is not evidence of complete
chrome presentation. Representative recovery frames show complete chrome,
wrapped explanations and the shared action. Installed repaired-layer acceptance,
cold-launch attribution and broader desktop states remain separate gates.
