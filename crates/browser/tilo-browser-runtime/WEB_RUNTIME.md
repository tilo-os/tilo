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
# Core + Books + Writer + test page in one directory (local development):
crates/browser/tilo-browser-runtime/scripts/build-web-dev.sh /abs/out [PACKAGE...]
python3 -m http.server -d /abs/out   # then open it in a browser with WebGPU

# The steps it runs:
crates/browser/tilo-browser-runtime/scripts/build-web-core.sh /abs/out [--features "..."]
crates/browser/tilo-browser-runtime/scripts/build-web-app.sh /abs/out ebook
crates/browser/tilo-browser-runtime/scripts/build-web-app.sh /abs/out word-processor
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
| (none, the default) | the toolkit core: toolkit, page host, catalogue; no apps, no peer |
| `dylink` | links the cdylib as a dynamic-linking main module (see below) |
| `static-apps` | compiles Writer, Chat, Notes, Calculator and Books into the module |
| `cloud` | the certified peer, iroh transport, delivery, onboarding, MoQ media, the canonical desktop |

Host tests (`cargo test -p tilo-browser-runtime`) always cover the peer modules
and the catalogue's apps.

## The core

`build-web-core.sh` builds `-p tilo-browser-runtime --features dylink`:

- `build.rs` passes `--export-all --export-table --growable-table` to the
  cdylib link, so the core exports every symbol, its function table (growable)
  and its stack pointer. The core is non-PIE; std is shared (apps import
  `__rust_alloc` and friends).
- `scripts/web-runtime/tilo-wasm-ld` is rustc's linker for it: the objects of
  the toolkit crates (`tilo_app`, `tilo_ui`, `tilo_resource`, `tilo_actions`)
  are linked whole, so an app may call any of their functions, not only those
  the core itself uses. This is the superset rule `runtime-dylib` states for
  native thin apps.
- `src/side_module.rs` exports `tilo_dl_alloc` (memory for a side module's
  data) and `tilo_runtime_register_app(&'static ApplicationDefinition)`.
- The mangled wasm-bindgen internals `--export-all` exposed are removed
  (`wasm-tools.mjs strip-exports`), then `wasm-bindgen --keep-lld-exports`,
  `wasm-opt -Oz`, and the ABI id section is added.
- `build/core-profile.txt` lists code size by crate and the largest functions
  (from the name section, before `wasm-opt` drops it).

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
2. Builds `-p tilo-browser-runtime -p PACKAGE` in one Cargo invocation with the
   core's features, so shared crates get the core's features. A shared crate
   whose Cargo metadata hash differs from the core's means the app enables a
   feature the core lacks: the build stops and names it (add the feature to the
   core's dependencies). The core's ABI id is recomputed from this build and
   must equal the core's (its sources must not have changed since).
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

The registry client (`registry-client.mjs`, from the registry side) supplies
verified modules through the same `resolveApp` shape. Signature verification
of snapshots and tapps belongs to its verification crate; once that lands it
can be linked into the core behind a feature, and the resolver is its hook.

## Fonts

`scripts/web-runtime/fonts.mjs` publishes the product fonts content-addressed
as `fonts/<sha256>.<ext>`; `fonts.json` maps the name Rust demands to that
path, with size and SHA-256 (`font-loader.mjs` verifies both). The five
`browser_startup` fonts are the mount barrier; everything else (Noto Sans CJK,
32.7 MB; emoji; math; symbols; Arabic, Hebrew, Devanagari, Bengali, Thai) is
fetched only when text demands it, or during idle prewarm within its budget.

## Tests

```sh
node --test crates/browser/tilo-browser-runtime/web/dylink.test.mjs \
  crates/browser/tilo-browser-runtime/web/app-installer.test.mjs \
  crates/browser/tilo-browser-runtime/web/app-directory.test.mjs
cargo test --locked -j4 -p tilo-browser-runtime --lib catalog::
# after build-web-dev.sh: links every app into the core in Node, checks that
# each registers and that a module stamped for another ABI is refused
node crates/browser/tilo-browser-runtime/scripts/web-runtime/smoke.mjs /abs/out
```
