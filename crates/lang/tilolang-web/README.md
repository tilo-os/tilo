# tilolang-web

Tilolang in a browser page: compile a tree of package files, run an app over one of its
scenarios with its window laid out by the toolkit and written as semantic HTML, and the
two-person playground over one source text. It is the engine of the tilolang.dev playground and
gallery, and nothing else: Tilo's own web pages are drawn on the server.

## Building the bundle

```sh
crates/lang/tilolang-web/build-page.sh OUT     # default OUT: target/tilolang-web
```

It needs the `wasm32-unknown-unknown` target and `wasm-bindgen` 0.2.108, and builds with
profile `wasm-release` (`PROFILE=wasm-dev` builds faster). In the tilo repo, build on the build
server: `t exec -- crates/lang/tilolang-web/build-page.sh OUT`, fetched back with `--fetch`.

| File | What it is |
| --- | --- |
| `tilolang_web.js`, `tilolang_web.d.ts`, `tilolang_web_bg.wasm`, `snippets/` | The module. `snippets/` holds JS that some toolkit crates import; keep it next to the `.js` file. |
| `apps.json` | Every file of `apps/tl/**` (`.tl`, `locales/*.ftl`, `app.ids`, `tilo-app.toml`, `surface.toml`) by its repo path, which is what `open_app` and `compile_package` take. |
| `fonts/` | The two startup fonts (Tilo Sans, Onest Cyrillic, about 400 KB) the page hands over before it opens an app. |

Every export takes and returns JSON **strings**, except `add_font` (bytes) and `app_close` (bool).
Panics print `tilolang-web: panicked at …` to `console.error` before the module traps.

## Loading

```js
import init, * as tl from "/wasm/tilolang_web.js";
await init();                       // or initSync({ module: bytes })

// Fonts first: windows lay out text, and a page has no fonts of its own.
for (const f of JSON.parse(tl.startup_fonts())) {            // [{file, bytes, sha256}]
  const bytes = new Uint8Array(await (await fetch(`/wasm/fonts/${encodeURIComponent(f.file)}`)).arrayBuffer());
  const digest = [...new Uint8Array(await crypto.subtle.digest("SHA-256", bytes))]
    .map(b => b.toString(16).padStart(2, "0")).join("");
  tl.add_font(f.file, bytes, digest);
}
const ready = JSON.parse(tl.fonts_ready());                 // {ok} | {ok:false, message}
```

`open_app` refuses with a message until `fonts_ready` succeeded. The playground and
`compile_package` do not need fonts.

## Packages: compile and run a real app

`files` is a JSON object `{ "<path>": "<text>", … }`. A directory with `.tl` files directly in it is a
package (all its `.tl` files are one module, `locales/*.ftl` its catalogues, `app.ids` its ids lock).
A directory with `tilo-app.toml` + `surface.toml` is a native app a package can `use`. Imports are
resolved among the files given, by app id, exactly as `tilolang build` resolves sibling packages
(`tilolang::source::order_of` / `compile_in_order`, the code the CLI's directory build now calls),
against the platform packages (`so.tilo.device`, `so.tilo.settings`, `so.tilo.session`) the CLI
also compiles against. To edit an app, change its entry in the object and call again.

### `compile_package(files, root) -> string`

`root` is a package directory, e.g. `"apps/tl/tasks"`.

```ts
{ ok: true,
  app: Package,              // root's package
  packages: Package[] }      // dependency order, root last
| { ok: false, message: string, diagnostics: Diagnostic[] }

type Package = { dir, app, title, wasmBytes, scenarios: string[], roots: string[] }
type Diagnostic = {
  code: string, message: string,
  file?: string,            // e.g. "apps/tl/tasks/scenarios.tl"
  line: number, col: number,  // 1-based
  start: number, end: number, // UTF-8 byte offsets in that file
  from?: number, to?: number  // the same span in UTF-16 units, for an editor
}
```

A missing import gives `ok:false` with an empty `diagnostics` and a `message` naming it
(`` `use so.tilo.notes`: no package with that app id in the files given ``).

### `open_app(files, root, options) -> string`

Compiles as above, runs the app in an instance over an in-memory workspace, stages a scenario,
lays its window out with the toolkit and writes the window as semantic HTML: the
same writer the server uses for tilo.sh's web pages (`ui::composition::page`).

`options` (JSON, may be `""`): `{ scenario?, root?, width?, height?, dark?, linkHome? }`

- `scenario`: name from `Package.scenarios`; omitted = the package's first; `""` = none (the app
  as on a first launch: empty).
