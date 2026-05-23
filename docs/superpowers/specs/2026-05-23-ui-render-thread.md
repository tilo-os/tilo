# UI render-thread design (Item 4 of perf plan)

> **Status:** designed, not implemented. Items 1–3 of the
> 2026-05-23 perf plan are landed and benched; Item 4 is a
> separate surface-lifetime refactor that warrants its own
> review pass.

## Goal

Move `vello::Renderer::render_to_texture` + swapchain
`get_current_texture` / `surface_texture.present()` /
`device.poll()` off the main winit thread so:

- Main thread can start preparing the next frame while the GPU
  is still draining the previous one.
- Latency-bound paths (input → first-pixel) drop the GPU
  acquire-block from the critical path.

Note: this **does not** improve throughput Hz in
benchmark-style steady state. wgpu's queue submit doesn't
block; the actual block is on the swapchain's next available
image, which we currently absorb on the main thread inside
`get_current_texture`. The render thread moves that wait off
main.

## What needs to move

Today's main thread (line numbers refer to `ui/src/app.rs`
@HEAD `bb16a09`):

```
~835: renderer.render_to_texture(...)
 846: surface.surface.get_current_texture()  ← swapchain block
 866: device.create_command_encoder(...)
 871: surface.blitter.copy(...)
 879: queue.submit(...)
 882: surface_texture.present()
 883: device.poll(Poll)
```

All seven steps move to a worker thread. The main thread:
1. Owns the widget tree + `Scene` building.
2. Sends `(Scene, ResizedDimensions, ThemeBgColor)` to the
   worker via a bounded channel (capacity 1 — strict
   double-buffer).
3. Returns from `RedrawRequested` immediately.

The worker:
1. Owns the `RenderContext`, `RenderSurface<'static>`, and
   `vello::Renderer`.
2. Receives a frame, does steps 835-883 above.
3. Loops back, blocks on channel.

## Send / Sync verification

Compile-time check (validated; `assert_send::<vello::Renderer>()`
and `assert_send::<vello::Scene>()` both compile):

| Type | `Send` | `Sync` | Notes |
|------|--------|--------|-------|
| `vello::Renderer` | ✓ | ? | Only need `Send` (single worker owns it). |
| `vello::Scene` | ✓ | ? | Built on main, moved to worker per frame. |
| `RenderContext` | ✓ | ? | Owns wgpu Device + Queue handles, both `Send` in wgpu 29. |
| `RenderSurface<'static>` | ✓ | – | `'static` lifetime is essential; the live App already uses this. |

## Refactor shape

`AppHandler` currently holds `render: Option<RenderState>`.
After the refactor:

```rust
struct RenderRequest {
    scene: Scene,
    width: u32,
    height: u32,
    base_color: peniko::Color,
    // Cleared when the scene-encoding work is done so main can
    // recycle the Scene buffer (rather than reallocating).
    done: std::sync::mpsc::SyncSender<Scene>,
}

struct AppHandler {
    // ...existing fields except render...
    render_tx: std::sync::mpsc::SyncSender<RenderRequest>,
    /// Scene buffer pool. Main pulls one for the next frame
    /// when the worker returns a finished one through the
    /// `done` channel. Capacity 2 keeps main and worker
    /// out of each other's way without unbounded queueing.
    scene_pool: std::sync::mpsc::Receiver<Scene>,
}

fn spawn_render_thread(
    render_cx: RenderContext,
    surface: RenderSurface<'static>,
    mut renderer: vello::Renderer,
) -> (SyncSender<RenderRequest>, Receiver<Scene>) {
    let (tx, rx) = mpsc::sync_channel::<RenderRequest>(1);
    let (pool_tx, pool_rx) = mpsc::sync_channel::<Scene>(2);
    // Seed the pool with two fresh Scenes so the first two
    // frames find a buffer waiting.
    pool_tx.send(Scene::new()).unwrap();
    pool_tx.send(Scene::new()).unwrap();
    std::thread::Builder::new()
        .name("ui-render".into())
        .spawn(move || {
            while let Ok(req) = rx.recv() {
                // ... do the seven GPU steps from ui/app.rs:835-883
                //     using req.scene, req.width, req.height,
                //     req.base_color, &mut renderer, &surface,
                //     &render_cx
                // After present + poll, hand the Scene back so
                // main can recycle the encoding buffer.
                let _ = req.done.send(req.scene);
            }
        })
        .unwrap();
    (tx, pool_rx)
}
```

