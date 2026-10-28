# Licensing

- **Tilo shell, OS and host runtime etc.: [AGPL-3.0-only](LICENSE).**
- **The SDK for developing and distributing your own apps: [MIT](LICENSE-MIT).**
  This includes `tilo-app`, its app-facing clients and required support
  libraries. Your own standalone Windows/macOS app can be open source or
  nonfree.
- **Bundled fonts:** OFL-1.1, LPPL-1.3c, or GPL-3.0-or-later with the Font
  Exception 2.0. The
  [font manifest](crates/sdk/ui-core/assets/fonts/manifest.toml) lists the
  license for each file and links its
  [license and copyright notices](crates/sdk/ui-core/assets/fonts/LICENSES/).
- **Third-party code and data retain their upstream licenses.** This includes
  [vendored code](vendor/), [spelling dictionaries](apps/osk/data/), and
  [Unicode emoji data](crates/text/emoji/data/LICENSE-UNICODE.txt).

The MIT SDK does not grant permission to incorporate additional AGPL code into a
nonfree app. In particular, the shared `runtime-dylib` used by thin Tilo OS apps
still contains AGPL components and remains AGPL.

We do not require a CLA or additional relicensing rights. Contributions use the
license of the component being changed; contribute only material you have the
right to submit. See [CONTRIBUTING.md](CONTRIBUTING.md).
