# Web runtime

`tilo-browser-runtime` is the Tilo UI runtime in the browser: one
WebAssembly module holding the toolkit and the page host, and the one
application an app's own bundle links into it. `wasm32` has no dynamic
linking, so apps are compiled in, not loaded at run time. `web/host.mjs`
owns the DOM (input and IME, clipboard, the accessibility projection, font
fetching, `IndexedDB`); the crate owns the components, their records and
the GPU.

## Building

```sh
crates/browser/tilo-browser-runtime/scripts/build.sh /abs/out \
  [--profile release|assert|dev|quick] [--[no-]wasm-opt] [--bundle runtime|app]
python3 -m http.server -d /abs/out   # then open it in a browser with WebGPU
```

`--bundle runtime` (the default) builds this crate: the web entry's pages and
the module that gets a browser into a workspace. `--bundle app` builds an
app's own bundle instead: the crate `tilo-app build --platform web`
generates for it (`--manifest-path`, built
with the target directory in `$CARGO_TARGET_DIR`) and its one-application
`apps.json` (`--apps-json`, which also names the app `build.json` records).
The output holds `pkg/` (wasm-bindgen `--target web`), the page
host modules and `index.html`, `fonts/` and `fonts.json`, `data/` and
`data.json` (application data fetched on request) and `build.json`
(revision, tools, profile and output hashes). `--profile release` compiles
with Cargo's `wasm-release` profile and runs `wasm-opt` with the arguments in
`tools/web/wasm-opt-flags` unless `--no-wasm-opt`. `--profile assert` is the same release optimisation with
the assertions a development build has (`wasm-assert`: `debug_assertions`
and overflow checks), for qualification and performance measurement: a
bundle that drops the qualification checks is faster for the wrong reason.
Tools: the pinned Rust toolchain with
`wasm32-unknown-unknown`, a wasm-bindgen CLI matching the crate's pinned
wasm-bindgen, binaryen, Node.js.

`--wasm-steps` (or `$TILO_BUNDLE_WASM_STEPS`) says where the steps that turn
the linked module into `pkg/` run: wasm-bindgen wants several gigabytes and
wasm-opt over the start module alone wants more, which is the build's memory
rather than the compile's. `auto` runs them on the builder when `$CARGO` is a
drop-in that runs a program there and brings a directory of the target dir
back (`$CARGO exec --fetch`, as the local stack's `os/containers/tilo-sh/cargo.sh`
does when a cargo that runs programs is named in `$TILO_SH_CARGO`), and beside
the compile otherwise, so a machine that only asked for the build needs no
wasm-bindgen or binaryen of its own; `builder` refuses to run them here, `local`
never sends them away. Only the finished directory comes back (`pkg/`, `data/`,
`apps.json`, `wasm.json`), the one a local run writes, so both make the same
`pkg/` and record the same `build.json`; a fetch that brings back too little,
or an earlier run's, stops the build and names what it looked for.

## Cargo features of `tilo-browser-runtime`

The crate has no Cargo features: the one wasm32 build carries the toolkit,
the page host, the catalogue and the canonical desktop over page-owned workspaces.
Native builds compile an empty library.

## Apps outside the Tilo tree

`tilo-app build --platform web` (and `dev`, `run`) builds the app's bundle,
as `build --target web --bundle app` does: `wasm32` has no dynamic linking,
so the runtime and the app are one module. A small generated crate under
`<target>/tilo-app/web/<package>/crate` depends on `tilo-browser-runtime` and
the app, registers the app's `definition()` at start, and builds into the app
workspace's target directory as its own Cargo workspace (with that
workspace's `[patch]`, profiles and lock file), so an app anywhere builds
against the SDK checkout it names. The generated crate is what `build.sh`
compiles (`--bundle app`): the module, the pages, the fonts and the compute
worker are the same build the site bundle is, so an app's bundle and the
site's never differ in how they were made. The bundle lands in
`<target>/tilo-app/web/<package>/bundle` in `build.sh`'s layout and opens at
`app/#/me/<id>`; `--output` copies it. `--release` with `--assertions` asks
for the same release optimisation with `debug_assertions` and overflow
checks kept (`wasm-assert`), which is what a qualified or measured bundle is
built with; `--assertions` alone is refused, because an unoptimised build
already asserts.
`package --target web` writes
it in the immutable layout Cloud serves.

