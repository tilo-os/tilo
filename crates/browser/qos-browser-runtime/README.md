# Shared qOS browser renderer

This crate mounts the actual Writer or Chat `ApplicationDefinition` through checked
`HostContext`, `PortableStorage`, and `FrameHarness`. The logical Vello Scene is
scaled by the shared DPR transform and presented with WebGPU. WebGL is unsupported.
This is a local component renderer; it does not authenticate peers, replicate a
workspace, or implement remote actions or media transport.

## Build

Rust and WASM dependencies follow the workspace pins. Install the wasm32 target
and wasm-bindgen CLI matching the pinned wasm-bindgen version, then run:

```sh
WASM_BINDGEN=/absolute/path/to/wasm-bindgen \
crates/browser/qos-browser-runtime/scripts/build.sh /absolute/path/to/assets
```

Serve the generated directory from a secure origin with WebGPU. `index.html` is
an executable diagnostic fixture. Production hosts import `host.mjs`; `host.d.ts`
describes its API. The build copies canonical product fonts, manifest and licenses.
The development WASM is intentionally unoptimized; production packaging is separate.

## Lifetime and persistence

`mountDesktop(element, options)` resolves after the first presentation. Supply an
AbortSignal to cancel during asynchronous initialization. Each returned `cleanup`
is idempotent and generation-scoped. A page has one Winit loop and independently
owned windows. Use `shutdown` only for terminal page-host teardown, not peer close.
A subsequent mount recreates the terminally stopped loop.

Profile/device storage requires an exclusive Web Lock across tabs. Mounts in one
page share the same actor owners. Chat uses the existing package/room primary
record and remains `record=None` on the wire. Writer uses ordinary checked records.
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
node --test crates/browser/qos-browser-runtime/web/host.test.mjs
cargo test --locked -j2 -p ui --test it package_primary
cargo test --locked -j2 -p word-processor --test it canonical_component
cargo test --locked -j2 -p chat --lib --no-default-features component_tests
```
