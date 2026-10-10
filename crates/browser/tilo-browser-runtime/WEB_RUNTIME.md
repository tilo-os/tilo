# Web runtime: the toolkit core and app side modules

The browser's equivalent of `runtime-dylib` and thin apps. Natively, a thin app
links the toolkit and std out of the shared object in the signed
`tilo-runtime/2026.07` bundle. In the browser, the **web core** is a WebAssembly
main module holding the toolkit, the page host and the application catalogue,
and each app is a small **side module** linked into it at run time.

App authors do nothing web-specific. A component app that exposes
`definition()` (`src/component.rs`), builds for `wasm32-unknown-unknown`, uses
no wasm-bindgen or web-sys itself, and declares
`runtime = "tilo-runtime/2026.07"` in `tilo-app.toml` builds as a side module
unchanged.

## Building

```sh
# Core + Books + Writer + Chat + test page in one directory (local development):
crates/browser/tilo-browser-runtime/scripts/build-web-dev.sh /abs/out [PACKAGE...]
python3 -m http.server -d /abs/out   # then open it in a browser with WebGPU

# The steps it runs:
crates/browser/tilo-browser-runtime/scripts/build-web-core.sh /abs/out [--features "..."]
crates/browser/tilo-browser-runtime/scripts/build-web-app.sh /abs/out ebook
crates/browser/tilo-browser-runtime/scripts/build-web-app.sh /abs/out word-processor
crates/browser/tilo-browser-runtime/scripts/build-web-app.sh /abs/out chat
```

Tools: the pinned Rust toolchain with `wasm32-unknown-unknown`, wasm-bindgen
0.2.108, binaryen (`wasm-opt`), lld (`wasm-ld`), Node.js 20+.

All steps share `scripts/web-runtime/env.sh`: every crate is compiled with
`-C relocation-model=pic` under the `release` profile with LTO off
(`CARGO_PROFILE_RELEASE_LTO=false`: it would internalise the symbols apps
import), no debug info, and `panic = "abort"`. The core and every app must be
built with exactly these settings.

The Cloud frontend's monolithic module is unchanged in behaviour:
`scripts/build.sh` builds with `--features cloud,static-apps`.

## Cargo features of `tilo-browser-runtime`

| feature | adds |
|---|---|
| (none, the default) | the toolkit core: toolkit, page host, catalogue, the canonical desktop over page-owned rooms (`local`); no apps, no peer |
| `dylink` | links the cdylib as a dynamic-linking main module (see below) |
| `static-apps` | compiles Writer, Chat, Notes, Calculator and Books into the module |
| `cloud` | the certified peer (HTTPS delivery, onboarding, invited guests), MoQ media, the canonical desktop |

Host tests (`cargo test -p tilo-browser-runtime`) always cover the peer modules
and the catalogue's apps.

`cloud` adds no dependency: every crate the peer uses is already in the core.
Since the ABI id leaves the core crate out, `build-web-core.sh OUT --features
cloud` builds a core with the peer (`connectPeer` in `peer.mjs`) that has the
default core's ABI id and links the same app side modules, from the same
registry. A page that needs the peer loads that core instead of the default
one, not a second runtime.

## The web app collection

`runtime-dylib` must be the superset of what every thin app in the default
collection needs; a crate built with other features is another crate with other
symbol names. The web core follows the same rule: it is built in one Cargo
invocation with the apps of the web collection (`TILO_WEB_COLLECTION`, default
`ebook word-processor chat`; recorded in `runtime.json`), so each crate they share
with the core carries every feature any of them needs. An app outside the
collection builds as long as it needs nothing more; otherwise
`build-web-app.sh` names the crates and it joins the collection, which changes
the runtime ABI id. `cargo tree` for each root separates the core's crates from
the app's own.

## The core

`build-web-core.sh` builds `-p tilo-browser-runtime --features dylink` (with
the collection, as above):

- Everything is compiled at `opt-level = "z"` (`TILO_WEB_OPT_LEVEL`), which
  also makes rustc share generic instantiations across crates: an app links
  against the core's copy of, say, `Vec<u32>::dedup_by` instead of carrying
  its own.
