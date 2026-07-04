//! `qos-install` is primarily a CLI binary; this library target only
//! exists so the crate can be tested with `cargo test --lib --tests`
//! without a "no library targets" error. All real behaviour lives in
//! `main.rs`.

/// The name of the binary as registered with Cargo. Exposed for
/// integration tests that want to invoke it via `CARGO_BIN_EXE_*`.
pub const BINARY_NAME: &str = "qos-install";
