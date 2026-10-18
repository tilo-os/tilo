# App rendering: apps describe, the room's render service draws

A toolkit app does not open the GPU. Its toolkit builds the widget tree, lays
it out and records each frame as a vello scene, exactly as before; instead of
rasterizing it, the window's render worker hands the scene to the **render
service of the app's room**, which validates it, rasterizes it on a device it
opened once, and returns a buffer the app attaches to its own `wl_surface`.
The compositor imports that buffer like any client's: zero extra copies, and
direct scanout still applies.

Why: the GPU kernel driver is one of the most exploited kernel surfaces, and
every app talking to it directly made every app a way in. Opening a device,
compiling pipelines and creating a swapchain also cost every launch ~30 ms,
and every app kept its own device, pipelines and caches in memory.

This is step 1 of the design. Step 2 (scene annotations, service-run
motion) and step 3 (the system composes apps' views) build on the same
scenes, layer ids and protocol; see "Extending" below.

## Parts

| Part | Where | What it does |
|---|---|---|
| Protocol | `crates/platform/tilo-render-protocol` | Frame records, vello encodings by resource id, the validator, limits, control messages, the sealed-memfd ring, the broker handshake |
| Render service | `crates/session/render-service` | One process per room: decodes, validates, rasterizes into dma-bufs, keeps every app's state apart |
| Broker | `shell::compositor::render_broker` | Picks the room's service for each app from the launcher's attribution, starts and restarts services |
| Launcher | `launcher::services::resolve_render_bind`, `launcher::gpu::granted` | Connects every app without `gpu` to the broker before the sandbox starts; binds no GPU node into it |
| Remote renderer | `ui::render_thread::remote` | The toolkit's render worker for a window drawn by the service, behind the same `RenderThreadHandle` |
| Client buffers | `vendor/winit` (`WindowExtWayland::create_client_buffer`, `attach_client_buffer`) | `wl_buffer`s of the service's dma-bufs, attached and committed under the window's state lock |

## One frame

1. The main thread lays out and records the frame into a vello `Scene`, with
   its damage and scroll copies, and sends it to the window's worker — the
   same request the in-process worker takes.
2. The worker writes a **frame record** into the connection's ring: the
   surface size, base colour, damage, scroll copies, the root layer's
   encoding and any backdrop blurs (`tilo_render_protocol::frame`). Fonts and
   images are referenced by id. A font the text renderer loaded from a file
   under the service's font directories is registered by its path (the
   service maps the same file from the read-only view); anything else, and
   every image, is uploaded once into a sealed memfd the app creates, so its
   memory is charged to the app's cgroup. The worker sends `Frame { sequence,
   offset, len }`.
3. The service reads the record out of the ring **once**, into its own memory
   (`scene::decode`), and validates that copy (`validate`). Nothing it read
   from shared memory is read twice, so the app cannot change a scene between
   validation and drawing.
4. The service applies the layer changes, composes the layers, and
   rasterizes the frame into the surface's retained target with exactly the
   raster path the app's own worker uses (`ui::render_thread::RetainedRaster`:
   damage-only fine raster, scroll copies first, backdrop blurs). Then it
   copies what changed into a buffer the app is not holding — what changed
   since that buffer last held a frame — waits for the GPU once for every
   app it drew in this pass, and answers `FrameReady { buffer, damage }`.
5. The worker attaches that buffer with the frame's damage and commits, with
   `wp_presentation` feedback; the compositor composites (or scans out) it
   like any client buffer. Presentation is forwarded to the service
   (`Presented`) for the app's frame timeline. When the compositor releases
   a buffer, the worker's release handler tells the service (`Released`).

Buffers: three linear `ABGR8888`/`XBGR8888` dma-bufs per surface from GBM on
the compositor's render node — R, G, B, A in memory, which is the service's
`Rgba8Unorm` target, so the copy is a plain texture copy. Without a GPU (a
VM, a software adapter) the service rasterizes on the CPU into `wl_shm`
buffers instead.

## The validator

Every scene is hostile until it passes `tilo_render_protocol::validate`, a
single CPU pass over the decoded encoding that checks exactly the invariants
vello's encoder establishes and its resolver and shaders rely on:

- path tags are tags vello writes; path data holds exactly the points the
  segments consume; one transform per transform tag, one style per style tag;
  path, segment and clip counts match the streams; one draw object per path
  (glyph runs bring theirs at resolve);
- every gradient and image draw has exactly one patch at its own offset, in
  stream order; an image draw's size is the registered image's, so a scene
  cannot read beyond its image in the shared atlas (another app's pixels);
  glyph runs are referenced once each, in order, with in-bounds ranges;
- coordinates, transforms, stroke widths, gradient geometry, layer alpha,
  blend modes, glyph sizes and embolden parameters are finite and bounded;
- clips balance, never deeper than the limit, and a layer leaves none open;
- the app's limits hold (`Limits`): encoded size, paths, segments, draw
  objects, glyphs, layers, surfaces, surface pixels, image bytes, images,
  fonts, rectangles, backdrops.

A frame that fails gets `FrameDropped` with the reason and is not drawn; a
protocol violation on the control channel closes that app's connection.
Neither stalls the service. Proptest checks that every scene the toolkit can
draw passes unchanged, and that scribbled-over records never panic the
decoder, the validator or vello's resolver; the `render_scene` fuzz target
(`fuzz/`, run nightly by the `nightly-fuzz` check) does the same with
coverage guidance.

**GPU time.** Each frame's time is measured from decode to GPU completion.
An app whose frame takes longer than `Limits::gpu_time_us` has its following
frames dropped (`Refusal::Throttled`) until it has waited out the overrun.

**Clean memory.** Targets and buffers belong to one surface of one app and
are never reused for another. When the renderer draws for a different app
than last time, its pooled scratch buffers are dropped first; fresh ones are
zeroed by wgpu, so nothing one app drew can be read by the next. Images live
in the atlas by their own blob, and validated image draws read only their
own slot.

## Rooms and the broker

**One service per room** (decision Q1). The person's private rooms — nobody
but them on the roster or present — share one service; every other room has
its own, so an app in a room with strangers never shares a device, a cache
or GPU memory with another room's apps. The key is computed from the
workspace tree when the app asks (`render_broker::render_key`); when a room
changes hands (someone joins a private room), the broker drops its apps'
channels on the old service and they ask again.

The broker never trusts an app for this: the launcher connects to the
broker's socket on the app's behalf and writes the app id and room
(`tilo_render_protocol::broker::handshake`) before the sandboxed app holds
the connection (`TILO_SVC_RENDER_FD`). Each request on it gets a fresh
`SOCK_SEQPACKET` pair; one end goes to the service with the app's identity,
the other to the app. Each window has a channel of its own.

**Process.** A service is `shell __render-service`: the shell binary in
render-service mode, started by the broker with nothing but its broker
socket (every other descriptor is closed first) and without the session's
display or bus. It opens the compositor's render node, builds every vello
pipeline, then confines itself (`tilo_render_service::confine`): Landlock to
the render node (read-write), the font directories and the driver's
libraries (read), with TCP denied, as a hard requirement; and seccomp with
the application baseline plus no socket creation, no connect, no exec. The
private rooms' service starts with the session, so the first app finds it
warm.

## Failure

- **A service dies** (a crash, a lost device): every app keeps showing its
  last frame, since the compositor still holds the buffers it attached. The
  app's next frame finds the channel closed; the worker asks the broker for
  a new one, the broker starts a new service, and the toolkit sends a whole
  frame. A service that keeps dying young is restarted with exponential
  backoff (to 5 s), never in a spin.
- **An app dies or misbehaves**: its connection closes and the service drops
  its surfaces, layers, fonts and images.
- **A panic while drawing** one app's frame (what the validator exists to
  prevent) is contained (`tilo_rt::panic_boundary`): that app's connection
  closes and the renderer is rebuilt.

## Pacing

Apps draw on the display clock as before: the compositor's frame callback
paces each window's redraws, and the worker commits the service's buffer on
the surface those callbacks belong to. The service draws a frame as soon as
it arrives and batches the GPU wait of every app it drew in one pass of its
loop. Apps learn when their frame was presented through `wp_presentation`
on their own surface.

## Apps that use the GPU themselves

An app that declares the `gpu` capability (`tilo-caps`; today the Browser,
Mail, Chat, Camera, Tube, Video Player and Video Editor: web engines, video,
camera) gets the compositor's render node, its loader pin and shader-cache
seed as before, and draws in-process as before. Every other app gets no GPU
node at all, and the launcher spends nothing on Vulkan or Mesa setup for it
(`launcher::gpu::granted`). The signed manifest is the ceiling, as for every
capability; the permission prompt that lets a person grant `gpu` to an app
they installed themselves is not built yet.

## Extending

- **Layers.** Frames already carry layers by id with per-layer content,
  transform, opacity and clip, and an order; the service composes them. The
  toolkit sends one root layer today; scrolling regions and popovers can
  move to layers of their own without a protocol change.
- **Sections.** A frame record is a list of typed sections. A section a
  reader does not know is skipped when its sender marked it optional (scene
  annotations, step 2) and refuses the frame otherwise (service-run motion,
  sent only to a service whose hello announced it).
- **External surfaces** (video, camera, webviews as dma-bufs by id, Q4) are
  the next resource kind beside fonts and images; vello's `draw_image_from`
  already draws an image from a texture.

## Measuring

`tilo_startup_metrics` marks on the app side: `remote-render-connect-start`,
`remote-render-connect-done`, `remote-render-first-frame-sent`, and
`first-frame-presented` when the first buffer is attached. Per-frame service
timing comes back as `FrameReady::gpu_us`. Numbers against the start-time
budget are in the design notes' phase table.