- `build.rs` passes `--export-table --growable-table --export=__stack_pointer`
  (and `--export-all`, for a core linked without these scripts). The core is
  non-PIE; std is shared (apps import `__rust_alloc` and friends).
- `scripts/web-runtime/tilo-wasm-ld` is rustc's linker for it: the objects of
  the toolkit crates (`tilo_app`, `tilo_ui`, `tilo_resource`, `tilo_actions`)
  are linked whole, so an app may call any of their functions, not only those
  the core itself uses (the superset rule `runtime-dylib` states for native
  thin apps). It keeps the link's inputs, and the script links the core again
  with an explicit export list instead of `--export-all`: every function and
  datum the toolkit crates define for other crates (their public surface),
  plus every symbol the collection's apps import from any other crate (found
  by linking each of them first). Everything else is dead-code eliminated;
  `--export-all` kept every function of every linked object alive. At
  `opt-level = "z"` the toolkit crates also export every instantiation of a
  foreign generic they made; those are left out unless the collection imports
  them (a third of the core). `TILO_WEB_EXPORT_GENERICS=1` keeps them all, for
  a core serving apps outside the collection that reuse them; `runtime.json`
  records which (`exports`). A third-party app that needs a symbol the core
  does not export is refused at build time with the symbol named, and joins
  the collection (or the core keeps the instantiations).
- `src/side_module.rs` exports `tilo_dl_alloc` (memory for a side module's
  data) and `tilo_runtime_register_app(&'static ApplicationDefinition)`.
- The mangled wasm-bindgen internals the toolkit surface exports (items of the
  bindgen crates, `WasmDescribe` impls) are removed
  (`wasm-tools.mjs strip-bindgen-internals`); a generic of another crate that
  was merely instantiated in `wasm_bindgen` stays, since apps link against it.
  PIC code takes a function's address through a constant GOT global, which
  wasm-bindgen's descriptor interpreter cannot follow, so
  `wasm-opt --simplify-globals --precompute` folds those first. Then
  `wasm-bindgen --keep-lld-exports`, `wasm-opt -Oz`, and the ABI id section.
- The glue is made self-contained (`bundle-glue.mjs` inlines its
  `snippets/`), so it can also run from a verified object URL.
- `build/core-profile.txt` lists size by crate (code and data) and the
  largest functions and data, from a second link with one data segment per
  symbol.

What the browser toolkit leaves out of tilo-ui, kept natively:

- ICU's dictionary/LSTM segmentation models (about 2 MB, nearly
  incompressible): in wasm32 lines and words break by the Unicode rules alone,
  so Thai, Lao, Khmer and Myanmar lose dictionary word boundaries.
- Hyphenation patterns other than English and German (about 1 MB).
- two-face's code-editor grammars (0.9 MB, feature `syntax-definitions`,
  implied by `native`): code blocks show as plain text.

## Runtime ABI id

Symbol names do not reveal an ABI break: a changed trait still links and traps
at run time. So every module carries the id of the core it was built against,
and the loader refuses a mismatch before instantiating anything.

The id is `tilo-runtime/2026.07+wasm.<32 hex>`: the native runtime name
`tilo-app.toml` declares, plus 128 bits of a SHA-256 (`scripts/web-runtime/abi.mjs`)
over:

- `rustc -vV` (compiler commit and LLVM version),
- RUSTFLAGS and `rustc --print cfg` for the target with `panic=abort` (target
  features, panic strategy),
- the `release` profile as declared, and the `CARGO_PROFILE_RELEASE_*`
  overrides,
- the `Cargo.lock` digest,
- every unit the core links except `tilo-browser-runtime` itself: package,
  features and Cargo's metadata hash (package, features, profile, dependency
  graph; independent of the checkout path), plus for workspace crates a digest
  of the source files rustc read (from dep-info).

The core crate is left out on purpose: apps reach it only through
`tilo_runtime_register_app`, whose contract is versioned by `REGISTRATION` in
`abi.mjs`. Changing the page host therefore keeps existing app modules valid;
changing the toolkit, a dependency, the lock file or the compiler does not.

