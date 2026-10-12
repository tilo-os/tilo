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

Publication aliases identify a host-local component lifetime. An alias received
from a native replica cannot resolve a browser component's alias registry, even
when the replicas share an app, record and raw resource address. A future
transport adapter needs an independently authenticated canonical mapping to the
browser's current component publication. Missing mapping stays a gap; labels or
decoded alias resource hex cannot establish that mapping.

The [panorama observer](panorama-capture.md) must still use its explicit mount
allowlist and apply identical transforms and clipping to pixels and geometry.
This increment does not enable semantic LIVE publication or claim real agent
activity, independent execution, native capture, 80-agent concurrency or a
measured frame rate. Tests use only synthetic retained SDK widgets.

## Browser publication/presentation seam

`FrameHarness::published_bound_resources_of(instance)` projects the currently
mounted permission-filtered component resources through `BoundPublication`.
`published_action_locus(instance, app, record, address)` accepts only one of
that host's `tilo-ui-bound:v1:<host UUID>:<lifetime UUID>:<hex resource>`
aliases. It checks the active registry owner, exact application ID, instance and
record checkout against the retained component before resolving that component's
real control. Geometry intersects every retained ancestor's child clip, including
scroll viewports outside the component owner: partially clipped controls report
only their visible rectangle, and entirely clipped controls return `None`.
It does not drain wakes, relayout, invoke an action, change focus,
selection, scrolling, or the CRDT. Missing resources, expired/replaced owners,
foreign aliases, ambiguous controls and hidden app presentations return `None`.
A component subtree containing a separate foreign component owner currently
returns a gap; observe that child through its own publication instead.

The browser mount exposes `publications()` alongside the existing domain-address
`actions()` API. Both `invokeAction` address forms enter the existing checked
component action door; aliases do not create a second dispatcher. Alias dispatch
remains restricted to its registered component owner even when another live
component uses the same numeric instance and raw resource address.
`observeSemanticTarget(target)` registers a local read-only request (at most
32 distinct targets per mount, each string at most 4096 UTF-8 bytes in Rust).
The exact target is `{app, instance, record, resource, action}`. Use the canonical
app ID from admission, `mount.state().componentInstance`, the actual mounted
record and the resource/action address returned by `publications()`:

```js
const address = mount.publications().find(d => d.address.action === admittedAction).address;
const watch = mount.observeSemanticTarget({
  app: admittedCanonicalAppId,
  instance: mount.state().componentInstance,
  record: mount.record ?? mount.state().record,
  resource: address.resource,
  action: address.action,
});
// After a subsequent successful presentation, or null for an explicit gap:
const locus = watch.read({maxAgeMs: 1000});
watch.close();
```

Registering a request asks the browser renderer for a frame; it does not send an
input event or execute an action. Rust supplies geometry only in the synchronous
status emitted after GPU submission and `output.present()`, marked
`presentation: true`. Ordinary status/semantics updates cannot relatch geometry.
The host callback pins its receipt at the existing `capture.present` boundary,
using the same observation timestamp as the pixel copy. A receipt carries the
original exact target, logical `[x,y,width,height]`, backing dimensions, logical
size, frame, layout/GPU generations and presentation observation timestamp. Reads return
copies and preserve the original timestamp. Stale/regressing/conflicting frames,
changed records, failed/suspended mounts, out-of-content bounds and backing resize before presentation
return a gap. Closing the watch unregisters its request; disposal clears all
requests and receipts.

This seam supports only a resource/action anchor on a single-app retained mount.
It supplies no authenticated publisher/execution/invocation admission, consumes
no semantic network events, reconstructs no caret/domain anchors and publishes
no presence. The next adapter must admit those exact identities, keep the locus
and panorama pane's record/frame/layout/GPU generations/time equal, and apply the pane's same
transform and clipping. Existing physical LIVE pointers are unchanged. These
synthetic correctness checks establish neither throughput nor capture readiness.