In `window_event WindowEvent::RedrawRequested`:

```rust
// 1. Pull a Scene buffer from the pool (blocks briefly if the
//    worker hasn't drained the previous frame — backpressure).
let mut scene = self.scene_pool.recv().expect("render thread died");
scene.reset();

// 2. Build the frame (existing draw_with_presence,
//    damage clip, debug overlay, …).
// ...

// 3. Send to worker. The done channel is per-request, so we
//    can recycle the Scene into the pool as soon as the
//    worker is finished with it.
let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
self.render_tx.send(RenderRequest {
    scene,
    width,
    height,
    base_color: app.theme.bg_app,
    done: done_tx,
}).expect("render thread died");

// 4. Forward the recycled Scene back to the pool once the
//    worker hands it back. Done on a tiny detached thread so
//    main doesn't block. (Or via a single shared pool
//    channel — the inline `done` is just for clarity.)
let pool = self.scene_pool_sender.clone();
std::thread::spawn(move || {
    if let Ok(s) = done_rx.recv() { let _ = pool.send(s); }
});
```

## Surface resize handling

`Resized` events fire on the main thread; the surface needs
`render_cx.resize_surface(…)` which currently lives on main.
After the refactor, the surface lives on the worker. Two
options:

1. **Worker handles resize**: `RenderRequest` includes new
   `width / height`. Worker compares against last seen
   dimensions and calls `resize_surface` before render.
2. **Send a `Resize` variant on the channel**: explicit
   `enum RenderMsg { Frame(RenderRequest), Resize(u32, u32) }`.

Option 1 is simpler; option 2 keeps the worker's main loop
declarative. Either works.

## Suboptimal / outdated / occluded handling

Today's code re-requests redraw on Outdated / Suboptimal /
Occluded / Timeout. After the refactor, the worker needs to
signal "frame skipped, please redraw" back to main. Easy via
a small `frame_complete: Receiver<FrameOutcome>` channel.

## Cost / benefit

- **Throughput Hz:** unchanged. The bench measures throughput
  by submitting frames back-to-back; the GPU is the
  bottleneck and stays the bottleneck.
- **First-pixel latency:** drops by `t_gpu` (≈ 3-4 ms on this
  hardware). Real-world feels: hover transitions, click flashes,
  scroll wheel responsiveness all feel "snappier" because the
  next event's frame can start being prepared while the
  previous frame is still in flight.
- **Engineering:** ~150 lines net change, mostly in
  `ui/src/app.rs`. One channel-receive crash-handling path
  to think through ("render thread died → quit the app").
- **Risk:** wgpu surface-lifetime gotchas on resize / window
  destruction. The worker thread needs to drop its surface
  before the window is destroyed, or wgpu may panic.

## Not doing yet because

1. The bench can't measure the win (throughput-bound, not
   latency-bound), so we can't auto-validate "this didn't
   regress." Item 4 needs a live-window p99 input-to-pixel
   latency probe before it can be safely landed.
2. Items 1+2 already drop main-thread CPU to ~200 µs / frame
   in the dashboard bench — there's not much else for main
   to do while the GPU drains. The thread split's biggest
   benefit is hidden until we have UI work that *isn't* the
   render itself (collab event handling, CRDT sync) competing
   for main-thread budget.

A follow-up plan should land Item 4 alongside a latency-
focused bench harness.
