//! `cfg(tilo_native_runner)`: the desktop native runner is linked.
//!
//! Emitted for every non-wasm target so SDK entry points that need the runner
//! (live window, captures, `--data-dir` hosts) compile on those targets without
//! apps selecting by `target_arch` themselves.

fn main() {
    println!("cargo::rustc-check-cfg=cfg(tilo_native_runner)");
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if arch != "wasm32" {
        println!("cargo::rustc-cfg=tilo_native_runner");
    }
}
