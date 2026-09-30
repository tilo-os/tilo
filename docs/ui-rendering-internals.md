# UI object, redraw, and GPU-copy audit

This describes the shared `ui` framework path, not application-specific model
logic. Counts are per primary window unless stated otherwise.

See [Performance controls](performance-flags.md) for source-checked environment
flags, production defaults, experimental dependencies, and profiling caveats.

## Object lifecycle

1. Widgets are retained as one `Box<dyn Widget>` tree. Layout and drawing borrow
   the widgets; the tree is not cloned per frame.
2. `Widget::update` visits the retained tree on every submitted frame.
   `Widget::sync` reads the Automerge document only when `App::needs_sync` is
   set. A structural change rebuilds the Taffy node tree; otherwise it is kept.
   Update, sync, and layout run while the render worker can still be presenting
   the preceding frame; the UI thread waits for a recycled scene only when it
   is ready to encode the next one.
3. Layout is also retained. A clean frame does no Taffy work. A scroll-only
   frame reruns `apply_layout` but normally skips measurement and
   `compute_layout`. Size, structure, or intrinsic-size changes run the full
   solve.
4. Drawing records commands into a pooled Vello `Scene`. `Row` and `Column`
   retain subtree scenes. A clean subtree is appended from that cache; a dirty
   descendant rebuilds the containing cache. Text shaping and measurement have
   their own bounded caches, so unchanged text is not reshaped.
5. The completed `Scene` is moved to the render thread. The worker renders it,
   resets it without freeing its vector capacities, and returns it through a
   two-entry pool.

## Copies and allocations

| Boundary | Frequency | What happens |
| --- | --- | --- |
| Model to widget | Only on `sync` | Borrowed Automerge reads. App widgets decide whether their own projection needs replacing. |
| Cached subtree to frame scene | Once per clean cached ancestor that is actually reached | `Scene::append` is O(N): encoded vectors are copied and resource handles are cloned. A root `Row`/`Column` cache collapses descendant traversal to one append, but it is not a zero-copy display list. |
| UI thread to render thread | Once per submitted frame | The `Scene` is moved inline. Its vector/image/font backing allocations are not copied and there is no per-frame scene box allocation. |
| Vello resolve/upload | Once per submitted frame | Vello packs the scene and uploads the packed scene buffer to the GPU. Glyph outlines can expand during packing. The persistent image atlas uploads only new, changed, or evicted images. |
| Vello damage target to retained target | Once per dirty region on a partial frame | Each region is rasterized at its exact native dimensions and copied GPU-local into the persistent full-size target. Disconnected regions keep separate scratch textures, so a wide header plus a tall scrollbar cannot accidentally allocate a nearly full-4K rectangle. |
| Retained target to swapchain | Once per presented frame | One full-surface GPU-local RGBA8 blit: `width * height * 4` logical bytes (7.9 MiB at 1920x1080, 31.6 MiB at 3840x2160). There is no CPU readback in the live path. |
| Recycled scene | Once after render | `reset()` changes stream lengths but retains allocations. The UI side does not reset it a second time. |

At peak overlap the bounded pipeline can hold two complete frame encodings
(one rendering and one queued) plus retained `Row`/`Column` subtree caches.
Nested caches are adaptive: after an outer cache is successfully reused it
releases redundant descendant scene buffers. If the outer cache later rebuilds,
descendants record normally and can retain caches again until the outer layer
proves stable. This keeps useful dirty-subtree rebuild caches during churn but
collapses duplicate resident encodings in steady state.

`TILO_UI_PRESENT=1` prints the live values needed to quantify a particular
app: acquire/encode/submit/present time, CPU encoded-scene bytes, path and draw
stream counts, and full-surface GPU blit bytes. The reported scene byte count is
the owned CPU stream/resource-record payload (referenced font and image blobs
are excluded), not the post-glyph-expansion packed upload size.

The dashboard harness separately records allocation calls/bytes, current and
peak RSS, known retained GPU bytes, native pixels rasterized, and warm-to-final
RSS/GPU growth. Its executable budgets cover frame/event p99, allocation rate,
absolute RSS/GPU use, retained growth, and pixels per redraw. The histogram is
allocated and page-touched before the growth baseline so harness bookkeeping is
not reported as a framework leak.

The persistent GPU allocations are the retained RGBA8 target (at least
`width * height * 4` bytes), exact-sized damage textures, Vello's image atlas,
and its size-keyed pool of path/bin/tile/segment scratch buffers. The swapchain
images are owned by the window system. Damage textures are reused while their
region dimensions remain stable and dropped promptly when there are fewer
regions. When the damage topology changes, obsolete pooled Vello size classes
are released rather than accumulating for the life of the window. Vello also
adjusts its scratch budget from small counter readbacks. Ordinary live rendering
never reads the rendered image back to the CPU. Renderer startup advertises
only area anti-aliasing—the mode every frame actually selects—so it does not
compile or retain unused 8x/16x MSAA shader variants.

