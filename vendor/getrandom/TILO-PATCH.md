# Tilo's patch: getrandom 0.4.3

Upstream: <https://crates.io/crates/getrandom/0.4.3>. One change, in
`src/error.rs`.

A host that is not a JavaScript page builds a WebAssembly module with
getrandom's `custom` backend (`--cfg getrandom_backend="custom"`), which is
selected before the `wasm_js` feature's backend
(`src/backends.rs`). Cargo enables `wasm_js` for wasm targets in this
repository whichever backend the cfg names, so both are on at once, and
upstream's error text still names `Error::WEB_CRYPTO` — a constant only the
`wasm_js` backend defines — under a cfg that asks about the feature alone.
The module therefore failed to compile before its own code was read.

The patch adds `not(getrandom_backend = "custom")` to that arm, so the arm is
compiled exactly when the backend that defines the constant is the one
selected. Behaviour with `wasm_js` selected is unchanged (the arm is still
compiled), so a browser page builds the same crate.
