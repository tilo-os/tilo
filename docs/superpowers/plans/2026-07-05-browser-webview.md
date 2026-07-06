# WebView component + WebKit browser

## Decision record

- **Engine**: WPE WebKit 2.52.x (WebKit's embedding port). Chosen for daily-drivable
  web compat + performance, non-Chromium, upstream-maintained. Servo (previous
  engine) stays possible as a future backend behind the same trait; its
  integration code is preserved in git history.
- **Packaging**: Fedora does not ship WPE → build `libwpe` + `wpebackend-fdo` +
  `wpewebkit` ourselves with `os/dev/build-wpe.sh` (installs into
  `~/.cache/qos-wpe/prefix` for dev). The OS image builds the same script in a
  dedicated `wpe-builder` Containerfile stage (prefix `/usr`, staged via
  DESTDIR, layer-cached until the pinned version changes) — matching how the
  image already builds its own Rust binaries, no RPM machinery. Its build deps
  currently come from live `dnf builddep webkitgtk6.0` (documented exception);
  folding them into the frozen snapshot is a follow-up.
- **Ad blocking**: built into the WebView — Brave/EasyList filter lists compiled to
  WebKit content-blocker JSON via adblock-rust (`content-blocking` feature), loaded
  through `WebKitUserContentFilterStore`. Blocks inside WebKit's network process.
  (WebKit exposes no embedder request-intercept hook, so the adblock-rust *engine*
  cannot sit on the request path; compiled content filters are the native
  equivalent.)
- **Extensions**: wait for upstream. WebKit 2.52 ships `WebKitWebExtension`
  (manifest parsing, MV2+MV3, match patterns); the controller/runtime classes are
  still being ported from Safari's implementation. Groundwork only: store-install
  flow + extension manager data model, activated when the runtime API lands.
  No self-built WebExtensions shim.
- **Tabs**: none in the app. Tabbing is the compositor WindowGroup frame primitive.

## Architecture

```
ui/src/webview/
  mod.rs        WebViewCommand / WebViewEvent / Frame / WebViewEngine trait
  test_engine.rs TestEngine — deterministic frames + scripted events (always built;
                 used by unit tests and --shots)
  adblock.rs    [feature webview-adblock] filter-list → content-blocker JSON compiler
  wpe/          [feature webview-wpe] hand-written FFI + engine thread (GLib main
                loop; wpebackend-fdo SHM export → RGBA Frame slot; DMA-BUF
                zero-copy is a later optimisation)
ui/src/widget/webview.rs
  WebView leaf widget: paints latest Frame (peniko Image), forwards pointer/key/
  scroll input as commands, emits Resize on layout change, focusable,
  frame-driven redraw via needs_animation_frame.
apps/browser/
  chrome (address bar, back/forward/reload, progress, error handling) on ui
  widgets; owns SharedEngine, drains WebViewEvents into CRDT state; picks
  backend at runtime (WPE with the `wpe` feature, TestEngine otherwise/--shots);
  extensions.rs = store fetch/unpack/manifest inventory groundwork.
```

The engine runs on its own thread (same pattern as the previous Servo engine
thread): commands in over a channel, frames out through a shared slot, UI events
out through a drain queue, host wake via a `set_waker` callback wired to the
winit event-loop proxy.

## Order of work

1. `ui` webview module: types + TestEngine (tests first).
2. `WebView` widget (tests against TestEngine).
3. Adblock compiler module (tests on fixture lists).
4. Browser app rewrite + `--shots` verification.
5. WPE backend + live engine integration test (`ui/tests/wpe_engine.rs`).
6. wpewebkit in the OS image (`wpe-builder` stage) + browser with
   `--features wpe`; snapshot-rpms re-run for the new runtime packages.
7. Extension groundwork (store fetch/unpack/parse, manager model).

## Risks

- WPE build or FFI surprises → engine trait isolates them; everything else ships
  against TestEngine regardless.
- WebKit's bubblewrap sandbox nested inside distrobox/cluster sandboxes may need
  `WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1` in dev; revisit for the OS image
  (qOS cluster sandbox is the outer boundary).
- Readback (SHM) frame path costs a copy per frame; acceptable v1, DMA-BUF→wgpu
  import is the follow-up optimisation.
