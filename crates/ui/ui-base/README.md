# tilo-ui-base

The base of Tilo UI, the toolkit every [Tilo](https://code.tilo.so/tilo/tilo) app
and the Tilo shell draw with: the widget contract and layout, paint, text
shaping, theming and its tokens, input, accessibility through AccessKit, motion
primitives and presence. Every widget is built on it, and nothing in it names a
widget.

## Which crate to use

To build an app, depend on [`tilo-app`](https://crates.io/crates/tilo-app). It
re-exports this crate and [`tilo-ui`](https://crates.io/crates/tilo-ui), the
widgets and app runner built on it.

Depend on `tilo-ui-base` directly for a crate that writes its own widgets
against the contract and needs none of tilo-ui's.

## License

MIT.
