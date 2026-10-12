# Web runtime

`tilo-browser-runtime` is the Tilo UI runtime in the browser: one
WebAssembly module holding the toolkit, the page host and every application
it was built with. `wasm32` has no dynamic linking, so apps are compiled in,
not loaded at run time. `web/host.mjs` owns the DOM (input and IME,
clipboard, the accessibility projection, font fetching, `IndexedDB`); the
crate owns the components, their records and the GPU. See
`docs/architecture/browser-runtime.md` for the design.

## Building

```sh
crates/browser/tilo-browser-runtime/scripts/build.sh /abs/out \
  [--profile release|dev] [--[no-]wasm-opt] [--bundle runtime|desktop]
python3 -m http.server -d /abs/out   # then open it in a browser with WebGPU
```

`--bundle runtime` (the default) builds this crate; `--bundle desktop` builds
`crates/browser/tilo-web-desktop`, the same runtime with every web-capable
app registered from its manifest, under the same module name, plus
`apps.json`. The output holds `pkg/` (wasm-bindgen `--target web`), the page
host modules and `index.html`, `fonts/` and `fonts.json`, `data/` and
`data.json` (application data fetched on request) and `build.json`
(revision, tools, profile and output hashes). `--profile release` compiles
with Cargo's `wasm-release` profile and runs `wasm-opt` unless
`--no-wasm-opt`. Tools: the pinned Rust toolchain with
`wasm32-unknown-unknown`, a wasm-bindgen CLI matching the crate's pinned
wasm-bindgen, binaryen, Node.js.

## Cargo features of `tilo-browser-runtime`

The crate has no Cargo features: the one wasm32 build carries the toolkit,
the page host, the catalogue and the canonical desktop over page-owned rooms.
Native builds compile an empty library.

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
it in the immutable layout Cloud serves.

## Mounting

```js
import {mountDesktop} from './host.mjs';
const mount = await mountDesktop(element, {app: 'so.tilo.word-processor', profile, device, room});
mount.actions();                                   // what the app publishes now
mount.invokeAction(resource, action, {/* args */});
```

`web/host.d.ts` is the page API. `app` is the id of an application
compiled into the bundle, or `desktop` (the default) for the
canonical desktop (`src/desktop.rs`: `tilo-desktop-surface` over a
`tilo-desktop-model` tree, every tab an in-process application). The tree
and each tab's record are kept per room in the profile's storage and
committed with the records themselves, so a reload restores the same
windows on the same documents. With `peer`, a started certified peer, the
mount's records and desktop replicate through that peer's room.

Mounts of one profile, device, room and app share a record: another mount
passing the same `record` edits the same document and shows the other's
changes live, each under its own `author`.

## Fonts

`build.sh` copies the product fonts of `crates/sdk/ui-core/assets/fonts`
into `fonts/`; `fonts.json` lists each with its size and SHA-256
(`web/font-loader.mjs` verifies both). The `browser_startup` fonts are the
mount barrier; everything else (CJK, emoji, math, symbols, other scripts)
is fetched only when text demands it, or during idle prewarm within its
budget.

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
# every web/*.test.mjs suite, as CI runs them (tools/ci/suites.toml)
node --test crates/browser/tilo-browser-runtime/web/*.test.mjs
# the wasm32 type check CI runs for every change these builds depend on
tools/ci/check-wasm.sh
```
