# tilo-format-host

The Tilo format-router daemon. Runs format providers (core WebAssembly
modules built for `wasm32-unknown-unknown`) under the wasmi interpreter and
serves a Cap'n Proto RPC interface over a Unix-domain socket. Builtin codecs
(markdown, odt, docx, …) ship the same Rust source as guest crates;
third-party codecs install through the same registry mechanism.

The provider ABI lives in `crates/doc/format-provider-abi`: exported
functions, byte encodings, and the `Provider` trait plus `export_provider!`
macro guests use. See `docs/app-model/formats.md` for the publisher view.

## Why an interpreter

Providers are batch converters, not an interactive hot path. wasmi gives
deterministic fuel metering and memory caps with a small dependency
footprint and no JIT, compilation cache, or WASI surface.

## Tests

Ordinary `cargo test -p format-host` runs the host-only RPC, ABI checks, and
resource-limit coverage (hostile guests written in WAT) and reports the
cases that need the real Markdown provider as ignored. Run the complete
provider boundary explicitly:

```bash
crates/doc/format-host/tests/run-provider-integration.sh
```

The runner builds the standalone guest for `wasm32-unknown-unknown` and then
runs every integration test, including the ignored Cap'n Proto round-trip,
in-process, and registry hot-reload tests. A missing Rust target, provider
build failure, or load failure exits non-zero.

## On-disk layout

- Registry: `~/.config/tilo/extensions/format-providers.toml`.
- Socket:   `$XDG_RUNTIME_DIR/tilo/services/format-router.sock`.

## Per-call resource limits

Every call instantiates the provider afresh in its own `Store`.

- Memory: 256 MiB linear-memory ceiling, enforced by wasmi's `StoreLimits`;
  growth past it traps and surfaces as a `resource_limit` `CodecError`.
- CPU: 500 billion units of wasmi fuel. Exhaustion traps and surfaces as a
  `resource_limit` `CodecError`. Importing 1 MB of Markdown with the builtin
  provider uses about 220 billion units (roughly three minutes); wasmi runs
  on the order of a billion units per second.
- Both are adjustable through the registry's optional `[limits]` table
  (`fuel`, `max_memory_mib`).
