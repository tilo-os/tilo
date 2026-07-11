# Embeddable WebView: per-instance ad-block, multi-layer zero-copy, OS integration

**Date:** 2026-07-11
**Status:** design approved (pending spec review)

## Goal

The `ui` toolkit's WebView is already a public, embeddable component
(`ui::widget::WebView::new(engine)`) — the browser is just an app that
embeds it. This work makes it a genuinely *ergonomic* embeddable
component and proves it runs in the OS:

1. **Ad-blocking as a per-WebView setting owned by `ui`** — any embedding
   app flips a boolean per WebView instead of re-implementing the browser's
   fetch/compile/install loop.
2. **A per-WebView zero-copy GPU layer registry** — any number of
   GPU-composited WebViews work, and embedders get the GPU path without
   hand-wiring a render-thread hook.
3. **The OS actually ships and launches the browser, verified by a full
   VM boot-test.**

Build order: **1 → 2 → 3**, re-syncing `main` between parts (a concurrent
OS session also writes `main`).

## Current state (grounding)

- **WebView widget:** `ui/src/widget/webview.rs`; engine trait + commands
  in `ui/src/webview/mod.rs` (`WebViewEngine`, `SharedEngine`,
  `WebViewCommand`). Public, not feature-gated.
- **Ad-block:** compiler `ui/src/webview/adblock.rs` (feature
  `webview-adblock`); *orchestration* lives in the browser app
  (`apps/browser/src/blocker.rs`: fetch → disk cache → chunked compile →
  install, plus YouTube ad-skip user-scripts). Engine commands
  `SetContentFilters { version, chunks }`, `SetUserScripts`,
  `ClearWebsiteData`. The WebKit-clean 7-list set is the proven-safe set
  (EasyList, EasyPrivacy, EasyList-Cookie, uBO filters/privacy/badware/
  resource-abuse).
- **Zero-copy:** opt-in via `QOS_WEBVIEW_DMABUF=1`. Engine EGL export
  (`ui/src/webview/wpe/egl.rs`) → linear dmabuf → wgpu import
  (`ui/src/webview/wpe/import.rs`); a **single global**
  `set_frame_post_processor` hook in `ui/src/render_thread.rs`; the browser
  composites through `apps/browser/src/compositor.rs`. Buffers release
  immediately (tearing risk). AMD-only, manual env.
- **OS:** the browser binary **and** the full WPE runtime (engine libs,
  `WPEWebProcess`/`WPENetworkProcess`, GStreamer/libsoup/… deps) are
  **already built into `os/Containerfile`** (`cargo build … -p browser
  --features browser/wpe`; `COPY … /usr/bin/browser`; WPE libs copied +
  `ldconfig`). Launch registration, sandbox nesting, and an actual boot
  test are unverified.

## Part 1 — Per-WebView ad-blocking (ui, self-contained)

### Public API (on `WebView`)

```rust
WebView::new(engine)
    .with_ad_blocking(true)        // builder; initial state
// and at runtime:
webview.set_ad_blocking(false);    // toggle; drives the next sync
```

- **Per widget instance** — two WebViews in one app can differ.
- Default **off**; an embedder opts in. The browser sets it on from its
  existing Settings toggle.

### Internal: shared filter service (`ui/src/webview/adblock_service.rs`)

- A process-wide, lazily-started loader (moved out of `blocker.rs`):
  fetch the canonical 7-list set on a background thread, cache under a
  default dir (`dirs::cache_dir()/qos-webview/filters`, overridable),
  compile once via `adblock::compile_filter_lists_chunked`, and expose a
  cheap shared handle to `(version, chunks, user_scripts)`.
- **Compiled once, shared** across every WebView; install/remove is
  per-instance.
- When a WebView has blocking enabled and the compiled filter is ready, it
  sends `SetContentFilters { version, chunks }` + `SetUserScripts`; when
  disabled it clears them (`SetContentFilters { version, [] }` + empty
  scripts). The waker re-syncs WebViews when the async compile finishes.

### Feature gating

- Behind `webview-adblock`. Without it, `with_ad_blocking`/`set_ad_blocking`
  compile to a no-op, so embedders that don't want blocking don't pull
  `adblock-rust`.

### Browser changes

- `apps/browser/src/blocker.rs` collapses to using the `ui` service +
  `webview.set_ad_blocking(settings.ad_block)`. Keep the browser Settings
  toggle already present; wire it through.

### Tests

