# tilolang

tilolang, the language Tilo's apps are written in, running in a browser page: compile a
package from its files, run its app over a scenario, and show its window as semantic HTML.
This is the WebAssembly build of the `tilolang-web` crate from the
[Tilo repository](https://code.tilo.so/tilo/tilo).

```sh
bun add tilolang      # or: npm install tilolang
```

## Use

The module is wasm-bindgen's `web` build: call the default export once to load the
WebAssembly, then the functions. Every function takes and answers JSON text.

```js
import init, {
  compile_package, open_app, app_act, app_render,
  startup_fonts, add_font, fonts_ready,
} from "tilolang";

await init();

// The fonts windows set their text in ship in this package (fonts/). Hand them over once,
// before the first window opens; each is checked against the digest the build pins.
for (const font of JSON.parse(startup_fonts())) {
  const url = new URL(`./node_modules/tilolang/fonts/${font.file}`, import.meta.url);
  const bytes = new Uint8Array(await (await fetch(url)).arrayBuffer());
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  const hex = [...digest].map((b) => b.toString(16).padStart(2, "0")).join("");
  add_font(font.file, bytes, hex);
}
JSON.parse(fonts_ready());

// A package is its files, by path: the .tl sources, tilo-app.toml and app.ids.
const files = JSON.stringify({
  "hello/tilo-app.toml": "…",
  "hello/main.tl": "…",
});

const built = JSON.parse(compile_package(files, "hello"));
if (!built.ok) console.error(built.diagnostics);

const app = JSON.parse(open_app(files, "hello", JSON.stringify({ width: 800, height: 600 })));
document.head.insertAdjacentHTML("beforeend", `<style>${app.css}</style>`);
document.body.innerHTML = app.body;
```

`app_act(handle, id, target, action)` sends what a person did in the window and answers its
next render; `app_render`, `app_resize`, `app_share` and `app_close` do what they say. The
TypeScript declarations (`tilolang_web.d.ts`) document every function.

Where your bundler copies assets, import the module and the fonts through it instead of
`node_modules`: the package exports `tilolang/tilolang_web_bg.wasm` and `tilolang/fonts/*`.

## The rest of tilolang

The `tilolang` command checks, formats and builds packages into Tilo apps:

```sh
curl -fsSL https://tilolang.dev/install | sh
```

The compiler, runtime and toolkit are also Rust crates on crates.io (`tilolang`,
`tilolang-runtime`, `tilo-ui`, `tilo-app`). All of them are released together, under one
version: this package's version is theirs.

## Licence

The licence `package.json` names, in `LICENSE`. The fonts are each under their own licence, in
`fonts/LICENSES`.
