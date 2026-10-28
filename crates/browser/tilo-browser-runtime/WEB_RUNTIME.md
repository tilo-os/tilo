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
  [--profile release|assert|dev|quick] [--[no-]wasm-opt] [--bundle runtime|desktop]
python3 -m http.server -d /abs/out   # then open it: WebGPU where the browser has it
```

`--bundle runtime` (the default) builds this crate; `--bundle desktop` builds
`crates/browser/tilo-web-desktop`, the same runtime with every web-capable
app registered from its manifest, under the same module name, plus
`apps.json`. The output holds `pkg/` (wasm-bindgen `--target web`), the page
host modules and `index.html`, `fonts/` and `fonts.json`, `data/` and
`data.json` (application data fetched on request) and `build.json`
(revision, tools, profile and output hashes). `--profile release` compiles
with Cargo's `wasm-release` profile and runs `wasm-opt` unless
`--no-wasm-opt`. `--profile assert` is the same release optimisation with
the assertions a development build has (`wasm-assert`: `debug_assertions`
and overflow checks), for qualification and performance measurement: a
bundle that drops the qualification checks is faster for the wrong reason.
Tools: the pinned Rust toolchain with
`wasm32-unknown-unknown`, a wasm-bindgen CLI matching the crate's pinned
wasm-bindgen, binaryen, Node.js.

## Drawing without WebGPU

A browser that offers no WebGPU (Firefox on Linux ships `dom.webgpu.enabled`
off) draws the same scene on the CPU: `rendererChoice` asks for an adapter
once per page, and a page without one rasterizes through
`ui::cpu_canvas`/`tilo-cpu-raster` and presents each changed region to its
canvas as a WebGL2 texture (`web/cpu-present.mjs`). The start module is built
with `+simd128` (`.cargo/config.toml`), which is where `vello_cpu` takes its
vector path, and the rasterizer is not in it: `tilo-web-desktop`'s
`src/cpu_raster.rs` is a split point, so the page fetches that module
(`load_cpu_raster`, `lazy-cpu-raster`) after `rendererChoice` picked the CPU
and before the first mount. A visitor whose browser has WebGPU never
downloads it.

Release desktop bundle, measured with the same tools (wasm-opt 126,
wasm-bindgen 0.2.108), start module in bytes:

| build | start module | CPU rasterizer |
|---|---|---|
| `main` before the CPU renderer | 22,322,887 | — |
| linked in, no SIMD | 22,956,739 | in the start module |
| `+simd128` | 22,462,472 | in the start module |
| `+simd128`, fetched on demand | 22,138,463 | 343,076 |

Frame work at 1440x900 with four windows open, in Playwright's Firefox with
`dom.webgpu.enabled=false` and Chromium whose adapter request answers
nothing, measuring the runtime's own `draw_total` stage:

| | Firefox before | Firefox now | Chromium before | Chromium now |
|---|---|---|---|---|
| first frame | 1.2–2.3 s | 1.2 s | 0.6 s | 0.6 s |
| open an app (p50/p95 ms) | 40 / 59 | 24 / 43 | 52–96 | 21 / 63 |
| typing (p50/p95 ms) | 97 / 128 | 32 / 49 | — | 33 / 46 |
| pan the strip (p50/p95 ms) | 83 / 97 | 32 / 39 | 38 / 128 | 34 / 41 |
| pan (frames/s) | 11 | 27 | 13 | 26 |
| scroll inside a window (p50/p95 ms) | 83 / 174 | 32 / 37 | — | 39 / 45 |
| idle (frames/s) | 0 | 0 | 0 | 0 |

Still above the 16 ms budget: a pan frame repaints the windows it moved (the
runtime reports `render:full:outside-parts`), since one stretch of the strip's
own draws outside every window leaves the damage proof no bound for what
follows it.

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
`app/#/me/<id>`; `--output` copies it. `--release` with `--assertions` asks
for the same release optimisation with `debug_assertions` and overflow
checks kept (`wasm-assert`), which is what a qualified or measured bundle is
built with; `--assertions` alone is refused, because an unoptimised build
already asserts. Page URLs (which room, app and
resource a page opens, desktop or full-tab app mode, history, share links) follow
`docs/architecture/web-routing.md`, which replaces these query parameters. `package --target web` writes
it in the immutable layout Cloud serves.

## Renderers: WebGPU, and the CPU where there is none

Frames are drawn on the page's WebGPU device when the browser has one, and
else on the CPU in the same module. `web/webgpu-safety.mjs` asks for an
adapter once per page (`rendererChoice`: no `navigator.gpu`, no adapter, or a
request that fails is `cpu`); `web/host.mjs` then presents through
`web/cpu-present.mjs`, and `mount` takes that presenter instead of opening a
device. Firefox on Linux ships `dom.webgpu.enabled` off, so its visitors used
to get about:config instructions: they now get the desktop, more slowly.

On the CPU, `ui::cpu_canvas` replays each frame's `vello::Scene` with
`tilo-cpu-raster` (`vello_cpu`) into a retained premultiplied RGBA8 canvas,
repainting only the damage `RetainedFrames` proves, and hands each changed
region to the page: a WebGL2 texture drawn as one quad per frame, or
`putImageData` where there is no WebGL2 (`?cpuPresent=2d` picks that path,
`?cpuPresent=webgl2` the other, to measure them against each other). The
canvas keeps the frame between presents, which the page's capture path reads
back. Both renderers draw the same scene from the same damage decisions; the
CPU one is single-threaded (`wasm32` has none) and keeps every image and glyph
outline the frame needs in the module's memory.

Not on the CPU: the pixels apps ask for beside the frame (an eyedropper, an
exported picture) need a device and are refused, and videos' pictures are not
copied into the frame; the page's feedback crops are GPU-only too. A WebKit
page with `?unsafeWebKitGPU=0` is still refused rather than drawn on the CPU:
that flag asks for no GPU, not for the slower renderer.

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
mount's records and desktop replicate through that peer's room, and
`invokeAction` also takes the room's own membership actions on its
governance resource, which the host decides:

```js
await mount.invokeAction(`workspace.permissions:${room}`, 'remove-member', {person}, {confirm: true});
```

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
# the wasm32 clippy CI runs for every change these builds depend on
# (CARGO=<any cargo drop-in> runs it through a remote builder or container)
tools/ci/check-wasm.sh
```