The id is stored as the custom section `tilo.runtime_abi` in the core
(`pkg/tilo_browser_runtime_bg.wasm`) and in every `app.wasm`, and in
`runtime.json` and each `app.json`. `web/dylink.mjs` reads both sections with
`WebAssembly.Module.customSections` and throws `RuntimeAbiMismatch`
(`code: 'TILO_RUNTIME_ABI_MISMATCH'`) when they differ.

| native | web |
|---|---|
| `runtime = "tilo-runtime/2026.07"` in `tilo-app.toml` | same; `build-web-app.sh` refuses an app naming another runtime |
| signed runtime bundle `tilo-runtime/2026.07` | web core `tilo-runtime/2026.07+wasm.<id>` (`runtime.json`) |
| `libruntime_dylib.so`, crate hashes in symbol names | core main module, ABI id section checked before linking |
| thin app linked with `-C prefer-dynamic` | `app.wasm` side module carrying the same ABI id |

## App side modules

`build-web-app.sh OUT PACKAGE`:

1. Reads the app id and runtime from the package's `tilo-app.toml`.
2. Builds the app in the same Cargo invocation as the core: the core, the web
   app collection and the app. A shared crate whose Cargo metadata hash differs
   from the core's means the app enables a feature the core lacks: the build
   stops and names it. The core's ABI id is recomputed from this build and must
   equal the core's (its sources must not have changed since).
3. Compiles a generated shim, the only generated code:
   `tilo_app_register()` calls `tilo_runtime_register_app(<crate>::definition())`.
4. Links the shim with every object of the crates in the app's graph that the
   core does not contain (for Books: `ebook`, `roxmltree`, `bounded-zip`, …):
   `wasm-ld --experimental-pic -shared --unresolved-symbols=import-dynamic
   -Bsymbolic --no-export-dynamic --export=tilo_app_register`. Crates with
   native libraries are refused.
5. Checks that every import is exported by the core (and none is a
   wasm-bindgen import), runs `wasm-opt -Oz`, and adds the ABI id section.

Output: `apps/<app id>/app.wasm`, `apps/<app id>/app.json`
(`{id, title, package, runtime, abi, module: {path, bytes, sha256}, assets}`)
and the app listed in `apps/index.json`. Static assets are the files under the
app's `assets/web/`, stored content-addressed as `assets/<sha256>.<ext>` and
named by their path below that directory (neither Books nor Writer has any; both
embed their data).

## Apps outside the Tilo tree

`tilo-app build --platform web` (and `dev`, `run`) builds the app's bundle,
as `build --target web --bundle app` does: `wasm32` has no dynamic linking,
so the runtime and the app are one module. A small generated crate under
`<target>/tilo-app/web/<package>/crate` depends on `tilo-browser-runtime` and
the app, registers the app's `definition()` at start, and builds into the app
workspace's target directory as its own Cargo workspace (with that
workspace's `[patch]`, profiles and lock file), so an app anywhere builds
against the SDK checkout it names. The bundle lands in
`<target>/tilo-app/web/<package>/bundle` in `build.sh`'s layout and opens at
`index.html?app=<id>`; `--output` copies it. `package --target web` writes
it in the immutable layout Cloud serves. There is no side-module web
package: this runtime has no separately published core to link one into.

## Loading apps in the page

```js
import {mountDesktop, preloadApps, setAppResolver} from './host.mjs';

// Any function with the registry client's resolveApp shape:
//   (appId, {signal}) => {module: WebAssembly.Module | Response | Uint8Array, abi, assets: Map}
setAppResolver(registry.resolveApp);

// Session restore: every app it will open, in one concurrent batch that runs
// alongside the core's own download. Rejects with an AggregateError
// (`failed`: Map of app id to error, `installed`: the rest).
await preloadApps(['so.tilo.ebook', 'so.tilo.word-processor']);

// A mount of an app the core lacks resolves and links it first (per-mount
// `resolveApp` overrides the default). onStatus reports resolving, linking,
// linked; an unresolvable app reports failed and rejects with
// code TILO_APP_UNAVAILABLE, a stale module with TILO_RUNTIME_ABI_MISMATCH.
const mount = await mountDesktop(element, {app: 'so.tilo.ebook', resolveApp});
```

