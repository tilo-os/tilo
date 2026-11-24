# Web runtime

`tilo-browser-runtime` is the web entry: the pages a browser comes in through
and the WebAssembly module those pages use to get it into a workspace. The
page reads the link, signs the person in or takes their invitation, makes this
browser a certified device of the workspace (`admission.mjs`, `peer.mjs`), and
then hands it to the page the server renders of that workspace at
`/w/<workspace>/<app>` (`crates/core/tilo-server/src/web/page.rs`,
[Widgets as HTML](../../../docs/architecture/html-renderer.md)). No app runs
in this module, and it draws no desktop.

The module holds what the pages need to be a device: the link grammar, the
way-in rules every device takes (`tilo-session-client`), the device key, the
iroh session to the host through its packet relay, and the profile's storage.
`web/host.mjs` loads it and opens the profile's storage; `web/entry.mjs` is
the boot of `index.html` and `app/index.html`; `web/sign-in.mjs` is the
`/sign-in` route a service that is not one of the person's devices sends their
browser to.

A shipped bundle carries that module cut into a start module and the modules a
page loads when it first needs one (`tools/web/wasm-split`): `peer`, the
certified peer and its session with a workspace's host, and `accounts`, the
accounts a profile holds projected for the entry page. A page's first screen
loads the start module alone, which is the link grammar, the workspaces this
browser entered and its device's tickets; entering a workspace loads `peer`,
and a profile that holds accounts loads `accounts` to show them.
`src/split.rs` names each module's exports, and `loadRuntime(part)` in
`web/host.mjs` loads one before a caller uses its exports. A development
bundle is not split: it binds and serves the whole module.

## Building

```sh
crates/core/tilo-browser-runtime/scripts/build.sh /abs/out \
  [--profile release|assert|check|local|dev|quick] [--[no-]wasm-opt]
python3 -m http.server -d /abs/out
```

The output holds `pkg/` (wasm-bindgen `--target web`), the pages
(`index.html`, `sign-in.html`, `app/index.html`) and the modules they load,
the home-screen icon and web app manifest, `locales/` (the arrival lines) and
`build.json` (revision, tools, profile and output hashes). The header of
`scripts/build.sh` describes each profile and the `--update`, `--wasm-only`,
`--module-only` and `--wasm-from` modes. `--profile release` compiles with
Cargo's `wasm-split` profile, splits the module, and runs `wasm-opt` with the
arguments in `tools/web/wasm-opt-flags` unless `--no-wasm-opt`. `--profile
assert` is the same release optimisation with the assertions a development
build has (`wasm-assert-split`: `debug_assertions` and overflow checks). Tools:
the pinned Rust toolchain with `wasm32-unknown-unknown`, a wasm-bindgen CLI
matching the crate's pinned wasm-bindgen, binaryen, Node.js.

`--wasm-steps` (or `$TILO_BUNDLE_WASM_STEPS`) says where the steps that turn
the linked module into `pkg/` run. `auto` runs them on the builder when
`$CARGO` is a drop-in that runs a program there and brings a directory of the
target dir back (`$CARGO exec --fetch`, as the local stack's
`os/containers/tilo-sh/cargo.sh` does when a cargo that runs programs is named
in `$TILO_SH_CARGO`), and beside the compile otherwise, so a machine that only
asked for the build needs no wasm-bindgen or binaryen of its own; `builder`
refuses to run them here, `local` never sends them away. Only the finished
directory comes back (`pkg/` and `wasm.json`), the one a local run writes, so
both make the same `pkg/` and record the same `build.json`; a fetch that
brings back too little, or an earlier run's, stops the build and names what it
looked for.

The site's published bundle is `tools/ci/publish/web-entry-bundle.sh`; the
local tilo.sh stack builds the same one (`os/containers/tilo-sh/stack.sh`).

## Cargo features of `tilo-browser-runtime`

The crate has no Cargo features. Native builds compile an empty library.

## Tests

```sh
# every web/*.test.mjs suite, as CI runs them (tools/ci/suites.toml)
node --test crates/core/tilo-browser-runtime/web/*.test.mjs
# the wasm32 clippy CI runs for every change these builds depend on
# (CARGO=<any cargo drop-in> runs it through a remote builder or container)
tools/ci/check-wasm.sh
```

The flows that drive the built entry in Chromium (signing in, an invitation,
the wait list, the directory, a passkey, the hand-over to the server's page)
are `os/containers/tilo-lounge/test-e2e.sh` and the tilo.sh stack's checks.

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
