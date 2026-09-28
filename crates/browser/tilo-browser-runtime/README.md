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

This is the Cloud frontend's monolithic module (`--features cloud,static-apps`).
The default build of the crate is the web toolkit core, which loads apps as
side modules at run time; see [WEB_RUNTIME.md](WEB_RUNTIME.md).

Serve the generated directory from a secure origin with WebGPU. `index.html` is
an executable diagnostic fixture. Production hosts import `host.mjs`; `host.d.ts`
describes its API. The build copies canonical product fonts, manifest and licenses.
The development WASM is intentionally unoptimized; production packaging is separate.

## Applications

`src/catalog.rs` is the one list of installed component apps and the
`ApplicationCatalog` both hosts resolve through: `mountDesktop({app})` for a
standalone app and the canonical desktop for its tabs. An app exposes
`definition()` (see `crates/sdk/ui-core/src/application.rs`) and builds for
wasm32; it is installed either compiled in (a line in `COMPILED`, with
`static-apps`) or at run time from its side module (WEB_RUNTIME.md).

`app` is a full app id (`so.tilo.word-processor`, `so.tilo.ebook`, ...), one of
the aliases `writer` and `chat`, or `desktop` for the canonical desktop, which
requires an admitted certified peer. An id the build does not contain rejects the
mount and reports `application "..." is not available` through `onStatus`; no
placeholder is mounted. `applications()` lists what the build contains, and a
mount's `application` names the resolved id, title and record scope.

### One record, several mounts

Mounts in a page with the same profile, device, room and app edit one in-memory
document of a record. A mount's `record` is the record it opened or created,
available once `mountDesktop` resolves; pass it as another mount's `record` to
show the same document. When one mount changes it, the others re-run their frame
passes and present the change without a reload.

```js
const phone = await mountDesktop(phoneEl, {app: 'so.tilo.ebook', newDocument: true, ...shared});
const laptop = await mountDesktop(laptopEl, {app: 'so.tilo.ebook', record: phone.record, ...shared});
```

### Scripting a mount

`actions(resource?)` lists what the mounted app publishes, described as
`tilo-control actions list` describes it, and `invokeAction(resource, action,
args, {confirm})` runs one through the app's bound action door: the same
availability, permission, argument binding and confirmation checks the palette,
agents and assistive technology get. There is no coordinate or widget path.
`resource` may name a resource the app resolves only on request, such as a book.

Books (`so.tilo.ebook`) in the browser opens a new library on its bundled book,
The Lantern-Keeper's Almanac, whose resource is its content id:

```js
const book = 'ebook.book:book:1fe2b755ca3ff5d7fba5764b70b74edb';
// Chapter 1 paragraphs 1-2 (block 0 is the chapter heading); end_char is exclusive.
mount.invokeAction(book, 'select-passage', {chapter: 0, start_block: 1, end_block: 2});
mount.invokeAction(book, 'highlight-selection');
mount.invokeAction(book, 'note-selection', {value: 'Why before the gulls wake?'});
// Annotations are the book's child resources: kind and the note text or passage.
mount.actions(book).filter(a => a.address.action === 'open' && a.address.resource !== book)
  .map(a => ({kind: a.resource_kind, text: a.resource_label}));
```

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
cargo test --locked -j2 -p ebook --lib component::
cargo test --locked -j2 -p ui --test it package_primary
cargo test --locked -j2 -p word-processor --test it canonical_component
cargo test --locked -j2 -p chat --lib --no-default-features component_tests
```