- `web/dylink.mjs`: the loader. It reserves the module's data through
  `tilo_dl_alloc`, gives it a slice of the core's table, fills its GOT from the
  core's exports, instantiates it, then applies its data relocations.
- `web/app-installer.mjs`: resolution (started immediately), compilation
  (streaming for an `application/wasm` Response), linking once the core is
  ready, registration, deduplication of concurrent requests, retry after a
  failure. A compiled-in app never consults the resolver.
- `web/app-directory.mjs`: a resolver over a directory laid out as above, for
  local development without a registry (`build-web-dev.sh`'s `index.html`).
  It checks the module against `app.json`'s size and SHA-256; it verifies no
  signature.

- `web/core-loader.mjs`: compiles the core module itself, reads its ABI id
  from that module and instantiates it through the glue's
  `init({module_or_path})`; nothing depends on glue internals.

By default the core is `pkg/` next to `host.mjs`. A page that fetched and
verified a core elsewhere runs it instead with `configureRuntime({glue,
module})` before its first mount: `glue` is the self-contained glue (its
namespace, or a URL such as the object URL of a verified asset; `blob:` URLs
are re-typed as JavaScript), `module` the core module; either may be a
promise, so the core and the apps can be fetched in one batch.
`runtimeAbiId()` reports the loaded core's id.

## Handing an app a file to open

A page opens a file in an app the way an OS file association does: it hands
the bytes over as an explicit launch input, and the app decides what to do
with them. Apps have no ambient filesystem and never see the URL.

```js
const phone = await mountDesktop(phoneElement, {
  app: 'so.tilo.ebook',
  newDocument: true,
  open: {url: '/books/homer_the-odyssey_william-cullen-bryant.epub'},
  onStatus: status => { if (status.opened) console.log(status.opened); },
});
phone.opened;   // {name, mediaType: 'application/epub+zip', resource: 'ebook.book:book:<32 hex>'}
// A second mount of the same record needs no `open`: Books keeps the book in
// its library and reopens the one last opened.
const laptop = await mountDesktop(laptopElement, {app: 'so.tilo.ebook', record: phone.record, instance: 2});
phone.invokeAction(phone.opened.resource, 'select-passage', {chapter: 3, start_block: 1, end_block: 2});
phone.invokeAction(phone.opened.resource, 'highlight-selection');
```

- `open` is `{url, name?, mediaType?}` or `{bytes, name, mediaType?}`. The
  bytes of a URL come through the same admitted path as an app's assets:
  the mount's `loadAsset` when it has one, else a same-origin fetch. `name`
  defaults to the URL's last segment and `mediaType` to what its extension
  implies (`.epub`); at most 128 MiB (`MAX_OPEN_FILE_BYTES`).
- An app declares the media types it opens (`ApplicationDefinition::opens`,
  listed as `opens` by `applications()` and `mount.application`). Any other
  type is refused before a byte is fetched, with `code:
  'TILO_OPEN_UNSUPPORTED'`; a malformed option with `'TILO_OPEN_INVALID'`, a
  failed fetch with `'TILO_OPEN_UNAVAILABLE'`. Each also reports
  `{phase: 'failed', error}` through `onStatus`. When the app cannot read
  the bytes the launch fails with its reason (Books: `Couldn’t open
  NAME: …`).
- The runtime hands the component an `OpenFile` through
  `HostContext::provide_open_file`; the factory takes it with
  `take_open_file` and reports the resource that now holds it
  (`OpenFile::report_opened`), which the page reads as `mount.opened` and as
  an `{opened}` status before `gpu-pending`.
- Every standalone mount of one profile/device (and every window of a local
  desktop of it) also shares a content store
  (`HostContext::content`, a `tilo_content::ContentStore`) for the page's
  lifetime, as it shares records. Books puts an opened EPUB there and records
  it in its library row by content hash, exactly as it records an imported
  file: the row (`ebook.book:book:<first 32 hex of BLAKE3>`), progress and
  annotations are in the record, so the other mount shows the same book and
  its annotations live. The store is memory only: after a reload the page
  hands the file over again (the row, progress and annotations persist).
- Books' `select-passage` takes `chapter` (0-based spine document; the open
  chapter when omitted), `start_block`/`end_block` (0-based blocks of that
  chapter: headings and paragraphs, so a Standard Ebooks chapter's first
  paragraph is usually block 1) and optional character offsets.
