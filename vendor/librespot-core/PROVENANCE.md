# librespot-core 0.8.0

Source: the published `librespot-core-0.8.0.crate`, upstream commit
`d36f9f1907e8cc9d68a93f8ebc6b627b1bf7267d` (librespot v0.8.0).
Archive SHA-256:
`168bbe1c416980ddd9a969ebd6b50fb6c924eb1a3ded194285fa8ec0e2b1c68b`.
The MIT `LICENSE` comes from the upstream v0.8.0 workspace because the
published core archive does not include that workspace file.

Tilo changes:

- Both manifests select quick-xml 0.41 instead of the banned 0.38 parser.
- Remove the obsolete deprecation lint expectation on `with_blob`; the locked
  cipher dependency does not emit that warning, so strict CI rejected it.
- Product-info and both attribute setters invalidate a non-Premium session
  instead of terminating the application. The dispatch task returns a
  recoverable permission error; rejected attributes cannot revive a session.
- Malformed product XML returns a parse error without replacing accepted
  attributes, rather than repeatedly warning and accepting a partial update.
- Library tests exercise the actual product-info dispatch and setters without
  network authorization. The upstream live connection integration test is not
  part of these checks.
- `Login5Manager` accepts an embedding host's approved OAuth bearer provider.
  Tilo keeps refresh, expiry and revocation in account custody; provider failure
  never falls back to a stored-credential exchange or a different client.
- Client-token requests retain `SessionConfig.client_id` on Linux as well as
  other platforms, including an explicit host OAuth registration. These changes
  are exercised through the native `tilo-spotify` dependency graph; this vendored
  dependency is excluded from workspace dev-dependency test selection.
- HTTP refusals read at most 8 KiB for at most one second and retain only an
  allowlisted error code or a fixed body classification. Unknown response text,
  headers and credentials are discarded. Status-to-error-kind mapping and
  refusal behavior remain unchanged; no alternate identity or bearer is tried.

The upstream TLS feature definitions/defaults are retained. Music explicitly
disables defaults and selects Rustls with native roots; the root workspace
patch supplies Tilo's existing fixed-path native-root loader.

The remaining source files are unmodified registry sources. Retire each local
change when upstream provides equivalent parser, session and host OAuth support.

The archive’s standalone Cargo.lock is omitted: these dependencies use Tilo’s
committed workspace lock, and upstream standalone locks describe optional TLS
backends that are outside Tilo’s allowed runtime dependency graph.
