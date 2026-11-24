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