- Books' `go-to` takes `chapter` and `block` (the same indices; each
  defaults as above, `block` to 0) and opens that mount's reader at the page
  holding the block, selecting nothing and clearing any selection, as
  showing an annotation does. Each mount has its own position, so call it on
  every mount: `mount.invokeAction(book, 'go-to', {chapter: 4, block: 3})`.
  A chapter or block the book does not have is refused.

`open.html` in the dev output exercises this with two mounts (see
[Building](#building)); put an EPUB next to it as `odyssey.epub`, or pass
`?open=URL`, or pick a file.

## The page's own desktop

`mountDesktop(element, {app: 'desktop', local})` runs the canonical desktop
(`tilo_desktop::composition::DesktopSurface` and its chrome, as the Cloud
frontend does) without a peer. `src/desktop.rs` runs `tilo-desktop-surface`
over a `tilo-desktop-model` tree, every tab an in-process application. The
tree and the record each tab shows are kept per room in the profile's
storage and committed with the records themselves, so a reload restores the
same windows on the same documents; a profile with no stored desktop is
seeded with a window per requested application.

- Windows are resolved through the same catalogue and resolver as standalone
  mounts: the host links every seeded app (and `local.apps`) before mounting,
  and an app `openApp`/`{kind: 'open'}` names before opening it. The
  launcher, search and palette offer the linked apps.
- A window with `record` opens that record (it must exist in the mount's room,
  as a standalone mount's `record` does). Every other window is one the owner
  created: its app makes a new record when the window first mounts
  (`HostContext::prepare_new_record`, the app's own initialiser); no other
  missing record is ever created.
- The page's mounts of one record re-sync each other, standalone or desktop
  window, so a Books library opened on a "phone" mount shows its book and
  annotations live in the desktop's window.
- Each window's app and record are in the navigation snapshot's `entries`
  (`{route, label, app, record}`; `record` is null for a package-primary
  app), and `openApp(app, {record})` (`{kind: 'open', app, record}`) opens a
  new window on an existing record. So a page can mount another author on a
  window's document, say an agent's hidden Writer mount on the notes the
  desktop's Writer shows:

  ```js
  await desktop.openApp('so.tilo.word-processor');
  const notes = desktop.navigation.getSnapshot().entries.at(-1).record;
  const agent = await mountDesktop(hidden, {app: 'so.tilo.word-processor', record: notes, author: 'Reading agent', ...shared});
  ```
- Activity names every change: the page's mounts of one profile and device
  share each record's document, and each writes its edits to it as its
  `author`'s own actor, so edits another mount made (the phone's Books, the
  agent's Writer and Chat) show under that mount's `author`, as an agent
  when the mount is an agent mount (`agent`) or the author is one of the
  workspace's agent `members`. Rows show the name; one author's changes
  count as one person, and only people history can name are offered as
  filters. Room changes carry the time the owner made them; a record's
  genesis (made at time zero, before anyone edited it; a package-primary
  document such as Chat's included) is left out of Activity, as nobody made
  it (the document's own history keeps it as the version to restore to).
  Housekeeping an app declares about a document, such as Chat's read marker
  or Books' reading position, stays in the document's own history but is
  not workspace activity.
- `actions`/`invokeAction` take `{window}` (a tab id; the focused window by
  default) and go through that window's action door.
- Permissions are the page author's, as for a standalone mount (`Owner`).
  A workspace's seeded `members` and `grants` become its
  `Workspace::capabilities`, which the workspace settings (People, Agents,
  Privacy) render read-only; nothing enforces them in this peer.
- The desktop's tile catalogue follows the page's installs, so an app linked
  after the mount opens in a window like one linked before it.

## Chat with an agent

Chat (`so.tilo.chat`) is package-primary: every mount of one
profile/device/room opens the room's one workspace, so two mounts are two
participants in the same conversation and show each other's messages live.
The first mount creates the workspace (its author owns it), so mount the
person first. A mount with `agent` acts as an agent: its `author` must be an
agent task participant, `agent-task:<task>`, which the app admits under the
display name, as a host admits a delivered agent task.

```js
const room = {profile: 'demo', device: 'demo-device', room: '0b5f1c2e-9d7a-4c55-8a1e-3f1d2c4b5a69'};
const you = await mountDesktop(youElement, {app: 'so.tilo.chat', ...room, author: 'Visitor', instance: 1});
const agent = await mountDesktop(agentElement, {app: 'so.tilo.chat', ...room, instance: 2,
  author: 'agent-task:reading', agent: 'Reading agent'});

const general = 'chat.channel:c_general';   // the starter workspace's channel
// Post from either mount; the message is that mount's author's.
agent.invokeAction(general, 'post', {value: 'Welcome aboard. Which book are we reading?'});

// Messages: a channel asked for by name lists its latest 20 messages as
// children, kind `chat.message`, label `<author name>: <first 72 characters>`.
const seen = new Set();
function newMessages(mount) {
  const ids = [...new Set(mount.actions(general)
    .filter(a => a.resource_kind === 'chat.message').map(a => a.address.resource))];
  return ids.filter(id => !seen.has(id) && seen.add(id))
    .map(id => mount.invokeAction(id, 'read').output.value);
}
// Each: {id, channel, parent, author, authorName, agent, mine, ts, edited, text, work}
for (const message of newMessages(agent)) {
  if (message.agent) continue;                 // the agent's own lines
  const reply = await answer(message.text);    // the page's model call
  agent.invokeAction(general, 'post', {value: reply});
}

// The agent's record of what it did, on its own message (the newest here):
agent.invokeAction(replyId, 'set-work', {state: 'done', did: 'Add note',
  steps: [{resource: book, label: 'Add note', outcome: 'applied'}]});
```

- `post` takes `{value}` (a prompted action; `@Name` of a member becomes a
  mention). An agent mount's post is recorded as the agent's
  (`agent_authors/<author>` in the document), so every mount shows it as
  `Reading agent · agent`; its own mount shows "You". The agent is not a row
  in the People panel.
- `read` on `chat.message:<id>` returns the message without changing
  anything: `author` is the participant id (the mount's `author`),
  `authorName` what the room calls them, `agent` whether it is an agent's,
  `mine` whether this mount wrote it, `text` the whole plain text, `parent`
  the thread it replies to or null, and `work` the agent's record or null.
  The eager `actions()` listing holds no messages (it stays bounded); only
  `actions('chat.channel:<id>')` and `actions('chat.message:<id>')` do.
- `set-work` is offered on an agent mount's own messages only. `state` is
  `taken`, `doing` (with `did`, `about`), `done` (with `did`) or `refused`;
  `why` is how it decided; `steps` lists the operations it dispatched as
  `{resource, label, outcome: asked | applied | no_change | refused, why?}`;
  `say` supplies the words of a message that has none yet. It writes the
  same `SetAgentWork` a delivered agent task writes, read back as `work`.
  Chat's timeline does not draw the work yet; a page shows it itself.

## Loading from a registry

The registry client (`crates/browser/web-registry-client`) verifies the
snapshot, tapps and blobs against a pinned owner key and caches them in
Cache Storage; `resolveRuntime()` returns the core as `{module, abi, assets}`
with the glue among the assets, and `resolveApp` plugs into `setAppResolver`.
`registry.html` in the dev output does exactly that: the core and the session
(`?apps=a,b`, default every published app) in one registry batch, then mounts
the first app. A returning visit costs one snapshot request (304 when
unchanged); everything else comes from Cache Storage. Fonts still come from
the page's directory.

Locally, in WSL (three terminals; page and registry are different origins):

```sh
crates/browser/tilo-browser-runtime/scripts/build-web-dev.sh ~/tb-scratch/out
# a fresh signed registry on http://127.0.0.1:8787 with the core and every app
# of the build, CORS for the page's origin; writes ~/tb-scratch/out/registry.json
tools/web/local-registry.sh --origin http://localhost:4321 ~/tb-scratch/out
python3 -m http.server 4321 --bind 127.0.0.1 -d ~/tb-scratch/out
```

Then open `http://localhost:4321/registry.html` in Chrome on Windows (WSL
forwards localhost). `http://localhost:4321/` is the same core and apps from
the directory. Each registry run makes new keys, so reload the page after
restarting it.

## Fonts

`scripts/web-runtime/fonts.mjs` publishes the product fonts content-addressed
as `fonts/<sha256>.<ext>`; `fonts.json` maps the name Rust demands to that
path, with size and SHA-256 (`font-loader.mjs` verifies both). The five
`browser_startup` fonts are the mount barrier; everything else (Noto Sans CJK,
32.7 MB; emoji; math; symbols; Arabic, Hebrew, Devanagari, Bengali, Thai) is
fetched only when text demands it, or during idle prewarm within its budget.

## Sizes

The canonical desktop over page-owned rooms (`local`) costs the core
1.23 MiB raw and 311 KiB brotli (`-Oz`: 18.27 MiB / 4.09 MiB before, 19.50
MiB / 4.40 MiB after, measured 2026-09-28): the shell itself
(`tilo_desktop`, 358 KiB linked; `tilo_desktop_model`, `tilo_automation`,
`collab_types`), which the core linked but dead-stripped before.

`build-web-dev.sh` writes `build/sizes.md`. Measured 2026-09-28 (collection
Books + Writer + Chat, no `cloud`):

| module | linked | linked, brotli | `-Oz` | `-Oz`, brotli |
|---|---:|---:|---:|---:|
| core | 36.60 MiB | 5.70 MiB | 18.82 MiB | 4.42 MiB |
| Books (`so.tilo.ebook`) | 2.69 MiB | 468 KiB | 1.49 MiB | 414 KiB |
| Writer (`so.tilo.word-processor`) | 5.39 MiB | 704 KiB | 2.21 MiB | 625 KiB |
| Chat (`so.tilo.chat`) | 2.56 MiB | 423 KiB | 1.39 MiB | 370 KiB |

Chat joining the collection grows the core from 18.27 MiB / 4.09 MiB (`-Oz`,
brotli) to 18.82 MiB / 4.42 MiB: mostly the emoji tables (`crates/text/emoji`,
in the core through `tilo-desktop` but dead code until Chat's picker imports
them), and the generic instantiations Chat imports.

| fonts | count | raw | brotli |
|---|---:|---:|---:|
| eager (mount barrier) | 5 | 3.51 MiB | 1.43 MiB |
| lazy (on text demand) | 10 | 38.28 MiB | 16.18 MiB |

The linked core still carries its name and debug sections, which `-Oz` drops.
How the core got there from 41.3 MiB / 12.0 MiB (`-Oz`, brotli):

| step | `-Oz` | brotli |
|---|---:|---:|
| `--export-all`, `opt-level = 3` | 41.3 MiB | 12.0 MiB |
| export the toolkit surface and the collection's imports | 24.2 MiB | 7.6 MiB |
| no ICU models, two hyphenation languages, no grammars; `opt-level = "z"` | 24.0 MiB | 3.85 MiB |
| toolkit items only, not their instantiations of foreign generics | 16.0 MiB | 3.59 MiB |

`opt-level = "z"` alone makes the core slightly larger raw but lets apps reuse
the core's generic instantiations (Books 1.31 MiB to 849 KiB). For comparison,
the same crates linked as an ordinary cdylib with no dynamic linking at all
come to 15.5 MB / 5.2 MB at `opt-level = 3`.

Largest contributors to the linked core (`build/core-profile.txt`; generics
count toward the crate that defines the item, so `core`/`alloc` are standard
library code instantiated for toolkit types; "other" is anonymous data: the
spell-check word list Writer needs, 691 KiB, hyphenation patterns, vello's
shaders):

| # | crate | KiB | | # | crate | KiB |
|---:|---|---:|---|---:|---|---:|
| 1 | other (data) | 3662 | | 11 | hexane | 381 |
| 2 | tilo_ui | 3161 | | 12 | harfrust | 327 |
| 3 | core | 1511 | | 13 | regex_automata | 283 |
| 4 | alloc | 1401 | | 14 | std | 273 |
| 5 | read_fonts | 758 | | 15 | serde | 268 |
| 6 | taffy | 652 | | 16 | tilo_actions | 163 |
| 7 | skrifa | 511 | | 17 | image | 131 |
| 8 | automerge | 428 | | 18 | zune_jpeg | 107 |
| 9 | hashbrown | 418 | | 19 | image_webp | 103 |
| 10 | serde_json | 387 | | 20 | regex_syntax | 102 |

## Collaboration end to end

`crates/browser/tilo-web-desktop/scripts/collab-e2e.mjs` puts native macOS
apps, headless Chrome pages and a host agent in one room of a local
`tilo-relay dev` host on spare ports, and checks what a person would see: the
same record on every side; edits native to browser, browser to native and
browser to browser, with the time until the other side shows them; named
cursors and carets both ways; a page that joins late; an agent's edit and
cursor on native and in the pages; and a reloaded page that keeps its device
and room membership. Per app it runs the native shell with `TILO_COLLAB_HOST`
under a throwaway runtime profile that carries the dev host's user root, two
pages, then a third, and (with `--agents --agent-model scripted`) an agent
spawned from a page. It writes `summary.json` and `summary.md` (a check-by-app
table) under `--out` and stops everything it started.

```sh
cargo build --release -p tilo-relay --features web,agents
cargo build -p shell --bin tilo-runtime-mcp -p notes -p sheets -p scratchpad
crates/browser/tilo-browser-runtime/scripts/build.sh /abs/bundle --bundle desktop
node crates/browser/tilo-web-desktop/scripts/collab-e2e.mjs --bundle /abs/bundle \
  --apps notes,sheets,scratchpad --out /abs/e2e
```

`--build` runs those builds itself (the bundle with `--profile dev` into
`OUT/bundle` when `--bundle` is not given); `--no-agent` leaves the agent out
for a relay without `agents`. The native window is read and edited from
outside the process through the macOS accessibility API
(`collab-e2e-ax.swift`, compiled with `swiftc` on first use), so the terminal
running the script needs Accessibility permission; the native pointer moves
through a `TILO_NATIVE_SCENARIO` timeline. Native presence from the pages is
read from the runtime daemon's log. The screen must stay unlocked for the whole
run: a locked screen presents no native window, so the script refuses to start
on one. Each page's channel state (`peer.channelDiagnostics()`) is saved as
`*-channels.txt`. macOS only.

## Tests

```sh
node --test crates/browser/tilo-browser-runtime/web/dylink.test.mjs \
  crates/browser/tilo-browser-runtime/web/core-loader.test.mjs \
  crates/browser/tilo-browser-runtime/web/app-installer.test.mjs \
  crates/browser/tilo-browser-runtime/web/app-directory.test.mjs \
  crates/browser/tilo-browser-runtime/web/open-file.test.mjs \
  crates/browser/tilo-browser-runtime/scripts/web-runtime/bundle-glue.test.mjs \
  crates/browser/tilo-browser-runtime/scripts/web-runtime/abi.test.mjs \
  crates/browser/tilo-browser-runtime/scripts/web-runtime/external-workspace.test.mjs
cargo test --locked -j4 -p tilo-browser-runtime --lib catalog::
# the wasm32 type check CI runs for every change these builds depend on
tools/ci/check-wasm.sh
# after build-web-dev.sh: links every app into the core in Node, checks that
# each registers and that a module stamped for another ABI is refused
node crates/browser/tilo-browser-runtime/scripts/web-runtime/smoke.mjs /abs/out
```
