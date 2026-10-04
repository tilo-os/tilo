# Retained semantic action geometry

`FrameHarness::action_ui_routes(address)` exposes the same canonical action
binding resolver used by native `App::action_ui_routes`. It reads the current
retained layout without draining component wakes, running layout, executing an
action, scrolling, focusing, or changing the document. `ActionUiRoute` is
available on portable/browser builds as well as native builds.

`FrameHarness::semantic_locus(interaction)` returns a rectangle only for an
unambiguous bound action target. A resource label is never an identity. Reused
control IDs, multiple affordances, missing controls, unsupported domain
anchors and non-durable semantic IDs return `None`. Transparent wrappers use
their actual descendant control's geometry. Caller search and History hiding
the live app suppress its locus.

Native semantic presence keeps the original resource/action anchor and resolves
it on each layout instead of retaining an arrival-time positional control ID.
Unknown list-item geometry remains app-level activity; it is not guessed from
a matching accessible label.

These methods supply local geometry, not authentication or a capture receipt.
A browser host must first admit the exact publisher, execution, invocation and
app/record/resource, translate any component publication alias through its
existing binding, then latch geometry with the successful presentation's
record, frame, layout/GPU generation and timestamp. Calling the resolver on a
later status update does not establish pixel/geometry coherence. Missing alias
translation or geometry must remain an explicit gap.

The [panorama observer](panorama-capture.md) must still use its explicit mount
allowlist and apply identical transforms and clipping to pixels and geometry.
This increment does not enable semantic LIVE publication or claim real agent
activity, independent execution, native capture, 80-agent concurrency or a
measured frame rate. Tests use only synthetic retained SDK widgets.
