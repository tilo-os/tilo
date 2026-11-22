# Headless media profile tool

Build this small tool once for the machine running installer acceptance:

```sh
cargo build --release --locked --manifest-path tools/imager-profile/Cargo.toml
```

The independent Cargo workspace resolves only serde, JSON, SHA-256 and platform
file-opening dependencies. It takes the production profile model, codec and
verified writer from `install-profile-model`, without that crate's `model`
feature, so it compiles no document or GUI stack and maintains no second
profile format. Its checked-in lockfile makes the tool independently
reproducible. Cross compilation uses the normal Cargo `--target` and linker
setup; otherwise the output matches the build host architecture.

Copy the resulting `tilo-imager-profile` executable into the acceptance tools
image. The test host needs its normal C runtime, not Cargo or the repository's
GUI dependencies. Record the executable digest alongside the tools artifact.

```sh
tilo-imager-profile --target disposable-installer.img --filesystem btrfs \
  --hostname studio --full-name 'Test User' --username testuser \
  --locale en_US.UTF-8 --keyboard de --timezone Europe/Berlin
```

The target must already be a regular file with the reserved 64 KiB tail.
The tool opens it without following links, validates the open handle, writes
the production secret-free profile and verifies the complete block by reading
it back through that same handle. It cannot create a target or accept a block
device.

`cargo test --locked --manifest-path tools/imager-profile/Cargo.toml` checks
the shared model and writes a temporary image consumed by the real live-media
profile extractor. It also checks unchanged image content before the reserved
tail and rejection of undersized files, symlinks and FIFOs.