- `root`: the layer of a package with several roots (`"Shell"`, `"Layout"`).
- `width`/`height`: window size in CSS px (default 1024×720). A narrow width shows one column of
  a split at a time, as on a phone.
- `dark`: dark theme (default light).
- `linkHome`: where the workspace's resources open on the web, for `app_share`.

```ts
{ ok: true, handle: number, app: Package, packages: Package[], scenario: string | null,
  css: string,   // the theme's custom properties + the page stylesheet; put it in a <style>
  body: string,  // the window's HTML
  gaps: Gaps }
| { ok: false, message: string, diagnostics: Diagnostic[] }

// What the page shows other than as the browser's own elements, by widget name and count:
type Gaps = {
  painted: Record<string, number>, // shown as a picture of their paint, empty in a page (nothing paints there)
  placed:  Record<string, number>  // place their children at their own rects; the page places them there too
}
```

### `app_act(handle, id, target, action) -> string`

What the person did on an element. Every element the page acts on carries `data-p` (its number,
`id` here), maybe `data-t` (semantic target, `target` here, or `null`) and `data-a` (what to send):

| `data-a` | Send `action` | When |
| --- | --- | --- |
| `activate` | `"activate"` | click |
| `toggle` | `"toggle"` | `change` of a checkbox, or a click on another element that toggles |
| `slider` | `"increment"` / `"decrement"` | arrow keys (Right/Up, Left/Down) |
| `value` | `'{"value": "<text>"}'` | `input` (and click when the element has `data-v`: send its `data-v`) |
| `edit` | `'{"value": "<text>"}'` | `input` of a document block (make `[data-a=edit]` `contenteditable="plaintext-only"`) |
| (focus) | `"focus"` | `focusin` on a `value`/`edit` element |
| (caret) | `'{"select": {"anchor": n, "focus": n}}'` | caret moved, offsets in graphemes |

Use the `data-p` of `element.closest('[data-p]')`. A bare word (`activate`) and its JSON
string (`"\"activate\""`) are the same action. `'{"resize": {"width": w, "height": h}}'` lays the
window out at a new size, as `app_resize` does.

```ts
{ ok: true, patches: { id: number, html: string }[] }   // replace the element with data-p=id by html
| { ok: false, message: string }
```

This is the protocol `crates/server/tilo-server/src/web/page/page.mjs` speaks to the server; its
event handlers (`click`, `change`, `input`, composition, `focusin`, caret) and its patch apply
(keeping the focused field's element so the caret survives) can be copied as they are.

### `app_share(handle, resource) -> string`

`{ ok, value: { url } }`: the link to one of the app's resources, minted under the workspace page
given as `options.linkHome` (`{ ok: false, message }` without one).

### `app_render(handle) -> string`

`{ ok, body, gaps }`: the whole body again, numbered afresh.

### `app_resize(handle, width, height) -> string`

`{ ok, patches }`.

### `app_close(handle) -> bool`

Frees the instance. Several apps can be open at once, each by its handle.

## Playground

One source text, two people (`alice`, `bob`) on two Automerge stores, the package's first view
open for each, view trees as JSON (`{kind, key, props, bound, events, anchor, children}`).

| Export | Does |
| --- | --- |
| `compile_and_run(source)` | compile, boot both peers, sync; `{ok, wasmBytes, app, online, pendingAlice, pendingBob, alice:{tree, canUndo}, bob:{…}, history}` or `{ok:false, diagnostics}` |
| `fire_peer(peer, key, event, valueJson?)` | fire a view event (`press`, `submit`, `change`) |
| `edit_peer(peer, key, valueJson)` | edit a bound control |
| `invoke_peer(peer, action, argsJson)` | invoke an action by name or id; the root is passed first when it is the first parameter |
| `set_online(bool)`, `sync_peers()` | sync control |
| `undo_peer(peer)` | drop that peer's last change and replay |
| `reset()` | back to the first synced state |
| `tokenize(source)` | `[{start, end, kind}]`, UTF-16 offsets, coloured as Tilo's own code editor colours tilolang; kinds `keyword type function number string comment punct ref constant` (identifiers and spaces carry no span) |

The playground's single source compiles with no other packages. For a source that
`use`s another package, use `compile_package` / `open_app` with the files.

