# Licensing

- **Tilo shell, OS and host runtime etc.: [AGPL-3.0-only](LICENSE).**
- **The SDK for developing and distributing your own apps: [MIT](LICENSE-MIT).**
  This includes `tilo-app`, its app-facing clients and required support
  libraries, and the lexer of the app language (`tilolang-lex`), which the
  toolkit's editor colours a program with exactly as the compiler reads it.
  Your own standalone Windows/macOS app can be open source or nonfree.
- **The client side you reach a workspace by: [MIT](LICENSE-MIT).** A program
  that acts on a person's workspaces from outside Tilo — entering one over iroh
  as one of their devices, with their key, and asking its host for records and
  actions — links the session client, the iroh transport, identity and device
  certificates, the action vocabulary and the libraries they stand on. Those are
  MIT, or Apache-2.0 where an upstream chose it; both can be embedded in a
  nonfree product. A Python or TypeScript SDK on top of them can be MIT too.
  [`tools/sdk/license-policy.toml`](tools/sdk/license-policy.toml) names the
  crates, and `tools/sdk/check_client_licenses.py` refuses an AGPL crate
  anywhere in their dependency graph. A crate that only a desktop, a relay or a
  workspace's own host needs — the desk, the live plane, the channels, the
  runtime that serves a workspace — is not part of that side and stays AGPL.
- **Bundled fonts:** OFL-1.1, LPPL-1.3c, or GPL-3.0-or-later with the Font
  Exception 2.0. The
  [font manifest](crates/sdk/ui-core/assets/fonts/manifest.toml) lists the
  license for each file and links its
  [license and copyright notices](crates/sdk/ui-core/assets/fonts/LICENSES/).
- **Third-party code and data retain their upstream licenses.** This includes
  [vendored code](vendor/), [spelling dictionaries](apps/osk/data/), and
  [Unicode emoji data](crates/text/emoji/data/LICENSE-UNICODE.txt).

The MIT SDK and the MIT client side do not grant permission to incorporate
additional AGPL code into a nonfree app. In particular, the shared
`runtime-dylib` used by thin Tilo OS apps still contains AGPL components and
remains AGPL.

We do not require a CLA or additional relicensing rights. Contributions use the
license of the component being changed; contribute only material you have the
right to submit. See [CONTRIBUTING.md](CONTRIBUTING.md).
