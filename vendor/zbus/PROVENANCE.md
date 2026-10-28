# zbus provenance

- Package: `zbus` 5.16.0 (crates.io)
- Registry checksum: `eee682d202a77e4a9f3b2c2bdf48a7b28af5c08c34ddf66f98c93e5e39464285`
- Repository: https://github.com/dbus2/zbus
- Upstream revision: `1e306d38b9e80ca45c9a7eb1b3fa4b75f59ad158` (the archive's `.cargo_vcs_info.json`)
- Upstream package path: `zbus`
- License: MIT (`LICENSE`, the archive's file).
- Vendored: 2026-10-28.

Every file is the release's (its `Cargo.lock` omitted) except `src/utils.rs`,
changed by `tilo-worker-threads.patch`: one line.

## Why

With the `tokio` feature, zbus's blocking API (`zbus::blocking::*`) runs every
call on a runtime of its own, built in `utils::block_on` with tokio's default
worker count: one worker thread per CPU the host reports. Its own `expect`
message calls it "single-threaded". Anything that makes one blocking D-Bus
call (a notification, an appearance query, a secret lookup) then keeps a
thread per CPU alive for the rest of the process: 384 threads in the desktop
compositor on a 384-thread machine, each with its own allocator arena, which
put the idle compositor over its memory budget. The patch gives that runtime
one worker, as its message says; the blocking calls only wait on the bus.

Remove this vendored copy once upstream bounds the runtime (5.19.0 still
builds it with the default worker count, checked 2026-10-28).
