# wayland-backend provenance

- Package: `wayland-backend` 0.3.15 (crates.io)
- Registry checksum: `2857dd20b54e916ec7253b3d6b4d5c4d7d4ca2c33c2e11c6c76a99bd8744755d`
- Repository: https://github.com/smithay/wayland-rs
- Upstream revision: `a3d7927d87799b2955bf491b51c7c2a3a82da661` (the archive's `.cargo_vcs_info.json`)
- Upstream package path: `wayland-backend`
- License: MIT; `LICENSE.txt` is the published archive's file, byte for byte.
- Vendored: 2026-09-20 (`2806eac627`)

Local changes:

- a client that disappears while the compositor callback creates an object
  (it posted a protocol error) is treated as a disconnect race instead of
  panicking the compositor (`src/rs/server_impl/common_poll.rs`;
  `TILO-PATCH.md`);
- bounded dispatch: `Backend::dispatch_single_client_bounded`,
  `dispatch_ready_clients_bounded` and `has_pending_client_dispatch` let the
  compositor dispatch at most a fixed number of requests per client per turn,
  for fairness between clients (`src/server_api.rs`, `src/rs/server_impl/`;
  the system backend falls back to ordinary dispatch);
- the in-crate tests gate their system-backend halves on the
  `client_system`/`server_system` features and cover the bounded dispatch
  (`src/test/sync.rs`), with `wayland-scanner` as a dev-dependency and the test
  protocol in `../wayland-scanner` (as in the upstream repository layout);
- the whole crate is reformatted with the workspace's rustfmt settings;
- the archive's `Cargo.lock` and `.cargo_vcs_info.json` are omitted.
