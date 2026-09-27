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

## The web app collection

`runtime-dylib` must be the superset of what every thin app in the default
collection needs; a crate built with other features is another crate with other
symbol names. The web core follows the same rule: it is built in one Cargo
invocation with the apps of the web collection (`TILO_WEB_COLLECTION`, default
`ebook word-processor`; recorded in `runtime.json`), so each crate they share
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

`build-web-dev.sh` writes `build/sizes.md`. Measured 2026-09-27 on main with
the Cloud stack merged (collection Books + Writer, no `cloud`):

| module | linked | linked, brotli | `-Oz` | `-Oz`, brotli |
|---|---:|---:|---:|---:|
| core | 32.02 MiB | 4.81 MiB | 16.25 MiB | 3.65 MiB |
| Books (`so.tilo.ebook`) | 1.50 MiB | 287 KiB | 854 KiB | 248 KiB |
| Writer (`so.tilo.word-processor`) | 5.03 MiB | 661 KiB | 2.07 MiB | 588 KiB |

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

## Tests

```sh
node --test crates/browser/tilo-browser-runtime/web/dylink.test.mjs \
  crates/browser/tilo-browser-runtime/web/core-loader.test.mjs \
  crates/browser/tilo-browser-runtime/web/app-installer.test.mjs \
  crates/browser/tilo-browser-runtime/web/app-directory.test.mjs \
  crates/browser/tilo-browser-runtime/scripts/web-runtime/bundle-glue.test.mjs
cargo test --locked -j4 -p tilo-browser-runtime --lib catalog::
# after build-web-dev.sh: links every app into the core in Node, checks that
# each registers and that a module stamped for another ABI is refused
node crates/browser/tilo-browser-runtime/scripts/web-runtime/smoke.mjs /abs/out
```
