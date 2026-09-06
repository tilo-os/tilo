# Deferred design: buffer-age-aware CPU XRGB writes

Read-only Terra design, reviewed after the canonical-order comparison.
No source change, benchmark change, default switch or performance claim.

## Production route

Keep the existing full-output bridge APIs unchanged for unknown callers.
A new private/direct-DRM entry point can accept a scanout-generated full/rect
write plan and use the existing mapped staging buffer. It must not expose a
generally trusted partial-plan contract to callers that cannot prove destination
history. The plan needs to cover all scene damage since that exact destination
was last complete, plus pixels occupied by its old software cursor.

The two-buffer scanout owns bounded per-slot state: validity, scene revision,
pending damage and prior software-cursor footprint, plus a configuration epoch
and last accepted sequential scene revision. Accumulate each admitted frame
delta for both slots; reset the selected slot only after successful output.
There is no extra full-frame image buffer.

The bridge keeps the current GPU render/readback transaction. It can swizzle
only planned rectangles from persistent staging when both its retained staging
and the scanout history are valid. A full GPU recovery produces complete staging;
any unproven history, invalid damage or failed transaction still falls back to
a full destination write.

## Cursor and invalidation

Persistent staging contains scene pixels without the software cursor. Restoring
the selected buffer's old cursor rectangle therefore needs CPU writes from
staging, not another Vello redraw. Existing cursor compositing then writes the
new cursor and records that exact footprint. Pointer leave restores the old
footprint; hardware cursor mode has no software footprint.

Bridge/map/write/page-flip errors invalidate age state conservatively. So do
buffer replacement, resize/modeset/hotplug, restore/DPMS, base-color or
transform/scale/ICC epoch changes, history gaps and use of another presentation
route. Frame throttling must not acknowledge a revision or consume damage if
presentation returns early. A later revision gap must force full restoration.

Limit the initial implementation to the existing direct 1:1 single-output,
no-ICC route. Preserve full dirty-framebuffer flushing initially: reducing
that clip is a different correctness/driver contract and is not included.

## Validation required before implementation can be promoted

- Pure two-slot history: seed R1; second buffer full at R2; first buffer at R3
  receives the union of D2 and D3.
- Bound region count/area; test gaps, invalid regions, epoch changes and failures.
- Moving/leaving software cursor and transition to/from hardware cursor.
- Two stale-filled destination buffers, padded pitches, disjoint/overlapping
  regions, retained GPU sparse work and forced full recovery compared byte-for-
  byte against complete XRGB output after each operation.
- No changes to old full-output tests, original native pixel assertions,
  acceptance workload, tolerance or timing gates.
- Explicit byte accounting for CPU output separately from GPU readback; no
  blanket GPU-memory-copy claim.
- A separate two-buffer integration diagnostic for the real production history
  contract. The existing single-Vec acceptance benchmark is not such a test.
  Any future harness use of a new production API needs an explicit equivalence
  rationale and must retain the original workload/timing boundaries.

## Scope and expected limit

Relevant current paths are bridge paint.rs, scanout.rs and direct routing in
backend/drm/mod.rs. This is a cross-layer change rather than a local swizzle
shortcut. The current callback supplies only bytes and pitch, not identity.
A new fallible direct-damage presentation method is safer than silently changing
every generic present callback's full-write guarantee.

Existing diagnostic logs show CPU swizzle around 2.0–2.2 ms mean while GPU
completion is much larger. The design could reduce CPU bandwidth and a portion
of shell mean time, but cannot be claimed sufficient for p99 <= 50 ms. It has
no direct benefit for the dashboard's zero-pixel-readback GPU-only route.