## Bounded resource lifetimes

- Remote images are requested lazily and fetched/decoded off the UI thread.
  Sources larger than 600 px are resized before becoming RGBA textures. Each
  store uses two workers by default (hard maximum four), a 64-job nonblocking
  queue, a 64-entry LRU, and a 64 MiB decoded-pixel ceiling. Queue saturation
  leaves the URL retryable instead of blocking the event loop.
- Baked soft-shadow sprites are limited to 32 RGB tints (about 8.1 MiB worst
  case). Eviction drops only the cache reference; scenes using the sprite keep
  it alive through `Arc`.
- Per-document undo and redo histories retain at most 512 recent actions.
  UI-graph edit history is independently capped at 100 revisions, profiler
  history at 240 samples, and render/scene/damage channels have fixed small
  capacities.
- A stalled floating-surface service can retain at most 4 MiB of outgoing
  protocol frames. Its incoming frame is capped at 1 MiB. Backpressure is an
  explicit error instead of unbounded buffering.
- Each action-bound control retains at most 64 pending invocations. Hosts drain
  these every frame; if a pathological stalled host reaches the limit, the
  oldest intent is evicted with a warning so the newest control state survives.
- Dropping the final image-store sender closes its worker queue; render workers
  have an explicit shutdown-and-join path. GPU damage targets are truncated as
  region count falls and discarded together with stale Vello pools on resize.

## Redraw policy

The event loop sleeps when idle. Redraws are requested for initial presentation,
resize/scale changes, mutations, collaboration/presence changes, tooltip or
scheduled deadlines, render-surface recovery, and active animation. Animation
uses a 16.666667 ms (60 Hz) wake deadline and is suppressed by reduced-motion
mode. The shell's background presence callback reports whether its visible
cursor projection actually changed; a quiet connected session can still poll
its transport without submitting a 4K frame.

Handled keyboard, button, wheel, and touch events conservatively redraw because
containers can change local presentation state. Pointer moves are the hot
exception: they redraw only when a widget reports dirty or floating geometry
changed. The animation-demand tree is read once per idle-loop iteration and the
result drives both the primary and floating surfaces. A primary exposure or
compositor retry with clean model/layout/widget state no longer invalidates all
native floating surfaces; those are redrawn only when shared content actually
updated, synchronized, relaid out, rebuilt, or reported visual dirtiness.

The primary surface retains its full-resolution target and performs native
partial-damage rasterization. Widgets conservatively report dirty rectangles;
rows merge adjacent child damage into a horizontal span, columns preserve
disconnected rows, and scrollbars remain separate from content damage. Bounds
are clipped to the viewport and inflated for antialiasing, focus rings, and
small shadows. Each region renders at 1:1 pixel resolution into an exact-sized
scratch target and is copied into the retained target—there is no lower-
resolution render or upscale. Layout/structure/model synchronization, overlays,
post-processing, invalid retained contents, and damage covering at least half
the surface fall back to a full render. A clean compositor-only presentation
performs no Vello raster pass. Spatial culling still avoids visiting
off-viewport widgets during scene recording; cached scrollable subtrees retain
all of their commands so scrolling cannot reveal a hole.

The worker acquires the swapchain before packing or uploading a scene. If the
surface is occluded, timed out, outdated, or gone, that attempt performs zero
Vello scene uploads and zero raster passes; recoverable cases request a later
full redraw.

## Remaining cost centers

- `Widget::for_each_child` is the allocation-free read-only traversal used by
  redraw/layout/animation/deadline checks, resource collection, handler
  discovery, and control inventory. Core containers implement it directly;
  third-party containers that only implement the compatibility
  `children() -> Vec` path still allocate until migrated.
- Resource/action discovery is cached by a host mutation generation. The
  permission-filtered projection has its own permission generation, and control
  availability is only pushed when that compound key changes. Input, sync,
  local operations, focus, structure, collaboration, and document/host-setting
  mutation boundaries invalidate it. The key intentionally avoids
  `AutoCommit::get_heads()`, because that call closes an open transaction and a
  read-only catalogue query must not split the pending undo unit.
- The authoritative stable subtree cache still pays the O(N) `Scene::append`
  into each full frame.
  Removing that copy requires renderer-level reusable display lists or a retained
  packed-scene/GPU representation, neither of which Vello exposes in this
  integration today.
- A presented 4K frame still performs the 31.6 MiB GPU-local retained-target to
  swapchain blit. Eliminating that final full-surface touch requires compositor
  damage/buffer-age integration rather than an app-only optimization.
- `TILO_UI_VELLO_CPU=1` is a diagnostic/fallback for software or unusually
  weak GPUs. It must be selected from target-hardware evidence; the normal
  hardware-GPU path remains the default.
