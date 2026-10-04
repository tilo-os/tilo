# Chromium browser engine

## Authorization and outcome

The user approved macOS and Linux together, everyday browsing and Chrome
extensions, with built-in ad blocking the main priority. They then instructed
implementation using subagent-driven development, with completion requiring
actually using this browser to reach https://tilo.so. That is an additional
acceptance gate, not a substitute for the agreed capabilities.

## Architecture

Retain the Rust Tilo browser chrome. A separate native CEF helper owns Chromium's
main loop and launches Chromium's sandboxed renderer/GPU/utility processes. Rust
implements WebViewEngine against bounded framed IPC. Missing or crashed helpers
produce a visible error, never simulated browsing. TestEngine remains explicitly
available for deterministic tests and screenshots.

CEF binaries are version-pinned per platform and cached separately from Cargo.
The helper is packaged as a macOS application with correctly nested frameworks
and helper applications, and as a Linux executable with its resources. Runtime
paths resolve from the package; a development override may select a built helper.
Do not disable certificate validation or Chromium sandboxing to pass acceptance.

## Rendering and input

Start with CEF offscreen BGRA frames converted to the toolkit's RGBA mailbox;
bound dimensions and packet sizes, discard superseded frames, and avoid blocking
the UI loop on pipe writes. Forward navigation, focus, resize, pointer, wheel,
keyboard and text input. Handle popups and context menus as explicit surfaces.
GPU export must copy CEF-owned textures into client-owned storage before its
callback returns. Add platform GPU transport only with lifetime and synchronization
tests; CPU fallback must remain functional and must be identified in diagnostics.

## Blocking

Use adblock-rust directly for network request matching, preserving source and
resource-type context. Initialize bundled/cached protection before navigation,
refresh filter lists asynchronously, atomically replace compiled state, and expose
global and per-site controls. Cosmetic filtering and document-start scripts run
only in web content. Do not route privileged internal/extension pages through
page-injection code. Cover exceptions, first/third-party rules and redirects in
tests. Updates failing must keep the previous working filter set.

## Browser capabilities

Persistent profile/session data; working downloads; explicit site permissions;
clipboard, audio/video, developer tools; Chrome Manifest V3 extension capability
tests (service worker, storage, content scripts, action UI); data clearing at actual
exit when enabled. Do not claim universal extension compatibility or DRM playback.
Preserve bookmarks, history, address suggestions and Tilo actions. Existing WPE is
an explicit migration option, not an automatic fallback after Chromium failure.

## Validation

Test IPC bounds, host disconnect, input/navigation, blocker semantics and existing
browser regressions. Build the real helper and browser on macOS and Linux; use
fixture pages for network blocking, permissions, download and extension behavior.
Record runtime version, frame backend, load result, rendered output and final URL
for https://tilo.so using the actual Rust browser. Measure warm build, startup,
frame delivery and memory without labeling simulated-engine tests as web evidence.

## Delivery constraints

Keep native runtime, IPC, filtering and Rust UI integration in focused modules. Record
remaining capabilities and platform verification explicitly; do not mark the
goal complete until the whole agreed scope has evidence.
