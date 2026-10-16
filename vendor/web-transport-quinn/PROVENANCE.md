# web-transport-quinn provenance

- Package: `web-transport-quinn` 0.12.1 (crates.io registry source, verbatim)
- Registry checksum:
  `63a239cba370789ab705c39b66d223e3b8aef85e2762046fd6b59e560f156756`
  (identical to the checksum previously recorded in the workspace `Cargo.lock`)
- Repository: `https://github.com/moq-dev/web-transport`
- Upstream revision: `bba6900a3335ceef6f0715c5c206557a72b18058`
  (`.cargo_vcs_info.json`), package path `rs/web-transport-quinn`
- License: MIT OR Apache-2.0. The published archive carries no licence text;
  `LICENSE-MIT` and `LICENSE-APACHE` are the repository root's files at the
  upstream revision, byte for byte.

Local patch (additive only, no behaviour change for existing callers):

- `src/server.rs`: `Request::from_parts(conn, settings_send, settings_recv,
  request, connect_send, connect_recv)` completes a WebTransport request whose
  HTTP/3 SETTINGS exchange and CONNECT header block were already read by a
  shared HTTP/3 dispatcher. The host serves plain HTTP/3 requests (the delivery
  `/peer/v3/*` routes) and WebTransport sessions on one QUIC endpoint; the
  dispatcher reads the first request stream to tell them apart and hands
  WebTransport CONNECTs to this crate unchanged.
- `src/settings.rs`: crate-private `Settings::from_streams` used by the above.
- `Cargo.toml` (and `Cargo.toml.orig`): `kio` 0.6 instead of 0.5.4, the
  version `moq-net` uses, so the workspace builds one kio. The crate uses none
  of the APIs 0.6 removed.

The workspace root selects this copy with its `[patch.crates-io]` entry and
excludes it from workspace membership. Keep any future source change explicit
here.
