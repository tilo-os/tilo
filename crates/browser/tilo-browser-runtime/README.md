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
requires an admitted certified peer or `local` (below). An id the build does not contain rejects the
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

### Opening a file

`open` hands the app a file as it would get one the user chose; the app lists
what it opens as `application.opens`. Books shelves an EPUB in the record's
library and opens it, and reports its resource, so a page never computes a
content id. Another mount of the record needs no `open`: it shows the book
last opened, with the same annotations.

```js
const phone = await mountDesktop(phoneEl, {app: 'so.tilo.ebook', newDocument: true,
  open: {url: '/books/the-odyssey.epub'}, ...shared});
const book = phone.opened.resource;   // 'ebook.book:book:<32 hex>'
const laptop = await mountDesktop(laptopEl, {app: 'so.tilo.ebook', record: phone.record, ...shared});
// Each mount reads at its own position: open both at Book I without selecting.
for (const mount of [phone, laptop]) mount.invokeAction(book, 'go-to', {chapter: 4, block: 3});
phone.invokeAction(book, 'select-passage', {chapter: 4, start_block: 3, end_block: 3});
```

Refusals reject the mount with `code` `TILO_OPEN_INVALID`,
`TILO_OPEN_UNSUPPORTED` or `TILO_OPEN_UNAVAILABLE` (WEB_RUNTIME.md).

### A local desktop

`mountDesktop(el, {app: 'desktop', local})` mounts the canonical desktop shell
(strips, workspaces, top bar, launcher, search, action palette, Activity) over
room documents the page seeds and alone holds: no peer, nothing replicated. This
mount is their topology owner, so the commands the shell binds (open, close,
reorder, move, split, strips, renames) are reduced at once with the same command
reduction and projection a workspace host runs. Each workspace is its own room;
the first is the mount's `room`. Windows name app ids or aliases; the host links
apps the core lacks through the resolver first, as for a standalone mount.

A window with `record` shows that record of the app in the mount's room, such
as one a standalone mount of the same profile, device and room opened (a Books
library with its book and annotations); both mounts edit one document and show
each other's changes live. A window without `record`, and every window opened
later, gets a new record from the app itself. `apps` lists more apps to link,
so the launcher, search and palette offer them.

```js
const shared = {profile: 'demo', device: 'visitor', room: ROOM, author: 'Visitor'};
const phone = await mountDesktop(phoneEl, {app: 'so.tilo.ebook', newDocument: true,
  open: {url: '/books/the-odyssey.epub'}, ...shared});
const book = phone.opened.resource;
await phone.cleanup();                       // the phone "expands" into the desktop
const desktop = await mountDesktop(desktopEl, {app: 'desktop', ...shared, local: {
  apps: ['so.tilo.word-processor'],
  workspaces: [
    {title: 'Reading', strips: [{title: 'Odyssey', windows: [
      {app: 'so.tilo.ebook', record: phone.record, focused: true}]}]},
    {title: 'Writing'},
  ]}});
const nav = desktop.navigation;              // DesktopNavigationV1
desktop.invokeAction(book, 'go-to', {chapter: 4, block: 3});    // the focused window's app
await desktop.openApp('so.tilo.word-processor');                 // a new, selected Writer window
const {generation, workspaces} = nav.getSnapshot();
await nav.dispatch({kind: 'workspace', generation, workspace: workspaces[1].id});
```

`actions(resource, {window})` and `invokeAction(resource, action, args,
{confirm, window})` address one window's app: `window` is its tab id (a route's
`tab`), by default the focused window. The snapshot lists every window
(`entries`), workspace and strip, the focused route (`active`) and the linked
apps (`applications`); `dispatch` selects windows, workspaces and strips, sets
width and maximization, pans, closes, opens (`{kind: 'open', app}`), and opens
the focused document's history (`{kind: 'history', route: active, target:
history.target}`) or the workspace's Activity (`{kind: 'workspace-activity',
workspace, view}`).

`{kind: 'workspace-settings', workspace, tab}` opens the workspace settings
(`general`, `members`, `agents`, `permissions`, `apps`) and
`{kind: 'close-workspace-settings', workspace}` closes them; the snapshot's
`workspaceSettings` reports `{workspace, tab}` while they are open. A
workspace's `members` (`{name, access, agent}`) and `grants` (`{app, access,
over}`) seed what its People, Agents and Privacy tabs list; they are shown,
not enforced (the page keeps owner rights). Workspace history is the
Activity pane over the room's real changes and the windows' records; a
document's own history is the app's (Writer: `invokeAction('word-processor.document',
'history', {}, {window})` opens its version strip, and its
`word-processor.revision:<hash>#show` resources name each revision).

The rooms live as long as the mount; records persist in the profile's storage
like a standalone mount's.

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
node --test crates/browser/tilo-browser-runtime/web/open-file.test.mjs
cargo test --locked -j2 -p tilo-browser-runtime --lib catalog::
cargo test --locked -j2 -p tilo-browser-runtime --lib local_desktop::
cargo test --locked -j2 -p ebook --lib component::
cargo test --locked -j2 -p ui --test it package_primary
cargo test --locked -j2 -p word-processor --test it canonical_component
cargo test --locked -j2 -p chat --lib --no-default-features component_tests
```
