# Web runtime

`tilo-browser-runtime` is the Tilo UI runtime in the browser: one
WebAssembly module holding the toolkit, the page host and every application
it was built with. `wasm32` has no dynamic linking, so apps are compiled in,
not loaded at run time. `web/host.mjs` owns the DOM (input and IME,
clipboard, the accessibility projection, font fetching, `IndexedDB`); the
crate owns the components, their records and the GPU.

## Building

```sh
crates/browser/tilo-browser-runtime/scripts/build.sh /abs/out \
  [--profile release|assert|dev|quick] [--[no-]wasm-opt] [--bundle runtime|desktop]
python3 -m http.server -d /abs/out   # then open it in a browser with WebGPU
```

`--bundle runtime` (the default) builds this crate; `--bundle desktop` builds
`crates/browser/tilo-web-desktop`, the same runtime with every web-capable
app registered from its manifest, under the same module name, plus
`apps.json`. The output holds `pkg/` (wasm-bindgen `--target web`), the page
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
against the SDK checkout it names. The bundle lands in
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
peer runs `tilo_session::sync::Client` over the page's live plane, so the
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
`web/app/index.html` gets into the workspace as any link does (its door, an
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

## Collaboration end to end

`crates/browser/tilo-web-desktop/scripts/collab-e2e.mjs` puts native macOS
apps, headless Chrome pages and a host agent in one workspace of a local
`tilo-relay dev` host on spare ports, and checks what a person would see: the
same record on every side; edits native to browser, browser to native and
browser to browser, with the time until the other side shows them; named
cursors and carets both ways; a page that joins late; an agent's edit and
cursor on native and in the pages; and a reloaded page that keeps its device
and workspace membership. Per app it runs the native shell with `TILO_COLLAB_HOST`
under a throwaway runtime profile that carries the dev host's user root, two
pages, then a third, and (with `--agents --agent-model scripted`) an agent
spawned from a page. It writes `summary.json` and `summary.md` (a check-by-app
table) under `--out` and stops everything it started.

```sh
cargo build --release -p tilo-relay --features web,agents
cargo build -p tilo-runtime --bin tilo-runtime-mcp -p notes -p sheets -p scratchpad
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

## The first visit, with a real agent

`crates/browser/tilo-web-desktop/scripts/agent-shell.mjs` walks the first
visit a person makes on a hosted Tilo, on this machine: a `tilo-relay room`
host with a workspace of its own for each person, the site in front of it (the
desktop bundle with a deployment's `entry.json`), and a first visit in headless
Chrome. It presses "Set up with your agent", reads the code out of the prompt
the tab copied, and logs in with the real CLI (`tilo login <code> --site
<origin>`, on a scratch HOME, so this computer becomes a device of the person),
then drives `tilo mcp` over stdio as an agent does: it lists the apps, makes a
note and writes in it, sets a sheet cell, and runs a command in the Terminal
where the workspace has one. It shoots the person's workspace just after the
agent connected (the agent followed, its card open), who is here with the agent
pinned on screen, and the follow while an edit lands, at a laptop's and a
phone's width in light and dark, and records the follow through a few of the
agent's actions. Each shot's tone says which theme the shell drew: a first
visit has chosen none, so it is the light or dark its browser is in, and the
run flips the emulated scheme to see the shell follow it. Everything lands
under `--out` (`shots/`, `video/`, and the run's log), and it exits non-zero
naming the step that broke.

```sh
# on a build machine; fetch the run's `--out` directory back after
CHROME=/usr/lib64/chromium-browser/chromium-browser \
  node crates/browser/tilo-web-desktop/scripts/agent-shell.mjs --build
```

`--build` builds the relay (features `web,agents`), the CLI and the bundle;
the shots then arrive in `target/look/agent-shell`. A recording needs ffmpeg or
gst-launch-1.0 (the build container has the latter); without either, the
screencast's frames are kept instead.

## Tests

```sh
# every web/*.test.mjs suite, as CI runs them (tools/ci/suites.toml)
node --test crates/browser/tilo-browser-runtime/web/*.test.mjs
# the wasm32 clippy CI runs for every change these builds depend on
# (CARGO=<any cargo drop-in> runs it through a remote builder or container)
tools/ci/check-wasm.sh
```
