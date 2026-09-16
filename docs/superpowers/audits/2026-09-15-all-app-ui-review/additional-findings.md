# Additional findings during implementation

These are separate from the original 90-finding visual register; keep their status explicit.

## API saved-request draft reconciliation — OPEN

Task 11 actual selection exposed `load_request` setting local method/URL/body that the next `sync_from_doc` rebind/reconcile overwrites from the prior draft. Preserve the reproduction and canonical setter/action seams in Task 11 report. Composition fixtures staged before first sync do not verify ordinary saved-request loading. Root ruled a separate bounded follow-up after Task 11 review, so this functional repair does not expand the current layout diff. No closure or baseline reproduction claim yet.

Source preflight now exists: api-draft-preflight.md. This is an ownership conflict, not a safe direct-setter patch: URL/Body/method are bound to a shared active draft while load/history are described as local presentation. Canonical action/undo and saved-Send/history resource semantics need an explicit separate repair design. Additional history View admission and in-flight history snapshot concerns are source-derived, not reproduced. Keep them open and distinguish them from the reviewed composition change.
