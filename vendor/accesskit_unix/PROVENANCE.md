# accesskit_unix provenance

- Package: `accesskit_unix` 0.23.0 (crates.io)
- Registry checksum: `665c9079b7325a8168a6793437a172dda44b19a05af8ed52d7e32abaada996fe`
- Repository: https://github.com/AccessKit/accesskit
- Upstream revision: `42e53b0d829e7a0b34dc8803bb06012db2e80cc6` (the archive's `.cargo_vcs_info.json`)
- Upstream package path: `adapters/unix`
- License: MIT OR Apache-2.0. The archive carries no licence text, so
  `LICENSE-MIT` and `LICENSE-APACHE` are the repository root's files at the
  upstream revision, byte for byte.
- Vendored: 2026-10-22, replacing the git fork `tilo/accesskit` at
  `a76088c4c36c03de8109ded02bd42a305fb1d569` (the same release plus the change
  below), so a build needs no git host.

Local changes:

1. In `Cargo.toml`: zbus no longer has its `async-io` feature
unconditionally. The crate's `async-io` feature enables `zbus/async-io` and its
`tokio` feature enables `zbus/tokio`, so a tokio build compiles one async
runtime instead of tokio next to async-io, async-executor, async-process,
blocking and their dependencies.
2. A read-only tree: `src/read_only.rs` (new, declared in `src/lib.rs`) reads
`TILO_ATSPI_READ_ONLY`, and every AT-SPI method that acts rather than reads
calls `crate::read_only::admit()` first: `Action.DoAction`, every
`EditableText` method, the `Value.CurrentValue` setter, `Selection`'s select
and deselect methods, `Component.GrabFocus`,
`Text.SetCaretOffset`/`AddSelection`/`RemoveSelection`/`SetSelection` and
`Document.SetTextSelections` (one added line each in `src/atspi/interfaces/`).
With the variable at `1` they answer `org.freedesktop.DBus.Error.AccessDenied`
before reaching the app's action handler. Tilo's launcher sets it for every
sandboxed graphical app (`launcher::dbus_proxy::ATSPI_READ_ONLY_ENV`), so a
process on the AT-SPI bus cannot press, type into or select in an app the
person never touched. Without it the crate behaves as the release, and the
public API is unchanged, so the published SDK still builds on the crates.io
release.

Every other source file is the release's. The archive's `Cargo.lock` is omitted. Its siblings `accesskit` 0.25.0,
`accesskit_consumer` 0.39.0 and `accesskit_atspi_common` 0.20.0 are unchanged
and come from crates.io. Upstream 0.24.0 still enables `zbus/async-io`
unconditionally (checked 2026-10-22). See `vendor/PATCHES.md`, "Accessibility
and YouTube manifests".