## Mounting

```js
import {mountDesktop} from './host.mjs';
const mount = await mountDesktop(element, {app: 'so.tilo.word-processor', profile, device, workspace});
mount.actions();                                   // what the app publishes now
mount.invokeAction(resource, action, {/* args */});
```

`web/host.d.ts` is the page API. `app` is the id of an application
compiled into the bundle, or `desktop` (the default) for the
canonical desktop (`src/desktop.rs`: `tilo-desktop-surface` over a
`tilo-desktop-model` tree, every tab an in-process application). The tree
and each tab's record are kept per workspace in the profile's storage and
committed with the records themselves, so a reload restores the same
windows on the same documents. Who is here is the session's own: a started
peer runs `tilo_session::sync::Client` over the page's live session, so the
board, its schedule and the certificates, waves and calls on it are the
desktop's (`src/peer_sync.rs`). With `peer`, a started certified peer, the
mount's records and desktop replicate through that peer's workspace, and
`invokeAction` also takes the workspace's own membership actions on its
governance resource, which the host decides:

```js
await mount.invokeAction(`workspace.permissions:${workspace}`, 'remove-member', {person}, {confirm: true});
```

Mounts of one profile, device, workspace and app share a record: another mount
passing the same `record` edits the same document and shows the other's
changes live, each under its own `author`.

## Links opened full tab

Every resource with a view has a link (`tilo_reference::link`), and the one
an app's Share hands out opens it full tab: `<entry>app/#/@<host>/w/<workspace>/<app>/<record>/<resource>`.
`web/app/index.html` gets into the workspace as any link does (its entry, an
invitation the link carries, or signing in), mounts that one app with no
desktop, and shows the resource through its view. Over it, `web/app-bar.mjs`
draws a thin bar: the workspace and the app, the people here from the
workspace's presence board (those in this document first; the page publishes
its own focus as a desktop window does), Share, and Open in Tilo. Share copies
the link as it stands, which opens for the workspace's people, or makes one
for anyone who has it: the same link with an invitation of the person's own in
its `invite` secret, written by the runtime (`formatInviteLink`). Open in Tilo
opens the same place on the desktop; a document of its own opens as its app
there. At phone width the bar keeps the same four things with the names cut
short. A view whose content the desktop's session serves (a run, read from
the workspace's history) is declared `on_desktop`, and its link opens the
desktop at that view instead. `os/containers/tilo-lounge/integration/share-link.mjs`
drives it with two people's browsers against the Lounge image.

## Fonts

`build.sh` copies the product fonts of `crates/sdk/ui-core/assets/fonts`
into `fonts/`; `fonts.json` lists each with its size and SHA-256
(`web/font-loader.mjs` verifies both). The `browser_startup` fonts are the
mount barrier; everything else (CJK, emoji, math, symbols, other scripts)
is fetched only when text demands it, or during idle prewarm within its
budget.

## Tests

```sh
# every web/*.test.mjs suite, as CI runs them (tools/ci/suites.toml)
node --test crates/browser/tilo-browser-runtime/web/*.test.mjs
# the wasm32 clippy CI runs for every change these builds depend on
# (CARGO=<any cargo drop-in> runs it through a remote builder or container)
tools/ci/check-wasm.sh
```

The page reaches the module as `import * as runtime from
'./pkg/tilo_browser_runtime.js'`, and nothing on either side of that boundary
sees the other's names: the `pkg/*.d.ts` exists only after a build, and a test
that fakes the module answers under whatever name it was given. So a page and
its fake can agree on a name the module never exported, and the call throws
only in a browser, in the build that reached a person.

`web/runtime-surface.test.mjs` closes that: it reads the crate's
`#[wasm_bindgen]` items out of the Rust sources and fails, naming the file and
the call, for every `runtime.name(…)` that names something the module does not
export. It reads Rust rather than the generated `.d.ts` because this suite runs
in the CI image, where nothing has been built.

A test that fakes the runtime builds it with the same reading, so an invented
name fails there too:

```js
import {runtimeFake} from './runtime-surface.mjs';

const runtime = runtimeFake({channel_vault: async () => new Uint8Array(32)},
  {calls, get peer() { return peer; }});
```

The first argument is the module's surface, held to the real exports; the
second is the test's own scaffolding, which the module knows nothing about and
which is therefore left alone.
