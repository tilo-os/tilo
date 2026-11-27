# tilo-ui

Tilo UI, the toolkit every [Tilo](https://code.tilo.so/tilo/tilo) app and the
Tilo shell draw with. It has the shared components (buttons, fields, lists,
tables, sheets, navigation and inspectors), layout that adapts to any window
size, text shaping and editing, theming, accessibility through AccessKit, and
animation through shared motion primitives. It draws on the GPU with vello and
wgpu, and on the CPU where there is no GPU.

## Which crate to use

To build an app, depend on [`tilo-app`](https://crates.io/crates/tilo-app). It
re-exports this toolkit and adds what an app is made of: resources, actions and
views bound to both, with sync, history, undo, permissions and the action
palette.

Depend on [`tilo-ui-app`](https://crates.io/crates/tilo-ui-app) for a program
that only draws: a custom surface, or a tool whose state lives in the program
rather than in a shared document. It re-exports this toolkit and adds the
runner that opens its window (`App`) and the headless host that drives it
(`headless::Driver`). Depend on `tilo-ui` alone for widgets a host of your own
lays out and draws.

The default `native` feature brings the window (winit), the AccessKit adapter
and the process services for the clipboard, file choosers and notifications.
With `default-features = false` the toolkit builds without them, for a headless
host or `wasm32-unknown-unknown`, where a page delivers the input and draws the
frame `tilo-ui-app`'s `Driver` lays out.

The [Tilo UI guide](https://tilolang.dev/docs/tilo-ui) builds a larger window
and covers theming, accessibility and the web.

## Dependencies

A program that draws with `tilo-ui` compiles none of Tilo's model: not its
record store or resources (`tilo-resource`, `tilo-resource-store`), its CRDT
sync (`tilo-automerge`, `tilo-collaboration`, `tilo-runtime-collab`,
`tilo-collab-types`), its settings store (`tilo-settings-store`) or its identity
(`tilo-user-identity`). Those come only with the `model` feature, which
`tilo-app` turns on.

Of this repository it builds Tilo's UI and platform crates: painting, layout,
themes, fonts, text, highlighting, spellchecking and emoji (`tilo-ui-paint`,
`tilo-ui-graph`, `tilo-theme-sdk`, `tilo-highlight`, `tilo-spellcheck`,
`tilo-emoji` and the rest), and files, paths, processes, the clipboard,
notifications and the clock (`tilo-files`, `tilo-paths`, `tilo-process`,
`tilo-clipboard`, `tilo-notify`, `tilo-clock` and the rest), with Tilo's forks
of winit, wgpu, vello, cosmic-text and taffy. A check on every push
(`tools/sdk/check_toolkit_closure.py`) holds that list to
`tools/sdk/toolkit_closure.toml`, and fails if the toolkit's build reaches
anything else or any of the model.

## Learn more

- [Tilo UI guidelines](https://tilolang.dev/docs/interface-guidelines): layout,
  navigation, text, chrome and the shared controls.
- [Components](https://tilolang.dev/docs/components) and
  [layout](https://tilolang.dev/docs/layout), as tilolang views use them.
- [Repository](https://code.tilo.so/tilo/tilo)

## License

MIT. The bundled fonts keep their own licenses (OFL-1.1, LPPL-1.3c, or
GPL-3.0-or-later with the Font Exception); `assets/fonts/manifest.toml` lists
each file's.