- Unit: service compiles the 7 lists → non-empty chunks, stable version.
- Integration (`--features wpe`): a WebView with blocking on installs
  filters (engine log "content filter installed"); off clears. Reuse
  `adblock_blank_repro` to re-confirm no reload-blank.

## Part 2 — Multi-layer zero-copy GPU (ui + shared render/App)

### Problem

The single global post-processor supports one WebView and requires the app
to hand-wire the content rect + engine. Not embeddable; single-instance.

### Design: external-layer registry

- Replace the single global processor in `render_thread` with a
  **registry of external GPU layers**, `Arc<Mutex<Vec<ExternalLayer>>>`,
  shared between the main thread (add/remove/rect-update) and the render
  thread (composite).
- `ExternalLayer = { id, engine: SharedEngine (source of `take_dmabuf`),
  rect: device-space fractions, z: order }`.
- The **`WebView` widget self-registers** a layer when the GPU path is
  active (on construction), updates its rect every `apply_layout`/`draw`,
  and unregisters on `Drop`. No app wiring.
- Composite step stays a **compute pass** (vello's intermediate is
  `STORAGE_BINDING`, no `RENDER_ATTACHMENT` — a render pass fails
  validation): iterate layers in z-order, import each pending dmabuf via
  `WebViewEngine::take_dmabuf`, `textureStore` into the intermediate at the
  layer rect.
- **Auto-enable:** at engine construction probe for a usable DRM render
  node + the required Vulkan external-memory exts; enable GPU path if
  present else SHM. Env override `QOS_WEBVIEW_DMABUF=auto|0|1` (default
  `auto`).
- **Hold/release handshake:** the engine holds the exported EGLImage/
  dmabuf until the render thread signals composite-done (release callback /
  fence), so WebKit doesn't recycle the buffer mid-read. Per-layer small
  double-buffer.

### Touchpoints

- `ui/src/render_thread.rs` — registry + multi-layer composite (replaces
  the single processor).
- `ui/src/app.rs` — own the registry handle, thread it to the render
  thread, expose a registration handle to widgets.
- `ui/src/widget/webview.rs` — self-register/update/unregister.
- `ui/src/webview/wpe/{mod,egl,import}.rs` — auto-detect, hold/release.
- `apps/browser` — delete `compositor.rs` manual wiring.
- **Shared-code caution:** `app.rs`/`render_thread.rs` are edited by the
  concurrent OS session — keep the registry surface minimal + additive and
  re-sync `main` before merge.

### Tests

- Existing `wpe_dmabuf_probe` still passes.
- New `ui` example: two WebViews side by side, both GPU-composited,
  pixel-verify each shows its own page in its own rect.
- SHM fallback verified when GPU forced off (`QOS_WEBVIEW_DMABUF=0`).
- No wgpu validation errors; no tearing under fast repaint (hold/release).

## Part 3 — OS embeds the browser, usably (os + shell)

### Work

- **Launcher registration:** make the browser launchable like
  `terminal`/`file-explorer` (confirm the exact mechanism — app registry /
  search provider / wheel-disc entry) with name + icon.
- **Sandbox / env:** WebKit's own bwrap can't nest inside the compositor's
  per-workspace bwrap → launch the browser with
  `WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1` (the workspace sandbox
  remains the security boundary) and ensure `/dev/dri` is available for the
  GPU path. Confirm `WEBKIT_EXEC_PATH`/lib paths resolve at launch.
- **Runtime libs:** confirm `ldconfig` + WebProcess/NetworkProcess paths
  resolve at launch under the sandbox.

### Verification — full VM boot-test (decisive)

- Build the image (`os/dev/build-image.sh`), boot in QEMU (`os/dev/vm.sh`
  / `os/test-vm.sh`), log in (`quickuser`/`quickdesktop`), launch the
  browser, navigate to a live page, capture a screendump proving it
  renders. In-VM GPU is virtio/llvmpipe so the GPU path will fall back to
  SHM — **SHM must render in the VM.**

## Risks / open items

- Concurrent OS session edits shared `ui`/`app.rs`/`render_thread.rs` and
  `os/` — re-fetch and re-sync `main` before each merge; keep changes
  additive.
- WebKit bwrap nesting inside the compositor sandbox is the riskiest
  Part-3 unknown.
- VM GPU is software — Part 2's GPU path is exercised on the host (AMD
  render node), not in the VM; the VM proves the SHM path.

## Rollout

Parts land in order 1 → 2 → 3, each committed + pushed + verified before
the next, re-syncing `main` between parts.
