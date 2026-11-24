# tilo-ui

Tilo UI, the toolkit every [Tilo](https://code.tilo.so/tilo/tilo) app and the
Tilo shell draw with. It has the shared components (buttons, fields, lists,
tables, sheets, navigation and inspectors), layout that adapts to any window
size, text shaping and editing, theming, accessibility through AccessKit, and
animation through shared motion primitives. It draws on the GPU with vello and
wgpu, and on the CPU where there is no GPU.

## Which crate to use

To build an app, depend on [`tilo-app`](https://crates.io/crates/tilo-app). It
re-exports this toolkit unchanged and adds what an app is made of: resources,
actions and views bound to both, with history, undo and the action palette.

Depend on `tilo-ui` directly for a program that only draws with the toolkit,
such as a custom surface or a tool that doesn't model its state as resources.
With `default-features = false` it builds without the native runner, for
headless use and the web.

```toml
[dependencies]
tilo-ui = { version = "0.4", default-features = false }
```

## Dependencies

The toolkit draws windows, so it is meant to depend only on Tilo's UI and
platform crates: the UI crates for painting, layout graphs, themes, fonts, text
highlighting, spellchecking and emoji (`tilo-ui-paint`, `tilo-ui-graph`,
`tilo-theme-sdk`, `tilo-formats`, `tilo-highlight`, `tilo-spellcheck`,
`tilo-emoji` and the rest of `crates/ui/`), and the platform crates for files,
paths, processes, clipboard, notifications, presentation and the clock
(`tilo-files`, `tilo-paths`, `tilo-process`, `tilo-clipboard`, `tilo-notify`,
`tilo-presentation`, `tilo-clock` and the rest of `crates/platform/`). It also
uses Tilo's forks of winit, wgpu, vello, cosmic-text and taffy. It must not
pull in Tilo's record store, its CRDT engine, a Tilo session or network access.

It doesn't meet that yet. The toolkit still holds the app layer (the app runner,
collaboration, history, presence, search and resource-backed views), and
through that it still reaches the record store (`tilo-resource-store`), the
Automerge CRDT engine and its worker process (`tilo-automerge`,
`tilo-automerge-isolate`), the collaboration client and other workspace
crates. That layer is being moved into `tilo-app`. A check run on every push
(`tools/sdk/check_builder_licenses.py --manifests`) fails if the toolkit picks
up any crate it doesn't already reach, and lists the crates that move must
remove.

To bind the toolkit to the Tilo model (resources, actions and views, with
sync, history, undo and permissions), depend on `tilo-app`.

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
