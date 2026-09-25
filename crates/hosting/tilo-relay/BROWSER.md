# Certified browser peer transport and local fixture

`tilo-relay` exposes the same auth, framing, relay client, coedit carrier and wire implementation on native and `wasm32-unknown-unknown` targets. Native daemon/filesystem/CLI dependencies and modules are target gated. Current clients prefer the `tilo-relay/1` and `tilo-relay/2` ALPNs and their Tilo challenge-signature domain. Rolling upgrades also accept the frozen `qos-relay/1` and `qos-relay/2` ALPNs with the deployed qOS challenge domain. A client retries a frozen ALPN only after TLS reports `no_application_protocol`; certificate, signature, revocation, policy and pinned-device failures never trigger fallback. The envelope and certificate encodings remain unchanged. `tilo-user-identity` keeps its native persistence API while browser builds use its actual certificate/revocation types and verification.

The browser client's task pump, timeout and wall clock use Iroh's existing cross-platform `n0-future` layer. Browser randomness enables the matching getrandom JS backends; it does not replace challenge randomness with deterministic fixtures. Browser admission must use actual current wall clock, never the incoming signed timestamp.

## Browser compile and link gate

Install the Rust wasm32 target and use a clang and archiver capable of generating and indexing WebAssembly. The gate performs a full build: a Rust-only check cannot detect a broken C archive:

```sh
rustup target add wasm32-unknown-unknown
crates/hosting/tilo-relay/scripts/check-browser.sh
```

On macOS where Apple clang lacks a wasm backend, an installed Zig can supply freestanding clang for ring:

```sh
QOS_USE_ZIG=1 ZIG_GLOBAL_CACHE_DIR=/absolute/zig-cache \
crates/hosting/tilo-relay/scripts/check-browser.sh
```

The compiler wrapper changes only Rust's `wasm32-unknown-unknown` target spelling to Zig's `wasm32-freestanding`. The archiver wrapper uses Zig's LLVM archiver because Apple ar/ranlib cannot index WASM objects. For another build entry point, set both `CC_wasm32_unknown_unknown` to `scripts/wasm-cc.sh` and `AR_wasm32_unknown_unknown` to `scripts/wasm-ar.sh` (absolute paths under this crate). Zig debug builds inject UBSan calls into C objects; the wrapper disables that implicit instrumentation to match ordinary clang because the final Rust/WASM link does not link Zig's UBSan runtime. No native compiler flags or cryptographic algorithms are changed. Verified toolchain: Zig 0.16.0 (`zig cc` and `zig ar`), Rust wasm32-unknown-unknown target. `QOS_ZIG` can select an explicit Zig executable.

## Start a real loopback fixture

```sh
cargo run --locked -j2 -p tilo-relay \
  --example browser-peer-harness -- --ready-file /absolute/ready.json
```

This creates a fresh isolated test root and certified host/native/browser-A/browser-B devices, starts a loopback HTTP/WebSocket Iroh packet relay and the qOS application relay, joins the native peer through a relay-only endpoint address, waits for the certified workspace `Ready`, and writes public metadata. It uses the same ordinary relay auth and membership path as native peers. Both browser enrollments refer to the same room and workspace; they have distinct private keys/certificates. The fixture reports the actual native target rather than a hosted target.

Read `fixture_origin` and `status_path` from the ready file. GET `/status` is public diagnostic data. GET `/enrollment/browser-a` and `/enrollment/browser-b` return test credentials in no-store response bodies. Never log those bodies or place keys in URLs. Feed the **raw JSON text** to Rust's serde_json parser: JavaScript JSON number conversion loses precision in signed nanosecond timestamps. Requests are loopback-only; the fixture permits the main frontend origin `http://127.0.0.1:9998`, rejects other origins, and carries no app-state JSON gateway. The frontend may proxy these small fixture responses; actual sync remains certified Iroh QUIC over the packet relay.

Ctrl+C shuts down and removes the ready file/temp state. Restarting this fixture creates a new identity/workspace and is **not** a reconnect test. Browser disconnect/reconnect within the process reuses the same enrolled identities. Browser actor custody must lease separate A/B slots, including when a tab is duplicated; two tabs must not silently share one actor.

## Transport-level evidence and remaining application layer

```sh
cargo test --locked -j2 -p tilo-relay --test it \
  browser_transport::certified_clients_use_only_the_injected_packet_relay -- --exact
```

The test starts real loopback packet/app relays, authenticates two enrolled clients using relay-only addresses (no direct socket address fallback), subscribes both to one workspace, and transports a retained checkpoint of the existing qOS `WorkspaceDirectory`. It checks the attributed device key and reconstructed directory. Existing certificate, revocation, coedit and offline tests continue covering native behavior.

The fixture now owns real governed Writer and Chat documents, initialized once through the shared app reservation and component seed helpers. Its shared `StatePeer` admits certified browser mutations. A `SyncReq` or changed app document causes a fresh ordered governor coverage index plus signed checkpoints using the same retained route hashes as the native shell. State messages received through self-attributed Presence are rejected; only relay-attributed Delta may carry room or app state.

GET `/status` reports actual admitted canonical record IDs and document heads, native connection state, and the latest admission/publication error. Enrollment bootstrap includes the same canonical record IDs as hints, while document bytes travel only over the certified stream. Transport Ready and envelope counts alone do not establish app convergence.

Run the fixture with an interactive terminal to test a native Writer action:

```text
writer-insert A disposable native edit
```

The command invokes the actual Writer `SpliceText` implementation exactly once, then publishes the resulting committed state through the shared snapshot coverage producer. There is no HTTP command endpoint. The process prints only a success/failure diagnostic, never enrollment secrets. Keep it running for browser disconnect/reconnect tests; restarting creates a different fixture.

A native integration test admits a real browser-signed Writer edit, bootstraps a fresh state peer to the same document heads, and rejects duplicate effects by retaining signed-op sequence state. Actual two-tab rendering and persistence are separate browser acceptance gates. Full desktop composition and remote agents remain later work; Writer/Chat fixture readiness is not a claim of full session delivery.
