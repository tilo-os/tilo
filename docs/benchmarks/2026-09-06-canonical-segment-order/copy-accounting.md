# Copy accounting and deferred priority

Read-only audit while the v4 release build was running. No source or workload
changed, and no additional runtime/profile was executed.

- Sparse GPU texture-to-buffer commands cover damage rectangles in persistent
  padded staging, not the whole output by default. Native oracle retained
  accounting reported75,915,264 readback payload bytes over13 attempts
  (2 full,11 sparse) in each successful combination.
- CPU staging-to-XRGB output still traverses every valid native destination
  pixel on every successful versioned paint. A tight3840x2160 output therefore
  receives33,177,600 bytes per paint. This is a full CPU conversion/write, not
  a claim that33,177,600 bytes crossed a physical GPU bus.
- Canonical sorting adds in-place GPU loads/stores and dispatch work. It reuses
  existing buffers and damage records and does not remove either output path.
- Tracked GPU allocation totals and peak process RSS are distinct measurements;
  neither establishes physical GPU transfer counts or peak dedicated VRAM.

Source evidence at v4:
- shell/src/compositor/backend/drm/paint.rs:1028-1036,1128-1151:
  full staging-to-destination output contract and accounting.
- paint.rs:1292-1347: full row/pixel conversion loop.
- paint.rs:1520-1593,1611-1619: per-damage GPU copy and payload accounting.
- shell/src/compositor/backend/drm/scanout.rs:635-656,952-958:
  two dumb buffers and back-buffer selection.
- scanout.rs:930-945,965-975: fill callback gets only output bytes and pitch,
  not buffer identity/revision.
- scanout.rs:976-989: software cursor is drawn after filling;992-999 flushes
  the full clip.
- shell/examples/redraw_bench.rs:1113-1125: one reused output Vec.

Future partial CPU output cannot simply skip the full write: a reused back
buffer may hold an older scene or cursor. A safe design needs per-buffer
last-complete revision, accumulated damage since that revision, cursor
restoration and transform/ICC/mode state. Unknown/recovery/resize/rotation or
history gaps must full-copy. It needs an identity-aware present/bridge API and
a two-buffer oracle before switching the default path. This is a future
priority candidate, not a measured improvement in this iteration.

