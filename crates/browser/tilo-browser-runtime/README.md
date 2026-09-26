# Shared Tilo browser renderer

This crate mounts the actual `ApplicationDefinition` of any app compiled into the
build through checked `HostContext`, `PortableStorage`, and `FrameHarness`. The logical Vello Scene is
scaled by the shared DPR transform and presented with WebGPU. WebGL is unsupported.
This is a local component renderer; it does not authenticate peers, replicate a
workspace, or implement remote actions or media transport.

## Build

Rust and WASM dependencies follow the workspace pins. Install the wasm32 target
and wasm-bindgen CLI matching the pinned wasm-bindgen version, then run:

```sh
WASM_BINDGEN=/absolute/path/to/wasm-bindgen \
crates/browser/tilo-browser-runtime/scripts/build.sh /absolute/path/to/assets
```

Serve the generated directory from a secure origin with WebGPU. `index.html` is
an executable diagnostic fixture. Production hosts import `host.mjs`; `host.d.ts`
describes its API. The build copies canonical product fonts, manifest and licenses.
The development WASM is intentionally unoptimized; production packaging is separate.

## Applications

`src/catalog.rs` is the one list of component apps in the build and the
`ApplicationCatalog` both hosts resolve through: `mountDesktop({app})` for a
standalone app and the canonical desktop for its tabs. An app is available once
it exposes `definition()` (see `crates/sdk/ui-core/src/application.rs`), builds for
wasm32, and has a line in `INSTALLED` declaring its record scope.

`app` is a full app id (`so.tilo.word-processor`, `so.tilo.ebook`, ...), one of
the aliases `writer` and `chat`, or `desktop` for the canonical desktop, which
requires an admitted certified peer. An id the build does not contain rejects the
mount and reports `application "..." is not available` through `onStatus`; no
placeholder is mounted. `applications()` lists what the build contains, and a
mount's `application` names the resolved id, title and record scope.

## Lifetime and persistence

`mountDesktop(element, options)` resolves after the first presentation. Supply an
AbortSignal to cancel during asynchronous initialization. Each returned `cleanup`
is idempotent and generation-scoped. A page has one Winit loop and independently
owned windows. Use `shutdown` only for terminal page-host teardown, not peer close.
A subsequent mount recreates the terminally stopped loop.

Profile/device storage requires an exclusive Web Lock across tabs. Mounts in one
page share the same actor owners. A package-primary app (Chat) uses the existing
package/room primary record and remains `record=None` on the wire. Every other
app uses ordinary checked records: `record` opens one, otherwise the room's last
record of that app reopens, and `newDocument` (or an empty room) creates one.
IndexedDB transactions store bytes and actor together with strict durability;
request success never acknowledges persistence. Failed writes stay pending until
`flush()`. Failed close retains actor custody and pending bytes; call exported
`retryPendingRecovery()` to commit and release that retained lease. Errors are
reported through `onStatus` and rejected promises. Clean newly acquired leases
rehydrate durable storage, including another tab's intervening changes.

## Platform adapters

Winit owns pointer, keyboard and wheel events. The DOM adapter publishes the
current modifier snapshot before Winit key delivery. A hidden text editor forwards
composition and beforeinput through the shared host IME dispatcher. Shared semantic
routes produce an accessible DOM projection and invoke the actual widget actions.
Clipboard text is scoped to trusted clipboard events. Empty/image-only paste is
not interpreted as text deletion, and cut requires successful copy acceptance.
Asynchronous image-copy and clipboard reads outside such an event are unsupported.

Images use the shared host-driven ImageStore and wake scope. Supply `loadAsset` for
an admitted byte transport; default fetch accepts only same-origin HTTP resources.
qdoc blob URLs require admitted bytes, are hash-checked and cached in memory.
Native file/attachment/media services report unsupported in the browser.

GPU errors and device loss publish explicit failure; `recover()` recreates surface,
renderer and device objects without recreating the document. Scene readiness and
GPU presentation are separate flags, so a drawn widget still presents its Scene.

## Focused tests

```sh
node --test crates/browser/tilo-browser-runtime/web/host.test.mjs
cargo test --locked -j2 -p tilo-browser-runtime --lib catalog::
cargo test --locked -j2 -p ui --test it package_primary
cargo test --locked -j2 -p word-processor --test it canonical_component
cargo test --locked -j2 -p chat --lib --no-default-features component_tests
```
